//! Custom-painted widgets: the polar stick plot (the signature visualization), time lanes,
//! property rows, toggles and sidebar items with 20px line icons.

use crate::correction::DeadzoneShape;
use crate::stats::SECTORS;
use crate::store::HistoryEntry;
use crate::theme::{self, ACCENT, LINE_SOFT, PLOT, PLOT_GRID, PLOT_GRID_SOFT, TEXT, TEXT_2, TEXT_3, mono, sans};
use eframe::egui::{self, Align2, Color32, Mesh, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use eframe::egui::{pos2, vec2};
use std::collections::VecDeque;
use std::f32::consts::TAU;

/// Ring buffer of the last N values of one signal, drawn oldest to newest.
#[derive(Clone, Debug, Default)]
pub struct Trace(VecDeque<f32>);

impl Trace {
    pub fn push(&mut self, v: f32, cap: usize) {
        self.0.push_back(v);
        while self.0.len() > cap {
            self.0.pop_front();
        }
    }
}

/// What a polar plot should draw. Coordinates are stick space (-1..1, y up).
#[derive(Default)]
pub struct PolarView<'a> {
    pub current: [f32; 2],
    pub trail: &'a [[f32; 2]],
    /// Rest-noise scatter cloud.
    pub cloud: &'a [[f32; 2]],
    /// Mean rest position and its r99 noise circle.
    pub center: Option<([f32; 2], f32)>,
    /// Measured drift radius (offset + r99), drawn as a dashed warning ring.
    pub ring: Option<f32>,
    /// Anisotropic deadzone outline.
    pub shape: Option<&'a DeadzoneShape>,
    /// Circularity peaks per sector, with the ideal circle for reference.
    pub peaks: Option<&'a [f32; SECTORS]>,
    /// 1.0 shows the full gate, 12.5 magnifies the center to +-8%.
    pub zoom: f32,
}

fn surface(ui: &mut Ui, size: Vec2) -> (Rect, Painter) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6, PLOT);
    (rect, p)
}

/// Polar stick plot: graticule, live trail, rest cloud, drift ring, deadzone shape, gate trace.
pub fn polar_plot(ui: &mut Ui, side: f32, v: &PolarView<'_>) {
    let (rect, p) = surface(ui, vec2(side, side));
    let (c, r) = (rect.center(), side * 0.5 - 20.0);
    let map = |q: [f32; 2]| pos2(c.x + q[0] * v.zoom * r, c.y - q[1] * v.zoom * r);
    let polar = |deg: f32, rr: f32| map([deg.to_radians().cos() * rr, deg.to_radians().sin() * rr]);
    for i in 1..=4 {
        p.circle_stroke(c, r * i as f32 / 4.0, theme::hairline(if i == 4 { PLOT_GRID } else { PLOT_GRID_SOFT }));
    }
    for k in 0..8 {
        let dir = vec2((k as f32 * TAU / 8.0).cos(), -(k as f32 * TAU / 8.0).sin());
        p.line_segment([c, c + dir * r], theme::hairline(PLOT_GRID_SOFT));
        if k % 2 == 0 {
            p.text(c + dir * (r + 11.0), Align2::CENTER_CENTER, format!("{}°", k * 45), mono(9.5), TEXT_3);
        }
    }
    if let Some(peaks) = v.peaks {
        p.circle_stroke(c, r * v.zoom, Stroke::new(1.0, TEXT_3));
        let pts = (0..SECTORS).filter(|i| peaks[*i] > 0.0).map(|i| polar((i as f32 + 0.5) * 10.0, peaks[i]));
        p.add(Shape::closed_line(pts.collect(), Stroke::new(1.8, ACCENT)));
    }
    if let Some(shape) = v.shape {
        let pts: Vec<Pos2> = (0..120).map(|i| polar(i as f32 * 3.0, shape.radius_at(i as f32 * 3.0))).collect();
        // Star-shaped, not convex: fill as a triangle fan around the origin.
        let (mut fan, fill) = (Mesh::default(), ACCENT.gamma_multiply(0.12));
        fan.colored_vertex(map([0.0, 0.0]), fill);
        pts.iter().for_each(|q| fan.colored_vertex(*q, fill));
        (0..120u32).for_each(|i| fan.add_triangle(0, 1 + i, 1 + (i + 1) % 120));
        p.add(fan);
        p.add(Shape::closed_line(pts, Stroke::new(1.5, ACCENT)));
    }
    for q in v.cloud {
        p.rect_filled(Rect::from_center_size(map(*q), vec2(2.0, 2.0)), 0, ACCENT.gamma_multiply(0.55));
    }
    if let Some((center, r99)) = v.center {
        let m = map(center);
        p.line_segment([c, m], Stroke::new(1.2, TEXT));
        p.line_segment([m - vec2(5.0, 0.0), m + vec2(5.0, 0.0)], Stroke::new(1.2, TEXT));
        p.line_segment([m - vec2(0.0, 5.0), m + vec2(0.0, 5.0)], Stroke::new(1.2, TEXT));
        p.circle_stroke(m, r99 * v.zoom * r, Stroke::new(1.0, TEXT));
    }
    if let Some(ring) = v.ring {
        let pts: Vec<Pos2> = (0..=96).map(|i| polar(i as f32 * 3.75, ring)).collect();
        p.extend(Shape::dashed_line(&pts, Stroke::new(1.3, theme::WARN), 5.0, 4.0));
    }
    let n = v.trail.len() as f32;
    for (i, w) in v.trail.windows(2).enumerate() {
        p.line_segment([map(w[0]), map(w[1])], Stroke::new(1.8, ACCENT.gamma_multiply((i as f32 + 1.0) / n)));
    }
    let dot = map([v.current[0].clamp(-1.2, 1.2), v.current[1].clamp(-1.2, 1.2)]);
    p.circle(dot, 5.0, TEXT, Stroke::new(2.0, PLOT));
}

/// One lane of a [`lanes`] chart: label, value range and (trace, color) series.
pub struct Lane<'a> {
    pub label: &'a str,
    pub range: (f32, f32),
    pub series: Vec<(&'a Trace, Color32)>,
}

/// Stacked time lanes sharing one time axis (newest sample at the right edge).
pub fn lanes(ui: &mut Ui, size: Vec2, lanes: &[Lane<'_>], capacity: usize) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let lh = rect.height() / lanes.len() as f32;
    let (x0, x1) = (rect.left() + 64.0, rect.right() - 12.0);
    for (i, lane) in lanes.iter().enumerate() {
        let (top, bottom) = (rect.top() + i as f32 * lh, rect.top() + (i + 1) as f32 * lh);
        if i > 0 {
            p.hline(rect.x_range(), top, theme::hairline(PLOT_GRID));
        }
        p.text(pos2(rect.left() + 14.0, (top + bottom) / 2.0), Align2::LEFT_CENTER, lane.label, sans(12.0), TEXT_2);
        p.hline(x0..=x1, (top + bottom) / 2.0, theme::hairline(PLOT_GRID));
        let to_y = |v: f32| egui::remap_clamp(v, lane.range.0..=lane.range.1, bottom - 8.0..=top + 8.0);
        for (trace, color) in &lane.series {
            let start = capacity.saturating_sub(trace.0.len());
            let x = |k: usize| x0 + (start + k) as f32 / (capacity.max(2) - 1) as f32 * (x1 - x0);
            let pts = trace.0.iter().enumerate().map(|(k, v)| pos2(x(k), to_y(*v))).collect();
            p.add(Shape::line(pts, Stroke::new(1.5, *color)));
        }
    }
}

/// Signed per-sector error bars (circularity), positive up, full scale +-8%.
pub fn sector_bars(ui: &mut Ui, size: Vec2, errors: &[f32; SECTORS]) {
    let (rect, p) = surface(ui, size);
    let (mid, w) = (rect.center().y, rect.width() / SECTORS as f32);
    p.hline(rect.x_range(), mid, theme::hairline(PLOT_GRID));
    for (i, e) in errors.iter().enumerate() {
        let (x, h) = (rect.left() + i as f32 * w, (e / 8.0).clamp(-1.0, 1.0) * (rect.height() * 0.5 - 8.0));
        let col = if e.abs() > 5.0 { theme::WARN } else { ACCENT.gamma_multiply(0.75) };
        p.rect_filled(Rect::from_two_pos(pos2(x + 1.0, mid), pos2(x + w - 1.0, mid - h)), 0, col);
    }
}

/// Drift Score over time: left stick in the accent, right stick in grey.
pub fn history_chart(ui: &mut Ui, size: Vec2, entries: &[HistoryEntry]) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter_at(rect);
    let plot = Rect::from_min_max(rect.min + vec2(48.0, 14.0), rect.max - vec2(110.0, 30.0));
    let y_of = |score: f32| egui::remap(score, 40.0..=100.0, plot.bottom()..=plot.top());
    for (s, name) in [(100.0, ""), (90.0, "Healthy"), (75.0, "Trace"), (50.0, "Mild")] {
        p.hline(plot.x_range(), y_of(s), theme::hairline(PLOT_GRID));
        p.text(pos2(plot.left() - 10.0, y_of(s)), Align2::RIGHT_CENTER, s.to_string(), mono(10.0), TEXT_3);
        p.text(pos2(plot.right() + 10.0, y_of(s)), Align2::LEFT_CENTER, name, sans(12.0), TEXT_2);
    }
    if entries.is_empty() {
        p.text(plot.center(), Align2::CENTER_CENTER, "No rest tests yet", sans(13.0), TEXT_2);
        return;
    }
    let t0 = entries.iter().map(|e| e.when).min().unwrap_or(0) as f32;
    let t1 = (entries.iter().map(|e| e.when).max().unwrap_or(0) as f32).max(t0 + 1.0);
    for (stick, color) in [ACCENT, theme::SERIES_2].into_iter().enumerate() {
        let pts: Vec<Pos2> = entries
            .iter()
            .filter(|e| e.result.stick as usize == stick)
            .map(|e| pos2(egui::remap(e.when as f32, t0..=t1, plot.x_range()), y_of(f32::from(e.result.score))))
            .collect();
        for q in &pts {
            p.circle_filled(*q, 3.0, color);
        }
        p.add(Shape::line(pts, Stroke::new(1.8, color)));
    }
}

/// Paints one of the 20px line icons used in the sidebar (0 monitor .. 5 report).
fn icon(p: &Painter, c: Pos2, kind: usize, color: Color32) {
    let s = Stroke::new(1.5, color);
    let at = |x: f32, y: f32| c + vec2(x - 10.0, y - 10.0);
    let path = |pts: &[(f32, f32)]| Shape::line(pts.iter().map(|(x, y)| at(*x, *y)).collect(), s);
    match kind {
        0 => {
            _ = p.add(path(&[
                (2.5, 10.5),
                (5.5, 10.5),
                (7.5, 5.5),
                (11.0, 15.5),
                (13.5, 9.0),
                (15.0, 10.5),
                (18.0, 10.5),
            ]))
        }
        1 => {
            p.circle_stroke(at(10.0, 11.0), 6.5, s);
            p.add(path(&[(10.0, 11.0), (10.0, 7.5)]));
            p.add(path(&[(8.0, 2.5), (12.0, 2.5)]));
        }
        2 => {
            p.circle_stroke(at(10.0, 10.0), 7.0, s);
            p.circle_stroke(at(10.0, 10.0), 2.5, s);
        }
        3 => {
            p.add(path(&[(3.0, 6.0), (17.0, 6.0)]));
            p.add(path(&[(3.0, 14.0), (17.0, 14.0)]));
            p.circle(at(12.0, 6.0), 2.0, theme::SIDEBAR, s);
            p.circle(at(8.0, 14.0), 2.0, theme::SIDEBAR, s);
        }
        4 => {
            p.circle_stroke(at(10.0, 10.0), 7.0, s);
            p.add(path(&[(10.0, 6.0), (10.0, 10.0), (12.5, 12.0)]));
        }
        _ => {
            p.add(Shape::closed_line(
                vec![at(5.0, 2.5), at(11.5, 2.5), at(15.0, 6.0), at(15.0, 17.5), at(5.0, 17.5)],
                s,
            ));
            p.add(path(&[(7.5, 10.0), (12.5, 10.0)]));
            p.add(path(&[(7.5, 13.0), (12.5, 13.0)]));
        }
    }
}

/// Sidebar navigation row with icon and label. Returns true when clicked.
pub fn nav_item(ui: &mut Ui, index: usize, label: &str, badge: Option<&str>, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let p = ui.painter_at(rect);
    if selected || resp.hovered() {
        p.rect_filled(rect, 5, if selected { theme::SELECTED } else { theme::BG });
    }
    if selected {
        p.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 10.0), vec2(3.0, 16.0)), 1, ACCENT);
    }
    let fg = if selected { TEXT } else { TEXT_2 };
    icon(&p, rect.left_center() + vec2(20.0, 0.0), index, fg);
    p.text(rect.left_center() + vec2(42.0, 0.0), Align2::LEFT_CENTER, label, sans(13.5), fg);
    if let Some(b) = badge {
        p.text(rect.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, b, sans(11.5), theme::WARN);
    }
    resp.clicked()
}

/// Section caption inside a details pane.
pub fn section(ui: &mut Ui, title: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
    ui.painter().text(rect.left_bottom() + vec2(14.0, -6.0), Align2::LEFT_BOTTOM, title, sans(12.5), TEXT_2);
}

/// Property row: label on the left, value on the right, hairline on top. 32px like Windows lists.
pub fn row(ui: &mut Ui, label: &str, value: &str, dot: Option<Color32>) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
    let p = ui.painter_at(rect);
    p.hline(rect.x_range(), rect.top(), theme::hairline(LINE_SOFT));
    p.text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, label, sans(13.0), TEXT_2);
    let vr = p.text(rect.right_center() - vec2(14.0, 0.0), Align2::RIGHT_CENTER, value, mono(12.5), TEXT);
    if let Some(color) = dot {
        p.circle_filled(pos2(vr.left() - 10.0, rect.center().y), 4.0, color);
    }
    resp
}

/// Windows 11 style toggle switch. Returns true when clicked.
pub fn toggle(ui: &mut Ui, on: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 20.0), Sense::click());
    let p = ui.painter_at(rect.expand(1.0));
    if on {
        p.rect_filled(rect, 10, ACCENT);
        p.circle_filled(rect.right_center() - vec2(10.0, 0.0), 6.0, theme::ACCENT_INK);
    } else {
        p.rect_stroke(rect, 10, theme::hairline(TEXT_3), egui::StrokeKind::Inside);
        p.circle_filled(rect.left_center() + vec2(10.0, 0.0), 6.0, TEXT_2);
    }
    resp.clicked()
}
