//! Mic monitor (hardware bring-up aid): a fast input-level meter plus an
//! optional audible passthrough, independent of the measurement pipeline.
//!
//! One thread owns the cpal input stream (its callback accumulates peak/RMS
//! into shared meter state and, when listening, pushes mono samples to a
//! bounded ring buffer) and, when listening, an output stream that drains the
//! ring with a live gain and zero-order (sample-and-hold) resampling. The same
//! thread parks in an emit loop, sending a `mic-level` event ~30×/s.
//!
//! This is deliberately separate from [`crate::live`]: it answers "is the mic
//! alive, and where's the best clip position?" with the lowest possible latency
//! and no analysis. Only one capture activity runs at a time (the commands stop
//! the live session and vice versa).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use serde::Serialize;
use tauri::Emitter;

use crate::audio::find_device;

/// Level-meter emission cadence (~30 Hz).
const EMIT: Duration = Duration::from_millis(33);
/// Cap on the passthrough buffer (seconds), bounding monitoring latency.
const MAX_LATENCY_S: f64 = 0.15;

/// Handle to a running monitor session.
pub struct MonitorHandle {
    stop: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
}

impl MonitorHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Adjust the audible-passthrough gain live (linear).
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }
}

/// Peak/RMS accumulated since the last meter emission.
#[derive(Default)]
struct Meter {
    peak: f32,
    sumsq: f64,
    count: u64,
}

impl Meter {
    fn snapshot(&self) -> MicLevel {
        let rms = if self.count > 0 {
            (self.sumsq / self.count as f64).sqrt() as f32
        } else {
            0.0
        };
        MicLevel {
            peak: self.peak,
            rms,
            db: 20.0 * self.peak.max(1e-6).log10(),
            clipping: self.peak >= 0.99,
        }
    }
}

/// One level reading sent to the UI as the `mic-level` event.
#[derive(Debug, Clone, Serialize)]
pub struct MicLevel {
    /// Linear peak since the last emit (0–1, may exceed 1 on clipping).
    pub peak: f32,
    /// Linear RMS since the last emit.
    pub rms: f32,
    /// Peak expressed in dBFS (≤ 0).
    pub db: f32,
    /// Whether the peak reached digital full scale.
    pub clipping: bool,
}

/// Start a monitor session on `device_name` (or the default input). When
/// `listen` is set, the input is also played to the default output device at
/// `gain`. Returns once capture has started (or with the startup error).
pub fn start(
    app: tauri::AppHandle,
    device_name: Option<String>,
    listen: bool,
    gain: f32,
) -> Result<MonitorHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let gain = Arc::new(AtomicU32::new(gain.to_bits()));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();

    {
        let stop = Arc::clone(&stop);
        let gain = Arc::clone(&gain);
        std::thread::spawn(move || monitor_thread(app, device_name, listen, stop, gain, tx));
    }

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "mic monitor did not start in time".to_string())??;
    Ok(MonitorHandle { stop, gain })
}

/// Owns the input (and optional output) stream and runs the emit loop.
fn monitor_thread(
    app: tauri::AppHandle,
    device_name: Option<String>,
    listen: bool,
    stop: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
    tx: Sender<Result<(), String>>,
) {
    let meter = Arc::new(Mutex::new(Meter::default()));
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));

    let setup = (|| {
        let device = find_device(device_name.as_deref())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("could not read device config: {e}"))?;
        let in_sr = config.sample_rate().0;
        let ring_cap = (MAX_LATENCY_S * f64::from(in_sr)) as usize;

        let input = build_input(
            &device,
            &config,
            &meter,
            listen.then(|| Arc::clone(&ring)),
            ring_cap,
        )?;
        input
            .play()
            .map_err(|e| format!("failed to start capture: {e}"))?;

        // Audible passthrough is best-effort: if no output device is available
        // the meter still works, so we warn rather than fail the session.
        let output = if listen {
            match open_output(in_sr, &ring, &gain) {
                Ok(o) => Some(o),
                Err(e) => {
                    eprintln!("mic monitor: audible passthrough unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        Ok::<_, String>((input, output))
    })();

    match setup {
        Ok((_input, _output)) => {
            let _ = tx.send(Ok(()));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(EMIT);
                let level = {
                    let mut m = meter.lock().expect("meter lock");
                    let level = m.snapshot();
                    *m = Meter::default();
                    level
                };
                let _ = app.emit("mic-level", level);
            }
            // `_input` / `_output` dropped here, stopping the streams.
        }
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            let _ = tx.send(Err(e));
        }
    }
}

/// Build the input stream, dispatching on sample format.
fn build_input(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    meter: &Arc<Mutex<Meter>>,
    ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    ring_cap: usize,
) -> Result<cpal::Stream, String> {
    let channels = config.channels() as usize;
    let cfg: cpal::StreamConfig = config.config();
    match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_input_t::<f32>(device, &cfg, channels, meter, ring, ring_cap)
        }
        cpal::SampleFormat::I16 => {
            build_input_t::<i16>(device, &cfg, channels, meter, ring, ring_cap)
        }
        cpal::SampleFormat::U16 => {
            build_input_t::<u16>(device, &cfg, channels, meter, ring, ring_cap)
        }
        other => Err(format!("unsupported sample format: {other:?}")),
    }
}

/// Input callback: downmix to mono, update the meter, and (when listening)
/// feed the bounded passthrough ring buffer.
fn build_input_t<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    meter: &Arc<Mutex<Meter>>,
    ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    ring_cap: usize,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let meter = Arc::clone(meter);
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut m = meter.lock().expect("meter lock");
                let mut r = ring.as_ref().map(|r| r.lock().expect("ring lock"));
                for frame in data.chunks(channels) {
                    let s: f32 =
                        frame.iter().map(|&x| f32::from_sample_(x)).sum::<f32>() / channels as f32;
                    m.peak = m.peak.max(s.abs());
                    m.sumsq += f64::from(s) * f64::from(s);
                    m.count += 1;
                    if let Some(q) = r.as_mut() {
                        q.push_back(s);
                    }
                }
                if let Some(q) = r.as_mut() {
                    while q.len() > ring_cap {
                        q.pop_front();
                    }
                }
            },
            |e| eprintln!("mic monitor input error: {e}"),
            None,
        )
        .map_err(|e| format!("failed to open input stream: {e}"))
}

/// Open the default output device and play back the ring buffer with gain.
fn open_output(
    in_sr: u32,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    gain: &Arc<AtomicU32>,
) -> Result<cpal::Stream, String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("could not read output config: {e}"))?;
    let out_sr = config.sample_rate().0;
    let channels = config.channels() as usize;
    // Input samples to advance per output sample (zero-order resampling).
    let ratio = f64::from(in_sr) / f64::from(out_sr);
    let cfg: cpal::StreamConfig = config.config();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            build_output_t::<f32>(&device, &cfg, channels, ratio, ring, gain)
        }
        cpal::SampleFormat::I16 => {
            build_output_t::<i16>(&device, &cfg, channels, ratio, ring, gain)
        }
        cpal::SampleFormat::U16 => {
            build_output_t::<u16>(&device, &cfg, channels, ratio, ring, gain)
        }
        other => Err(format!("unsupported output format: {other:?}")),
    }?;
    stream
        .play()
        .map_err(|e| format!("failed to start playback: {e}"))?;
    Ok(stream)
}

/// Output callback: drain the ring with zero-order resampling and gain,
/// writing the same mono value to every channel. Underruns hold the last
/// sample (good enough for a monitor).
fn build_output_t<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    ratio: f64,
    ring: &Arc<Mutex<VecDeque<f32>>>,
    gain: &Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ring = Arc::clone(ring);
    let gain = Arc::clone(gain);
    let mut frac = 0.0_f64;
    let mut held = 0.0_f32;
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
                let g = f32::from_bits(gain.load(Ordering::Relaxed));
                let mut q = ring.lock().expect("ring lock");
                for frame in out.chunks_mut(channels) {
                    frac += ratio;
                    while frac >= 1.0 {
                        if let Some(s) = q.pop_front() {
                            held = s;
                        }
                        frac -= 1.0;
                    }
                    let v = T::from_sample_(held * g);
                    for ch in frame.iter_mut() {
                        *ch = v;
                    }
                }
            },
            |e| eprintln!("mic monitor output error: {e}"),
            None,
        )
        .map_err(|e| format!("failed to open output stream: {e}"))
}
