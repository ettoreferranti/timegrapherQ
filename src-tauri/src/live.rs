//! Live mode (M3): continuous capture plus periodic sliding-window analysis,
//! streamed to the UI as Tauri events.
//!
//! Two threads per session. The **capture thread** owns the `cpal` stream
//! (which is not `Send`) and parks until told to stop; its callback appends
//! mono samples to a shared buffer. The **analysis thread** wakes every
//! [`TICK`], re-analyses the last [`WINDOW_S`] seconds with the same batch
//! pipeline as record mode (so live numbers match recorded ones by
//! construction), and emits:
//!
//! - `live-beats`: newly seen beats as absolute timestamps, for the trace;
//! - `live-metrics`: a rolling measurement snapshot (rate, beat error, ...).
//!
//! The band scan runs every tick until a confident measurement appears, then
//! the winning band is locked for the rest of the session to keep ticks cheap
//! and the trace stable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, StreamTrait};
use serde::Serialize;
use tauri::Emitter;

use timegrapherq_core::{analyze_with_beats, AnalysisConfig};

use crate::audio::{find_device, open_input_stream, validate_movement_params};

/// Cadence of analysis/emission.
const TICK: Duration = Duration::from_millis(750);
/// Sliding analysis window (seconds): long enough for steady rate numbers,
/// short enough to react when the watch is repositioned.
const WINDOW_S: f64 = 15.0;
/// Samples retained in the shared buffer (seconds).
const KEEP_S: f64 = 25.0;
/// Don't attempt analysis before this much audio exists.
const WARMUP_S: f64 = 5.0;
/// Quality at which the band scan locks onto its current best band.
const LOCK_QUALITY: f64 = 0.5;

/// A running live session; dropping the handle does not stop it — call
/// [`SessionHandle::stop`].
pub struct SessionHandle {
    stop: Arc<AtomicBool>,
}

impl SessionHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// One beat for the trace.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct LiveBeat {
    /// Absolute time since session start (seconds).
    pub t_s: f64,
    /// Whether the rate fit kept this beat (rejected ones are noise dots).
    pub kept: bool,
}

/// The full classification of the current analysis window. Re-emitted every
/// tick so the trace repaints dots whose kept/rejected status changed as the
/// window slid (right after a disturbance, the newest beats start out as the
/// minority segment and would otherwise stay wrongly red forever).
#[derive(Debug, Clone, Serialize)]
pub struct LiveBeatsBatch {
    /// Absolute start of the analysis window (seconds since session start);
    /// the frontend replaces all dots at or after this time.
    pub window_start_s: f64,
    pub beats: Vec<LiveBeat>,
}

/// Rolling snapshot for the live readouts.
#[derive(Debug, Clone, Serialize)]
pub struct LiveMetrics {
    /// Seconds since the session started.
    pub elapsed_s: f64,
    /// Peak level of the last second (VU-style input check).
    pub peak_level: f32,
    /// Whether the window produced a measurement.
    pub measured: bool,
    pub rate_s_per_day: f64,
    pub rate_ci95_s_per_day: f64,
    pub beat_error_ms: f64,
    pub amplitude_deg: Option<f64>,
    pub quality: f64,
    pub periodicity: f64,
    pub band_center_hz: f64,
    pub beats_used: usize,
    pub beats_expected: usize,
    /// True until enough audio exists to analyse.
    pub warming_up: bool,
}

/// Start a live session: open the device, spawn the capture and analysis
/// threads, and return a handle that can stop them.
pub fn start(
    app: tauri::AppHandle,
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
) -> Result<SessionHandle, String> {
    validate_movement_params(bph, lift_angle_deg)?;

    let stop = Arc::new(AtomicBool::new(false));
    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    // The capture thread reports startup success/failure (and the sample rate)
    // through this channel so `start` can return a meaningful error.
    let (tx, rx) = std::sync::mpsc::channel::<Result<u32, String>>();

    {
        let stop = Arc::clone(&stop);
        let buf = Arc::clone(&buf);
        std::thread::spawn(move || capture_thread(device_name, buf, stop, tx));
    }

    let sample_rate = rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "audio capture did not start in time".to_string())??;

    {
        let stop = Arc::clone(&stop);
        let buf = Arc::clone(&buf);
        std::thread::spawn(move || analysis_thread(app, buf, stop, sample_rate, bph, lift_angle_deg));
    }

    Ok(SessionHandle { stop })
}

/// Owns the cpal stream for the lifetime of the session.
fn capture_thread(
    device_name: Option<String>,
    buf: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<Result<u32, String>>,
) {
    let started = (|| {
        let device = find_device(device_name.as_deref())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("could not read device config: {e}"))?;
        let sample_rate = config.sample_rate().0;
        let stream = open_input_stream(&device, &config, &buf)?;
        stream
            .play()
            .map_err(|e| format!("failed to start capture: {e}"))?;
        Ok::<_, String>((stream, sample_rate))
    })();

    match started {
        Ok((stream, sample_rate)) => {
            let _ = tx.send(Ok(sample_rate));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(100));
            }
            drop(stream);
        }
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            let _ = tx.send(Err(e));
        }
    }
}

/// Re-analyses the sliding window and emits events until stopped.
fn analysis_thread(
    app: tauri::AppHandle,
    buf: Arc<Mutex<Vec<f32>>>,
    stop: Arc<AtomicBool>,
    sample_rate: u32,
    bph: u32,
    lift_angle_deg: f64,
) {
    let sr = f64::from(sample_rate);
    let keep_n = (KEEP_S * sr) as usize;
    let window_n = (WINDOW_S * sr) as usize;

    let mut trimmed: u64 = 0; // samples discarded from the front of `buf`
    let mut locked_band: Option<f64> = None;

    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(TICK);
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // Snapshot the window and trim the shared buffer.
        let (window, window_start, total) = {
            let mut b = buf.lock().expect("audio buffer lock");
            if b.len() > keep_n {
                let excess = b.len() - keep_n;
                b.drain(..excess);
                trimmed += excess as u64;
            }
            let start = b.len().saturating_sub(window_n);
            let window: Vec<f32> = b[start..].to_vec();
            let window_start = trimmed + start as u64;
            (window, window_start, trimmed + b.len() as u64)
        };

        let elapsed_s = total as f64 / sr;
        let last_second = &window[window.len().saturating_sub(sample_rate as usize)..];
        let peak_level = last_second.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));

        if elapsed_s < WARMUP_S {
            let _ = app.emit("live-metrics", warmup_metrics(elapsed_s, peak_level));
            continue;
        }

        let mut cfg = AnalysisConfig::new(bph, lift_angle_deg);
        if let Some(hz) = locked_band {
            cfg.band_centers_hz = vec![hz];
        }

        match analyze_with_beats(&window, sample_rate, &cfg) {
            Some((m, dots)) => {
                if locked_band.is_none() && m.quality >= LOCK_QUALITY {
                    locked_band = Some(m.band_center_hz);
                }

                // Emit the whole window's dots each tick; the frontend
                // replaces everything at or after `window_start_s`, so a
                // beat's colour updates when a later, better-informed window
                // reclassifies it.
                let window_start_s = window_start as f64 / sr;
                let beats: Vec<LiveBeat> = dots
                    .iter()
                    .map(|d| LiveBeat {
                        t_s: window_start_s + d.onset_s,
                        kept: d.kept,
                    })
                    .collect();
                let _ = app.emit(
                    "live-beats",
                    LiveBeatsBatch {
                        window_start_s,
                        beats,
                    },
                );

                let _ = app.emit(
                    "live-metrics",
                    LiveMetrics {
                        elapsed_s,
                        peak_level,
                        measured: true,
                        rate_s_per_day: m.rate_s_per_day,
                        rate_ci95_s_per_day: m.rate_ci95_s_per_day,
                        beat_error_ms: m.beat_error_ms,
                        amplitude_deg: m.amplitude_deg,
                        quality: m.quality,
                        periodicity: m.periodicity,
                        band_center_hz: m.band_center_hz,
                        beats_used: m.beats_used,
                        beats_expected: m.beats_expected,
                        warming_up: false,
                    },
                );
            }
            None => {
                let _ = app.emit(
                    "live-metrics",
                    LiveMetrics {
                        measured: false,
                        ..warmup_metrics(elapsed_s, peak_level)
                    },
                );
            }
        }
    }
}

fn warmup_metrics(elapsed_s: f64, peak_level: f32) -> LiveMetrics {
    LiveMetrics {
        elapsed_s,
        peak_level,
        measured: false,
        rate_s_per_day: 0.0,
        rate_ci95_s_per_day: 0.0,
        beat_error_ms: 0.0,
        amplitude_deg: None,
        quality: 0.0,
        periodicity: 0.0,
        band_center_hz: 0.0,
        beats_used: 0,
        beats_expected: 0,
        warming_up: elapsed_s < WARMUP_S,
    }
}
