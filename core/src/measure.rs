//! Measurement pipeline: turn raw audio samples into the headline timegrapher
//! metrics — rate (s/day), beat error (ms) and amplitude (degrees).
//!
//! Pipeline: band-pass → envelope → onset detection (`crate::dsp`) → group
//! transients into beats → derive metrics. Each beat normally contains two
//! transients (the impulse pair); their spacing encodes amplitude, while the
//! beat onsets encode rate and beat error.
//!
//! Validated against `crate::synth` ground-truth signals in the tests below
//! (see `docs/TEST_PLAN.md`). Robust outlier rejection / confidence scoring is
//! M1-9; this module assumes every beat is detected (true for clean and mildly
//! noisy signals).

use crate::{amplitude_degrees, dsp};

/// Tunable parameters for [`analyze`]. Construct with [`AnalysisConfig::new`]
/// to get sensible DSP defaults and override fields as needed.
#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    /// Design beat frequency (beats/hour).
    pub bph: u32,
    /// Movement lift angle (degrees) — required for amplitude.
    pub lift_angle_deg: f64,
    /// Band-pass centre frequency (Hz).
    pub bandpass_center_hz: f64,
    /// Band-pass Q (bandwidth ≈ centre / Q).
    pub bandpass_q: f64,
    /// Envelope smoothing time constant (seconds).
    pub envelope_tau_s: f64,
    /// Onset threshold as a fraction of the peak envelope.
    pub threshold_ratio: f64,
    /// Minimum spacing between accepted onsets (seconds).
    pub refractory_s: f64,
    /// A gap larger than this fraction of the nominal beat period starts a new
    /// beat group (separates beats from the within-beat impulse pair).
    pub beat_gap_ratio: f64,
}

impl AnalysisConfig {
    /// Defaults suitable for a clean line-level recording of a wristwatch.
    pub fn new(bph: u32, lift_angle_deg: f64) -> Self {
        Self {
            bph,
            lift_angle_deg,
            bandpass_center_hz: 3000.0,
            bandpass_q: 0.7,
            envelope_tau_s: 0.0008,
            threshold_ratio: 0.3,
            refractory_s: 0.003,
            beat_gap_ratio: 0.4,
        }
    }
}

/// The result of analysing a recording.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// Rate deviation in seconds/day (positive = fast).
    pub rate_s_per_day: f64,
    /// Beat error in milliseconds (>= 0).
    pub beat_error_ms: f64,
    /// Amplitude in degrees, or `None` if it could not be determined.
    pub amplitude_deg: Option<f64>,
    /// Beat frequency used for the computation.
    pub bph: u32,
    /// Lift angle used for the amplitude computation.
    pub lift_angle_deg: f64,
    /// Number of beats used (after trimming edge beats).
    pub beats_detected: usize,
}

/// One detected beat: the onset of its first transient and, when present, the
/// spacing to the second transient (used for amplitude).
#[derive(Clone, Copy, Debug)]
struct Beat {
    onset: f64,
    spacing: Option<f64>,
}

/// Analyse `samples` and return the timegrapher metrics, or `None` if too few
/// beats were detected to measure reliably.
pub fn analyze(samples: &[f32], sample_rate: u32, cfg: &AnalysisConfig) -> Option<Measurement> {
    let sr = f64::from(sample_rate);
    let filtered = dsp::bandpass(samples, sr, cfg.bandpass_center_hz, cfg.bandpass_q);
    let env = dsp::envelope(&filtered, sr, cfg.envelope_tau_s);
    let onsets = dsp::detect_onsets(&env, sr, cfg.threshold_ratio, cfg.refractory_s);

    let nominal_period = 3600.0 / f64::from(cfg.bph);
    let beats = trim_edges(group_beats(&onsets, nominal_period, cfg.beat_gap_ratio));
    if beats.len() < 4 {
        return None;
    }

    let beat_onsets: Vec<f64> = beats.iter().map(|b| b.onset).collect();
    let measured_period = regression_slope(&beat_onsets);
    if measured_period <= 0.0 {
        return None;
    }

    // A watch that gains time has a shorter beat period than nominal.
    let rate_s_per_day = 86_400.0 * (nominal_period / measured_period - 1.0);
    let beat_error_ms = beat_error_ms(&beat_onsets);
    let amplitude_deg = amplitude_from_beats(&beats, cfg);

    Some(Measurement {
        rate_s_per_day,
        beat_error_ms,
        amplitude_deg,
        bph: cfg.bph,
        lift_angle_deg: cfg.lift_angle_deg,
        beats_detected: beats.len(),
    })
}

/// Group consecutive onsets into beats. Onsets within `beat_gap_ratio` of the
/// nominal period of each other belong to the same beat (the impulse pair);
/// a larger gap starts a new beat.
fn group_beats(onsets: &[f64], nominal_period: f64, gap_ratio: f64) -> Vec<Beat> {
    let gap = gap_ratio * nominal_period;
    let mut beats = Vec::new();
    let mut i = 0;
    while i < onsets.len() {
        let start = onsets[i];
        let mut j = i + 1;
        let mut spacing = None;
        while j < onsets.len() && onsets[j] - onsets[j - 1] <= gap {
            if spacing.is_none() {
                spacing = Some(onsets[j] - start);
            }
            j += 1;
        }
        beats.push(Beat {
            onset: start,
            spacing,
        });
        i = j;
    }
    beats
}

/// Drop the first and last beat to avoid filter-settling and truncation
/// artefacts at the buffer edges.
fn trim_edges(mut beats: Vec<Beat>) -> Vec<Beat> {
    if beats.len() > 4 {
        beats.remove(0);
        beats.pop();
    }
    beats
}

/// Best-fit slope of `ys` against the index 0,1,2,… — i.e. the average step
/// (here, the measured beat period). Beat error is zero-mean across beats and
/// so does not bias the slope.
fn regression_slope(ys: &[f64]) -> f64 {
    let n = ys.len() as f64;
    let sx: f64 = (0..ys.len()).map(|k| k as f64).sum();
    let sy: f64 = ys.iter().sum();
    let sxx: f64 = (0..ys.len()).map(|k| (k as f64) * (k as f64)).sum();
    let sxy: f64 = ys.iter().enumerate().map(|(k, &y)| k as f64 * y).sum();
    (n * sxy - sx * sy) / (n * sxx - sx * sx)
}

/// Beat error: the difference between the two interleaved half-period
/// intervals (tick vs tock), in milliseconds. Robust to outliers via medians.
fn beat_error_ms(onsets: &[f64]) -> f64 {
    if onsets.len() < 3 {
        return 0.0;
    }
    let mut even = Vec::new();
    let mut odd = Vec::new();
    for k in 0..onsets.len() - 1 {
        let d = onsets[k + 1] - onsets[k];
        if k.is_multiple_of(2) {
            even.push(d);
        } else {
            odd.push(d);
        }
    }
    (median(&mut even) - median(&mut odd)).abs() * 1000.0
}

/// Amplitude from the median within-beat impulse spacing.
fn amplitude_from_beats(beats: &[Beat], cfg: &AnalysisConfig) -> Option<f64> {
    let mut spacings: Vec<f64> = beats.iter().filter_map(|b| b.spacing).collect();
    if spacings.is_empty() {
        return None;
    }
    let dt = median(&mut spacings);
    amplitude_degrees(dt, cfg.lift_angle_deg, cfg.bph)
}

/// Median of a slice (sorts in place). Returns 0.0 for an empty slice.
fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n.is_multiple_of(2) {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    } else {
        v[n / 2]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth::{synth_escapement, SignalSpec};

    fn analyze_spec(spec: &SignalSpec) -> Measurement {
        let sig = synth_escapement(spec);
        let cfg = AnalysisConfig::new(spec.bph, spec.lift_angle_deg);
        analyze(&sig.samples, sig.sample_rate, &cfg).expect("a measurement")
    }

    #[test]
    fn detects_most_beats_on_clean_signal() {
        // ~8 beats/s over 5 s ≈ 40 beats, minus the two trimmed edges.
        let m = analyze_spec(&SignalSpec {
            duration_s: 5.0,
            ..Default::default()
        });
        assert!(m.beats_detected >= 36, "beats={}", m.beats_detected);
    }

    #[test]
    fn rate_zero_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            ..Default::default()
        });
        assert!(m.rate_s_per_day.abs() < 1.0, "rate={}", m.rate_s_per_day);
    }

    #[test]
    fn rate_fast_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            rate_s_per_day: 12.0,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day - 12.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
    }

    #[test]
    fn rate_slow_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            rate_s_per_day: -25.0,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day + 25.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
    }

    #[test]
    fn beat_error_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            beat_error_ms: 0.8,
            ..Default::default()
        });
        assert!(
            (m.beat_error_ms - 0.8).abs() < 0.1,
            "beat_error={}",
            m.beat_error_ms
        );
    }

    #[test]
    fn beat_error_zero_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            beat_error_ms: 0.0,
            ..Default::default()
        });
        assert!(m.beat_error_ms < 0.1, "beat_error={}", m.beat_error_ms);
    }

    #[test]
    fn amplitude_270_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            amplitude_deg: 270.0,
            ..Default::default()
        });
        let a = m.amplitude_deg.expect("amplitude");
        assert!((a - 270.0).abs() < 3.0, "amplitude={a}");
    }

    #[test]
    fn amplitude_220_within_tolerance() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            amplitude_deg: 220.0,
            ..Default::default()
        });
        let a = m.amplitude_deg.expect("amplitude");
        assert!((a - 220.0).abs() < 3.0, "amplitude={a}");
    }

    #[test]
    fn combined_metrics_with_mild_noise() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 15.0,
            rate_s_per_day: 6.0,
            beat_error_ms: 0.5,
            amplitude_deg: 290.0,
            noise_amplitude: 0.02,
            seed: 7,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day - 6.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
        assert!(
            (m.beat_error_ms - 0.5).abs() < 0.1,
            "beat_error={}",
            m.beat_error_ms
        );
        assert!(
            (m.amplitude_deg.expect("amplitude") - 290.0).abs() < 3.0,
            "amplitude={:?}",
            m.amplitude_deg
        );
    }

    #[test]
    fn handles_alternate_beat_rate() {
        // 21600 bph (6 beats/s) movement.
        let m = analyze_spec(&SignalSpec {
            duration_s: 12.0,
            bph: 21_600,
            rate_s_per_day: -3.0,
            amplitude_deg: 280.0,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day + 3.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
        assert!(
            (m.amplitude_deg.expect("amplitude") - 280.0).abs() < 3.0,
            "amplitude={:?}",
            m.amplitude_deg
        );
    }
}
