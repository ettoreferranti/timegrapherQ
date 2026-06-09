//! Microphone capture and the record-then-analyse flow.
//!
//! Capture uses `cpal` (cross-platform). A recording runs on a dedicated
//! blocking thread: we build an input stream, collect mono samples for the
//! requested duration, then drop the stream and hand the buffer to
//! `timegrapherq_core::analyze`.
//!
//! Pure helpers (validation, degraded-input detection, warning text) are kept
//! separate and unit-tested; the audio I/O itself is exercised manually on a
//! real device.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use serde::Serialize;

use timegrapherq_core::{analyze, AnalysisConfig};

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

/// A measurement plus capture context, returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct MeasurementDto {
    pub rate_s_per_day: f64,
    pub beat_error_ms: f64,
    pub amplitude_deg: Option<f64>,
    pub bph: u32,
    pub lift_angle_deg: f64,
    pub beats_detected: usize,
    pub sample_rate: u32,
    pub device_name: String,
    pub clip_seconds: f64,
    pub peak_level: f32,
    pub degraded_input: bool,
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

/// Record from `device_name` (or the default input) for `seconds`, then analyse.
///
/// Returns a populated [`MeasurementDto`], or an error string suitable for
/// display if capture failed or no steady tick could be detected.
pub fn record_and_analyze(
    device_name: Option<String>,
    bph: u32,
    lift_angle_deg: f64,
    seconds: f64,
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
    let peak_level = samples.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));

    let cfg = AnalysisConfig::new(bph, lift_angle_deg);
    let measurement = analyze(&samples, sample_rate, &cfg);
    let degraded_input = is_degraded(sample_rate);
    let warning = build_warning(degraded_input, peak_level);

    let m = measurement.ok_or_else(|| {
        warning.clone().unwrap_or_else(|| {
            "could not detect a steady tick — check microphone placement, the \
             selected bph, and that the watch is running"
                .to_string()
        })
    })?;

    Ok(MeasurementDto {
        rate_s_per_day: m.rate_s_per_day,
        beat_error_ms: m.beat_error_ms,
        amplitude_deg: m.amplitude_deg,
        bph: m.bph,
        lift_angle_deg: m.lift_angle_deg,
        beats_detected: m.beats_detected,
        sample_rate,
        device_name: actual_name,
        clip_seconds: seconds,
        peak_level,
        degraded_input,
        warning,
    })
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
    if !(3600..=72_000).contains(&bph) {
        return Err("beat rate (bph) must be between 3600 and 72000".to_string());
    }
    if !(10.0..=120.0).contains(&lift_angle_deg) {
        return Err("lift angle must be between 10° and 120°".to_string());
    }
    if !(1.0..=120.0).contains(&seconds) {
        return Err("recording duration must be between 1 and 120 seconds".to_string());
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
}
