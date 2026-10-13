//! Map layers: the bundled world countries projected through the layer's animatable view
//! (`map` group: centre, zoom, rotation, projection) into a comp-sized frame, with ocean, land,
//! highlighted countries and borders.
//!
//! The layer's origin is the frame centre (like shape and text layers), so the Map layer's
//! Transform moves, scales and turns the drawn frame as a whole.

use effectcraft_effects::Buf;
use effectcraft_geo::{Projection, View};
use effectcraft_geom::{Mat3, vec2};
use effectcraft_keyframe::{TextDoc, Value};
use effectcraft_path::{BezPath, FillRule, StrokeStyle};
use effectcraft_project::{Layer, LayerSource, PropGroup};
use effectcraft_raster::{Image, Mask};

use crate::eval::EvalCtx;

/// The map view at the context time.
pub fn view(ctx: &EvalCtx, layer: &Layer, g: &PropGroup) -> View {
    let (w, h) = (ctx.comp.width as f64, ctx.comp.height as f64);
    let mut v = View::new(ctx.v2(layer, g, "center", [0.0, 20.0]), ctx.f(layer, g, "zoom", 0.0), Projection::from_index(ctx.e(layer, g, "projection")), w, h);
    v.rotation = ctx.f(layer, g, "rotation", 0.0);
    v
}

/// Projected points further than this from the frame are clamped: the rasteriser must never
/// see huge or non-finite coordinates (hostile centres and zooms produce them).
const MAX_COORD: f64 = 1.0e6;

fn sane(p: &[f64; 2]) -> Option<(f64, f64)> {
    (p[0].is_finite() && p[1].is_finite()).then(|| (p[0].clamp(-MAX_COORD, MAX_COORD), p[1].clamp(-MAX_COORD, MAX_COORD)))
}

fn closed_path(pts: &[[f64; 2]]) -> BezPath {
    let mut p = BezPath::new();
    let mut it = pts.iter().filter_map(sane);
    if let Some(a) = it.next() {
        p.move_to(a);
        for b in it {
            p.line_to(b);
        }
        p.close_path();
    }
    p
}

/// Composite `color` over `img` through coverage `cov` (same size).
fn paint(img: &mut Image, cov: &Mask, color: [f32; 4]) {
    if color[3] <= 0.0 {
        return;
    }
    for (px, &c) in img.data.iter_mut().zip(&cov.data) {
        let a = c.clamp(0.0, 1.0) * color[3];
        if a <= 0.0 {
            continue;
        }
        let k = 1.0 - a;
        *px = [color[0] * a + px[0] * k, color[1] * a + px[1] * k, color[2] * a + px[2] * k, a + px[3] * k];
    }
}

/// Render a Map layer into a layer-space buffer at scale `s`.
pub fn render(ctx: &EvalCtx, layer: &Layer, g: &PropGroup, s: f64) -> Buf {
    let v = view(ctx, layer, g);
    let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
    let bw = ((v.width * s).ceil() as u32).clamp(1, 16384);
    let bh = ((v.height * s).ceil() as u32).clamp(1, 16384);
    // Frame pixels → buffer pixels; layer origin at the frame centre.
    let m = Mat3::scale(vec2(s, s));
    let offset = [v.width / 2.0 * s, v.height / 2.0 * s];
    let mut img = Image::new(bw, bh);

    let ocean = ctx.color(layer, g, "ocean");
    let land = ctx.color(layer, g, "land");
    let border = ctx.color(layer, g, "border");
    let hi_color = ctx.color(layer, g, "highlightColor");
    let border_w = ctx.f(layer, g, "borderWidth", 1.0);
    let hi_keys: Vec<String> = match ctx.group_value(layer, g, "highlight") {
        Some(Value::Str(s)) => effectcraft_geo::split_keys(&s).into_iter().map(str::to_ascii_lowercase).collect(),
        _ => Vec::new(),
    };

    // Ocean: the globe's disk, or the whole frame for flat projections.
    let ocean_path = if v.projection == Projection::Orthographic {
        let r = v.scale().min(MAX_COORD);
        effectcraft_path::ellipse([2.0 * r, 2.0 * r], [v.width / 2.0, v.height / 2.0])
    } else {
        effectcraft_path::rect([v.width, v.height], [v.width / 2.0, v.height / 2.0], 0.0)
    };
    paint(&mut img, &effectcraft_path::fill_coverage(&[ocean_path], &m, bw, bh, FillRule::NonZero), ocean);

    let mut land_paths = Vec::new();
    let mut hi_paths = Vec::new();
    for f in effectcraft_geo::world_at(v.detail()) {
        let hi = !hi_keys.is_empty() && hi_keys.iter().any(|k| f.iso.eq_ignore_ascii_case(k) || f.name.eq_ignore_ascii_case(k));
        for poly in &f.geometry.polygons {
            for ring in poly {
                if !effectcraft_geo::path_bounds(ring).is_some_and(|b| v.may_show(b)) {
                    continue;
                }
                if let Some(pts) = v.project_path(ring, true) {
                    let p = closed_path(&pts);
                    if hi {
                        hi_paths.push(p.clone());
                    }
                    land_paths.push(p);
                }
            }
        }
    }
    // Holes and outer rings together under even-odd (rings don't overlap across countries).
    paint(&mut img, &effectcraft_path::fill_coverage(&land_paths, &m, bw, bh, FillRule::EvenOdd), land);
    if !hi_paths.is_empty() {
        paint(&mut img, &effectcraft_path::fill_coverage(&hi_paths, &m, bw, bh, FillRule::EvenOdd), hi_color);
    }
    if border_w > 0.0 && border_w.is_finite() && border[3] > 0.0 && !land_paths.is_empty() {
        let style = StrokeStyle { width: border_w.min(1000.0), ..StrokeStyle::default() };
        paint(&mut img, &effectcraft_path::stroke_coverage(&land_paths, &style, &m, bw, bh), border);
    }
    Buf { img, offset, scale: s }
}

// ---------------------------------------------------------------------------------------------
// Overlays: routes and pins follow a Map layer's view.

/// The Map layer an overlay follows: its `map` property, else the comp's first Map layer.
fn followed<'a>(ctx: &EvalCtx<'a>, layer: &Layer, g: &PropGroup) -> Option<(&'a Layer, &'a PropGroup)> {
    let chosen = match ctx.group_value(layer, g, "map") {
        Some(Value::Layer(Some(id))) => ctx.layer(effectcraft_project::LayerId(id)),
        _ => None,
    };
    let l = chosen.filter(|l| matches!(l.source, LayerSource::Map)).or_else(|| ctx.comp.layers.iter().find(|l| matches!(l.source, LayerSource::Map)))?;
    Some((l, l.props.sub("map")?))
}

/// The view an overlay draws through; without a Map layer, a whole-world view.
fn overlay_view(ctx: &EvalCtx, layer: &Layer, g: &PropGroup) -> View {
    match followed(ctx, layer, g) {
        Some((ml, mg)) => view(ctx, ml, mg),
        None => View::new([0.0, 20.0], 0.0, Projection::Equirectangular, ctx.comp.width as f64, ctx.comp.height as f64),
    }
}

/// Overlay drawing, recorded first so the buffer covers only what is drawn (a pin is a few
/// hundred pixels, not a whole frame).
#[derive(Default)]
struct Canvas {
    ops: Vec<(Vec<BezPath>, Option<StrokeStyle>, [f32; 4])>,
}

impl Canvas {
    fn fill(&mut self, paths: Vec<BezPath>, color: [f32; 4]) {
        if color[3] > 0.0 && !paths.is_empty() {
            self.ops.push((paths, None, color));
        }
    }

    fn stroke(&mut self, paths: Vec<BezPath>, style: StrokeStyle, color: [f32; 4]) {
        if color[3] > 0.0 && style.width > 0.0 && !paths.is_empty() {
            self.ops.push((paths, Some(style), color));
        }
    }

    /// Paint the ops into a buffer over their bounds (clipped to the frame) at scale `s`.
    fn finish(self, v: &View, s: f64) -> Buf {
        use kurbo::Shape;
        let s = if s.is_finite() && s > 0.0 { s } else { 1.0 };
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (paths, st, _) in &self.ops {
            let pad = st.as_ref().map_or(0.0, |st| st.width) + 2.0;
            for p in paths {
                let b = p.bounding_box();
                (x0, y0, x1, y1) = (x0.min(b.x0 - pad), y0.min(b.y0 - pad), x1.max(b.x1 + pad), y1.max(b.y1 + pad));
            }
        }
        let (x0, y0) = (x0.max(0.0).floor(), y0.max(0.0).floor());
        let (x1, y1) = (x1.min(v.width).ceil(), y1.min(v.height).ceil());
        if !(x1 > x0 && y1 > y0) {
            return Buf { img: Image::new(1, 1), offset: [0.0, 0.0], scale: s };
        }
        let bw = (((x1 - x0) * s).ceil() as u32).clamp(1, 16384);
        let bh = (((y1 - y0) * s).ceil() as u32).clamp(1, 16384);
        let m = Mat3::scale(vec2(s, s)) * Mat3::translate(vec2(-x0, -y0));
        let mut img = Image::new(bw, bh);
        for (paths, st, color) in &self.ops {
            let cov = match st {
                Some(st) => effectcraft_path::stroke_coverage(paths, st, &m, bw, bh),
                None => effectcraft_path::fill_coverage(paths, &m, bw, bh, FillRule::NonZero),
            };
            paint(&mut img, &cov, *color);
        }
        // The layer origin is the frame centre.
        Buf { img, offset: [(v.width / 2.0 - x0) * s, (v.height / 2.0 - y0) * s], scale: s }
    }
}

fn str_value(ctx: &EvalCtx, layer: &Layer, g: &PropGroup, m: &str) -> String {
    match ctx.group_value(layer, g, m) {
        Some(Value::Str(s)) => s,
        _ => String::new(),
    }
}

/// Places of a route: `;`-separated (or one per line); unknown names are skipped.
pub fn route_points(text: &str) -> Vec<effectcraft_geo::LonLat> {
    text.split([';', '\n']).filter_map(|k| effectcraft_geo::lookup(k).map(|h| h.at())).take(MAX_ROUTE_PLACES).collect()
}

const MAX_ROUTE_PLACES: usize = 256;

/// Frame-pixel polyline of a route through `v`.
fn route_polyline(v: &View, pts: &[effectcraft_geo::LonLat], mode: u32, arc: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    match mode {
        // Straight on screen.
        1 => out.extend(pts.iter().filter_map(|p| sane(&v.project(*p).xy))),
        // Arcs bulging up the screen, by `arc` % of each leg's length.
        2 => {
            for w in pts.windows(2) {
                let (Some(a), Some(b)) = (sane(&v.project(w[0]).xy), sane(&v.project(w[1]).xy)) else { continue };
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = dx.hypot(dy);
                // The perpendicular pointing up the screen.
                let (mut nx, mut ny) = if len > 1e-9 { (-dy / len, dx / len) } else { (0.0, -1.0) };
                if ny > 0.0 {
                    (nx, ny) = (-nx, -ny);
                }
                let k = arc.clamp(-100.0, 100.0) / 100.0 * len;
                let c = ((a.0 + b.0) / 2.0 + nx * k, (a.1 + b.1) / 2.0 + ny * k);
                let steps = 48;
                for i in 0..=steps {
                    if i == 0 && !out.is_empty() {
                        continue;
                    }
                    let t = i as f64 / steps as f64;
                    let u = 1.0 - t;
                    out.push((u * u * a.0 + 2.0 * u * t * c.0 + t * t * b.0, u * u * a.1 + 2.0 * u * t * c.1 + t * t * b.1));
                }
            }
        }
        // Great circles: densified so they curve, projected as one path (no tears).
        _ => {
            let mut dense: Vec<effectcraft_geo::LonLat> = Vec::new();
            for w in pts.windows(2) {
                let steps = (effectcraft_geo::great_circle_deg(w[0], w[1]) / 0.5).ceil().clamp(1.0, 720.0) as usize;
                for i in 0..steps {
                    dense.push(effectcraft_geo::slerp(w[0], w[1], i as f64 / steps as f64));
                }
            }
            if let Some(last) = pts.last() {
                dense.push(*last);
            }
            // Unwrap longitudes so a leg across the antimeridian stays continuous.
            for i in 1..dense.len() {
                let prev = dense[i - 1][0];
                let d = dense[i][0] - prev;
                dense[i][0] = prev + (d + 180.0).rem_euclid(360.0) - 180.0;
            }
            if let Some(px) = v.project_path(&dense, false) {
                out.extend(px.iter().filter_map(sane));
            }
        }
    }
    out
}

/// The part of a polyline from fraction `a` to `b` of its length.
fn trim(pts: &[(f64, f64)], a: f64, b: f64) -> Vec<(f64, f64)> {
    let seg = |i: usize| (pts[i + 1].0 - pts[i].0).hypot(pts[i + 1].1 - pts[i].1);
    let total: f64 = (0..pts.len().saturating_sub(1)).map(seg).sum();
    if total <= 1e-9 || b <= a {
        return Vec::new();
    }
    let (da, db) = (a.clamp(0.0, 1.0) * total, b.clamp(0.0, 1.0) * total);
    let mut out = Vec::new();
    let mut acc = 0.0;
    for i in 0..pts.len().saturating_sub(1) {
        let l = seg(i);
        let (p, q) = (pts[i], pts[i + 1]);
        let at = |d: f64| {
            let t = if l > 1e-12 { ((d - acc) / l).clamp(0.0, 1.0) } else { 0.0 };
            (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t)
        };
        if acc + l >= da && acc <= db {
            if out.is_empty() {
                out.push(at(da.max(acc)));
            }
            out.push(at(db.min(acc + l)));
        }
        acc += l;
    }
    out
}

fn open_path(pts: &[(f64, f64)]) -> BezPath {
    let mut p = BezPath::new();
    if let Some(a) = pts.first() {
        p.move_to(*a);
        for b in &pts[1..] {
            p.line_to(*b);
        }
    }
    p
}

/// Render a Route layer into a layer-space buffer at scale `s`.
pub fn render_route(ctx: &EvalCtx, layer: &Layer, g: &PropGroup, s: f64) -> Buf {
    let v = overlay_view(ctx, layer, g);
    let mut cv = Canvas::default();
    let pts = route_points(&str_value(ctx, layer, g, "places"));
    let line = route_polyline(&v, &pts, ctx.e(layer, g, "path"), ctx.f(layer, g, "arcHeight", 25.0));
    let mut line = trim(&line, ctx.f(layer, g, "start", 0.0) / 100.0, ctx.f(layer, g, "end", 100.0) / 100.0);
    let color = ctx.color(layer, g, "color");
    let width = ctx.f(layer, g, "width", 5.0);
    let width = if width.is_finite() { width.clamp(0.0, 1000.0) } else { 0.0 };
    // Arrowhead at the drawn end, along the last stretch of the line.
    let arrow = ctx.f(layer, g, "arrowSize", 22.0);
    let arrow = if ctx.b(layer, g, "arrowhead") && arrow.is_finite() { arrow.clamp(0.0, 2000.0) } else { 0.0 };
    let mut head = None;
    if arrow > 0.0 && line.len() >= 2 {
        let tip = line[line.len() - 1];
        // Direction from a point about one arrow length back.
        let back = line.iter().rev().find(|p| (p.0 - tip.0).hypot(p.1 - tip.1) >= arrow * 0.5).or(line.first()).copied().unwrap_or(tip);
        let (dx, dy) = (tip.0 - back.0, tip.1 - back.1);
        let len = dx.hypot(dy);
        if len > 1e-6 {
            let (ux, uy) = (dx / len, dy / len);
            let base = (tip.0 - ux * arrow, tip.1 - uy * arrow);
            let half = arrow * 0.55;
            let mut p = BezPath::new();
            p.move_to(tip);
            p.line_to((base.0 - uy * half, base.1 + ux * half));
            p.line_to((base.0 + uy * half, base.1 - ux * half));
            p.close_path();
            head = Some(p);
            // Stop the line inside the head so its round cap doesn't poke past the tip.
            let keep = (arrow * 0.6) / line.iter().zip(line.iter().skip(1)).map(|(a, b)| (b.0 - a.0).hypot(b.1 - a.1)).sum::<f64>().max(1e-9);
            line = trim(&line, 0.0, 1.0 - keep);
        }
    }
    if width > 0.0 && line.len() >= 2 {
        let dash = ctx.f(layer, g, "dash", 14.0);
        let dash = (ctx.b(layer, g, "dashed") && dash.is_finite() && dash >= 1.0).then(|| (vec![dash.min(5000.0), (dash * 0.8 + width).min(5000.0)], 0.0));
        let cap = if dash.is_some() { effectcraft_path::Cap::Butt } else { effectcraft_path::Cap::Round };
        let style = StrokeStyle { width, cap, join: effectcraft_path::Join::Round, dash, ..StrokeStyle::default() };
        cv.stroke(vec![open_path(&line)], style, color);
    }
    if let Some(h) = head {
        cv.fill(vec![h], color);
    }
    cv.finish(&v, s)
}

/// Render a Pin layer into a layer-space buffer at scale `s`.
pub fn render_pin(ctx: &EvalCtx, layer: &Layer, g: &PropGroup, s: f64) -> Buf {
    let v = overlay_view(ctx, layer, g);
    let mut cv = Canvas::default();
    let Some(hit) = effectcraft_geo::lookup(&str_value(ctx, layer, g, "place")) else { return cv.finish(&v, s) };
    let pr = v.project(hit.at());
    let Some(at) = sane(&pr.xy).filter(|_| pr.visible) else { return cv.finish(&v, s) };
    let reveal = (ctx.f(layer, g, "reveal", 100.0) / 100.0).clamp(0.0, 1.0);
    if reveal <= 0.0 {
        return cv.finish(&v, s);
    }
    // Grow in with a little overshoot.
    let grow = {
        let t = reveal - 1.0;
        1.0 + 2.2 * t * t * t + 1.2 * t * t
    };
    let size = ctx.f(layer, g, "size", 18.0);
    let size = if size.is_finite() { size.clamp(0.0, 5000.0) * grow.max(0.0) } else { 0.0 };
    let color = ctx.color(layer, g, "color");
    let outline = ctx.color(layer, g, "outline");
    let r = size / 2.0;
    let ring = StrokeStyle { width: (size * 0.14).max(1.5), ..StrokeStyle::default() };
    if r > 0.0 {
        match ctx.e(layer, g, "marker") {
            // Pin: a round head above a point at the place.
            1 => {
                let head = (at.0, at.1 - size * 1.1);
                let mut p = effectcraft_path::ellipse([size, size], [head.0, head.1]);
                p.move_to((head.0 - r * 0.7, head.1 + r * 0.7));
                p.line_to(at);
                p.line_to((head.0 + r * 0.7, head.1 + r * 0.7));
                p.close_path();
                cv.stroke(vec![p.clone()], ring.clone(), outline);
                cv.fill(vec![p], color);
                let dot = effectcraft_path::ellipse([size * 0.38, size * 0.38], [head.0, head.1]);
                cv.fill(vec![dot], outline);
            }
            // Ring.
            2 => {
                let p = effectcraft_path::ellipse([size, size], [at.0, at.1]);
                let st = StrokeStyle { width: (size * 0.18).max(1.5), ..StrokeStyle::default() };
                cv.stroke(vec![p], st, color);
            }
            // None.
            3 => {}
            // Dot with an outline.
            _ => {
                let p = effectcraft_path::ellipse([size, size], [at.0, at.1]);
                cv.stroke(vec![p.clone()], ring.clone(), outline);
                cv.fill(vec![p], color);
            }
        }
    }
    let label = str_value(ctx, layer, g, "label");
    let label_size = ctx.f(layer, g, "labelSize", 36.0);
    if !label.trim().is_empty() && label_size.is_finite() && label_size > 0.0 {
        let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * reveal as f32];
        let size = label_size.min(2000.0);
        let doc = TextDoc { text: label.chars().take(500).collect(), size, style: "Bold".into(), ..TextDoc::default() };
        let lay = effectcraft_text::layout_doc(&doc);
        let off = ctx.v2(layer, g, "labelOffset", [18.0, 0.0]);
        let off = if off.iter().all(|v| v.is_finite()) { off } else { [18.0, 0.0] };
        // Vertically centred on the marker: the baseline sits about a third of the size below.
        // A label that would run off the frame's side goes on the marker's other side.
        let text_w = (lay.bounds[2] - lay.bounds[0]).max(0.0);
        let mut x = at.0 + off[0] - lay.bounds[0];
        if off[0] > 0.0 && x + text_w > v.width - 8.0 && at.0 - off[0] - text_w >= 8.0 {
            x = at.0 - off[0] - text_w - lay.bounds[0];
        } else if off[0] < 0.0 && x < 8.0 && at.0 - off[0] + text_w <= v.width - 8.0 {
            x = at.0 - off[0] - lay.bounds[0];
        }
        let t = Mat3::translate(vec2(x.clamp(-MAX_COORD, MAX_COORD), (at.1 + off[1] + size * 0.36).clamp(-MAX_COORD, MAX_COORD)));
        let glyphs: Vec<BezPath> = lay
            .glyphs
            .iter()
            .flat_map(|gl| effectcraft_path::transform(std::slice::from_ref(&gl.path), &Mat3::translate(vec2(gl.origin.x, gl.origin.y))))
            .collect();
        let glyphs = effectcraft_path::transform(&glyphs, &t);
        let halo_w = ctx.f(layer, g, "haloWidth", 6.0);
        if halo_w.is_finite() && halo_w > 0.0 {
            let st = StrokeStyle { width: halo_w.min(500.0) * 2.0, join: effectcraft_path::Join::Round, ..StrokeStyle::default() };
            cv.stroke(glyphs.clone(), st, fade(ctx.color(layer, g, "halo")));
        }
        cv.fill(glyphs, fade(ctx.color(layer, g, "labelColor")));
    }
    cv.finish(&v, s)
}
