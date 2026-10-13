//! Geography for map layers.
//!
//! - [`geojson`]: a lenient GeoJSON reader (RFC 7946) into [`Feature`]s with polygon rings and
//!   lines in longitude/latitude degrees;
//! - [`Projection`] and [`View`]: equirectangular, Web Mercator and orthographic (globe)
//!   projections, and the map view a Map layer animates (centre, zoom, rotation), projecting
//!   geography into frame pixels;
//! - [`fly`]: smooth zoom-and-pan camera paths (van Wijk & Nuij, "Smooth and efficient zooming
//!   and panning", IEEE InfoVis 2003), the zoom out / pan / zoom in move of explainer maps;
//! - [`world`]: the bundled Natural Earth 1:110m countries (public domain).
//!
//! Conventions: positions are `[longitude, latitude]` in degrees (GeoJSON order); frame pixels
//! have y growing downwards; zoom is a power of two, 0 fitting the whole world's width (360° of
//! longitude) to the frame width.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod fly;
pub mod geojson;

use std::f64::consts::{FRAC_PI_4, PI, TAU};
use std::sync::OnceLock;

pub use geojson::{Feature, Geometry};

/// `[longitude, latitude]` in degrees.
pub type LonLat = [f64; 2];

/// Mercator is cut off at this latitude (as web maps are), where it would run to infinity.
pub const MERCATOR_MAX_LAT: f64 = 85.051_128_78;

/// Map projections.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Projection {
    /// Plate carrée: longitude and latitude as plain x and y.
    #[default]
    Equirectangular,
    /// Web Mercator: conformal, the projection of web maps.
    Mercator,
    /// A globe seen from infinitely far away; the far hemisphere is hidden.
    Orthographic,
}

impl Projection {
    pub const ALL: [Projection; 3] = [Projection::Equirectangular, Projection::Mercator, Projection::Orthographic];
    pub fn label(self) -> &'static str {
        match self {
            Projection::Equirectangular => "Equirectangular",
            Projection::Mercator => "Mercator",
            Projection::Orthographic => "Globe (Orthographic)",
        }
    }
    /// The projection at a popup index, Equirectangular when out of range.
    pub fn from_index(i: u32) -> Projection {
        Projection::ALL.get(i as usize).copied().unwrap_or_default()
    }
    pub fn index(self) -> u32 {
        Projection::ALL.iter().position(|p| *p == self).unwrap_or(0) as u32
    }
    /// Parse a projection name (case-insensitive; also "globe", "equirect", "plate carree").
    pub fn parse(s: &str) -> Option<Projection> {
        match s.trim().to_ascii_lowercase().as_str() {
            "equirectangular" | "equirect" | "plate carree" | "platecarree" | "flat" => Some(Projection::Equirectangular),
            "mercator" | "webmercator" | "web mercator" => Some(Projection::Mercator),
            "orthographic" | "globe" | "ortho" => Some(Projection::Orthographic),
            _ => None,
        }
    }
}

/// What a Map layer shows: where it looks, how close, and how the frame is turned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub center: LonLat,
    /// Power of two: 0 fits 360° of longitude to the frame width; each step doubles the scale.
    pub zoom: f64,
    /// Degrees clockwise on screen.
    pub rotation: f64,
    pub projection: Projection,
    /// Frame size in pixels.
    pub width: f64,
    pub height: f64,
}

/// A projected point in frame pixels, and whether it is on the visible side of the globe.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projected {
    pub xy: [f64; 2],
    pub visible: bool,
}

/// The largest zoom a view accepts (about 1 cm per pixel at the equator on a 1920 px frame).
pub const MAX_ZOOM: f64 = 32.0;
pub const MIN_ZOOM: f64 = -4.0;

impl View {
    pub fn new(center: LonLat, zoom: f64, projection: Projection, width: f64, height: f64) -> View {
        View { center, zoom, rotation: 0.0, projection, width, height }
    }

    /// Pixels per radian (of longitude, at the equator, for the flat projections; of arc at the
    /// globe's centre for the orthographic one).
    pub fn scale(&self) -> f64 {
        let z = if self.zoom.is_finite() { self.zoom.clamp(MIN_ZOOM, MAX_ZOOM) } else { 0.0 };
        let w = if self.width.is_finite() && self.width > 0.0 { self.width } else { 1.0 };
        w / TAU * z.exp2()
    }

    fn center_rad(&self) -> (f64, f64) {
        let lon = if self.center[0].is_finite() { self.center[0] } else { 0.0 };
        let lat = if self.center[1].is_finite() { self.center[1].clamp(-90.0, 90.0) } else { 0.0 };
        (lon.to_radians(), lat.to_radians())
    }

    /// Projection-plane coordinates (radians-ish, y up) of a point, relative to the view centre,
    /// with the globe visibility. Flat projections pick the longitude copy nearest the centre
    /// unless `lon_shift` (multiples of 360°) is given.
    fn plane(&self, p: LonLat, lon_shift: f64) -> ([f64; 2], bool) {
        let (l0, p0) = self.center_rad();
        let lon = (p[0] + lon_shift).to_radians();
        let lat = p[1].clamp(-90.0, 90.0).to_radians();
        match self.projection {
            Projection::Equirectangular => ([lon - l0, lat - p0], true),
            Projection::Mercator => {
                let m = |phi: f64| {
                    let phi = phi.clamp(-MERCATOR_MAX_LAT.to_radians(), MERCATOR_MAX_LAT.to_radians());
                    (FRAC_PI_4 + phi / 2.0).tan().ln()
                };
                ([lon - l0, m(lat) - m(p0)], true)
            }
            Projection::Orthographic => {
                let dl = lon - l0;
                let (sp, cp) = lat.sin_cos();
                let (s0, c0) = p0.sin_cos();
                let x = cp * dl.sin();
                let y = c0 * sp - s0 * cp * dl.cos();
                let cos_c = s0 * sp + c0 * cp * dl.cos();
                ([x, y], cos_c >= 0.0)
            }
        }
    }

    fn frame_px(&self, q: [f64; 2]) -> [f64; 2] {
        let k = self.scale();
        let (x, y) = (q[0] * k, -q[1] * k);
        let (s, c) = self.rotation.to_radians().sin_cos();
        let (x, y) = if self.rotation.is_finite() && self.rotation != 0.0 { (x * c - y * s, x * s + y * c) } else { (x, y) };
        [self.width / 2.0 + x, self.height / 2.0 + y]
    }

    /// Project a point to frame pixels. Hidden globe points are pulled onto the horizon.
    pub fn project(&self, p: LonLat) -> Projected {
        let shift = self.nearest_shift(p[0]);
        self.project_shifted(p, shift)
    }

    fn project_shifted(&self, p: LonLat, shift: f64) -> Projected {
        let (mut q, visible) = self.plane(p, shift);
        if !visible {
            let r = q[0].hypot(q[1]);
            if r > 1e-12 {
                q = [q[0] / r, q[1] / r];
            }
        }
        Projected { xy: self.frame_px(q), visible }
    }

    /// The multiple of 360° that brings longitude `lon` nearest the view centre (flat
    /// projections; 0 for the globe).
    fn nearest_shift(&self, lon: f64) -> f64 {
        if self.projection == Projection::Orthographic || !lon.is_finite() || !self.center[0].is_finite() {
            return 0.0;
        }
        ((self.center[0] - lon) / 360.0).round() * 360.0
    }

    /// Inverse of [`View::project`] for flat projections and the visible globe: the geography
    /// under a frame pixel, `None` off the globe.
    pub fn unproject(&self, xy: [f64; 2]) -> Option<LonLat> {
        let k = self.scale();
        if k <= 0.0 {
            return None;
        }
        let (x, y) = (xy[0] - self.width / 2.0, xy[1] - self.height / 2.0);
        let (s, c) = (-self.rotation.to_radians()).sin_cos();
        let (x, y) = (x * c - y * s, x * s + y * c);
        let (qx, qy) = (x / k, -y / k);
        let (l0, p0) = self.center_rad();
        let (lon, lat) = match self.projection {
            Projection::Equirectangular => (l0 + qx, p0 + qy),
            Projection::Mercator => {
                let m0 = (FRAC_PI_4 + p0.clamp(-MERCATOR_MAX_LAT.to_radians(), MERCATOR_MAX_LAT.to_radians()) / 2.0).tan().ln();
                (l0 + qx, 2.0 * (m0 + qy).exp().atan() - PI / 2.0)
            }
            Projection::Orthographic => {
                let rho = qx.hypot(qy);
                if rho > 1.0 {
                    return None;
                }
                if rho < 1e-12 {
                    return Some([self.center[0], self.center[1]]);
                }
                let cc = rho.asin();
                let (sc, ccos) = cc.sin_cos();
                let (s0, c0) = p0.sin_cos();
                let lat = (ccos * s0 + qy * sc * c0 / rho).clamp(-1.0, 1.0).asin();
                let lon = l0 + (qx * sc).atan2(rho * c0 * ccos - qy * s0 * sc);
                (lon, lat)
            }
        };
        let lon = lon.to_degrees();
        let lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
        Some([lon, lat.to_degrees().clamp(-90.0, 90.0)])
    }

    /// The outline detail this view should draw.
    pub fn detail(&self) -> Detail {
        if self.zoom.is_finite() && self.zoom >= MEDIUM_DETAIL_ZOOM { Detail::Medium } else { Detail::Low }
    }

    /// Whether any of a path with these bounds (`[west, south, east, north]`, degrees) can be in
    /// the frame. Conservative: `true` when unsure (the globe, rotated views, paths wider than a
    /// hemisphere).
    pub fn may_show(&self, b: [f64; 4]) -> bool {
        if self.projection == Projection::Orthographic || !b.iter().all(|v| v.is_finite()) || b[2] - b[0] > 180.0 {
            return true;
        }
        let shift = self.nearest_shift((b[0] + b[2]) / 2.0);
        let corners = [[b[0], b[1]], [b[2], b[1]], [b[0], b[3]], [b[2], b[3]]].map(|c| self.project_shifted(c, shift).xy);
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for [x, y] in corners {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        // Rotation turns the box; widen the frame test by its diagonal so rotated views stay safe.
        let m = if self.rotation.is_finite() && self.rotation != 0.0 { self.width.hypot(self.height) } else { 2.0 };
        x1 >= -m && y1 >= -m && x0 <= self.width + m && y0 <= self.height + m
    }

    /// Project a polygon ring or line into frame pixels. Long edges are subdivided along great
    /// circles on the globe (so they curve) and hidden stretches follow the horizon. A ring
    /// keeps one longitude copy for all its points on flat projections, so it doesn't tear.
    /// Returns `None` when the whole path is on the far side of the globe.
    pub fn project_path(&self, pts: &[LonLat], closed: bool) -> Option<Vec<[f64; 2]>> {
        let first = pts.first()?;
        let shift = self.nearest_shift(first[0]);
        let globe = self.projection == Projection::Orthographic;
        let mut out = Vec::with_capacity(pts.len());
        let mut any_visible = false;
        let n = pts.len();
        let edges = if closed { n } else { n.saturating_sub(1) };
        let mut push = |p: LonLat, out: &mut Vec<[f64; 2]>| {
            let pr = self.project_shifted(p, shift);
            any_visible |= pr.visible;
            out.push(pr.xy);
        };
        push(*first, &mut out);
        for i in 0..edges {
            let (Some(a), Some(b)) = (pts.get(i), pts.get((i + 1) % n)) else { break };
            if globe {
                let steps = (great_circle_deg(*a, *b) / GLOBE_STEP_DEG).ceil().clamp(1.0, 256.0) as usize;
                for s in 1..steps {
                    push(slerp(*a, *b, s as f64 / steps as f64), &mut out);
                }
            }
            if closed && i + 1 == n {
                break;
            }
            push(*b, &mut out);
        }
        any_visible.then_some(out)
    }
}

/// Globe edges are subdivided into steps of at most this many degrees of arc.
const GLOBE_STEP_DEG: f64 = 2.0;

fn unit(p: LonLat) -> [f64; 3] {
    let (lon, lat) = (p[0].to_radians(), p[1].clamp(-90.0, 90.0).to_radians());
    [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()]
}

fn lonlat(v: [f64; 3]) -> LonLat {
    let r = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if r < 1e-15 {
        return [0.0, 0.0];
    }
    [v[1].atan2(v[0]).to_degrees(), (v[2] / r).clamp(-1.0, 1.0).asin().to_degrees()]
}

/// Great-circle distance between two points, in degrees of arc.
pub fn great_circle_deg(a: LonLat, b: LonLat) -> f64 {
    let (u, v) = (unit(a), unit(b));
    let d = (u[0] * v[0] + u[1] * v[1] + u[2] * v[2]).clamp(-1.0, 1.0);
    d.acos().to_degrees()
}

/// The point a fraction `t` of the way from `a` to `b` along the great circle.
pub fn slerp(a: LonLat, b: LonLat, t: f64) -> LonLat {
    let (u, v) = (unit(a), unit(b));
    let d = (u[0] * v[0] + u[1] * v[1] + u[2] * v[2]).clamp(-1.0, 1.0);
    let om = d.acos();
    if om.abs() < 1e-9 || !t.is_finite() {
        return a;
    }
    let s = om.sin();
    if s.abs() < 1e-9 {
        // Antipodal: no unique great circle; fall back to plain interpolation.
        return [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    }
    let (wa, wb) = (((1.0 - t) * om).sin() / s, (t * om).sin() / s);
    lonlat([wa * u[0] + wb * v[0], wa * u[1] + wb * v[1], wa * u[2] + wb * v[2]])
}

/// Bundled country outlines at two levels of detail (Natural Earth, public domain; `assets/geo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// 1:110m: whole-world views.
    Low,
    /// 1:50m: continents and closer.
    Medium,
}

/// Views at this zoom and closer draw the 1:50m outlines.
pub const MEDIUM_DETAIL_ZOOM: f64 = 1.5;

/// The bundled countries at `detail`.
pub fn world_at(detail: Detail) -> &'static [Feature] {
    static LOW: OnceLock<Vec<Feature>> = OnceLock::new();
    static MEDIUM: OnceLock<Vec<Feature>> = OnceLock::new();
    let (cell, text) = match detail {
        Detail::Low => (&LOW, WORLD_110M),
        Detail::Medium => (&MEDIUM, WORLD_50M),
    };
    cell.get_or_init(|| geojson::parse(text).map(|r| r.features).unwrap_or_default())
}

/// The bundled Natural Earth 1:110m countries.
pub fn world() -> &'static [Feature] {
    world_at(Detail::Low)
}

static WORLD_110M: &str = include_str!("../../../assets/geo/ne_110m_countries.geojson");
static WORLD_50M: &str = include_str!("../../../assets/geo/ne_50m_countries.geojson");

/// The bundled country whose ISO 3166-1 alpha-3 code or name matches `key` (case-insensitive),
/// from the 1:50m set (it has the small countries the 1:110m set leaves out).
pub fn country(key: &str) -> Option<&'static Feature> {
    let k = key.trim();
    let w = world_at(Detail::Medium);
    w.iter().find(|f| f.iso.eq_ignore_ascii_case(k)).or_else(|| w.iter().find(|f| f.name.eq_ignore_ascii_case(k)))
}

/// `[west, south, east, north]` of a path's points; `None` when empty.
pub fn path_bounds(pts: &[LonLat]) -> Option<[f64; 4]> {
    let first = pts.first()?;
    Some(pts.iter().fold([first[0], first[1], first[0], first[1]], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]))
}

/// Split a highlight list ("UKR, POL; Russia") into keys.
pub fn split_keys(s: &str) -> Vec<&str> {
    s.split([',', ';', '\n']).map(str::trim).filter(|k| !k.is_empty()).collect()
}

#[cfg(test)]
mod tests;
