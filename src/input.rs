//! High-rate XInput sampler running on its own thread.
//!
//! The thread polls `XInputGetState` for slots 0-3 roughly every `interval_us` microseconds,
//! normalizes the raw report and ships batches of [`Sample`]s to the UI over a channel.
//! Empty slots are re-probed only once per second, because querying a disconnected slot is
//! comparatively expensive inside XInput. Native HID backends (DualSense, Switch Pro) can be
//! added as a second producer of the same [`InputEvent`] stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::UI::Input::XboxController::{
    XINPUT_CAPABILITIES, XINPUT_CAPS_WIRELESS, XINPUT_FLAG_GAMEPAD, XINPUT_STATE, XInputGetCapabilities, XInputGetState,
};

/// Number of XInput user slots.
pub const SLOTS: usize = 4;
const ERROR_SUCCESS: u32 = 0;
const FLUSH_EVERY: Duration = Duration::from_millis(8);
const PROBE_EVERY: Duration = Duration::from_secs(1);

/// One normalized controller report. Sticks are -1.0..=1.0 with y up, triggers 0.0..=1.0.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// Seconds since the sampler started (monotonic, QPC-backed).
    pub t: f64,
    /// XInput packet number; it changes whenever the device sends a different report.
    pub packet: u32,
    pub left: [f32; 2],
    pub right: [f32; 2],
    pub lt: f32,
    pub rt: f32,
    /// Raw `wButtons` bit mask.
    pub buttons: u16,
}

/// Messages from the sampler thread.
#[derive(Debug)]
pub enum InputEvent {
    Connected { slot: usize, wireless: bool },
    Disconnected(usize),
    Batch { slot: usize, samples: Vec<Sample> },
}

/// Owns the background polling thread; dropping it stops the thread.
pub struct Sampler {
    rx: Receiver<InputEvent>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Sampler {
    /// Starts polling with the requested interval (clamped to 250 us .. 20 ms).
    pub fn spawn(interval_us: u64) -> Self {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let interval = Duration::from_micros(interval_us.clamp(250, 20_000));
        let handle = thread::Builder::new().name("xinput-sampler".into()).spawn(move || run(tx, flag, interval)).ok();
        Self { rx, stop, handle }
    }

    /// Non-blocking drain of everything received since the last frame.
    pub fn drain(&self) -> impl Iterator<Item = InputEvent> + '_ {
        self.rx.try_iter()
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn axis(v: i16) -> f32 {
    (f32::from(v) / 32767.0).clamp(-1.0, 1.0)
}

fn read_sample(slot: usize, t: f64) -> Option<Sample> {
    let mut s = XINPUT_STATE::default();
    // SAFETY: `s` is a valid, writable XINPUT_STATE for the duration of the call.
    if unsafe { XInputGetState(slot as u32, &mut s) } != ERROR_SUCCESS {
        return None;
    }
    let g = &s.Gamepad;
    Some(Sample {
        t,
        packet: s.dwPacketNumber,
        left: [axis(g.sThumbLX), axis(g.sThumbLY)],
        right: [axis(g.sThumbRX), axis(g.sThumbRY)],
        lt: f32::from(g.bLeftTrigger) / 255.0,
        rt: f32::from(g.bRightTrigger) / 255.0,
        buttons: g.wButtons.0,
    })
}

fn is_wireless(slot: usize) -> bool {
    let mut caps = XINPUT_CAPABILITIES::default();
    // SAFETY: `caps` is a valid, writable XINPUT_CAPABILITIES for the duration of the call.
    let rc = unsafe { XInputGetCapabilities(slot as u32, XINPUT_FLAG_GAMEPAD, &mut caps) };
    rc == ERROR_SUCCESS && caps.Flags.contains(XINPUT_CAPS_WIRELESS)
}

fn run(tx: Sender<InputEvent>, stop: Arc<AtomicBool>, interval: Duration) {
    let start = Instant::now();
    let mut connected = [false; SLOTS];
    let mut batches: [Vec<Sample>; SLOTS] = Default::default();
    let (mut last_probe, mut last_flush) = (start - PROBE_EVERY, start);

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        let probe = now.duration_since(last_probe) >= PROBE_EVERY;
        if probe {
            last_probe = now;
        }
        let t = now.duration_since(start).as_secs_f64();
        let scan = connected.map(|c| c || probe);
        for slot in (0..SLOTS).filter(|s| scan[*s]) {
            match read_sample(slot, t) {
                Some(sample) => {
                    if !connected[slot] {
                        connected[slot] = true;
                        let _ = tx.send(InputEvent::Connected { slot, wireless: is_wireless(slot) });
                    }
                    batches[slot].push(sample);
                }
                None if connected[slot] => {
                    connected[slot] = false;
                    let _ = tx.send(InputEvent::Disconnected(slot));
                }
                None => {}
            }
        }
        if now.duration_since(last_flush) >= FLUSH_EVERY {
            last_flush = now;
            for (slot, batch) in batches.iter_mut().enumerate().filter(|(_, b)| !b.is_empty()) {
                if tx.send(InputEvent::Batch { slot, samples: std::mem::take(batch) }).is_err() {
                    return;
                }
            }
        }
        thread::sleep(interval);
    }
}
