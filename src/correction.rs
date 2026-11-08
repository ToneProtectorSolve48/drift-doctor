//! Anisotropic anti-drift correction.
//!
//! A circular deadzone has to be as large as the worst direction of the drift cloud, which
//! throws away precision everywhere else. [`DeadzoneShape`] stores one radius per 5-degree
//! direction, taken from the support function of the measured rest cloud, so the dead area is
//! shaped like the actual drift. Corrected output goes to a [`CorrectionBackend`], normally a
//! ViGEmBus virtual controller that games read instead of the physical pad.

use crate::input::Sample;
use crate::stats::{angle_deg, length, percentile};
use serde::{Deserialize, Serialize};

/// Directions stored in a shape (5 degrees each).
pub const SHAPE_DIRS: usize = 72;
const MIN_RADIUS: f32 = 0.01;

/// Per-direction inner deadzone radius.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeadzoneShape {
    radii: Vec<f32>,
}

impl DeadzoneShape {
    /// Plain circular deadzone of radius `r`.
    pub fn circle(r: f32) -> Self {
        Self { radii: vec![r.max(MIN_RADIUS); SHAPE_DIRS] }
    }

    /// Tightest star-shaped zone holding `pct` of the rest cloud in every direction, grown by
    /// `margin` (fractional) and smoothed so neighbouring directions never dip below their mean.
    pub fn from_cloud(points: &[[f32; 2]], pct: f32, margin: f32) -> Self {
        if points.is_empty() {
            return Self::circle(MIN_RADIUS);
        }
        let raw: Vec<f32> = (0..SHAPE_DIRS)
            .map(|i| {
                let (uy, ux) = (i as f32 / SHAPE_DIRS as f32 * std::f32::consts::TAU).sin_cos();
                let mut proj: Vec<f32> = points.iter().map(|p| p[0] * ux + p[1] * uy).collect();
                proj.sort_by(f32::total_cmp);
                (percentile(&proj, pct) * (1.0 + margin)).max(MIN_RADIUS)
            })
            .collect();
        let at = |i: usize| raw[i % SHAPE_DIRS];
        let radii =
            (0..SHAPE_DIRS).map(|i| at(i).max((at(i + SHAPE_DIRS - 1) + 2.0 * at(i) + at(i + 1)) / 4.0)).collect();
        Self { radii }
    }

    /// Interpolated radius toward `deg`.
    pub fn radius_at(&self, deg: f32) -> f32 {
        let pos = deg.rem_euclid(360.0) / 360.0 * SHAPE_DIRS as f32;
        let i = pos.floor() as usize % SHAPE_DIRS;
        self.radii[i] + (self.radii[(i + 1) % SHAPE_DIRS] - self.radii[i]) * pos.fract()
    }

    /// Dead area relative to the unit circle.
    pub fn area_fraction(&self) -> f32 {
        self.radii.iter().map(|r| r * r).sum::<f32>() / SHAPE_DIRS as f32
    }

    /// Largest radius: the circle a conventional deadzone would need.
    pub fn max_radius(&self) -> f32 {
        self.radii.iter().copied().fold(0.0, f32::max)
    }

    /// Zeroes input inside the shape and rescales the rest so the edge maps to 0 and the gate to 1.
    pub fn apply(&self, v: [f32; 2]) -> [f32; 2] {
        let m = length(v);
        let dz = self.radius_at(angle_deg(v));
        if m <= dz {
            return [0.0, 0.0];
        }
        let scaled = ((m - dz) / (1.0 - dz).max(1e-3)).min(1.0);
        [v[0] / m * scaled, v[1] / m * scaled]
    }
}

/// Correction profile for both sticks of one controller.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CorrectionProfile {
    pub left: DeadzoneShape,
    pub right: DeadzoneShape,
}

impl Default for CorrectionProfile {
    fn default() -> Self {
        Self { left: DeadzoneShape::circle(0.05), right: DeadzoneShape::circle(0.05) }
    }
}

impl CorrectionProfile {
    /// Applies both stick shapes; triggers and buttons pass through untouched.
    pub fn correct(&self, s: &Sample) -> Sample {
        Sample { left: self.left.apply(s.left), right: self.right.apply(s.right), ..*s }
    }
}

/// Sink for corrected controller state.
///
/// The ViGEmBus implementation you plug in here connects to the bus driver (`vigem-client`
/// crate or `\\.\ViGEmBus` IOCTLs), plugs a virtual Xbox 360 target in `start`, converts each
/// corrected [`Sample`] into an `XUSB_REPORT` (sticks * 32767, triggers * 255, buttons as-is) in
/// `submit`, and unplugs in `stop`. Pair it with HidHide so games only see the virtual pad.
pub trait CorrectionBackend: Send {
    /// Short name shown in the UI.
    fn name(&self) -> &'static str;
    /// Plugs in the virtual device mirroring physical slot `slot`.
    fn start(&mut self, slot: usize) -> Result<(), String>;
    /// Sends one corrected report.
    fn submit(&mut self, state: &Sample) -> Result<(), String>;
    /// Unplugs the virtual device. Must be safe to call repeatedly.
    fn stop(&mut self);
    /// Reports submitted since `start`.
    fn frames(&self) -> u64;
}

/// No-op backend: counts frames so the correction can be previewed without any driver.
#[derive(Debug, Default)]
pub struct NoopBackend {
    running: bool,
    frames: u64,
}

impl CorrectionBackend for NoopBackend {
    fn name(&self) -> &'static str {
        "Preview (no virtual pad)"
    }

    fn start(&mut self, _slot: usize) -> Result<(), String> {
        (self.running, self.frames) = (true, 0);
        Ok(())
    }

    fn submit(&mut self, _state: &Sample) -> Result<(), String> {
        self.frames += u64::from(self.running);
        Ok(())
    }

    fn stop(&mut self) {
        self.running = false;
    }

    fn frames(&self) -> u64 {
        self.frames
    }
}

/// True when the ViGEmBus kernel driver service is registered on this machine.
pub fn vigem_installed() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW};
    let mut key = HKEY::default();
    let subkey = windows::core::w!(r"SYSTEM\CurrentControlSet\Services\ViGEmBus");
    // SAFETY: constant, NUL-terminated subkey and a writable HKEY out-parameter.
    if unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, subkey, None, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return false;
    }
    // SAFETY: `key` was opened successfully above and is closed exactly once.
    let _ = unsafe { RegCloseKey(key) };
    true
}

#[cfg(test)]
mod tests {
    #[test]
    fn shape_follows_cloud() {
        use super::DeadzoneShape;
        let cloud: Vec<[f32; 2]> = (0..200).map(|i| [0.06 + (i % 7) as f32 * 0.001, 0.0]).collect();
        let shape = DeadzoneShape::from_cloud(&cloud, 0.99, 0.1);
        assert!(shape.radius_at(0.0) > 0.06 && shape.radius_at(180.0) < 0.02);
        assert_eq!(shape.apply([0.05, 0.0]), [0.0, 0.0]);
    }
}
