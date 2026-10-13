//! Smooth zoom-and-pan camera paths after van Wijk & Nuij, "Smooth and efficient zooming and
//! panning" (IEEE InfoVis 2003): the camera rises (zooms out) while it travels and descends
//! (zooms in) as it arrives, along the path the paper shows is optimal for a perceived-speed
//! metric. `RHO` trades zooming for panning (the paper's recommended √2).
//!
//! The path is computed in a plane of longitude/latitude degrees, where the view's width is
//! `360 / 2^zoom` degrees; longitude travels the short way round.

use crate::LonLat;

/// The paper's ρ: higher values zoom out further.
pub const RHO: f64 = std::f64::consts::SQRT_2;

/// One end of a fly-to: where the camera looks and how close.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stop {
    pub center: LonLat,
    pub zoom: f64,
}

/// A precomputed fly-to path from one stop to another.
#[derive(Clone, Copy, Debug)]
pub struct Path {
    p0: [f64; 2],
    d: [f64; 2],
    d1: f64,
    w0: f64,
    w1: f64,
    r0: f64,
    /// Path length in the paper's units (proportional to a natural duration).
    pub length: f64,
}

fn width(zoom: f64) -> f64 {
    let z = if zoom.is_finite() { zoom.clamp(crate::MIN_ZOOM, crate::MAX_ZOOM) } else { 0.0 };
    360.0 / z.exp2()
}

impl Path {
    pub fn new(a: Stop, b: Stop) -> Path {
        let mut dx = b.center[0] - a.center[0];
        if dx.is_finite() {
            dx = (dx + 180.0).rem_euclid(360.0) - 180.0;
        } else {
            dx = 0.0;
        }
        let dy = if (b.center[1] - a.center[1]).is_finite() { b.center[1] - a.center[1] } else { 0.0 };
        let (w0, w1) = (width(a.zoom), width(b.zoom));
        let d2 = dx * dx + dy * dy;
        let d1 = d2.sqrt();
        let (rho2, rho4) = (RHO * RHO, RHO.powi(4));
        if d1 < 1e-9 {
            let length = (w1 / w0).ln().abs() / RHO;
            return Path { p0: a.center, d: [0.0, 0.0], d1: 0.0, w0, w1, r0: 0.0, length };
        }
        let b0 = (w1 * w1 - w0 * w0 + rho4 * d2) / (2.0 * w0 * rho2 * d1);
        let b1 = (w1 * w1 - w0 * w0 - rho4 * d2) / (2.0 * w1 * rho2 * d1);
        let r0 = ((b0 * b0 + 1.0).sqrt() - b0).ln();
        let r1 = ((b1 * b1 + 1.0).sqrt() - b1).ln();
        let length = (r1 - r0) / RHO;
        Path { p0: a.center, d: [dx, dy], d1, w0, w1, r0, length: if length.is_finite() { length } else { 0.0 } }
    }

    /// The camera a fraction `t` (0…1) of the way along the path.
    pub fn at(&self, t: f64) -> Stop {
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        if self.d1 < 1e-9 {
            // Pure zoom: exponential in width.
            let w = self.w0 * (self.w1 / self.w0).powf(t);
            return Stop { center: self.p0, zoom: zoom_of(w) };
        }
        let s = t * self.length;
        let (rho2, c0) = (RHO * RHO, self.r0.cosh());
        let u = self.w0 / (rho2 * self.d1) * (c0 * (RHO * s + self.r0).tanh() - self.r0.sinh());
        let w = self.w0 * c0 / (RHO * s + self.r0).cosh();
        let mut lon = self.p0[0] + u * self.d[0];
        lon = (lon + 180.0).rem_euclid(360.0) - 180.0;
        let lat = (self.p0[1] + u * self.d[1]).clamp(-90.0, 90.0);
        // Land exactly on the target (no floating drift at t = 1).
        if t >= 1.0 {
            return Stop { center: [lon, lat], zoom: zoom_of(self.w1) };
        }
        Stop { center: [lon, lat], zoom: zoom_of(w) }
    }
}

fn zoom_of(w: f64) -> f64 {
    if w > 0.0 && w.is_finite() { (360.0 / w).log2().clamp(crate::MIN_ZOOM, crate::MAX_ZOOM) } else { 0.0 }
}
