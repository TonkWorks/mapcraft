//! Map layers: Layer ▸ New ▸ Map, setting a map's view and look (`map.set`), and baking a
//! smooth fly-to camera move into keyframes (`map.flyTo`).
//!
//! Places are the bundled countries (`effectcraft_geo::country`: ISO 3166-1 alpha-3 code or
//! name); a place sets the centre to the country's bounds centre and the zoom to fit them.

use effectcraft_geo::fly;
use effectcraft_keyframe::{Keyframe, Value as KV};
use effectcraft_project::{Layer, LayerSource, build};
use effectcraft_time::Tick;
use serde_json::{Value, json};

use super::layer::{color_p, insert_layer};
use super::{CommandSpec, bad, comp_id, f_p, has_comp, has_layers, layer_mut, layer_p, str_p};
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

/// A place (`place`: country code or name) or an explicit `center` [lon, lat] (+ `zoom`).
fn target(p: &Value, w: f64, h: f64, cmd: &str) -> Result<Option<([f64; 2], Option<f64>)>> {
    if let Some(name) = str_p(p, "place") {
        let f = effectcraft_geo::country(name).ok_or_else(|| bad(cmd, format!("unknown place `{name}` (use a country name or ISO 3166-1 alpha-3 code)")))?;
        let b = f.geometry.bounds().ok_or_else(|| bad(cmd, format!("`{name}` has no geometry")))?;
        let (c, z) = fit(b, w, h);
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

pub(crate) fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "layer.newMap",
            "Map",
            ["Layer", "New"],
            None,
            "{layer?, place? (country name or ISO alpha-3), center? [lon, lat], zoom?, rotation?, projection?: equirectangular|mercator|globe, highlight? [countries] | \"UKR, POL\", ocean?, land?, border?, highlightColor?, borderWidth?, time?}",
            has_comp,
            new_map
        ),
        cmd!(
            "map.set",
            "Map Settings",
            [],
            None,
            "{layer?, place? (country name or ISO alpha-3), center? [lon, lat], zoom?, rotation?, projection?: equirectangular|mercator|globe, highlight? [countries] | \"UKR, POL\", ocean?, land?, border?, highlightColor?, borderWidth?, time?}",
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
        let jp = effectcraft_geo::country("JPN").unwrap().geometry.bounds().unwrap();
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
}
