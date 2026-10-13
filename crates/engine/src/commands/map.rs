//! Map layers: Layer ▸ New ▸ Map, setting a map's view and look (`map.set`), and baking a
//! smooth fly-to camera move into keyframes (`map.flyTo`).
//!
//! Places resolve through `effectcraft_geo::lookup` (countries, the bundled populated places,
//! coordinates); a country sets the centre and zoom to frame its main landmass.

use effectcraft_geo::fly;
use effectcraft_keyframe::{Keyframe, Value as KV};
use effectcraft_project::{Layer, LayerSource, build};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::layer::{color_p, insert_layer};
use super::{CommandSpec, b_p, bad, comp_id, f_p, has_comp, has_layers, layer_mut, layer_p, resolve_layer, str_p};
use crate::{EngineError, Result, Session, cmd};

/// Fraction of the frame a fitted place fills.
const FIT: f64 = 0.8;

/// Centre and zoom that frame a longitude/latitude box in a `w`×`h` frame.
fn fit(b: [f64; 4], w: f64, h: f64) -> ([f64; 2], f64) {
    let center = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
    let dl = (b[2] - b[0]).abs().max(0.01);
    let dp = (b[3] - b[1]).abs().max(0.01);
    let aspect = if w > 0.0 && h > 0.0 { h / w } else { 9.0 / 16.0 };
    let z = (FIT * 360.0 / dl).log2().min((FIT * 360.0 * aspect / dp).log2());
    (center, z.clamp(effectcraft_geo::MIN_ZOOM, 12.0))
}

/// Zoom for a city or a coordinate pair when none is given: a region around it.
const POINT_ZOOM: f64 = 6.0;

/// A place (`place`: a country, a city, "City, Country" or "lat, lon"; see
/// `effectcraft_geo::lookup`) or an explicit `center` [lon, lat] (+ `zoom`). A country is framed
/// by its main landmass.
fn target(p: &Value, w: f64, h: f64, cmd: &str) -> Result<Option<([f64; 2], Option<f64>)>> {
    if let Some(name) = str_p(p, "place") {
        let hit = effectcraft_geo::lookup(name)
            .ok_or_else(|| bad(cmd, format!("unknown place `{name}` (use a country, a city, \"City, Country\" or \"lat, lon\")")))?;
        let (c, z) = match hit.bounds() {
            Some(b) => fit(b, w, h),
            None => (hit.at(), POINT_ZOOM),
        };
        return Ok(Some((c, Some(f_p(p, "zoom").unwrap_or(z)))));
    }
    match p.get("center").and_then(Value::as_array) {
        Some(a) => {
            let lon = a.first().and_then(Value::as_f64).ok_or_else(|| bad(cmd, "center is [longitude, latitude]"))?;
            let lat = a.get(1).and_then(Value::as_f64).ok_or_else(|| bad(cmd, "center is [longitude, latitude]"))?;
            if !lon.is_finite() || !lat.is_finite() {
                return Err(bad(cmd, "center must be finite"));
            }
            Ok(Some(([lon, lat.clamp(-90.0, 90.0)], f_p(p, "zoom"))))
        }
        None => Ok(None),
    }
}

fn set(l: &mut Layer, m: &str, t: Tick, v: KV) -> bool {
    let lt = l.layer_time(t);
    match l.props.prop_mut(&format!("map/{m}")) {
        Some(pr) => {
            pr.set_value_at(lt, v);
            true
        }
        None => false,
    }
}

/// Apply `map.set` parameters to a Map layer at comp time `t`; the number of properties set.
fn apply(l: &mut Layer, p: &Value, t: Tick, w: f64, h: f64, cmd: &str) -> Result<usize> {
    let mut n = 0;
    if let Some((c, z)) = target(p, w, h, cmd)? {
        n += set(l, "center", t, KV::Vec2(c)) as usize;
        if let Some(z) = z {
            n += set(l, "zoom", t, KV::Scalar(z.clamp(effectcraft_geo::MIN_ZOOM, effectcraft_geo::MAX_ZOOM))) as usize;
        }
    } else if let Some(z) = f_p(p, "zoom").filter(|z| z.is_finite()) {
        n += set(l, "zoom", t, KV::Scalar(z.clamp(effectcraft_geo::MIN_ZOOM, effectcraft_geo::MAX_ZOOM))) as usize;
    }
    if let Some(r) = f_p(p, "rotation").filter(|r| r.is_finite()) {
        n += set(l, "rotation", t, KV::Scalar(r)) as usize;
    }
    if let Some(s) = str_p(p, "projection") {
        let pr = effectcraft_geo::Projection::parse(s).ok_or_else(|| bad(cmd, format!("unknown projection `{s}` (equirectangular, mercator, globe)")))?;
        n += set(l, "projection", t, KV::Enum(pr.index())) as usize;
    }
    if let Some(v) = p.get("highlight") {
        let list = match v {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
            _ => return Err(bad(cmd, "highlight is a list of country codes or names")),
        };
        if let Some(bad_key) = effectcraft_geo::split_keys(&list).into_iter().find(|k| effectcraft_geo::country(k).is_none()) {
            return Err(bad(cmd, format!("unknown country `{bad_key}`")));
        }
        n += set(l, "highlight", t, KV::Str(list)) as usize;
    }
    for (k, m) in [("ocean", "ocean"), ("land", "land"), ("border", "border"), ("highlightColor", "highlightColor")] {
        if let Some(c) = color_p(p, k) {
            n += set(l, m, t, KV::Color([c[0] as f64, c[1] as f64, c[2] as f64, 1.0])) as usize;
        }
    }
    if let Some(bw) = f_p(p, "borderWidth").filter(|v| v.is_finite()) {
        n += set(l, "borderWidth", t, KV::Scalar(bw.clamp(0.0, 100.0))) as usize;
    }
    Ok(n)
}

fn new_map(s: &mut Session, p: &Value) -> Result<Value> {
    let cid = comp_id(s, p)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?.clone();
    let name = str_p(p, "name").unwrap_or("Map").to_string();
    let t = super::time_p(s, p, Some(&comp));
    let (w, h) = (comp.width as f64, comp.height as f64);
    let id = s.edit("New Map", None, |proj, st| {
        let mut l = build::layer(proj, &comp, &name, LayerSource::Map, (comp.width, comp.height), None);
        apply(&mut l, p, t, w, h, "layer.newMap")?;
        insert_layer(proj, st, cid, l)
    })?;
    Ok(json!({"layer": id.0}))
}

fn map_layer<'a>(l: &'a mut Layer, cmd: &str) -> Result<&'a mut Layer> {
    if matches!(l.source, LayerSource::Map) { Ok(l) } else { Err(bad(cmd, format!("`{}` is not a map layer", l.name))) }
}

fn map_set(s: &mut Session, p: &Value) -> Result<Value> {
    let (cid, lid) = layer_p(s, p, "map.set")?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let (w, h) = (comp.width as f64, comp.height as f64);
    let t = super::time_p(s, p, Some(comp));
    let n = s.edit("Map Settings", None, |proj, _| apply(map_layer(layer_mut(proj, cid, lid)?, "map.set")?, p, t, w, h, "map.set"))?;
    Ok(json!({"layer": lid.0, "set": n}))
}

/// Keys per second of a baked fly-to (one per frame up to 60 fps).
const MAX_KEYS_PER_SECOND: f64 = 60.0;

fn fly_to(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "map.flyTo";
    let (cid, lid) = layer_p(s, p, C)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?.clone();
    let (w, h) = (comp.width as f64, comp.height as f64);
    let start = super::time_p(s, p, Some(&comp));
    let Some((to_c, to_z)) = target(p, w, h, C)? else { return Err(bad(C, "give a place or a center")) };
    let layer = comp.layer(lid).ok_or_else(|| bad(C, "no such layer"))?;
    let g = layer.props.sub("map").ok_or_else(|| bad(C, format!("`{}` is not a map layer", layer.name)))?;
    let lt0 = layer.layer_time(start);
    let from_c = g.get("center").map(|pr| pr.value_at(lt0).as_vec2()).unwrap_or([0.0, 20.0]);
    let from_z = g.get("zoom").map(|pr| pr.value_at(lt0).as_f64()).unwrap_or(0.0);
    let a = fly::Stop { center: from_c, zoom: from_z };
    let b = fly::Stop { center: to_c, zoom: to_z.unwrap_or(from_z) };
    let path = fly::Path::new(a, b);
    // Natural duration ∝ path length (about 1.2 s per unit, at least 1 s), unless given.
    let dur = f_p(p, "duration").filter(|d| d.is_finite() && *d > 0.0).unwrap_or((path.length * 1.2).clamp(1.0, 20.0)).min(600.0);
    let fps = comp.frame_rate.as_f64().clamp(1.0, MAX_KEYS_PER_SECOND);
    let n = ((dur * fps).round() as usize).clamp(1, 36_000);
    let end = Tick::from_seconds_f64(start.seconds() + dur);
    s.edit("Fly To", None, |proj, _| {
        let l = map_layer(layer_mut(proj, cid, lid)?, C)?;
        let (l0, l1) = (l.layer_time(start), l.layer_time(end));
        for (m, is_center) in [("center", true), ("zoom", false)] {
            let Some(pr) = l.props.prop_mut(&format!("map/{m}")) else { continue };
            // The move replaces whatever keys were inside it.
            pr.keys.retain(|k| k.time < l0 || k.time > l1);
            for i in 0..=n {
                let u = i as f64 / n as f64;
                let st = path.at(u);
                let kt = Tick::from_seconds_f64(l0.seconds() + u * (l1.seconds() - l0.seconds()));
                let v = if is_center { KV::Vec2(st.center) } else { KV::Scalar(st.zoom) };
                effectcraft_keyframe::set_key(&mut pr.keys, Keyframe::new(kt, v));
            }
        }
        Ok(())
    })?;
    Ok(json!({"layer": lid.0, "start": start.seconds(), "duration": dur, "keys": n + 1, "to": {"center": b.center, "zoom": b.zoom}}))
}

// ---------------------------------------------------------------- routes and pins

/// Set `group/m` of an overlay layer at comp time `t`.
fn set_in(l: &mut Layer, group: &str, m: &str, t: Tick, v: KV) -> usize {
    let lt = l.layer_time(t);
    match l.props.prop_mut(&format!("{group}/{m}")) {
        Some(pr) => {
            pr.set_value_at(lt, v);
            1
        }
        None => 0,
    }
}

/// Key `group/m` from `a` to `b` over `[t, t + dur]` (Easy Ease), replacing keys inside.
fn animate(l: &mut Layer, group: &str, m: &str, t: Tick, dur: f64, a: f64, b: f64) {
    let (l0, l1) = (l.layer_time(t), l.layer_time(Tick::from_seconds_f64(t.seconds() + dur)));
    let Some(pr) = l.props.prop_mut(&format!("{group}/{m}")) else { return };
    pr.keys.retain(|k| k.time < l0 || k.time > l1);
    effectcraft_keyframe::set_key(&mut pr.keys, Keyframe::new(l0, KV::Scalar(a)).eased());
    effectcraft_keyframe::set_key(&mut pr.keys, Keyframe::new(l1, KV::Scalar(b)).eased());
}

/// A positive, finite duration in seconds (at most ten minutes).
fn seconds(p: &Value, k: &str) -> Option<f64> {
    f_p(p, k).filter(|d| d.is_finite() && *d > 0.0).map(|d| d.min(600.0))
}

/// `places` as a list (`["Paris", "Berlin"]`) or text (`"Paris; Berlin"`); every place must resolve.
fn places_p(p: &Value, cmd: &str) -> Result<Option<String>> {
    let list: Vec<String> = match p.get("places") {
        None => return Ok(None),
        Some(Value::String(s)) => s.split([';', '\n']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => Ok(s.trim().to_string()),
                // [lon, lat] pairs, as everywhere else in the API.
                Value::Array(c) => match (c.first().and_then(Value::as_f64), c.get(1).and_then(Value::as_f64)) {
                    (Some(lon), Some(lat)) => Ok(format!("{lat}, {lon}")),
                    _ => Err(bad(cmd, "a place is a name or [lon, lat]")),
                },
                _ => Err(bad(cmd, "a place is a name or [lon, lat]")),
            })
            .collect::<Result<_>>()?,
        Some(_) => return Err(bad(cmd, "places is a list of places")),
    };
    if list.len() < 2 {
        return Err(bad(cmd, "a route needs at least two places"));
    }
    if let Some(u) = list.iter().find(|k| effectcraft_geo::lookup(k).is_none()) {
        return Err(bad(cmd, format!("unknown place `{u}`")));
    }
    Ok(Some(list.join("; ")))
}

/// Apply `route.set` parameters; the number of properties set.
fn apply_route(l: &mut Layer, p: &Value, t: Tick, cmd: &str) -> Result<usize> {
    const G: &str = "route";
    let mut n = 0;
    if let Some(list) = places_p(p, cmd)? {
        n += set_in(l, G, "places", t, KV::Str(list));
    }
    if let Some(m) = str_p(p, "path") {
        let i = match m.to_ascii_lowercase().as_str() {
            "greatcircle" | "great circle" | "geodesic" => 0,
            "straight" | "line" => 1,
            "arc" => 2,
            _ => return Err(bad(cmd, format!("unknown path `{m}` (greatCircle, straight, arc)"))),
        };
        n += set_in(l, G, "path", t, KV::Enum(i));
    }
    for (k, lo, hi) in
        [("arcHeight", -100.0, 100.0), ("start", 0.0, 100.0), ("end", 0.0, 100.0), ("width", 0.0, 200.0), ("dash", 1.0, 500.0), ("arrowSize", 0.0, 500.0)]
    {
        if let Some(v) = f_p(p, k).filter(|v| v.is_finite()) {
            n += set_in(l, G, k, t, KV::Scalar(v.clamp(lo, hi)));
        }
    }
    for k in ["dashed", "arrowhead"] {
        if let Some(b) = b_p(p, k) {
            n += set_in(l, G, k, t, KV::Bool(b));
        }
    }
    if let Some(c) = color_p(p, "color") {
        n += set_in(l, G, "color", t, KV::Color([c[0] as f64, c[1] as f64, c[2] as f64, 1.0]));
    }
    if let Some(d) = seconds(p, "drawOn") {
        animate(l, G, "end", t, d, 0.0, 100.0);
        n += 1;
    }
    Ok(n)
}

/// Apply `pin.set` parameters; the number of properties set.
fn apply_pin(l: &mut Layer, p: &Value, t: Tick, cmd: &str) -> Result<usize> {
    const G: &str = "pin";
    let mut n = 0;
    if let Some(place) = str_p(p, "place") {
        let hit = effectcraft_geo::lookup(place).ok_or_else(|| bad(cmd, format!("unknown place `{place}`")))?;
        n += set_in(l, G, "place", t, KV::Str(place.trim().to_string()));
        // The label defaults to the place's own name ("Kyiv" for "kyiv").
        if p.get("label").is_none() && matches!(hit, effectcraft_geo::Hit::City(_) | effectcraft_geo::Hit::Country(_)) {
            n += set_in(l, G, "label", t, KV::Str(hit.name()));
        }
    }
    if let Some(label) = str_p(p, "label") {
        n += set_in(l, G, "label", t, KV::Str(label.chars().take(500).collect()));
    }
    if let Some(m) = str_p(p, "marker") {
        let i = build::PIN_MARKERS
            .iter()
            .position(|x| x.eq_ignore_ascii_case(m))
            .ok_or_else(|| bad(cmd, format!("unknown marker `{m}` (dot, pin, ring, none)")))?;
        n += set_in(l, G, "marker", t, KV::Enum(i as u32));
    }
    for (k, lo, hi) in [("size", 0.0, 1000.0), ("labelSize", 0.0, 1000.0), ("haloWidth", 0.0, 100.0), ("reveal", 0.0, 100.0)] {
        if let Some(v) = f_p(p, k).filter(|v| v.is_finite()) {
            n += set_in(l, G, k, t, KV::Scalar(v.clamp(lo, hi)));
        }
    }
    for k in ["color", "outline", "labelColor", "halo"] {
        if let Some(c) = color_p(p, k) {
            n += set_in(l, G, k, t, KV::Color([c[0] as f64, c[1] as f64, c[2] as f64, 1.0]));
        }
    }
    if let Some(o) = p.get("labelOffset").and_then(Value::as_array)
        && let (Some(x), Some(y)) = (o.first().and_then(Value::as_f64).filter(|v| v.is_finite()), o.get(1).and_then(Value::as_f64).filter(|v| v.is_finite()))
    {
        n += set_in(l, G, "labelOffset", t, KV::Vec2([x.clamp(-1e5, 1e5), y.clamp(-1e5, 1e5)]));
    }
    if let Some(d) = seconds(p, "popIn") {
        animate(l, G, "reveal", t, d, 0.0, 100.0);
        n += 1;
    }
    Ok(n)
}

/// A new Route or Pin layer following `map` (or the comp's topmost Map layer), parented to it
/// so it moves with the map's own Transform.
fn new_overlay(s: &mut Session, p: &Value, source: LayerSource) -> Result<Value> {
    let (cmd, group, title, default_name) = match source {
        LayerSource::Route => ("layer.newRoute", "route", "New Route", "Route"),
        _ => ("layer.newPin", "pin", "New Pin", "Pin"),
    };
    let cid = comp_id(s, p)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?.clone();
    let map = match p.get("map") {
        Some(v) => {
            let id = resolve_layer(&comp, v).ok_or_else(|| bad(cmd, format!("no layer {v}")))?;
            if !comp.layer(id).is_some_and(|l| matches!(l.source, LayerSource::Map)) {
                return Err(bad(cmd, "`map` is not a map layer"));
            }
            Some(id)
        }
        None => comp.layers.iter().find(|l| matches!(l.source, LayerSource::Map)).map(|l| l.id),
    };
    match source {
        LayerSource::Route if p.get("places").is_none() => return Err(bad(cmd, "give `places` (at least two)")),
        LayerSource::Pin if str_p(p, "place").is_none() => return Err(bad(cmd, "give a `place`")),
        _ => {}
    }
    let t = super::time_p(s, p, Some(&comp));
    let name = match (str_p(p, "name"), source == LayerSource::Pin) {
        (Some(n), _) => n.to_string(),
        (None, true) => str_p(p, "place").and_then(effectcraft_geo::lookup).map(|h| h.name()).unwrap_or_else(|| default_name.into()),
        (None, false) => default_name.to_string(),
    };
    let id = s.edit(title, None, |proj, st| {
        let mut l = build::layer(proj, &comp, &name, source.clone(), (comp.width, comp.height), None);
        match source {
            LayerSource::Route => apply_route(&mut l, p, t, cmd)?,
            _ => apply_pin(&mut l, p, t, cmd)?,
        };
        if let Some(m) = map {
            set_in(&mut l, group, "map", t, KV::Layer(Some(m.0)));
            // Parented with its origin on the map's: it follows the map's Transform.
            l.parent = Some(m);
            let lt = l.layer_time(t);
            if let Some(pos) = l.props.prop_mut("transform/position") {
                pos.set_value_at(lt, KV::Vec3([0.0; 3]));
            }
        }
        insert_layer(proj, st, cid, l)
    })?;
    Ok(json!({"layer": id.0}))
}

fn overlay_set(s: &mut Session, p: &Value, route: bool) -> Result<Value> {
    let cmd = if route { "route.set" } else { "pin.set" };
    let (cid, lid) = layer_p(s, p, cmd)?;
    let comp = s.project.comp(cid).ok_or(EngineError::NoComp)?;
    let t = super::time_p(s, p, Some(comp));
    let n = s.edit(if route { "Route Settings" } else { "Pin Settings" }, None, |proj, _| {
        let l = layer_mut(proj, cid, lid)?;
        match (&l.source, route) {
            (LayerSource::Route, true) => apply_route(l, p, t, cmd),
            (LayerSource::Pin, false) => apply_pin(l, p, t, cmd),
            _ => Err(bad(cmd, format!("`{}` is not a {} layer", l.name, if route { "route" } else { "pin" }))),
        }
    })?;
    Ok(json!({"layer": lid.0, "set": n}))
}

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "layer.newMap",
            "Map",
            ["Layer", "New"],
            None,
            "{layer?, place? (country, city, \"City, Country\" or \"lat, lon\"), center? [lon, lat], zoom?, rotation?, projection?: equirectangular|mercator|globe, highlight? [countries] | \"UKR, POL\", ocean?, land?, border?, highlightColor?, borderWidth?, time?}",
            has_comp,
            new_map
        ),
        cmd!(
            "map.set",
            "Map Settings",
            [],
            None,
            "{layer?, place? (country, city, \"City, Country\" or \"lat, lon\"), center? [lon, lat], zoom?, rotation?, projection?: equirectangular|mercator|globe, highlight? [countries] | \"UKR, POL\", ocean?, land?, border?, highlightColor?, borderWidth?, time?}",
            has_layers,
            map_set
        ),
        cmd!(
            "map.flyTo",
            "Fly To",
            [],
            None,
            "{layer?, place? | center? [lon, lat], zoom?, time? (start, seconds; default current time), duration? (seconds; default from the distance)}: bakes a smooth zoom-out/pan/zoom-in camera move into Center and Zoom keyframes",
            has_layers,
            fly_to
        ),
        cmd!(
            "layer.newRoute",
            "Route",
            ["Layer", "New"],
            None,
            "{places: [\"Paris\", \"Berlin\", [lon, lat]] | \"Paris; Berlin\", map? (layer; default the topmost map), name?, path?: greatCircle|straight|arc, arcHeight? (%), start?, end? (%), color?, width?, dashed?, dash?, arrowhead?, arrowSize?, drawOn? (seconds: keys End 0→100% from time), time?}",
            has_comp,
            |s, p| new_overlay(s, p, LayerSource::Route)
        ),
        cmd!(
            "layer.newPin",
            "Pin",
            ["Layer", "New"],
            None,
            "{place (country, city, \"City, Country\" or \"lat, lon\"), map? (layer; default the topmost map), name?, label? (default the place's name; \"\" for none), marker?: dot|pin|ring|none, size?, color?, outline?, labelSize?, labelColor?, halo?, haloWidth?, labelOffset? [x, y], reveal? (%), popIn? (seconds: keys Reveal 0→100% from time), time?}",
            has_comp,
            |s, p| new_overlay(s, p, LayerSource::Pin)
        ),
        cmd!("route.set", "Route Settings", [], None, "{layer?, …the layer.newRoute options}", has_layers, |s, p| overlay_set(s, p, true)),
        cmd!("pin.set", "Pin Settings", [], None, "{layer?, …the layer.newPin options}", has_layers, |s, p| overlay_set(s, p, false)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests_tail::comp;

    fn px(s: &Session, x: u32, y: u32, t: f64) -> [f32; 4] {
        let cid = s.active_comp_id().unwrap();
        let img = s.render(cid, Tick::from_seconds_f64(t), Default::default());
        img.data[(y * img.width + x) as usize]
    }

    fn near(a: [f32; 4], b: [f32; 3]) -> bool {
        (a[0] - b[0]).abs() < 0.08 && (a[1] - b[1]).abs() < 0.08 && (a[2] - b[2]).abs() < 0.08
    }

    #[test]
    fn new_map_frames_and_highlights_a_country() {
        let mut s = comp(320, 180);
        let r = s
            .execute(
                "layer.newMap",
                json!({"place": "Ukraine", "highlight": ["UKR"], "highlightColor": "#ff0000", "ocean": "#0000ff", "land": "#00ff00", "borderWidth": 0}),
            )
            .unwrap();
        let lid = r["layer"].as_u64().unwrap();
        // Ukraine sits in the middle of the frame, in the highlight colour.
        assert!(near(px(&s, 160, 90, 0.0), [1.0, 0.0, 0.0]), "{:?}", px(&s, 160, 90, 0.0));
        // The Black Sea is south of it.
        let sea = px(&s, 175, 160, 0.0);
        assert!(near(sea, [0.0, 0.0, 1.0]) || near(sea, [0.0, 1.0, 0.0]), "{sea:?}");
        // Undo removes it; serde keeps it.
        let back = crate::tests_tail::roundtrip(&s);
        let l = back.comp(s.active_comp_id().unwrap()).unwrap().layer(effectcraft_project::LayerId(lid)).unwrap();
        assert!(matches!(l.source, LayerSource::Map));
    }

    #[test]
    fn fly_to_bakes_keys_that_land_on_the_target() {
        let mut s = comp(320, 180);
        let lid = s.execute("layer.newMap", json!({"place": "France", "zoom": 5})).unwrap()["layer"].as_u64().unwrap();
        let r = s.execute("map.flyTo", json!({"layer": lid, "place": "Japan", "zoom": 5, "time": 0, "duration": 1.5})).unwrap();
        assert_eq!(r["keys"], json!(46));
        let l = s.active_comp().unwrap().layer(effectcraft_project::LayerId(lid)).unwrap();
        let c = l.props.prop("map/center").unwrap();
        let end = c.value_at(Tick::from_seconds_f64(1.5)).as_vec2();
        let jp = effectcraft_geo::lookup("JPN").unwrap().bounds().unwrap();
        assert!((end[0] - (jp[0] + jp[2]) / 2.0).abs() < 1e-6, "{end:?}");
        // Zoomed out mid-flight.
        let z = l.props.prop("map/zoom").unwrap();
        assert!(z.value_at(Tick::from_seconds_f64(0.75)).as_f64() < z.value_at(Tick::ZERO).as_f64() - 1.0);
    }

    #[test]
    fn bad_map_params_are_errors_not_crashes() {
        let mut s = comp(64, 64);
        assert!(s.execute("layer.newMap", json!({"place": "Atlantis"})).is_err());
        assert!(s.execute("layer.newMap", json!({"center": ["x", 1]})).is_err());
        assert!(s.execute("layer.newMap", json!({"highlight": ["Narnia"]})).is_err());
        let lid = s.execute("layer.newMap", json!({"center": [1e308, -1e308], "zoom": 1e300, "projection": "globe"})).unwrap()["layer"].as_u64().unwrap();
        let _ = px(&s, 10, 10, 0.0);
        assert!(s.execute("map.flyTo", json!({"layer": lid})).is_err());
        let _ = s.execute("map.flyTo", json!({"layer": lid, "center": [0, 0], "duration": 1e300}));
        let solid = s.execute("layer.newSolid", json!({"color": "#ffffff"})).unwrap()["layer"].as_u64().unwrap();
        assert!(s.execute("map.set", json!({"layer": solid, "zoom": 2})).is_err());
    }

    #[test]
    fn globe_draws_a_disk() {
        let mut s = comp(200, 200);
        s.execute("layer.newMap", json!({"projection": "globe", "zoom": 0, "ocean": "#0000ff"})).unwrap();
        // Globe radius = 200 / 2π ≈ 31.8 px: the corners are empty, the centre is not.
        assert!(px(&s, 2, 2, 0.0)[3] < 0.01);
        assert!(px(&s, 100, 100, 0.0)[3] > 0.99);
    }

    fn project(s: &Session, map: u64, place: &str, t: f64) -> (u32, u32) {
        let comp = s.active_comp().unwrap();
        let ml = comp.layer(effectcraft_project::LayerId(map)).unwrap();
        let g = ml.props.sub("map").unwrap();
        let lt = ml.layer_time(Tick::from_seconds_f64(t));
        let v = effectcraft_geo::View::new(
            g.get("center").unwrap().value_at(lt).as_vec2(),
            g.get("zoom").unwrap().value_at(lt).as_f64(),
            effectcraft_geo::Projection::Equirectangular,
            comp.width as f64,
            comp.height as f64,
        );
        let xy = v.project(effectcraft_geo::lookup(place).unwrap().at()).xy;
        (xy[0].round() as u32, xy[1].round() as u32)
    }

    fn plain_map(s: &mut Session) -> u64 {
        s.execute("layer.newMap", json!({"center": [10, 50], "zoom": 2.5, "ocean": "#0000ff", "land": "#00ff00", "borderWidth": 0})).unwrap()["layer"]
            .as_u64()
            .unwrap()
    }

    #[test]
    fn route_draws_between_places_and_draws_on() {
        let mut s = comp(320, 180);
        let map = plain_map(&mut s);
        let r = s
            .execute(
                "layer.newRoute",
                json!({"places": ["Paris", "Berlin"], "path": "straight", "color": "#ff0000", "width": 6, "arrowhead": false, "drawOn": 1, "time": 0}),
            )
            .unwrap();
        let rid = r["layer"].as_u64().unwrap();
        // Parented to the map, following it.
        let l = s.active_comp().unwrap().layer(effectcraft_project::LayerId(rid)).unwrap().clone();
        assert_eq!(l.parent, Some(effectcraft_project::LayerId(map)));
        let (a, b) = (project(&s, map, "Paris", 0.0), project(&s, map, "Berlin", 0.0));
        let mid = ((a.0 + b.0) / 2, (a.1 + b.1) / 2);
        assert!(!near(px(&s, mid.0, mid.1, 0.0), [1.0, 0.0, 0.0]), "not drawn yet at the start");
        assert!(near(px(&s, mid.0, mid.1, 1.5), [1.0, 0.0, 0.0]), "{:?}", px(&s, mid.0, mid.1, 1.5));
        // A route needs two known places.
        assert!(s.execute("layer.newRoute", json!({"places": ["Paris"]})).is_err());
        assert!(s.execute("layer.newRoute", json!({"places": "Paris; Atlantis"})).is_err());
        assert!(s.execute("layer.newRoute", json!({})).is_err());
        assert!(s.execute("route.set", json!({"layer": map, "width": 3})).is_err());
        assert!(s.execute("route.set", json!({"layer": rid, "path": "zigzag"})).is_err());
        assert_eq!(s.execute("route.set", json!({"layer": rid, "path": "arc", "width": 1e300, "places": [[2.35, 48.86], "Moscow"]})).unwrap()["set"], json!(3));
        let _ = px(&s, mid.0, mid.1, 1.5);
    }

    #[test]
    fn pin_marks_its_place_and_follows_the_map() {
        let mut s = comp(320, 180);
        let map = plain_map(&mut s);
        let r = s.execute("layer.newPin", json!({"place": "kyiv", "size": 16, "color": "#ff0000", "outline": "#ff0000", "popIn": 1, "time": 0})).unwrap();
        let pid = r["layer"].as_u64().unwrap();
        let l = s.active_comp().unwrap().layer(effectcraft_project::LayerId(pid)).unwrap().clone();
        // Named and labelled after the place.
        assert_eq!(l.name, "Kyiv");
        assert_eq!(l.props.prop("pin/label").unwrap().value, KV::Str("Kyiv".into()));
        let k = project(&s, map, "Kyiv", 0.0);
        assert!(!near(px(&s, k.0, k.1, 0.0), [1.0, 0.0, 0.0]), "Reveal starts at 0");
        assert!(near(px(&s, k.0, k.1, 1.5), [1.0, 0.0, 0.0]), "{:?}", px(&s, k.0, k.1, 1.5));
        // The map pans; the pin stays on Kyiv.
        s.execute("map.set", json!({"layer": map, "center": [25, 48], "time": 0})).unwrap();
        let k2 = project(&s, map, "Kyiv", 1.5);
        assert_ne!(k, k2);
        assert!(near(px(&s, k2.0, k2.1, 1.5), [1.0, 0.0, 0.0]));
        // Bad and hostile values.
        assert!(s.execute("layer.newPin", json!({"place": "Atlantis"})).is_err());
        assert!(s.execute("layer.newPin", json!({})).is_err());
        assert!(s.execute("pin.set", json!({"layer": pid, "marker": "star"})).is_err());
        s.execute("pin.set", json!({"layer": pid, "labelSize": 1e300, "size": -5, "labelOffset": [1e300, 0], "label": "x".repeat(10_000)})).unwrap();
        let _ = px(&s, k2.0, k2.1, 1.5);
    }

    #[test]
    fn overlays_without_a_map_draw_on_a_world_view() {
        let mut s = comp(160, 90);
        s.execute("layer.newPin", json!({"place": "0, 0", "marker": "dot", "size": 10, "color": "#ff0000", "outline": "#ff0000", "label": ""})).unwrap();
        // A whole-world view centred on (0°, 20°N): (0, 0) is a little below the middle.
        let v = effectcraft_geo::View::new([0.0, 20.0], 0.0, effectcraft_geo::Projection::Equirectangular, 160.0, 90.0);
        let xy = v.project([0.0, 0.0]).xy;
        assert!(near(px(&s, xy[0].round() as u32, xy[1].round() as u32, 0.0), [1.0, 0.0, 0.0]));
    }
}
