//! Map layers: the bundled world countries projected through the layer's animatable view
//! (`map` group: centre, zoom, rotation, projection) into a comp-sized frame, with ocean, land,
//! highlighted countries and borders.
//!
//! The layer's origin is the frame centre (like shape and text layers), so the Map layer's
//! Transform moves, scales and turns the drawn frame as a whole.

use effectcraft_effects::Buf;
use effectcraft_geo::{Projection, View};
use effectcraft_geom::{Mat3, vec2};
use effectcraft_keyframe::Value;
use effectcraft_path::{BezPath, FillRule, StrokeStyle};
use effectcraft_project::{Layer, PropGroup};
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
    for f in effectcraft_geo::world() {
        let hi = !hi_keys.is_empty() && hi_keys.iter().any(|k| f.iso.eq_ignore_ascii_case(k) || f.name.eq_ignore_ascii_case(k));
        for poly in &f.geometry.polygons {
            for ring in poly {
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
