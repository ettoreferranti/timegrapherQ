//! Analyze a mono 16-bit PCM WAV file from the command line.
//!
//! Usage: cargo run --example analyze_wav -- <file.wav> [bph]
//!
//! With no bph argument, tries the standard beat rates and also reports the
//! autocorrelation estimate, so it doubles as a "is there a tick in here?"
//! diagnostic for arbitrary recordings.

use std::env;
use std::fs;

use timegrapherq_core::{analyze, dsp, measure, AnalysisConfig};

const STANDARD_BPH: [u32; 7] = [12_000, 18_000, 19_800, 21_600, 25_200, 28_800, 36_000];

fn main() {
    let args: Vec<String> = env::args().collect();
    let path = args.get(1).expect("usage: analyze_wav <file.wav> [bph]");
    let forced_bph: Option<u32> = args.get(2).map(|s| s.parse().expect("bph must be a number"));

    let (samples, sample_rate) = read_wav_mono_16(path);
    let sr = f64::from(sample_rate);
    let duration = samples.len() as f64 / sr;
    let peak = samples.iter().fold(0.0_f32, |m, &s| m.max(s.abs()));
    let rms = (samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>()
        / samples.len().max(1) as f64)
        .sqrt();

    println!("file: {path}");
    println!("{:.1} s @ {} Hz | peak {:.4} | rms {:.5}", duration, sample_rate, peak, rms);

    // Per-band signal diagnostics with the same DSP front-end as the app,
    // computed on the outlier-suppressed signal that `analyze` measures.
    let cfg = AnalysisConfig::new(28_800, 52.0);
    let (cleaned, masked_s) = measure::suppress_outliers(&samples, sample_rate, &cfg);
    if masked_s > 0.0 {
        println!("outlier suppression silenced {masked_s:.1} s of loud noise");
    }
    for hz in measure::candidate_band_centers(&cfg, sr) {
        let filtered = dsp::bandpass(&cleaned, sr, hz, cfg.bandpass_q);
        let env = dsp::envelope(&filtered, sr, cfg.envelope_tau_s);
        let reference = dsp::percentile(&env, cfg.reference_percentile);
        let threshold = cfg.threshold_ratio * reference;
        let onsets =
            dsp::detect_onsets(&env, sr, threshold, cfg.refractory_s, cfg.onset_edge_fraction);
        let (periodicity, detected_bph) = measure::dominant_periodicity(&env, sr);
        println!(
            "band {:>5.0} Hz: {:>5} onsets | periodicity {:.2} | detected bph {:?}",
            hz, onsets.len(), periodicity, detected_bph
        );
    }
    println!();

    let candidates: Vec<u32> = match forced_bph {
        Some(b) => vec![b],
        None => STANDARD_BPH.to_vec(),
    };

    // Optional single-band override (disables the scan), e.g. TGQ_BAND_HZ=8000.
    let band_hz: Option<f64> = env::var("TGQ_BAND_HZ").ok().and_then(|v| v.parse().ok());

    for bph in candidates {
        let mut cfg = AnalysisConfig::new(bph, 52.0);
        if let Some(hz) = band_hz {
            cfg.band_centers_hz = vec![hz];
        }
        match analyze(&samples, sample_rate, &cfg) {
            Some(m) => println!(
                "bph {:>6}: rate {:+8.1} ±{:4.1} s/d | beat error {:5.2} ms | amplitude {} | beats {}/{} used/detected (~{} expected) | quality {:.2} | band {:.0} Hz | masked {:.1} s",
                bph,
                m.rate_s_per_day,
                m.rate_ci95_s_per_day,
                m.beat_error_ms,
                m.amplitude_deg.map_or("  n/a ".to_string(), |a| format!("{a:5.0}°")),
                m.beats_used,
                m.beats_detected,
                m.beats_expected,
                m.quality,
                m.band_center_hz,
                m.masked_s
            ),
            None => println!("bph {bph:>6}: no measurement"),
        }
    }
}

/// Minimal reader for the WAV files we care about: PCM, 16-bit. Multi-channel
/// input is downmixed to mono.
fn read_wav_mono_16(path: &str) -> (Vec<f32>, u32) {
    let bytes = fs::read(path).expect("read wav file");
    assert!(&bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE", "not a WAV file");

    let mut pos = 12;
    let mut sample_rate = 0u32;
    let mut channels = 1usize;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8 + len).min(bytes.len())];
        match id {
            b"fmt " => {
                let format = u16::from_le_bytes(body[0..2].try_into().unwrap());
                // 1 = PCM, 0xFFFE = extensible (afconvert uses this for PCM too).
                assert!(format == 1 || format == 0xFFFE, "only PCM WAV is supported");
                channels = u16::from_le_bytes(body[2..4].try_into().unwrap()) as usize;
                sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
                assert!(bits == 16, "only 16-bit WAV is supported");
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos += 8 + len + (len & 1);
    }

    let data = data.expect("no data chunk");
    let frames = data.chunks_exact(2 * channels);
    let samples: Vec<f32> = frames
        .map(|frame| {
            let sum: f32 = frame
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                .sum();
            sum / channels as f32
        })
        .collect();
    (samples, sample_rate)
}
