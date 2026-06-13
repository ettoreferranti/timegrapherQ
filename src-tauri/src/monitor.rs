//! Mic monitor (hardware bring-up aid): a fast input-level meter, a scrolling
//! spectrogram, optional audible passthrough, and optional band-isolation —
//! all independent of the measurement pipeline.
//!
//! A watch tick is a faint, narrow-band transient buried in broadband mic
//! noise, so a single broadband meter can't show it. The monitor therefore
//! also runs a constant-Q **filterbank** (a bank of the core's band-pass
//! biquads) to produce a spectrogram column each frame — a ticking watch
//! appears as periodic vertical streaks at its band — and an optional
//! **isolation** band-pass that feeds the meter and the audible passthrough so
//! the tick stands out from the noise.
//!
//! Architecture: the capture callback just appends raw mono samples to a
//! buffer (kept light). A ~40 Hz processing thread drains the buffer, runs the
//! filterbank and isolation filter (whose biquad state persists across
//! frames), updates the meter, feeds the passthrough ring, and emits events:
//!
//! - `mic-spectrum-config` (once): the filterbank band centres, for axis labels;
//! - `mic-spectrum`: a column of per-band levels (dBFS);
//! - `mic-level`: the peak/RMS meter (of the isolated signal when isolating).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use serde::Serialize;
use tauri::Emitter;

use timegrapherq_core::dsp::Biquad;

use crate::audio::{find_device, open_input_stream};

/// Processing/emission cadence (~40 Hz): smooth spectrogram and passthrough.
const FRAME: Duration = Duration::from_millis(25);
/// Number of constant-Q filterbank bands (spectrogram rows).
const BANDS: usize = 48;
/// Lowest filterbank centre (Hz).
const BAND_LO_HZ: f64 = 300.0;
/// Highest filterbank centre as a fraction of the sample rate (below Nyquist).
const BAND_HI_FRAC: f64 = 0.45;
/// Hard cap on the highest band (Hz); ticks live well below this.
const BAND_HI_MAX_HZ: f64 = 20_000.0;
/// Q of the isolation band-pass (moderately narrow to reject neighbouring noise).
const ISO_Q: f64 = 6.0;
/// Cap on the passthrough buffer (seconds), bounding monitoring latency.
const MAX_LATENCY_S: f64 = 0.15;

/// Handle to a running monitor session.
pub struct MonitorHandle {
    stop: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
    iso_hz: Arc<AtomicU32>,
}

impl MonitorHandle {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Adjust the audible-passthrough gain live (linear).
    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Set the isolation band-pass centre (Hz); 0 disables isolation.
    pub fn set_iso_hz(&self, hz: f32) {
        self.iso_hz.store(hz.to_bits(), Ordering::Relaxed);
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

/// One-time spectrogram description (`mic-spectrum-config` event).
#[derive(Debug, Clone, Serialize)]
pub struct SpectrumConfig {
    /// Filterbank band centre frequencies (Hz), low to high.
    pub centers_hz: Vec<f32>,
}

/// Start a monitor session on `device_name` (or the default input). When
/// `listen` is set, the (optionally isolated) signal is played to the default
/// output device at `gain`. `iso_hz` > 0 isolates that band; 0 is broadband.
pub fn start(
    app: tauri::AppHandle,
    device_name: Option<String>,
    listen: bool,
    gain: f32,
    iso_hz: f32,
) -> Result<MonitorHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let gain = Arc::new(AtomicU32::new(gain.to_bits()));
    let iso_hz = Arc::new(AtomicU32::new(iso_hz.to_bits()));
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();

    {
        let stop = Arc::clone(&stop);
        let gain = Arc::clone(&gain);
        let iso = Arc::clone(&iso_hz);
        std::thread::spawn(move || {
            monitor_thread(app, device_name, listen, stop, gain, iso, tx);
        });
    }

    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "mic monitor did not start in time".to_string())??;
    Ok(MonitorHandle { stop, gain, iso_hz })
}

/// Owns the streams, runs the filterbank/isolation, and emits events.
fn monitor_thread(
    app: tauri::AppHandle,
    device_name: Option<String>,
    listen: bool,
    stop: Arc<AtomicBool>,
    gain: Arc<AtomicU32>,
    iso_hz: Arc<AtomicU32>,
    tx: Sender<Result<(), String>>,
) {
    let raw: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));

    let setup = (|| {
        let device = find_device(device_name.as_deref())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("could not read device config: {e}"))?;
        let sr = f64::from(config.sample_rate().0);

        let input = open_input_stream(&device, &config, &raw)?;
        input
            .play()
            .map_err(|e| format!("failed to start capture: {e}"))?;

        // Audible passthrough is best-effort: the meter and spectrogram still
        // work without an output device.
        let output = if listen {
            match open_output(config.sample_rate().0, &ring, &gain) {
                Ok(o) => Some(o),
                Err(e) => {
                    eprintln!("mic monitor: audible passthrough unavailable: {e}");
                    None
                }
            }
        } else {
            None
        };
        Ok::<_, String>((input, output, sr))
    })();

    let (_input, _output, sr) = match setup {
        Ok(v) => v,
        Err(e) => {
            stop.store(true, Ordering::Relaxed);
            let _ = tx.send(Err(e));
            return;
        }
    };
    let _ = tx.send(Ok(()));

    let (centers, mut bank) = build_filterbank(sr);
    let _ = app.emit(
        "mic-spectrum-config",
        SpectrumConfig {
            centers_hz: centers.iter().map(|&c| c as f32).collect(),
        },
    );
    let ring_cap = (MAX_LATENCY_S * sr) as usize;

    let mut iso: Option<(f64, Biquad)> = None; // (centre, filter) when isolating
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(FRAME);
        let chunk = std::mem::take(&mut *raw.lock().expect("raw buffer lock"));
        if chunk.is_empty() {
            continue;
        }

        // (Re)build the isolation filter when the requested centre changes.
        let target = f32::from_bits(iso_hz.load(Ordering::Relaxed)) as f64;
        if target > 0.0 {
            if iso.as_ref().map(|(c, _)| *c) != Some(target) {
                iso = Some((target, Biquad::bandpass(target, ISO_Q, sr)));
            }
        } else {
            iso = None;
        }

        // Spectrogram: energy per band over this frame.
        let mut column = vec![0f32; BANDS];
        for (b, bq) in bank.iter_mut().enumerate() {
            let mut sumsq = 0.0;
            for &s in &chunk {
                let y = bq.process(f64::from(s));
                sumsq += y * y;
            }
            let meansq = sumsq / chunk.len() as f64;
            column[b] = (10.0 * (meansq + 1e-12).log10()) as f32;
        }
        let _ = app.emit("mic-spectrum", &column);

        // Meter and passthrough operate on the isolated signal when isolating.
        let mut peak = 0.0f32;
        let mut sumsq = 0.0f64;
        let mut filtered = listen.then(|| Vec::with_capacity(chunk.len()));
        for &s in &chunk {
            let v = match iso.as_mut() {
                Some((_, bq)) => bq.process(f64::from(s)) as f32,
                None => s,
            };
            peak = peak.max(v.abs());
            sumsq += f64::from(v) * f64::from(v);
            if let Some(out) = filtered.as_mut() {
                out.push(v);
            }
        }
        let rms = (sumsq / chunk.len() as f64).sqrt() as f32;
        let _ = app.emit(
            "mic-level",
            MicLevel {
                peak,
                rms,
                db: 20.0 * peak.max(1e-6).log10(),
                clipping: peak >= 0.99,
            },
        );

        if let Some(samples) = filtered {
            let mut q = ring.lock().expect("ring lock");
            q.extend(samples);
            while q.len() > ring_cap {
                q.pop_front();
            }
        }
    }
}

/// Build a constant-Q band-pass filterbank: log-spaced centres from
/// [`BAND_LO_HZ`] up to `min(BAND_HI_MAX_HZ, BAND_HI_FRAC * sr)`, with Q set so
/// adjacent bands roughly tile. Returns the centres and the biquads.
fn build_filterbank(sr: f64) -> (Vec<f64>, Vec<Biquad>) {
    let hi = (BAND_HI_FRAC * sr).clamp(BAND_LO_HZ * 2.0, BAND_HI_MAX_HZ);
    let ratio = (hi / BAND_LO_HZ).powf(1.0 / (BANDS - 1) as f64);
    let q = (1.0 / (ratio - 1.0)).max(1.0);
    let mut centers = Vec::with_capacity(BANDS);
    let mut bank = Vec::with_capacity(BANDS);
    for i in 0..BANDS {
        let c = BAND_LO_HZ * ratio.powi(i as i32);
        centers.push(c);
        bank.push(Biquad::bandpass(c, q, sr));
    }
    (centers, bank)
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
