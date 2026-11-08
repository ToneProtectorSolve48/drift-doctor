//! Application state, sample routing and the six pages (sidebar, toolbar, content, status bar).

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Align, Align2, Color32, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2, vec2};

use crate::correction::{CorrectionBackend, CorrectionProfile, DeadzoneShape, NoopBackend, vigem_installed};
use crate::input::{InputEvent, SLOTS, Sample, Sampler};
use crate::stats::{Circularity, PollMeter, RestResult, Stick, TriggerProbe, analyze_rest, severity};
use crate::store::{self, HistoryEntry, Settings};
use crate::theme::{self, ACCENT, PANEL, TEXT, TEXT_2, TEXT_3, mono, sans};
use crate::widgets::{self, Lane, PolarView, Trace};

const TRAIL: usize = 110;
/// 1250 points every 4 ms: the last 5 seconds of input.
const TRACE_CAP: usize = 1250;
const TRACE_STEP: f64 = 0.004;
const PAGES: [&str; 6] = ["Monitor", "Rest test", "Gate & triggers", "Correction", "History", "Report"];
const STICKS: [&str; 2] = ["Left", "Right"];

/// Live per-slot signal state.
#[derive(Default)]
struct Live {
    last: Sample,
    trail: [VecDeque<[f32; 2]>; 2],
    /// LX, LY, RX, RY, LT, RT.
    traces: [Trace; 6],
    last_push: f64,
    poll: PollMeter,
    circ: [Circularity; 2],
    triggers: [TriggerProbe; 2],
}

impl Live {
    fn feed(&mut self, s: &Sample) {
        self.poll.feed(s.t, s.packet);
        for (i, v) in [s.left, s.right].into_iter().enumerate() {
            self.circ[i].feed(v);
            self.trail[i].push_back(v);
            if self.trail[i].len() > TRAIL {
                self.trail[i].pop_front();
            }
        }
        self.triggers[0].feed(s.lt);
        self.triggers[1].feed(s.rt);
        if s.t - self.last_push >= TRACE_STEP {
            self.last_push = s.t;
            let values = [s.left[0], s.left[1], s.right[0], s.right[1], s.lt, s.rt];
            self.traces.iter_mut().zip(values).for_each(|(trace, v)| trace.push(v, TRACE_CAP));
        }
        self.last = *s;
    }
}

enum RestState {
    Idle,
    Running { started: Option<f64>, elapsed: f64 },
    Done,
}

/// The Drift Doctor window.
pub struct App {
    sampler: Sampler,
    settings: Settings,
    history: Vec<HistoryEntry>,
    page: usize,
    /// `Some(wireless)` for connected slots.
    pads: [Option<bool>; SLOTS],
    live: [Live; SLOTS],
    /// Achieved sampler rate of the selected slot: (Hz, window start, samples in window).
    sampler_hz: (f32, f64, u32),
    rest: RestState,
    clouds: [Vec<[f32; 2]>; 2],
    results: [Option<RestResult>; 2],
    backend: Box<dyn CorrectionBackend>,
    vigem: bool,
    correcting: bool,
    status: String,
    report_tx: Sender<String>,
    report_rx: Receiver<String>,
    /// Screen rect of the report card, cropped out of a screenshot on export.
    report_rect: Option<egui::Rect>,
}

fn pct(v: f32) -> String {
    format!("{:.1}%", v * 100.0)
}

fn signed(v: [f32; 2]) -> String {
    format!("x {:+.1}  y {:+.1}", v[0] * 100.0, v[1] * 100.0)
}

fn button(ui: &mut Ui, label: &str, primary: bool) -> bool {
    let (fill, fg) = if primary { (ACCENT, theme::ACCENT_INK) } else { (theme::CONTROL, TEXT) };
    let b = egui::Button::new(RichText::new(label).font(sans(13.0)).color(fg)).fill(fill).corner_radius(5);
    ui.add(b.min_size(vec2(0.0, 32.0)).stroke(Stroke::new(1.0, if primary { ACCENT } else { theme::LINE }))).clicked()
}

/// Rounded panel with an optional 40px header (title left, value right).
fn card(ui: &mut Ui, size: Vec2, title: &str, value: &str, add: impl FnOnce(&mut Ui)) {
    let frame = Frame::new().fill(PANEL).stroke(Stroke::new(1.0, theme::LINE_SOFT)).corner_radius(8);
    frame.show(ui, |ui| {
        ui.vertical(|ui| {
            let inner = (size - vec2(2.0, 2.0)).max(Vec2::ZERO);
            ui.set_min_size(inner);
            ui.set_max_size(inner);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            if !title.is_empty() {
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
                ui.painter().text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, title, sans(13.5), TEXT);
                ui.painter().text(
                    rect.right_center() - vec2(14.0, 0.0),
                    Align2::RIGHT_CENTER,
                    value,
                    mono(12.0),
                    TEXT_2,
                );
            }
            add(ui);
        });
    });
}

/// Fixed-column table row; `header` rows use the tertiary text color.
fn table_row(ui: &mut Ui, widths: &[f32], cells: &[String], header: bool) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), if header { 34.0 } else { 38.0 }), Sense::hover());
    let p = ui.painter_at(rect);
    p.hline(rect.x_range(), rect.bottom() - 0.5, theme::hairline(if header { theme::LINE } else { theme::LINE_SOFT }));
    let mut x = rect.left() + 14.0;
    for (w, cell) in widths.iter().zip(cells) {
        let font = if header || cell.chars().next().is_some_and(char::is_alphabetic) { sans(13.0) } else { mono(12.5) };
        p.text(egui::pos2(x, rect.center().y), Align2::LEFT_CENTER, cell, font, if header { TEXT_3 } else { TEXT });
        x += w;
    }
}

fn polar_in(ui: &mut Ui, side: f32, view: &PolarView<'_>) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        widgets::polar_plot(ui, side, view);
    });
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let settings = Settings::load();
        let (report_tx, report_rx) = mpsc::channel();
        Self {
            sampler: Sampler::spawn(settings.sample_interval_us),
            history: store::load_history(),
            settings,
            page: 0,
            pads: [None; SLOTS],
            live: Default::default(),
            sampler_hz: (0.0, 0.0, 0),
            rest: RestState::Idle,
            clouds: Default::default(),
            results: [None, None],
            backend: Box::new(NoopBackend::default()),
            vigem: vigem_installed(),
            correcting: false,
            status: String::new(),
            report_tx,
            report_rx,
            report_rect: None,
        }
    }

    fn slot(&self) -> usize {
        self.settings.slot.min(SLOTS - 1)
    }

    fn device_name(&self) -> String {
        format!("XInput controller P{}", self.slot() + 1)
    }

    fn connection(&self) -> &'static str {
        match self.pads[self.slot()] {
            Some(true) => "Wireless · Connected",
            Some(false) => "Wired · Connected",
            None => "Not connected",
        }
    }

    /// The profile actually applied: the measured shape, or its bounding circle.
    fn profile(&self) -> CorrectionProfile {
        let c = &self.settings.correction;
        if self.settings.shaped {
            return c.clone();
        }
        CorrectionProfile {
            left: DeadzoneShape::circle(c.left.max_radius()),
            right: DeadzoneShape::circle(c.right.max_radius()),
        }
    }

    fn pump(&mut self) {
        let events: Vec<InputEvent> = self.sampler.drain().collect();
        for ev in events {
            match ev {
                InputEvent::Connected { slot, wireless } => {
                    self.pads[slot] = Some(wireless);
                    if self.pads[self.slot()].is_none() {
                        self.settings.slot = slot;
                    }
                }
                InputEvent::Disconnected(slot) => self.pads[slot] = None,
                InputEvent::Batch { slot, samples } => {
                    samples.iter().for_each(|s| self.live[slot].feed(s));
                    if slot == self.slot() {
                        self.route(&samples);
                    }
                }
            }
        }
        if let Ok(msg) = self.report_rx.try_recv() {
            self.status = msg;
        }
    }

    /// Feeds the selected pad's samples into the rate meter, rest test and correction backend.
    fn route(&mut self, samples: &[Sample]) {
        let (hz, since, count) = &mut self.sampler_hz;
        *count += samples.len() as u32;
        if let Some(t) = samples.last().map(|s| s.t).filter(|t| t - *since >= 1.0) {
            (*hz, *since, *count) = (*count as f32 / (t - *since) as f32, t, 0);
        }
        if let RestState::Running { started, elapsed } = &mut self.rest {
            for s in samples {
                *elapsed = s.t - *started.get_or_insert(s.t);
                self.clouds[0].push(s.left);
                self.clouds[1].push(s.right);
            }
            if *elapsed >= f64::from(self.settings.rest_seconds) {
                self.finish_rest();
            }
        }
        if self.correcting {
            let profile = self.profile();
            for s in samples {
                if let Err(e) = self.backend.submit(&profile.correct(s)) {
                    (self.status, self.correcting) = (e, false);
                    break;
                }
            }
        }
    }

    fn finish_rest(&mut self) {
        self.rest = RestState::Done;
        let (margin, device) = (self.settings.deadzone_margin, self.device_name());
        for (i, stick) in [Stick::Left, Stick::Right].into_iter().enumerate() {
            self.results[i] = analyze_rest(stick, &self.clouds[i], margin);
            self.history.extend(self.results[i].as_ref().map(|r| HistoryEntry::from_result(&device, r)));
        }
        self.settings.correction.left = DeadzoneShape::from_cloud(&self.clouds[0], 0.99, margin);
        self.settings.correction.right = DeadzoneShape::from_cloud(&self.clouds[1], 0.99, margin);
        self.status = match self.settings.save().and(store::save_history(&self.history)) {
            Ok(()) => "Rest test saved to history".into(),
            Err(e) => format!("Could not save: {e}"),
        };
    }

    fn toggle_correction(&mut self) {
        if self.correcting {
            self.backend.stop();
            self.correcting = false;
            return;
        }
        match self.backend.start(self.slot()) {
            Ok(()) => self.correcting = true,
            Err(e) => self.status = e,
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.pump();
        let ctx = ui.ctx().clone();
        let keys =
            [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6];
        if let Some(i) = keys.iter().position(|k| ctx.input(|inp| inp.key_pressed(*k))) {
            self.page = i;
        }
        let side = Frame::new().fill(theme::SIDEBAR).inner_margin(Margin::symmetric(10, 8));
        egui::Panel::left("side").exact_size(232.0).resizable(false).frame(side).show(ui, |ui| self.sidebar(ui));
        let bar = Frame::new().fill(theme::SIDEBAR).inner_margin(Margin::symmetric(14, 4));
        egui::Panel::bottom("status").exact_size(26.0).frame(bar).show(ui, |ui| self.status_bar(ui));
        let tools = Frame::new().fill(theme::BG).inner_margin(Margin::symmetric(16, 10));
        egui::Panel::top("toolbar").exact_size(52.0).frame(tools).show(ui, |ui| self.toolbar(ui));
        let body = Frame::new().fill(theme::BG).inner_margin(Margin::same(16));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(12.0, 12.0);
            match self.page {
                0 => self.monitor(ui),
                1 => self.rest_page(ui),
                2 => self.gate(ui),
                3 => self.correction(ui),
                4 => self.history_page(ui),
                _ => self.report(ui),
            }
        });
        ctx.request_repaint();
    }
}

impl App {
    fn sidebar(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::hover());
        ui.painter().rect_filled(rect, 6, theme::BG);
        ui.painter().text(
            rect.left_center() + vec2(12.0, -8.0),
            Align2::LEFT_CENTER,
            self.device_name(),
            sans(13.5),
            TEXT,
        );
        ui.painter().text(
            rect.left_center() + vec2(12.0, 10.0),
            Align2::LEFT_CENTER,
            self.connection(),
            sans(12.0),
            TEXT_2,
        );
        ui.add_space(10.0);
        let badge = self.results[0].as_ref().filter(|r| r.score < 90).map(|r| format!("L {}", r.score));
        for (i, label) in PAGES.iter().enumerate() {
            if widgets::nav_item(ui, i, label, badge.as_deref().filter(|_| i == 1), self.page == i) {
                self.page = i;
            }
        }
        ui.add_space(16.0);
        ui.label(RichText::new("   Controllers").font(sans(11.5)).color(TEXT_3));
        ui.add_space(4.0);
        for slot in 0..SLOTS {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
            let p = ui.painter_at(rect);
            if slot == self.slot() || resp.hovered() {
                p.rect_filled(rect, 5, theme::SELECTED);
            }
            let on = self.pads[slot].is_some();
            p.circle_filled(rect.left_center() + vec2(14.0, 0.0), 4.0, if on { ACCENT } else { TEXT_3 });
            let name = if on { "XInput controller" } else { "Empty slot" };
            p.text(
                rect.left_center() + vec2(28.0, 0.0),
                Align2::LEFT_CENTER,
                name,
                sans(13.0),
                if on { TEXT_2 } else { TEXT_3 },
            );
            p.text(
                rect.right_center() - vec2(10.0, 0.0),
                Align2::RIGHT_CENTER,
                format!("P{}", slot + 1),
                sans(12.0),
                TEXT_3,
            );
            if resp.clicked() && slot != self.settings.slot {
                self.settings.slot = slot;
                if let Err(e) = self.settings.save() {
                    self.status = format!("Could not save settings: {e}");
                }
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            let small = |s: String| RichText::new(s).font(sans(12.0)).color(TEXT_2);
            ui.label(small(format!("{} · {}", self.device_name(), self.connection())));
            ui.label(small(format!("{:.0} Hz", self.live[self.slot()].poll.hz())));
            ui.label(small(format!("Sampler {:.0} Hz", self.sampler_hz.0)));
            ui.label(small(format!("ViGEmBus {}", if self.vigem { "installed" } else { "not installed" })));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let text = if !self.status.is_empty() {
                    self.status.clone()
                } else if self.correcting {
                    "Correction on".into()
                } else {
                    "Correction off".into()
                };
                ui.label(small(text));
            });
        });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal_centered(|ui| {
            ui.label(RichText::new(PAGES[self.page]).font(sans(15.5)).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match self.page {
                1 => {
                    let running = matches!(self.rest, RestState::Running { .. });
                    if button(ui, if running { "Stop" } else { "Start test" }, !running) {
                        self.rest =
                            if running { RestState::Idle } else { RestState::Running { started: None, elapsed: 0.0 } };
                        (self.clouds, self.results) = (Default::default(), [None, None]);
                    }
                }
                2 if button(ui, "Reset", false) => {
                    let live = &mut self.live[self.settings.slot.min(SLOTS - 1)];
                    (live.circ, live.triggers) = Default::default();
                }
                3 if button(ui, if self.correcting { "Stop correction" } else { "Apply" }, !self.correcting) => {
                    self.toggle_correction();
                }
                5 if button(ui, "Export report", true) => self.export_report(ui),
                _ => {}
            });
        });
    }

    fn view<'a>(&'a self, stick: usize, trail: &'a [[f32; 2]], zoom: f32) -> PolarView<'a> {
        let live = &self.live[self.slot()];
        PolarView {
            current: if stick == 0 { live.last.left } else { live.last.right },
            trail,
            zoom,
            ..Default::default()
        }
    }

    fn trail(&self, stick: usize) -> Vec<[f32; 2]> {
        self.live[self.slot()].trail[stick].iter().copied().collect()
    }

    fn monitor(&mut self, ui: &mut Ui) {
        let live = &self.live[self.slot()];
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for (stick, name) in STICKS.into_iter().enumerate() {
                        let trail = self.trail(stick);
                        let mut v = self.view(stick, &trail, 1.0);
                        v.ring = self.results[stick].as_ref().map(|r| r.offset + r.r99);
                        card(ui, vec2(346.0, 384.0), &format!("{name} stick"), &signed(v.current), |ui| {
                            polar_in(ui, 330.0, &v)
                        });
                    }
                });
                let t = &live.traces;
                let lanes = [
                    Lane { label: "Left", range: (-1.0, 1.0), series: vec![(&t[1], theme::SERIES_2), (&t[0], ACCENT)] },
                    Lane {
                        label: "Right",
                        range: (-1.0, 1.0),
                        series: vec![(&t[3], theme::SERIES_2), (&t[2], ACCENT)],
                    },
                    Lane {
                        label: "LT / RT",
                        range: (0.0, 1.0),
                        series: vec![(&t[4], theme::SERIES_2), (&t[5], ACCENT)],
                    },
                ];
                card(ui, vec2(704.0, ui.available_height()), "Live input", "last 5 s", |ui| {
                    widgets::lanes(ui, ui.available_size(), &lanes, TRACE_CAP);
                });
            });
            card(ui, vec2(ui.available_width(), ui.available_height()), "", "", |ui| {
                for (i, name) in STICKS.into_iter().enumerate() {
                    widgets::section(ui, &format!("{name} stick"));
                    match &self.results[i] {
                        Some(r) => {
                            let dot = if r.score >= 90 { ACCENT } else { theme::WARN };
                            widgets::row(ui, "Drift score", &format!("{} · {}", r.score, severity(r.score)), Some(dot));
                            widgets::row(
                                ui,
                                "Center offset",
                                &format!("{} at {:.0}°", pct(r.offset), r.angle_deg),
                                None,
                            );
                            widgets::row(ui, "Rest noise (r99)", &pct(r.r99), None);
                            widgets::row(ui, "Suggested deadzone", &pct(r.deadzone), None);
                        }
                        None => _ = widgets::row(ui, "Drift score", "Run a rest test", None),
                    }
                }
                widgets::section(ui, "Triggers");
                widgets::row(ui, "LT", &format!("{:.0}", live.last.lt * 255.0), None);
                widgets::row(ui, "RT", &format!("{:.0}", live.last.rt * 255.0), None);
                widgets::section(ui, "Connection");
                widgets::row(ui, "Report rate", &format!("{:.0} Hz", live.poll.hz()), None);
                widgets::row(ui, "Report interval", &format!("{:.2} ms", live.poll.median_interval_ms()), None);
                widgets::row(ui, "Buttons", &format!("0x{:04X}", live.last.buttons), None);
            });
        });
    }

    fn rest_page(&mut self, ui: &mut Ui) {
        let (msg, progress) = match self.rest {
            RestState::Running { elapsed, .. } => {
                let total = f64::from(self.settings.rest_seconds);
                (
                    format!(
                        "Recording. Keep your hands off the controller.   {:.1} s left",
                        (total - elapsed).max(0.0)
                    ),
                    (elapsed / total) as f32,
                )
            }
            RestState::Done if self.results.iter().all(Option::is_none) => {
                ("Not enough samples. Is a controller connected?".into(), 0.0)
            }
            RestState::Done => ("Test complete. Results saved to history.".into(), 0.0),
            RestState::Idle => ("Put the controller on a flat surface and don't touch the sticks.".into(), 0.0),
        };
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        ui.painter().rect(
            rect,
            6,
            Color32::from_rgb(0x26, 0x31, 0x3A),
            Stroke::new(1.0, Color32::from_rgb(0x2F, 0x3E, 0x4A)),
            egui::StrokeKind::Inside,
        );
        ui.painter().text(rect.left_center() + vec2(14.0, 0.0), Align2::LEFT_CENTER, msg, sans(13.0), TEXT);
        let bar = egui::Rect::from_min_size(
            rect.left_bottom() - vec2(0.0, 3.0),
            vec2(rect.width() * progress.clamp(0.0, 1.0), 3.0),
        );
        ui.painter().rect_filled(bar, 0, ACCENT);
        ui.horizontal(|ui| {
            for (stick, name) in STICKS.into_iter().enumerate() {
                let trail = self.trail(stick);
                let mut v = self.view(stick, &trail, 12.5);
                v.cloud = &self.clouds[stick];
                if let Some(r) = &self.results[stick] {
                    (v.center, v.ring) = (Some((r.center, r.r99)), Some(r.offset + r.r99));
                }
                card(ui, vec2(502.0, 370.0), &format!("{name} stick"), "scale ±8%", |ui| polar_in(ui, 322.0, &v));
            }
        });
        let widths = [110.0, 110.0, 150.0, 120.0, 100.0, 110.0, 120.0, 160.0];
        card(ui, vec2(ui.available_width(), ui.available_height()), "", "", |ui| {
            let head = [
                "Stick",
                "Drift score",
                "Diagnosis",
                "Center offset",
                "Direction",
                "Noise (r99)",
                "Max excursion",
                "Suggested deadzone",
            ];
            table_row(ui, &widths, &head.map(String::from), true);
            for (i, name) in STICKS.into_iter().enumerate() {
                let cells = match &self.results[i] {
                    Some(r) => vec![
                        name.into(),
                        r.score.to_string(),
                        severity(r.score).into(),
                        pct(r.offset),
                        format!("{:.0}°", r.angle_deg),
                        pct(r.r99),
                        pct(r.max_dist),
                        pct(r.deadzone),
                    ],
                    None => vec![name.into(), "-".into()],
                };
                table_row(ui, &widths, &cells, false);
            }
            ui.add_space(12.0);
            let samples = self.clouds[0].len();
            ui.label(RichText::new(format!("    Samples: {samples} per stick")).font(sans(12.0)).color(TEXT_3));
        });
    }

    fn gate(&mut self, ui: &mut Ui) {
        let live = &self.live[self.slot()];
        ui.horizontal_top(|ui| {
            for (stick, name) in STICKS.into_iter().enumerate() {
                let circ = &live.circ[stick];
                let trail = self.trail(stick);
                let mut v = self.view(stick, &trail, 0.86);
                v.peaks = Some(circ.peaks());
                let avg = format!("avg {:.1}%", circ.average_error());
                card(ui, vec2(346.0, 482.0), &format!("{name} stick gate"), &avg, |ui| {
                    polar_in(ui, 330.0, &v);
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        widgets::sector_bars(ui, vec2(318.0, 76.0), &circ.sector_errors());
                    });
                    ui.add_space(8.0);
                    widgets::row(ui, "Suggested outer deadzone", &pct(circ.outer_deadzone()), None);
                });
            }
            card(ui, vec2(ui.available_width(), ui.available_height()), "Triggers", "", |ui| {
                for (probe, name) in live.triggers.iter().zip(["LT", "RT"]) {
                    widgets::row(
                        ui,
                        &format!("{name} rest floor / travel"),
                        &format!("{} / {}", pct(probe.floor()), pct(probe.peak())),
                        None,
                    );
                    widgets::row(
                        ui,
                        &format!("{name} steps seen"),
                        &format!("{:.0}%", probe.resolution() * 100.0),
                        None,
                    );
                }
                widgets::section(ui, "Polling");
                widgets::row(ui, "Report rate", &format!("{:.0} Hz", live.poll.hz()), None);
                widgets::row(ui, "Median interval", &format!("{:.2} ms", live.poll.median_interval_ms()), None);
                widgets::row(ui, "Sampler", &format!("{:.0} Hz", self.sampler_hz.0), None);
            });
        });
    }

    fn correction(&mut self, ui: &mut Ui) {
        let profile = self.profile();
        let raw = self.live[self.slot()].last;
        let out = profile.correct(&raw);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                let setting = |ui: &mut Ui, title: &str, sub: &str, add: &mut dyn FnMut(&mut Ui)| {
                    card(ui, vec2(600.0, 64.0), "", "", |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.add_space(16.0);
                            ui.vertical(|ui| {
                                ui.add_space(14.0);
                                ui.label(RichText::new(title).font(sans(13.5)).color(TEXT));
                                ui.label(RichText::new(sub).font(sans(12.0)).color(TEXT_2));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(16.0);
                                add(ui);
                            });
                        });
                    });
                };
                let mut toggle = false;
                setting(
                    ui,
                    "Anti-drift correction",
                    "Games read a virtual Xbox 360 controller with the drift removed",
                    &mut |ui| {
                        toggle = widgets::toggle(ui, self.correcting);
                    },
                );
                let mut shaped = self.settings.shaped;
                setting(ui, "Deadzone shape", "Shaped follows your measured drift cloud, per 5°", &mut |ui| {
                    ui.selectable_value(&mut shaped, false, "Circle");
                    ui.selectable_value(&mut shaped, true, "Shaped");
                });
                let (l, r) = (pct(profile.left.max_radius()), pct(profile.right.max_radius()));
                setting(ui, "Inner deadzone", "From your last rest test", &mut |ui| {
                    ui.label(RichText::new(format!("L {l}   R {r}")).font(mono(12.5)).color(TEXT_2));
                });
                let driver = if self.vigem { "ViGEmBus installed" } else { "ViGEmBus not installed" };
                setting(ui, "Virtual controller driver", driver, &mut |ui| {
                    ui.label(RichText::new(self.backend.name()).font(sans(12.5)).color(TEXT_2));
                });
                if toggle {
                    self.toggle_correction();
                }
                if shaped != self.settings.shaped {
                    self.settings.shaped = shaped;
                    if let Err(e) = self.settings.save() {
                        self.status = format!("Could not save settings: {e}");
                    }
                }
            });
            let trail = self.trail(0);
            let mut v = self.view(0, &trail, 10.0);
            (v.cloud, v.shape) = (&self.clouds[0], Some(&profile.left));
            let shape = &profile.left;
            let freed = (1.0 - shape.area_fraction() / shape.max_radius().powi(2).max(1e-6)) * 100.0;
            card(ui, vec2(ui.available_width(), ui.available_height()), "Left stick deadzone", "scale ±10%", |ui| {
                polar_in(ui, 380.0, &v);
                ui.add_space(12.0);
                widgets::row(ui, "Dead area", &format!("{:.2}%", shape.area_fraction() * 100.0), None);
                widgets::row(ui, "Precision recovered", &format!("{freed:.0}%"), None);
                widgets::row(ui, "Raw input", &signed(raw.left), None);
                widgets::row(ui, "Corrected output", &signed(out.left), None);
                widgets::row(ui, "Frames sent", &self.backend.frames().to_string(), None);
            });
        });
    }

    fn history_page(&mut self, ui: &mut Ui) {
        card(ui, vec2(ui.available_width(), 316.0), "Drift score", "", |ui| {
            widgets::history_chart(ui, ui.available_size(), &self.history);
        });
        let widths = [200.0, 90.0, 80.0, 160.0, 110.0, 110.0, 110.0, 110.0];
        card(ui, vec2(ui.available_width(), ui.available_height()), "", "", |ui| {
            let head = ["Date", "Stick", "Score", "Diagnosis", "Offset", "Direction", "Noise (r99)", "Deadzone"];
            table_row(ui, &widths, &head.map(String::from), true);
            for e in self.history.iter().rev().take(7) {
                let r = &e.result;
                let days = store::now_unix().saturating_sub(e.when) / 86_400;
                let cells = [
                    format!("{days} days ago"),
                    STICKS[r.stick as usize].into(),
                    r.score.to_string(),
                    severity(r.score).into(),
                    pct(r.offset),
                    format!("{:.0}°", r.angle_deg),
                    pct(r.r99),
                    pct(r.deadzone),
                ];
                table_row(ui, &widths, &cells, false);
            }
        });
    }

    /// The printed report card; its rect is remembered so "Export report" can crop a screenshot.
    fn report(&mut self, ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(vec2(540.0, 600.0), Sense::hover());
        self.report_rect = Some(rect);
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0, theme::PAPER);
        for i in 0..76 {
            p.vline(rect.left() + i as f32 * 8.0, rect.y_range(), theme::hairline(theme::PAPER_GRID));
            p.hline(rect.x_range(), rect.top() + i as f32 * 8.0, theme::hairline(theme::PAPER_GRID));
        }
        let (ink, live) = (theme::PAPER_INK, &self.live[self.slot()]);
        let line = |dy: f32, s: String, font| p.text(rect.left_top() + vec2(28.0, dy), Align2::LEFT_TOP, s, font, ink);
        line(24.0, "Controller diagnosis report".into(), sans(16.0));
        line(52.0, format!("{} · {}", self.device_name(), self.connection()), sans(12.5));
        for (i, r) in self.results.iter().enumerate() {
            let dy = 100.0 + i as f32 * 90.0;
            line(dy, format!("{} stick", STICKS[i]), sans(11.0));
            let Some(r) = r else {
                line(dy + 18.0, "Not measured: run the rest test first".into(), sans(16.0));
                continue;
            };
            line(dy + 18.0, r.diagnosis(), sans(17.0));
            let (off, r99, dz, circ) = (pct(r.offset), pct(r.r99), pct(r.deadzone), live.circ[i].average_error());
            line(
                dy + 44.0,
                format!("score {}   offset {off}   noise r99 {r99}   deadzone {dz}   gate error {circ:.1}%", r.score),
                mono(11.0),
            );
        }
        line(290.0, format!("Report rate {:.0} Hz   sampler {:.0} Hz", live.poll.hz(), self.sampler_hz.0), mono(11.0));
        line(560.0, "Raw XInput reports, no deadzone applied. Generated by Drift Doctor.".into(), sans(11.0));
    }

    fn export_report(&mut self, ui: &mut Ui) {
        let Some(rect) = self.report_rect else { return };
        let (tx, ppp) = (self.report_tx.clone(), ui.ctx().pixels_per_point());
        ui.ctx().request_screenshot(move |img| {
            let path = store::report_path();
            let msg = match store::write_bmp(&path, &img.region(&rect, Some(ppp))) {
                Ok(()) => format!("Report saved: {}", path.display()),
                Err(e) => format!("Report failed: {e}"),
            };
            let _ = tx.send(msg);
        });
    }
}
