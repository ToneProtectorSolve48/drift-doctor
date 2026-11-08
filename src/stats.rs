//! Drift statistics: rest test analysis, Drift Score, circularity, triggers and polling rate.
//! All distances are fractions of full stick deflection; angles are degrees counter-clockwise from +X.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Which thumbstick a measurement belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stick {
    Left,
    Right,
}

impl Stick {
    pub fn label(self) -> &'static str {
        if self == Stick::Left { "left stick" } else { "right stick" }
    }
}

/// Clinical severity bands: minimum Drift Score and label, best first.
const BANDS: [(u8, &str); 5] =
    [(90, "No drift"), (75, "Trace drift"), (50, "Mild drift"), (25, "Moderate drift"), (0, "Severe drift")];

/// Severity label for a Drift Score.
pub fn severity(score: u8) -> &'static str {
    BANDS.iter().find(|(min, _)| score >= *min).map_or("Severe drift", |b| b.1)
}

/// Result of a rest test for one stick.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RestResult {
    pub stick: Stick,
    pub samples: usize,
    /// Mean resting position (the cloud's center).
    pub center: [f32; 2],
    /// Distance of the cloud's mean from true zero.
    pub offset: f32,
    /// Direction of that offset.
    pub angle_deg: f32,
    /// 99th percentile distance of samples from the cloud center (the noise radius).
    pub r99: f32,
    /// Farthest any resting sample got from true zero.
    pub max_dist: f32,
    /// Recommended circular inner deadzone.
    pub deadzone: f32,
    /// 0-100, 100 = clinically healthy.
    pub score: u8,
}

impl RestResult {
    /// One-line diagnosis, e.g. "Mild drift, left stick, 4.1% toward 37°".
    pub fn diagnosis(&self) -> String {
        if self.score >= BANDS[0].0 {
            return format!("No drift, {}, rest noise {:.1}%", self.stick.label(), self.r99 * 100.0);
        }
        let (sev, stick) = (severity(self.score), self.stick.label());
        format!("{sev}, {stick}, {:.1}% toward {:.0}°", self.offset * 100.0, self.angle_deg)
    }
}

/// Value at percentile `p` (0..=1) of an ascending slice, linear interpolation.
pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let pos = p.clamp(0.0, 1.0) * (sorted.len() - 1) as f32;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f32)
}

pub fn angle_deg(v: [f32; 2]) -> f32 {
    v[1].atan2(v[0]).to_degrees().rem_euclid(360.0)
}

pub fn length(v: [f32; 2]) -> f32 {
    v[0].hypot(v[1])
}

/// Drift Score: 100 minus a penalty for center offset (60%) and noise radius (40%),
/// saturating at 15% of full scale.
pub fn drift_score(offset: f32, r99: f32) -> u8 {
    let badness = ((offset * 0.6 + r99 * 0.4) / 0.15).clamp(0.0, 1.0);
    (100.0 * (1.0 - badness.powf(0.8))).round() as u8
}

/// Analyzes resting samples of one stick. Returns `None` with fewer than 50 samples.
pub fn analyze_rest(stick: Stick, points: &[[f32; 2]], margin: f32) -> Option<RestResult> {
    if points.len() < 50 {
        return None;
    }
    let n = points.len() as f32;
    let c = points.iter().fold([0.0, 0.0], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n]);
    let mut dists: Vec<f32> = points.iter().map(|p| length([p[0] - c[0], p[1] - c[1]])).collect();
    dists.sort_by(f32::total_cmp);
    let (r99, offset) = (percentile(&dists, 0.99), length(c));
    // Cover offset + noise radius, add the safety margin, round up to the next 0.5%.
    let deadzone = (((offset + r99) * (1.0 + margin) + 0.005) * 200.0).ceil() / 200.0;
    Some(RestResult {
        stick,
        samples: points.len(),
        center: c,
        offset,
        angle_deg: angle_deg(c),
        r99,
        max_dist: points.iter().map(|p| length(*p)).fold(0.0, f32::max),
        deadzone: deadzone.min(0.5),
        score: drift_score(offset, r99),
    })
}

/// Sector count for the circularity test (10 degrees each).
pub const SECTORS: usize = 36;

/// Outer-gate circularity test: records the peak radius reached in each sector while the user
/// rotates the stick along its gate.
#[derive(Clone, Debug)]
pub struct Circularity {
    peak: [f32; SECTORS],
}

impl Default for Circularity {
    fn default() -> Self {
        Self { peak: [0.0; SECTORS] }
    }
}

impl Circularity {
    pub fn feed(&mut self, v: [f32; 2]) {
        let r = length(v);
        if r >= 0.5 {
            let i = ((angle_deg(v) / 360.0 * SECTORS as f32) as usize).min(SECTORS - 1);
            self.peak[i] = self.peak[i].max(r);
        }
    }

    pub fn peaks(&self) -> &[f32; SECTORS] {
        &self.peak
    }

    fn visited(&self) -> impl Iterator<Item = f32> + '_ {
        self.peak.iter().copied().filter(|r| *r > 0.0)
    }

    /// Signed error per sector against an ideal unit circle, percent.
    pub fn sector_errors(&self) -> [f32; SECTORS] {
        self.peak.map(|r| if r > 0.0 { (r - 1.0) * 100.0 } else { 0.0 })
    }

    /// Mean absolute error over visited sectors, percent.
    pub fn average_error(&self) -> f32 {
        let n = self.visited().count().max(1) as f32;
        (self.visited().map(|r| (r - 1.0).abs()).sum::<f32>() / n * 100.0).abs()
    }

    /// Outer deadzone needed so every visited direction can reach 100%.
    pub fn outer_deadzone(&self) -> f32 {
        (1.0 - self.visited().fold(1.0, f32::min)).max(0.0)
    }
}

/// Trigger probe: which of the 256 8-bit steps were ever reported. The lowest step seen is the
/// rest floor (trigger drift), the highest is peak travel, and the fill ratio between them
/// exposes skipped steps (linearity / resolution).
#[derive(Clone, Debug)]
pub struct TriggerProbe([bool; 256]);

impl Default for TriggerProbe {
    fn default() -> Self {
        Self([false; 256])
    }
}

impl TriggerProbe {
    pub fn feed(&mut self, v: f32) {
        self.0[((v * 255.0).round() as usize).min(255)] = true;
    }

    fn range(&self) -> (usize, usize) {
        (self.0.iter().position(|s| *s).unwrap_or(0), self.0.iter().rposition(|s| *s).unwrap_or(0))
    }

    pub fn floor(&self) -> f32 {
        self.range().0 as f32 / 255.0
    }

    pub fn peak(&self) -> f32 {
        self.range().1 as f32 / 255.0
    }

    pub fn resolution(&self) -> f32 {
        let (lo, hi) = self.range();
        self.0[lo..=hi].iter().filter(|s| **s).count() as f32 / (hi - lo + 1) as f32
    }
}

/// Device report-rate meter based on XInput packet-number changes. The packet number only
/// advances when the report content changes, so this is the true device rate while a stick is
/// moving and an under-estimate at perfect rest.
#[derive(Clone, Debug, Default)]
pub struct PollMeter {
    last_packet: Option<u32>,
    changes: VecDeque<f64>,
}

impl PollMeter {
    pub fn feed(&mut self, t: f64, packet: u32) {
        if self.last_packet != Some(packet) {
            self.last_packet = Some(packet);
            self.changes.push_back(t);
        }
        while self.changes.front().is_some_and(|f| t - f > 1.0) {
            self.changes.pop_front();
        }
    }

    /// Reports per second over the last second.
    pub fn hz(&self) -> f32 {
        match (self.changes.front(), self.changes.back()) {
            (Some(a), Some(b)) if b > a => ((self.changes.len() - 1) as f64 / (b - a)) as f32,
            _ => 0.0,
        }
    }

    /// Median interval between reports, milliseconds.
    pub fn median_interval_ms(&self) -> f32 {
        let mut gaps: Vec<f32> =
            self.changes.iter().zip(self.changes.iter().skip(1)).map(|(a, b)| ((b - a) * 1000.0) as f32).collect();
        gaps.sort_by(f32::total_cmp);
        percentile(&gaps, 0.5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_cloud_reports_direction() {
        let r = analyze_rest(Stick::Left, &vec![[0.033, 0.025]; 100], 0.15).expect("enough samples");
        assert!(r.diagnosis().ends_with("4.1% toward 37°"), "{}", r.diagnosis());
        assert!(drift_score(0.002, 0.004) >= 90);
    }
}
