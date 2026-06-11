//! Analyse an audio file instead of a live recording.
//!
//! Phone recordings pressed against the watch case are often the best capture
//! available, so the app accepts files (WAV, M4A/AAC voice memos, MP3, FLAC,
//! AIFF, OGG) and runs them through the same analysis path as the microphone.
//! Decoding uses `symphonia`; the decoded audio is also written to a temp WAV
//! so "keep clip" works when saving the measurement to the history.

use std::fs::File;
use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::audio::{build_dto, save_wav, validate_movement_params, MeasurementDto};

/// Files shorter than this carry too few beats to be worth analysing.
const MIN_DURATION_S: f64 = 2.0;
/// Cap so a misclicked multi-hour file doesn't stall the app.
const MAX_DURATION_S: f64 = 600.0;

/// Decode `path` and analyse it like a recording. The DTO's device name is
/// `file: <name>` and its recording path points at a decoded temp WAV.
pub fn analyze_file(path: &str, bph: u32, lift_angle_deg: f64) -> Result<MeasurementDto, String> {
    validate_movement_params(bph, lift_angle_deg)?;

    let (samples, sample_rate) = decode_audio(Path::new(path))?;
    let duration = samples.len() as f64 / f64::from(sample_rate.max(1));
    if duration < MIN_DURATION_S {
        return Err(format!(
            "file is too short to analyse ({duration:.1} s; need at least {MIN_DURATION_S} s)"
        ));
    }
    if duration > MAX_DURATION_S {
        return Err(format!(
            "file is too long ({:.0} s; the limit is {:.0} s) — trim it first",
            duration, MAX_DURATION_S
        ));
    }

    let file_label = Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let recording_path = save_wav(&samples, sample_rate, &std::env::temp_dir()).ok();

    Ok(build_dto(
        &samples,
        sample_rate,
        bph,
        lift_angle_deg,
        &format!("file: {file_label}"),
        recording_path,
    ))
}

/// Decode any supported audio file to mono `f32` samples plus sample rate.
fn decode_audio(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let file = File::open(path).map_err(|e| format!("could not open file: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("unrecognised audio file: {e}"))?;
    let mut format = probed.format;

    let track = format
        .default_track()
        .ok_or_else(|| "no audio track in file".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported audio codec: {e}"))?;

    let mut samples = Vec::new();
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Normal end of stream (symphonia reports EOF as an IO error).
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(format!("error reading audio: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                sample_rate = spec.rate;
                let channels = spec.channels.count().max(1);
                let mut buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
                buf.copy_interleaved_ref(decoded);
                for frame in buf.samples().chunks(channels) {
                    samples.push(frame.iter().sum::<f32>() / channels as f32);
                }
            }
            // A corrupt packet is skippable; anything else is fatal.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("could not decode audio: {e}")),
        }
    }

    if samples.is_empty() || sample_rate == 0 {
        return Err("no audio could be decoded from the file".to_string());
    }
    Ok((samples, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp dir per test, so parallel tests' WAV files (named by
    /// timestamp) cannot collide.
    fn test_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tgq-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    /// Round-trip: a synthetic tick written as WAV decodes and analyses the
    /// same way a live recording would.
    #[test]
    fn analyzes_wav_file() {
        let sig = timegrapherq_core::synth::synth_escapement(&Default::default());
        let dir = test_dir();
        let path = save_wav(&sig.samples, sig.sample_rate, &dir).expect("write wav");

        let dto = analyze_file(&path, 28_800, 52.0).expect("analyse file");
        assert!(dto.measured, "expected a measurement");
        assert!(dto.quality > 0.8, "quality={}", dto.quality);
        assert!(dto.rate_s_per_day.abs() < 1.0, "rate={}", dto.rate_s_per_day);
        assert!(dto.device_name.starts_with("file: "));
        assert!(dto.recording_path.is_some());

        let _ = std::fs::remove_file(&path);
        if let Some(p) = dto.recording_path {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Manual smoke test against a real file (any supported format):
    /// `TGQ_IMPORT_TEST_FILE=/path/to/clip.m4a cargo test -- --nocapture`
    #[test]
    fn analyzes_real_file_if_provided() {
        let Some(path) = std::env::var_os("TGQ_IMPORT_TEST_FILE") else {
            return;
        };
        let dto = analyze_file(&path.to_string_lossy(), 28_800, 52.0).expect("analyse file");
        println!(
            "{}: rate {:+.1} ±{:.1} s/d | quality {:.2} | band {:.0} Hz | masked {:.1} s",
            dto.device_name,
            dto.rate_s_per_day,
            dto.rate_ci95_s_per_day,
            dto.quality,
            dto.band_center_hz,
            dto.masked_seconds
        );
        assert!(dto.measured, "expected a measurement from the real file");
    }

    #[test]
    fn rejects_bad_inputs() {
        assert!(analyze_file("/nonexistent/file.wav", 28_800, 52.0).is_err());
        assert!(analyze_file("/tmp/x.wav", 0, 52.0).is_err());
    }

    #[test]
    fn rejects_too_short_files() {
        let dir = test_dir();
        let samples = vec![0.1_f32; 4800]; // 0.1 s at 48 kHz
        let path = save_wav(&samples, 48_000, &dir).expect("write wav");
        let err = analyze_file(&path, 28_800, 52.0).unwrap_err();
        assert!(err.contains("too short"), "{err}");
        let _ = std::fs::remove_file(&path);
    }
}
