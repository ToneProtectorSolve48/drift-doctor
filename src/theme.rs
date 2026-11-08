//! Neutral desktop palette with one accent, mirroring the CSS tokens in `docs/design/mockup.html`.

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, Stroke};
use std::path::PathBuf;
use std::sync::Arc;

// Surfaces, from darkest (sidebar) to raised controls.
pub const SIDEBAR: Color32 = Color32::from_rgb(0x1B, 0x1C, 0x1F);
pub const BG: Color32 = Color32::from_rgb(0x22, 0x23, 0x27);
pub const PANEL: Color32 = Color32::from_rgb(0x2A, 0x2B, 0x2F);
pub const CONTROL: Color32 = Color32::from_rgb(0x32, 0x33, 0x38);
pub const SELECTED: Color32 = Color32::from_rgb(0x2C, 0x2D, 0x32);
pub const LINE: Color32 = Color32::from_rgb(0x3A, 0x3B, 0x41);
pub const LINE_SOFT: Color32 = Color32::from_rgb(0x30, 0x31, 0x36);
// Text: primary, secondary, tertiary.
pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xE9, 0xEC);
pub const TEXT_2: Color32 = Color32::from_rgb(0xA6, 0xA9, 0xB0);
pub const TEXT_3: Color32 = Color32::from_rgb(0x72, 0x75, 0x7D);
// The single accent, its on-accent ink, and the one semantic warning color.
pub const ACCENT: Color32 = Color32::from_rgb(0x3C, 0xCB, 0x7F);
pub const ACCENT_INK: Color32 = Color32::from_rgb(0x08, 0x20, 0x14);
pub const WARN: Color32 = Color32::from_rgb(0xE3, 0xA3, 0x3A);
// Plot surfaces for the signature stick visualization.
pub const PLOT: Color32 = Color32::from_rgb(0x18, 0x19, 0x1C);
pub const PLOT_GRID: Color32 = Color32::from_rgb(0x34, 0x36, 0x3C);
pub const PLOT_GRID_SOFT: Color32 = Color32::from_rgb(0x26, 0x28, 0x2D);
pub const SERIES_2: Color32 = Color32::from_rgb(0x7E, 0x81, 0x89);
// ECG-paper look of the exported diagnosis report.
pub const PAPER: Color32 = Color32::WHITE;
pub const PAPER_GRID: Color32 = Color32::from_rgb(0xF6, 0xDE, 0xDE);
pub const PAPER_INK: Color32 = Color32::from_rgb(0x1D, 0x21, 0x27);

/// Monospace (tabular) font for numeric values.
pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// UI font.
pub fn sans(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn hairline(color: Color32) -> Stroke {
    Stroke::new(1.0, color)
}

/// Applies the dark neutral visuals to every egui style.
pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    (v.panel_fill, v.window_fill, v.extreme_bg_color) = (BG, PANEL, PLOT);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
    v.widgets.inactive.weak_bg_fill = CONTROL;
    v.widgets.inactive.bg_stroke = hairline(LINE);
    v.widgets.hovered.weak_bg_fill = LINE;
    v.widgets.hovered.bg_stroke = hairline(TEXT_3);
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| s.spacing.item_spacing = egui::vec2(12.0, 12.0));
    install_fonts(ctx);
}

/// Font files probed at runtime, in priority order. Nothing is embedded in the binary.
const SANS_CANDIDATES: &[&str] =
    &["SF-Pro-Text-Regular.otf", "SF-Pro-Display-Regular.otf", "SF-Pro.ttf", "SegUIVar.ttf", "segoeui.ttf"];
const MONO_CANDIDATES: &[&str] = &["SF-Mono-Regular.otf", "SFMono-Regular.otf", "CascadiaMono.ttf", "consola.ttf"];

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(r"C:\Windows\Fonts")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
    }
    dirs
}

fn find_font(candidates: &[&str]) -> Option<(String, Vec<u8>)> {
    let dirs = font_dirs();
    candidates.iter().find_map(|name| {
        dirs.iter().find_map(|d| std::fs::read(d.join(name)).ok()).map(|bytes| ((*name).to_owned(), bytes))
    })
}

/// Loads SF Pro / SF Mono when installed, else Segoe UI Variable, else egui defaults.
fn install_fonts(ctx: &egui::Context) {
    let mut defs = FontDefinitions::default();
    for (family, candidates) in [(FontFamily::Proportional, SANS_CANDIDATES), (FontFamily::Monospace, MONO_CANDIDATES)]
    {
        if let Some((name, bytes)) = find_font(candidates) {
            defs.font_data.insert(name.clone(), Arc::new(FontData::from_owned(bytes)));
            defs.families.entry(family).or_default().insert(0, name);
        }
    }
    ctx.set_fonts(defs);
}
