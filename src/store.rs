//! Persistence: settings, drift history and the exported warranty report image.
//! Everything lives in `%APPDATA%\DriftDoctor\` as human-readable JSON.

use crate::correction::CorrectionProfile;
use crate::stats::RestResult;
use eframe::egui::ColorImage;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// User settings (`settings.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Sampler sleep between XInput polls, microseconds.
    pub sample_interval_us: u64,
    /// Rest test duration, seconds.
    pub rest_seconds: f32,
    /// Extra safety margin applied to recommended deadzones (0.15 = +15%).
    pub deadzone_margin: f32,
    /// Selected controller slot.
    pub slot: usize,
    /// Use the measured per-angle shape (true) or its bounding circle (false).
    pub shaped: bool,
    /// Last computed anisotropic correction.
    pub correction: CorrectionProfile,
}

impl Default for Settings {
    fn default() -> Self {
        let correction = CorrectionProfile::default();
        Self { sample_interval_us: 1000, rest_seconds: 10.0, deadzone_margin: 0.15, slot: 0, shaped: true, correction }
    }
}

impl Settings {
    pub fn load() -> Self {
        load_json("settings.json")
    }

    pub fn save(&self) -> io::Result<()> {
        save_json("settings.json", self)
    }
}

/// One rest-test measurement in the drift history timeline (`history.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Unix seconds.
    pub when: u64,
    pub device: String,
    #[serde(flatten)]
    pub result: RestResult,
}

impl HistoryEntry {
    pub fn from_result(device: &str, r: &RestResult) -> Self {
        Self { when: now_unix(), device: device.to_owned(), result: r.clone() }
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `%APPDATA%\DriftDoctor`. Only created when something is saved, never on load.
pub fn data_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("DriftDoctor"))
}

fn load_json<T: for<'de> Deserialize<'de> + Default>(name: &str) -> T {
    let bytes = data_dir().and_then(|d| std::fs::read(d.join(name)).ok());
    bytes.and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Writes atomically: temp file first, then rename over the old one.
fn save_json<T: Serialize>(name: &str, value: &T) -> io::Result<()> {
    let dir = data_dir().ok_or_else(|| io::Error::other("APPDATA not available"))?;
    std::fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    std::fs::rename(tmp, dir.join(name))
}

/// Loads the drift history, oldest first.
pub fn load_history() -> Vec<HistoryEntry> {
    load_json("history.json")
}

pub fn save_history(entries: &[HistoryEntry]) -> io::Result<()> {
    save_json("history.json", &entries)
}

/// Writes an egui screenshot region as an uncompressed 24-bit BMP (opens everywhere, no deps).
pub fn write_bmp(path: &Path, img: &ColorImage) -> io::Result<()> {
    let [w, h] = img.size;
    let row = (w * 3).div_ceil(4) * 4;
    let mut out = Vec::with_capacity(54 + row * h);
    let le32 = |v: usize| (v as u32).to_le_bytes();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&le32(54 + row * h));
    out.extend_from_slice(&[0, 0, 0, 0, 54, 0, 0, 0, 40, 0, 0, 0]);
    out.extend_from_slice(&le32(w));
    out.extend_from_slice(&le32(h));
    out.extend_from_slice(&[1, 0, 24, 0, 0, 0, 0, 0]);
    out.extend_from_slice(&le32(row * h));
    out.extend_from_slice(&[0x13, 0x0B, 0, 0, 0x13, 0x0B, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    for y in (0..h).rev() {
        let start = out.len();
        img.pixels[y * w..(y + 1) * w].iter().for_each(|px| out.extend_from_slice(&[px.b(), px.g(), px.r()]));
        out.resize(start + row, 0);
    }
    std::fs::File::create(path)?.write_all(&out)
}

/// Default location for an exported warranty report: the Desktop, else the data folder.
pub fn report_path() -> PathBuf {
    let desktop = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Desktop")).filter(|p| p.is_dir());
    let base = desktop
        .or_else(|| data_dir().filter(|d| std::fs::create_dir_all(d).is_ok()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join(format!("DriftDoctor-report-{}.bmp", now_unix()))
}
