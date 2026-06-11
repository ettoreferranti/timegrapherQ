//! Microphone capture and the record-then-analyse flow.
//!
//! Capture uses `cpal` (cross-platform). A recording runs on a dedicated
//! blocking thread: we build an input stream, collect mono samples for the
//! requested duration, then drop the stream and hand the buffer to
//! `timegrapherq_core::analyze`.
//!
//! Pure helpers (validation, degraded-input detection, warning text, WAV
//! writing) are kept separate and unit-tested; the audio I/O itself is
//! exercised manually on a real device.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use serde::Serialize;

use timegrapherq_core::{analyze, dsp, AnalysisConfig};

/// Effective sample rates at or below this are flagged as degraded (e.g. a
/// Bluetooth hands-free profile), which harms tick-transient capture.
pub const DEGRADED_SAMPLE_RATE_HZ: u32 = 24_000;

/// An available input device, for the device picker.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    pub default_sample_rate: u32,
    pub channels: u16,
}

/// A measurement plus capture context and diagnostics, returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct MeasurementDto {
    pub rate_s_per_day: f64,
    /// ~95% confidence half-width on the rate (s/day).
    pub rate_ci95_s_per_day: f64,
    pub beat_error_ms: f64,
    pub amplitude_deg: Option<f64>,
    pub bph: u32,
    pub lift_angle_deg: f64,
    pub beats_detected: usize,
    pub beats_used: usize,
    pub beats_expected: usize,
    pub periodicity: f64,
    pub detected_bph: Option<f64>,
    /// Band-pass centre (Hz) the analyzer locked onto (or, without a
    /// measurement, the most periodic band found).
    pub band_center_hz: f64,
    /// Seconds silenced as loud outliers (handling bumps, coughs).
    pub masked_seconds: f64,
    pub quality: f64,
    pub sample_rate: u32,
    pub device_name: String,
    pub clip_seconds: f64,
    pub peak_level: f32,
    /// RMS level of the whole clip (diagnostic).
    pub rms_level: f32,
    /// Raw transients detected before grouping into beats (diagnostic).
    pub raw_onsets: usize,
    pub degraded_input: bool,
    /// Whether a measurement was obtained at all (vs. only diagnostics).
    pub measured: bool,
    /// Path to the saved WAV, if recording was requested.
    pub recording_path: Option<String>,
    pub warning: Option<String>,
}

/// List available input devices with their default configuration.
pub fn list_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().and_then(|d| d.name().ok());

    let mut out = Vec::new();
    if let Ok(devices) = host.input_devices() {
        for d in devices {
            let Ok(name) = d.name() else { continue };
            let (sample_rate, channels) = match d.default_input_config() {
                Ok(c) => (c.sample_rate().0, c.channels()),
                Err(_) => (0, 0),
            };
            let is_default = default_name.as_ref() == Some(&name);
            out.push(DeviceInfo {
                name,
                is_default,
                default_sample_rate: sample_rate,
                channels,
            });
        }
    }
    out
}

/// Record from `device_name` (or the default input) for `seconds`, then
/// analyse. Always returns a [`MeasurementDto`] when capture succeeds — even if
/// no steady tick was found (`measured == false`, `quality == 0`) — so the UI
/// can show diagnostics. Returns an error only when capture itself fails.
pub fn record_and_analyze(
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
    seconds: f64,
    save_recording: bool,
) -> Result<MeasurementDto, String> {
    validate_params(bph, lift_angle_deg, seconds)?;

    let device = find_device(device_name.as_deref())?;
    let actual_name = device.name().unwrap_or_else(|_| "unknown".to_string());
    let config = device
        .default_input_config()
        .map_err(|e| format!("could not read device config: {e}"))?;
    let sample_rate = config.sample_rate().0;

    let samples = record_samples(&device, &config, seconds)?;
    if samples.is_empty() {
        return Err("no audio was captured (is microphone permission granted?)".to_string());
    }

    let recording_path = if save_recording {
        save_wav(&samples, sample_rate, &recording_dir()).ok()
    } else {
        None
    };

    Ok(build_dto(
        &samples,
        sample_rate,
        bph,
        lift_angle_deg,
        &actual_name,
        recording_path,
    ))
}

/// Analyse captured or imported samples and assemble the full DTO with
/// diagnostics and warnings. Shared by the record and file-import paths.
pub fn build_dto(
    samples: &[f32],
    sample_rate: u32,
    bph: u32,
    lift_angle_deg: f64,
    device_name: &str,
    recording_path: Option<String>,
) -> MeasurementDto {
    let seconds = samples.len() as f64 / f64::from(sample_rate);
    let cfg = AnalysisConfig::new(bph, lift_angle_deg);
    let peak_level = samples.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
    // Diagnostics are computed on the outlier-suppressed signal — the same one
    // `analyze` measures — so they aren't dominated by bumps and coughs.
    let (cleaned, masked_seconds) =
        timegrapherq_core::measure::suppress_outliers(samples, sample_rate, &cfg);
    let stats = signal_stats(&cleaned, sample_rate, &cfg);
    let beats_expected = expected_beats(samples.len(), sample_rate, bph);

    let degraded_input = is_degraded(sample_rate);
    let measurement = analyze(samples, sample_rate, &cfg);

    let mut warnings = Vec::new();
    if let Some(w) = build_warning(degraded_input, peak_level) {
        warnings.push(w);
    }

    let dto = match measurement {
        Some(m) => {
            if m.quality < 0.6 {
                warnings.push(low_confidence_message(
                    m.quality,
                    m.beats_used,
                    m.beats_expected,
                ));
            }
            if let Some(msg) = bph_mismatch_message(stats.periodicity, stats.detected_bph, bph) {
                warnings.push(msg);
            }
            MeasurementDto {
                rate_s_per_day: m.rate_s_per_day,
                rate_ci95_s_per_day: m.rate_ci95_s_per_day,
                beat_error_ms: m.beat_error_ms,
                amplitude_deg: m.amplitude_deg,
                beats_detected: m.beats_detected,
                beats_used: m.beats_used,
                beats_expected: m.beats_expected,
                band_center_hz: m.band_center_hz,
                quality: m.quality,
                measured: true,
                ..base_dto(bph, lift_angle_deg, sample_rate, device_name, seconds)
            }
        }
        None => {
            warnings.push(format!(
                "No steady tick detected (found {} transients, ~{} expected). \
                 Press the microphone firmly against the watch, use a quiet room, \
                 and confirm the beat rate (bph). The built-in mic often works \
                 better than phone/Bluetooth mics, which filter out ticks.",
                stats.raw_onsets, beats_expected
            ));
            MeasurementDto {
                beats_expected,
                band_center_hz: stats.band_center_hz,
                measured: false,
                ..base_dto(bph, lift_angle_deg, sample_rate, device_name, seconds)
            }
        }
    };

    MeasurementDto {
        peak_level,
        rms_level: stats.rms,
        raw_onsets: stats.raw_onsets,
        periodicity: stats.periodicity,
        detected_bph: stats.detected_bph,
        masked_seconds,
        degraded_input,
        recording_path,
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
        ..dto
    }
}

/// A zeroed DTO with the fields that are known regardless of outcome.
fn base_dto(
    bph: u32,
    lift_angle_deg: f64,
    sample_rate: u32,
    device_name: &str,
    seconds: f64,
) -> MeasurementDto {
    MeasurementDto {
        rate_s_per_day: 0.0,
        rate_ci95_s_per_day: 0.0,
        beat_error_ms: 0.0,
        amplitude_deg: None,
        bph,
        lift_angle_deg,
        beats_detected: 0,
        beats_used: 0,
        beats_expected: 0,
        periodicity: 0.0,
        detected_bph: None,
        band_center_hz: 0.0,
        masked_seconds: 0.0,
        quality: 0.0,
        sample_rate,
        device_name: device_name.to_string(),
        clip_seconds: seconds,
        peak_level: 0.0,
        rms_level: 0.0,
        raw_onsets: 0,
        degraded_input: false,
        measured: false,
        recording_path: None,
        warning: None,
    }
}

/// Diagnostic signal statistics, computed with the same DSP front-end as
/// [`analyze`] so they reflect what the analyzer "sees".
struct SignalStats {
    rms: f32,
    raw_onsets: usize,
    periodicity: f64,
    detected_bph: Option<f64>,
    /// The scanned band with the strongest periodicity (Hz).
    band_center_hz: f64,
}

fn signal_stats(samples: &[f32], sample_rate: u32, cfg: &AnalysisConfig) -> SignalStats {
    let sr = f64::from(sample_rate);
    let n = samples.len().max(1) as f64;
    let rms = (samples
        .iter()
        .map(|&s| f64::from(s) * f64::from(s))
        .sum::<f64>()
        / n)
        .sqrt() as f32;

    // Scan the same bands as the analyzer and report the most periodic one, so
    // the diagnostics describe the best view of the signal, not a fixed band.
    let mut best = SignalStats {
        rms,
        raw_onsets: 0,
        periodicity: -1.0,
        detected_bph: None,
        band_center_hz: cfg.bandpass_center_hz,
    };
    for hz in timegrapherq_core::measure::candidate_band_centers(cfg, sr) {
        let filtered = dsp::bandpass(samples, sr, hz, cfg.bandpass_q);
        let env = dsp::envelope(&filtered, sr, cfg.envelope_tau_s);
        let reference = dsp::percentile(&env, cfg.reference_percentile);
        let threshold = cfg.threshold_ratio * reference;
        let raw_onsets =
            dsp::detect_onsets(&env, sr, threshold, cfg.refractory_s, cfg.onset_edge_fraction)
                .len();
        let (periodicity, detected_bph) =
            timegrapherq_core::measure::dominant_periodicity(&env, sr);
        if periodicity > best.periodicity {
            best = SignalStats {
                rms,
                raw_onsets,
                periodicity,
                detected_bph,
                band_center_hz: hz,
            };
        }
    }
    best.periodicity = best.periodicity.max(0.0);
    best
}

fn expected_beats(n_samples: usize, sample_rate: u32, bph: u32) -> usize {
    let duration = n_samples as f64 / f64::from(sample_rate);
    (duration / (3600.0 / f64::from(bph))).round() as usize
}

/// Resolve a device by name, or the system default input.
fn find_device(name: Option<&str>) -> Result<cpal::Device, String> {
    let host = cpal::default_host();
    match name {
        Some(n) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().map(|dn| dn == n).unwrap_or(false))
            .ok_or_else(|| format!("input device not found: {n}")),
        None => host
            .default_input_device()
            .ok_or_else(|| "no default input device available".to_string()),
    }
}

/// Capture mono samples for `seconds`, downmixing multi-channel input.
fn record_samples(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    seconds: f64,
) -> Result<Vec<f32>, String> {
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.config();
    let buf = Arc::new(Mutex::new(Vec::<f32>::new()));

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build_stream::<f32>(device, &stream_config, channels, &buf)?,
        cpal::SampleFormat::I16 => build_stream::<i16>(device, &stream_config, channels, &buf)?,
        cpal::SampleFormat::U16 => build_stream::<u16>(device, &stream_config, channels, &buf)?,
        other => return Err(format!("unsupported sample format: {other:?}")),
    };

    stream
        .play()
        .map_err(|e| format!("failed to start capture: {e}"))?;
    std::thread::sleep(Duration::from_secs_f64(seconds));
    drop(stream); // stop capture

    let samples = std::mem::take(&mut *buf.lock().expect("audio buffer lock"));
    Ok(samples)
}

/// Build an input stream whose callback downmixes frames to mono `f32` and
/// appends them to `buf`.
fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buf: &Arc<Mutex<Vec<f32>>>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let buf = Arc::clone(buf);
    device
        .build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let mut b = buf.lock().expect("audio buffer lock");
                for frame in data.chunks(channels) {
                    let sum: f32 = frame.iter().map(|&s| f32::from_sample_(s)).sum();
                    b.push(sum / channels as f32);
                }
            },
            |e| eprintln!("audio stream error: {e}"),
            None,
        )
        .map_err(|e| format!("failed to open input stream: {e}"))
}

/// Whether an effective sample rate is too low for reliable measurement.
fn is_degraded(sample_rate: u32) -> bool {
    sample_rate <= DEGRADED_SAMPLE_RATE_HZ
}

/// Validate user-supplied parameters, returning a friendly message if invalid.
fn validate_params(bph: u32, lift_angle_deg: f64, seconds: f64) -> Result<(), String> {
    validate_movement_params(bph, lift_angle_deg)?;
    if !(1.0..=120.0).contains(&seconds) {
        return Err("recording duration must be between 1 and 120 seconds".to_string());
    }
    Ok(())
}

/// Validate the movement parameters alone (shared with the file-import path,
/// which has no recording duration to check).
pub fn validate_movement_params(bph: u32, lift_angle_deg: f64) -> Result<(), String> {
    if !(3600..=72_000).contains(&bph) {
        return Err("beat rate (bph) must be between 3600 and 72000".to_string());
    }
    if !(10.0..=120.0).contains(&lift_angle_deg) {
        return Err("lift angle must be between 10° and 120°".to_string());
    }
    Ok(())
}

/// Compose a non-fatal advisory from capture conditions, if any.
fn build_warning(degraded: bool, peak: f32) -> Option<String> {
    let mut msgs = Vec::new();
    if degraded {
        msgs.push(
            "Input sample rate is low (a Bluetooth hands-free profile?). Tick \
             transients may be degraded — prefer a wired or contact microphone.",
        );
    }
    if peak < 0.01 {
        msgs.push("Signal is very quiet — move the microphone closer to the watch.");
    } else if peak >= 0.99 {
        msgs.push("Signal is clipping — reduce the input gain.");
    }
    if msgs.is_empty() {
        None
    } else {
        Some(msgs.join(" "))
    }
}

/// Warn when a confidently-periodic signal implies a different bph than the
/// one selected — a common cause of nonsense rate readings.
fn bph_mismatch_message(
    periodicity: f64,
    detected_bph: Option<f64>,
    selected: u32,
) -> Option<String> {
    let detected = detected_bph?;
    if periodicity < 0.5 {
        return None; // not periodic enough to trust the estimate
    }
    let rel = (detected - f64::from(selected)).abs() / f64::from(selected);
    if rel > 0.08 {
        Some(format!(
            "Detected a tick near {:.0} bph, but {} bph is selected — check the \
             beat-rate setting (the rate reading depends on it).",
            detected, selected
        ))
    } else {
        None
    }
}

fn low_confidence_message(quality: f64, used: usize, expected: usize) -> String {
    format!(
        "Low confidence ({:.0}%): used {} of ~{} expected ticks. Press the \
         microphone firmly against the watch, reduce background noise, and \
         confirm the beat rate (bph).",
        quality * 100.0,
        used,
        expected
    )
}

/// Directory for saved recordings: the user's Downloads folder if present,
/// otherwise the system temp directory.
fn recording_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        let downloads = Path::new(&home).join("Downloads");
        if downloads.is_dir() {
            return downloads;
        }
    }
    std::env::temp_dir()
}

/// Save mono `f32` samples as a 16-bit PCM WAV in `dir`; returns the path.
pub fn save_wav(samples: &[f32], sample_rate: u32, dir: &Path) -> Result<String, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("timegrapherq-{ts}.wav"));
    write_wav_16(&path, samples, sample_rate).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Write a mono 16-bit PCM WAV file.
fn write_wav_16(path: &Path, samples: &[f32], sample_rate: u32) -> std::io::Result<()> {
    let mut f = BufWriter::new(File::create(path)?);
    let data_len = (samples.len() as u32) * 2;
    let byte_rate = sample_rate * 2;

    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for &s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&v.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degraded_threshold() {
        assert!(is_degraded(8_000));
        assert!(is_degraded(16_000));
        assert!(is_degraded(DEGRADED_SAMPLE_RATE_HZ));
        assert!(!is_degraded(44_100));
        assert!(!is_degraded(48_000));
    }

    #[test]
    fn param_validation() {
        assert!(validate_params(28_800, 52.0, 20.0).is_ok());
        assert!(validate_params(0, 52.0, 20.0).is_err());
        assert!(validate_params(28_800, 5.0, 20.0).is_err());
        assert!(validate_params(28_800, 52.0, 0.5).is_err());
        assert!(validate_params(28_800, 52.0, 999.0).is_err());
    }

    #[test]
    fn warning_text() {
        assert!(build_warning(false, 0.3).is_none());
        assert!(build_warning(true, 0.3).unwrap().contains("sample rate"));
        assert!(build_warning(false, 0.001).unwrap().contains("quiet"));
        assert!(build_warning(false, 1.0).unwrap().contains("clipping"));
    }

    #[test]
    fn bph_mismatch_warns_only_when_periodic_and_different() {
        // Periodic and matching: no warning.
        assert!(bph_mismatch_message(0.9, Some(28_800.0), 28_800).is_none());
        // Periodic and clearly different: warn.
        assert!(bph_mismatch_message(0.9, Some(21_600.0), 28_800).is_some());
        // Not periodic enough: stay quiet even if different.
        assert!(bph_mismatch_message(0.3, Some(21_600.0), 28_800).is_none());
        // No estimate: no warning.
        assert!(bph_mismatch_message(0.9, None, 28_800).is_none());
    }

    #[test]
    fn expected_beats_for_clip() {
        // 28800 bph = 8 beats/s; 10 s of 44.1 kHz => ~80 beats.
        assert_eq!(expected_beats(441_000, 44_100, 28_800), 80);
    }

    #[test]
    fn wav_header_and_size() {
        let samples = vec![0.0_f32, 0.5, -0.5, 1.0];
        let path = std::env::temp_dir().join("timegrapherq-test.wav");
        write_wav_16(&path, &samples, 44_100).expect("write wav");
        let bytes = std::fs::read(&path).expect("read wav");
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(bytes.len(), 44 + samples.len() * 2);
        let _ = std::fs::remove_file(&path);
    }
}
