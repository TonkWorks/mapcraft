//! Place lookup: turn what a creator types ("Kyiv", "Paris, France", "48.85, 2.35", "UKR") into
//! coordinates, from the bundled Natural Earth populated places and countries. Never guesses:
//! an unknown name is `None`, so callers can say so instead of drawing something in the ocean.

use std::sync::OnceLock;

use crate::{Detail, Feature, LonLat, world_at};

/// A populated place from the bundled list.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    /// ASCII spelling when it differs from `name` ("Kyiv" for "Kyiv", "Sao Paulo" for "São Paulo").
    pub ascii: String,
    /// First-level region (state, province, oblast).
    pub region: String,
    /// ISO 3166-1 alpha-3 code of the country.
    pub iso: String,
    pub country: String,
    pub at: LonLat,
    pub population: u64,
    /// National capital.
    pub capital: bool,
}

/// What a lookup found.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Coordinates(LonLat),
    City(&'static Place),
    Country(&'static Feature),
}

impl Hit {
    /// Where to put a pin or aim a camera.
    pub fn at(&self) -> LonLat {
        match self {
            Hit::Coordinates(p) => *p,
            Hit::City(p) => p.at,
            Hit::Country(f) => anchor(f).unwrap_or([0.0, 0.0]),
        }
    }

    /// A display name.
    pub fn name(&self) -> String {
        match self {
            Hit::Coordinates(p) => format!("{:.4}, {:.4}", p[1], p[0]),
            Hit::City(p) => p.name.clone(),
            Hit::Country(f) => f.name.clone(),
        }
    }

    /// `[west, south, east, north]` to frame: a country's main landmass (not its overseas
    /// territories), or `None` for a point.
    pub fn bounds(&self) -> Option<[f64; 4]> {
        match self {
            Hit::Country(f) => main_polygon(f).and_then(crate::path_bounds),
            _ => None,
        }
    }
}

static PLACES_TSV: &str = include_str!("../../../assets/geo/ne_places.tsv");

/// The bundled populated places, most populous first.
pub fn places() -> &'static [Place] {
    static P: OnceLock<Vec<Place>> = OnceLock::new();
    P.get_or_init(|| parse_tsv(PLACES_TSV))
}

/// Rows that don't parse are skipped.
pub fn parse_tsv(text: &str) -> Vec<Place> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let c: Vec<&str> = l.split('\t').collect();
            let num = |i: usize| c.get(i).and_then(|s| s.trim().parse::<f64>().ok()).filter(|v| v.is_finite());
            let (lat, lon) = (num(5)?, num(6)?);
            if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
                return None;
            }
            let s = |i: usize| c.get(i).map(|s| s.trim().to_string()).unwrap_or_default();
            let name = s(0);
            if name.is_empty() {
                return None;
            }
            Some(Place {
                ascii: Some(s(1)).filter(|a| !a.is_empty()).unwrap_or_else(|| name.clone()),
                name,
                region: s(2),
                iso: s(3),
                country: s(4),
                at: [lon, lat],
                population: num(7).map(|p| p.max(0.0) as u64).unwrap_or(0),
                capital: s(8) == "1",
            })
        })
        .collect()
}

/// The outer ring of a country's largest polygon (France without French Guiana).
fn main_polygon(f: &Feature) -> Option<&[LonLat]> {
    f.geometry.polygons.iter().filter_map(|p| p.first()).max_by(|a, b| ring_area(a).total_cmp(&ring_area(b))).map(Vec::as_slice)
}

fn ring_area(r: &[LonLat]) -> f64 {
    let n = r.len();
    (0..n)
        .map(|i| {
            let (a, b) = (r[i], r[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum::<f64>()
        .abs()
        / 2.0
}

/// A label point for a country: the centroid of its largest polygon.
pub fn anchor(f: &Feature) -> Option<LonLat> {
    let r = main_polygon(f)?;
    let n = r.len();
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (p, q) = (r[i], r[(i + 1) % n]);
        let k = p[0] * q[1] - q[0] * p[1];
        a += k;
        cx += (p[0] + q[0]) * k;
        cy += (p[1] + q[1]) * k;
    }
    if a.abs() < 1e-12 {
        let b = crate::path_bounds(r)?;
        return Some([(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0]);
    }
    let c = [cx / (3.0 * a), cy / (3.0 * a)];
    c.iter().all(|v| v.is_finite()).then_some(c)
}

/// One coordinate: a number with an optional hemisphere letter ("48.85N", "2.35 E", "-33.9").
fn coord(s: &str) -> Option<(f64, Option<char>)> {
    let s = s.trim().trim_end_matches('°');
    let last = s.chars().last()?.to_ascii_uppercase();
    let (num, hemi) = if matches!(last, 'N' | 'S' | 'E' | 'W') { (&s[..s.len() - 1], Some(last)) } else { (s, None) };
    let v: f64 = num.trim().trim_end_matches('°').trim().parse().ok()?;
    if !v.is_finite() {
        return None;
    }
    Some(match hemi {
        Some('S' | 'W') => (-v, hemi),
        _ => (v, hemi),
    })
}

/// "lat, lon" as map apps copy them ("48.8566, 2.3522"), or with hemispheres in either order
/// ("2.35E 48.85N").
pub fn parse_coordinates(s: &str) -> Option<LonLat> {
    let parts: Vec<&str> = if s.contains(',') { s.split(',').collect() } else { s.split_whitespace().collect() };
    let [a, b] = parts.as_slice() else { return None };
    let ((a, ha), (b, hb)) = (coord(a)?, coord(b)?);
    let (lat, lon) = match (ha, hb) {
        (Some('E' | 'W'), _) | (_, Some('N' | 'S')) => (b, a),
        _ => (a, b),
    };
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some([lon, lat])
}

fn same(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b) || (!a.is_ascii() || !b.is_ascii()) && a.to_lowercase() == b.to_lowercase()
}

/// Resolve what a creator typed. Order: coordinates, a country (ISO code or name), a city
/// (largest first), "City, Country/Region" to pick among cities of the same name.
pub fn lookup(query: &str) -> Option<Hit> {
    let q = query.trim();
    if q.is_empty() {
        return None;
    }
    if let Some(p) = parse_coordinates(q) {
        return Some(Hit::Coordinates(p));
    }
    if let Some(f) = crate::country(q) {
        return Some(Hit::Country(f));
    }
    let city = |name: &str| places().iter().find(|p| same(&p.name, name) || same(&p.ascii, name));
    if let Some(p) = city(q) {
        return Some(Hit::City(p));
    }
    let (name, qual) = q.rsplit_once(',')?;
    let (name, qual) = (name.trim(), qual.trim());
    let country_iso = crate::country(qual).map(|f| f.iso.as_str());
    places()
        .iter()
        .find(|p| {
            (same(&p.name, name) || same(&p.ascii, name))
                && (same(&p.iso, qual) || same(&p.country, qual) || same(&p.region, qual) || country_iso.is_some_and(|i| same(&p.iso, i)))
        })
        .map(Hit::City)
}

/// Up to `limit` suggestions for a search box: countries and cities whose name starts with the
/// query, then ones that contain it; bigger places first.
pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<Hit> = Vec::new();
    for prefix in [true, false] {
        let m = |s: &str| {
            let s = s.to_lowercase();
            if prefix { s.starts_with(&q) } else { !s.starts_with(&q) && s.contains(&q) }
        };
        for f in world_at(Detail::Medium) {
            if out.len() >= limit {
                return out;
            }
            if m(&f.name) || prefix && f.iso.eq_ignore_ascii_case(&q) {
                out.push(Hit::Country(f));
            }
        }
        for p in places() {
            if out.len() >= limit {
                return out;
            }
            if m(&p.name) || p.ascii != p.name && m(&p.ascii) {
                out.push(Hit::City(p));
            }
        }
    }
    out
}
