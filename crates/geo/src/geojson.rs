//! A lenient GeoJSON (RFC 7946) reader. Features keep a display name and code, their polygons
//! as rings and their lines and points; unknown members are ignored, bad coordinates skipped and
//! counted, and the input sizes are capped.

use serde_json::Value;

use crate::LonLat;

/// Inputs larger than this are refused.
pub const MAX_BYTES: usize = 256 << 20;
/// Nesting deeper than this (GeometryCollections inside GeometryCollections) is skipped.
const MAX_DEPTH: usize = 16;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    /// Polygons, each an outer ring followed by its holes (closing point dropped).
    pub polygons: Vec<Vec<Vec<LonLat>>>,
    pub lines: Vec<Vec<LonLat>>,
    pub points: Vec<LonLat>,
}

impl Geometry {
    pub fn is_empty(&self) -> bool {
        self.polygons.is_empty() && self.lines.is_empty() && self.points.is_empty()
    }
    /// Longitude/latitude bounds `[west, south, east, north]`.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        let mut b: Option<[f64; 4]> = None;
        let all = self.polygons.iter().flatten().flatten().chain(self.lines.iter().flatten()).chain(self.points.iter());
        for p in all {
            b = Some(match b {
                None => [p[0], p[1], p[0], p[1]],
                Some(b) => [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])],
            });
        }
        b
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Feature {
    /// `name` (or `NAME`, `title`) property; empty if none.
    pub name: String,
    /// `iso` (or `ISO_A3`, `iso_a3`, `id`) property; empty if none.
    pub iso: String,
    pub geometry: Geometry,
    pub properties: serde_json::Map<String, Value>,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub features: Vec<Feature>,
    /// Coordinates or geometries that were malformed and skipped.
    pub skipped: usize,
}

#[derive(Debug)]
pub enum GeoJsonError {
    TooLarge(usize),
    Json(String),
    NotGeoJson,
}

impl std::fmt::Display for GeoJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeoJsonError::TooLarge(n) => write!(f, "GeoJSON is {n} bytes; the limit is {MAX_BYTES}"),
            GeoJsonError::Json(e) => write!(f, "not valid JSON: {e}"),
            GeoJsonError::NotGeoJson => write!(f, "not GeoJSON (expected a FeatureCollection, Feature or geometry)"),
        }
    }
}

impl std::error::Error for GeoJsonError {}

/// Parse a FeatureCollection, a Feature or a bare geometry.
pub fn parse(text: &str) -> Result<Parsed, GeoJsonError> {
    if text.len() > MAX_BYTES {
        return Err(GeoJsonError::TooLarge(text.len()));
    }
    let v: Value = serde_json::from_str(text).map_err(|e| GeoJsonError::Json(e.to_string()))?;
    let mut out = Parsed::default();
    match v.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for f in v.get("features").and_then(Value::as_array).into_iter().flatten() {
                feature(f, &mut out);
            }
        }
        Some("Feature") => feature(&v, &mut out),
        Some(_) if v.get("coordinates").is_some() || v.get("geometries").is_some() => {
            let mut g = Geometry::default();
            geometry(&v, &mut g, &mut out.skipped, 0);
            out.features.push(Feature { geometry: g, ..Feature::default() });
        }
        _ => return Err(GeoJsonError::NotGeoJson),
    }
    Ok(out)
}

fn str_prop(props: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|k| match props.get(*k) {
            Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

fn feature(f: &Value, out: &mut Parsed) {
    let props = f.get("properties").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut g = Geometry::default();
    if let Some(geo) = f.get("geometry").filter(|g| !g.is_null()) {
        geometry(geo, &mut g, &mut out.skipped, 0);
    }
    let mut iso = str_prop(&props, &["iso", "ISO_A3", "iso_a3", "ADM0_A3"]);
    if iso.is_empty()
        && let Some(Value::String(id)) = f.get("id")
    {
        iso = id.clone();
    }
    out.features.push(Feature { name: str_prop(&props, &["name", "NAME", "Name", "title"]), iso, geometry: g, properties: props });
}

fn pos(v: &Value) -> Option<LonLat> {
    let a = v.as_array()?;
    let lon = a.first()?.as_f64()?;
    let lat = a.get(1)?.as_f64()?;
    (lon.is_finite() && lat.is_finite()).then_some([lon, lat.clamp(-90.0, 90.0)])
}

fn line(v: &Value, skipped: &mut usize) -> Vec<LonLat> {
    let mut pts = Vec::new();
    for p in v.as_array().into_iter().flatten() {
        match pos(p) {
            Some(p) => pts.push(p),
            None => *skipped += 1,
        }
    }
    pts
}

fn ring(v: &Value, skipped: &mut usize) -> Option<Vec<LonLat>> {
    let mut r = line(v, skipped);
    if r.len() >= 2 && r.first() == r.last() {
        r.pop();
    }
    (r.len() >= 3).then_some(r)
}

fn polygon(v: &Value, skipped: &mut usize) -> Option<Vec<Vec<LonLat>>> {
    let rings: Vec<Vec<LonLat>> = v.as_array()?.iter().filter_map(|r| ring(r, skipped)).collect();
    (!rings.is_empty()).then_some(rings)
}

fn geometry(v: &Value, g: &mut Geometry, skipped: &mut usize, depth: usize) {
    if depth > MAX_DEPTH {
        *skipped += 1;
        return;
    }
    let c = v.get("coordinates");
    match (v.get("type").and_then(Value::as_str), c) {
        (Some("Point"), Some(c)) => match pos(c) {
            Some(p) => g.points.push(p),
            None => *skipped += 1,
        },
        (Some("MultiPoint"), Some(c)) => g.points.extend(line(c, skipped)),
        (Some("LineString"), Some(c)) => {
            let l = line(c, skipped);
            if l.len() >= 2 {
                g.lines.push(l);
            }
        }
        (Some("MultiLineString"), Some(c)) => {
            for l in c.as_array().into_iter().flatten() {
                let l = line(l, skipped);
                if l.len() >= 2 {
                    g.lines.push(l);
                }
            }
        }
        (Some("Polygon"), Some(c)) => g.polygons.extend(polygon(c, skipped)),
        (Some("MultiPolygon"), Some(c)) => {
            for p in c.as_array().into_iter().flatten() {
                g.polygons.extend(polygon(p, skipped));
            }
        }
        (Some("GeometryCollection"), _) => {
            for sub in v.get("geometries").and_then(Value::as_array).into_iter().flatten() {
                geometry(sub, g, skipped, depth + 1);
            }
        }
        _ => *skipped += 1,
    }
}
