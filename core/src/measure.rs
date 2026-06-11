//! Measurement pipeline: turn raw audio samples into the headline timegrapher
//! metrics — rate (s/day), beat error (ms) and amplitude (degrees) — together
//! with a **confidence score** so unreliable readings can be flagged rather
//! than presented as fake precision.
//!
//! Pipeline: band-pass → envelope → onset detection (`crate::dsp`) → group
//! transients into beats → reconstruct each beat's number from timing (robust
//! to missed beats) → reject outliers → derive metrics + confidence.
//!
//! Validated against `crate::synth` ground-truth signals in the tests below
//! (see `docs/TEST_PLAN.md`).

use crate::{amplitude_degrees, dsp};

/// Band-pass centres scanned by default. Tick energy depends on the capture
/// path: contact microphones pick up body-conducted sound around 2–4 kHz,
/// while air-coupled microphones (phone, laptop) mostly catch the escapement's
/// high-frequency snap at 8–16 kHz.
pub const DEFAULT_BAND_CENTERS_HZ: [f64; 5] = [3000.0, 6000.0, 9000.0, 12000.0, 15000.0];

/// Tunable parameters for [`analyze`]. Construct with [`AnalysisConfig::new`]
/// to get sensible DSP defaults and override fields as needed.
#[derive(Debug, Clone)]
pub struct AnalysisConfig {
    /// Design beat frequency (beats/hour).
    pub bph: u32,
    /// Movement lift angle (degrees) — required for amplitude.
    pub lift_angle_deg: f64,
    /// Band-pass centre frequency (Hz), used when `band_centers_hz` is empty.
    pub bandpass_center_hz: f64,
    /// Band-pass centres to scan; [`analyze`] keeps the band that yields the
    /// most confident measurement. Leave empty to use `bandpass_center_hz`
    /// alone (no scan).
    pub band_centers_hz: Vec<f64>,
    /// Band-pass Q (bandwidth ≈ centre / Q).
    pub bandpass_q: f64,
    /// Envelope smoothing time constant (seconds).
    pub envelope_tau_s: f64,
    /// Envelope quantile (0–1) used as the detection reference level; robust to
    /// a few loud outliers, unlike the raw maximum.
    pub reference_percentile: f64,
    /// Onset threshold as a fraction of the reference level.
    pub threshold_ratio: f64,
    /// Minimum spacing between accepted onsets (seconds).
    pub refractory_s: f64,
    /// A gap larger than this fraction of the nominal beat period starts a new
    /// beat group (separates beats from the within-beat impulse pair).
    pub beat_gap_ratio: f64,
    /// Beats whose onset deviates from the fitted line by more than this
    /// fraction of the nominal period are rejected as outliers.
    pub max_residual_ratio: f64,
    /// Window length (seconds) for loud-outlier suppression; should exceed one
    /// beat period so a typical window's peak is the tick peak.
    pub outlier_window_s: f64,
    /// Windows whose peak exceeds this multiple of the median window peak are
    /// silenced before analysis (handling bumps, coughs). `<= 0` disables.
    pub outlier_peak_ratio: f64,
}

impl AnalysisConfig {
    /// Defaults suitable for a reasonable recording of a wristwatch.
    pub fn new(bph: u32, lift_angle_deg: f64) -> Self {
        Self {
            bph,
            lift_angle_deg,
            bandpass_center_hz: 3000.0,
            band_centers_hz: DEFAULT_BAND_CENTERS_HZ.to_vec(),
            bandpass_q: 0.7,
            envelope_tau_s: 0.0008,
            reference_percentile: 0.99,
            threshold_ratio: 0.3,
            refractory_s: 0.003,
            beat_gap_ratio: 0.4,
            max_residual_ratio: 0.25,
            outlier_window_s: 0.25,
            outlier_peak_ratio: 5.0,
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
    /// Beats grouped from detected transients (before outlier rejection).
    pub beats_detected: usize,
    /// Beats actually used after outlier rejection.
    pub beats_used: usize,
    /// Beats expected for the clip duration at this bph.
    pub beats_expected: usize,
    /// Strength of the dominant envelope periodicity in [0, 1]; low means the
    /// signal is not a steady tick (noise).
    pub periodicity: f64,
    /// Beat frequency implied by the dominant periodicity (bph), if any.
    pub detected_bph: Option<f64>,
    /// Band-pass centre (Hz) that produced this measurement.
    pub band_center_hz: f64,
    /// Seconds silenced as loud outliers (handling bumps, coughs) before
    /// analysis.
    pub masked_s: f64,
    /// Confidence in [0, 1]: how much to trust this measurement.
    pub quality: f64,
}

/// One detected beat: the onset of its first transient, the spacing to the
/// second transient (for amplitude), and its reconstructed beat number.
#[derive(Clone, Copy, Debug)]
struct Beat {
    onset: f64,
    spacing: Option<f64>,
    index: f64,
}

/// Analyse `samples` and return the timegrapher metrics, or `None` if too few
/// beats were detected to measure at all. A successful return may still carry a
/// low [`Measurement::quality`] — callers should surface that to the user.
///
/// Runs the pipeline once per candidate band centre (see
/// [`candidate_band_centers`]) and keeps the most confident result, so the
/// tick is found whether its energy sits low (contact mic) or high (air mic).
pub fn analyze(samples: &[f32], sample_rate: u32, cfg: &AnalysisConfig) -> Option<Measurement> {
    let (cleaned, masked_s) = suppress_outliers(samples, sample_rate, cfg);
    let mut best: Option<Measurement> = None;
    for hz in candidate_band_centers(cfg, f64::from(sample_rate)) {
        let m = analyze_band(&cleaned, sample_rate, cfg, hz, masked_s);
        let better = match (&m, &best) {
            (Some(m), Some(b)) => m.quality > b.quality,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if better {
            best = m;
        }
    }
    best
}

/// Silence loud outlier sections (handling bumps, coughs) per the config,
/// returning the cleaned samples and the seconds silenced. [`analyze`] applies
/// this itself; exposed so callers can compute diagnostics on the same signal
/// the analyzer sees.
pub fn suppress_outliers(
    samples: &[f32],
    sample_rate: u32,
    cfg: &AnalysisConfig,
) -> (Vec<f32>, f64) {
    dsp::suppress_loud_windows(
        samples,
        f64::from(sample_rate),
        cfg.outlier_window_s,
        cfg.outlier_peak_ratio,
    )
}

/// The band-pass centres [`analyze`] will scan for `cfg` at `sample_rate`:
/// `cfg.band_centers_hz` (or `cfg.bandpass_center_hz` if that list is empty)
/// with centres at or above the Nyquist region dropped. Exposed so callers can
/// compute diagnostics over the same bands the analyzer sees.
pub fn candidate_band_centers(cfg: &AnalysisConfig, sample_rate: f64) -> Vec<f64> {
    let max_center = 0.45 * sample_rate;
    let mut centers: Vec<f64> = if cfg.band_centers_hz.is_empty() {
        vec![cfg.bandpass_center_hz]
    } else {
        cfg.band_centers_hz.clone()
    };
    centers.retain(|&hz| hz > 0.0 && hz < max_center);
    if centers.is_empty() {
        centers.push(cfg.bandpass_center_hz.min(max_center * 0.99));
    }
    centers
}

/// The single-band measurement pipeline behind [`analyze`]. Expects samples
/// already cleaned by [`suppress_outliers`]; `masked_s` (the silenced
/// duration) is excluded from the expected beat count.
fn analyze_band(
    samples: &[f32],
    sample_rate: u32,
    cfg: &AnalysisConfig,
    band_center_hz: f64,
    masked_s: f64,
) -> Option<Measurement> {
    let sr = f64::from(sample_rate);
    let duration_s = samples.len() as f64 / sr;
    let nominal_period = 3600.0 / f64::from(cfg.bph);
    let beats_expected = ((duration_s - masked_s).max(0.0) / nominal_period).round() as usize;

    let filtered = dsp::bandpass(samples, sr, band_center_hz, cfg.bandpass_q);
    let env = dsp::envelope(&filtered, sr, cfg.envelope_tau_s);
    let (periodicity, detected_bph) = dominant_periodicity(&env, sr);
    let reference = dsp::percentile(&env, cfg.reference_percentile);
    let threshold = cfg.threshold_ratio * reference;
    let onsets = dsp::detect_onsets(&env, sr, threshold, cfg.refractory_s);

    let mut beats = trim_edges(group_beats(&onsets, nominal_period, cfg.beat_gap_ratio));
    let beats_detected = beats.len();
    if beats_detected < 4 {
        return None;
    }

    // Reconstruct each beat's number from its timing. Because the rate error is
    // tiny relative to the period, the nominal period dates each beat correctly
    // even when some beats are missed — so a gap no longer corrupts the fit.
    let t0 = beats[0].onset;
    for b in &mut beats {
        b.index = ((b.onset - t0) / nominal_period).round();
    }

    // First fit, then reject onsets that lie far from the line (spurious
    // detections / merged beats), then refit on the survivors.
    let idx: Vec<f64> = beats.iter().map(|b| b.index).collect();
    let onset_t: Vec<f64> = beats.iter().map(|b| b.onset).collect();
    let (slope, intercept) = ols(&idx, &onset_t)?;
    let tol = cfg.max_residual_ratio * nominal_period;
    let kept: Vec<Beat> = beats
        .iter()
        .copied()
        .filter(|b| (b.onset - (intercept + slope * b.index)).abs() <= tol)
        .collect();
    if kept.len() < 4 {
        return None;
    }

    let kept_idx: Vec<f64> = kept.iter().map(|b| b.index).collect();
    let kept_onset: Vec<f64> = kept.iter().map(|b| b.onset).collect();
    let (measured_period, _) = ols(&kept_idx, &kept_onset)?;
    if measured_period <= 0.0 {
        return None;
    }

    // A watch that gains time has a shorter beat period than nominal.
    let rate_s_per_day = 86_400.0 * (nominal_period / measured_period - 1.0);
    let beat_error_ms = beat_error_ms_from_beats(&kept);
    let amplitude_deg = amplitude_from_beats(&kept, cfg);
    let quality = confidence(
        &kept,
        beats_expected,
        measured_period,
        nominal_period,
        periodicity,
    );

    Some(Measurement {
        rate_s_per_day,
        beat_error_ms,
        amplitude_deg,
        bph: cfg.bph,
        lift_angle_deg: cfg.lift_angle_deg,
        beats_detected,
        beats_used: kept.len(),
        beats_expected,
        periodicity,
        detected_bph,
        band_center_hz,
        masked_s,
        quality,
    })
}

/// Estimate the dominant envelope periodicity (0–1) and the bph it implies,
/// from a precomputed envelope and its sample rate.
///
/// The envelope is decimated to keep the autocorrelation sweep cheap, then we
/// search the lag range spanning plausible beat periods (~16000–80000 bph).
/// Exposed so the app can report periodicity even when [`analyze`] declines to
/// produce a measurement (the "is this just noise?" diagnostic).
pub fn dominant_periodicity(env: &[f64], sr: f64) -> (f64, Option<f64>) {
    let factor = (sr / 2400.0).round().max(1.0) as usize;
    let denv = dsp::decimate_mean(env, factor);
    let dsr = sr / factor as f64;
    let min_lag = (dsr * 0.045).round() as usize; // ~80000 bph
    let max_lag = (dsr * 0.225).round() as usize; // ~16000 bph
    match dsp::dominant_period(&denv, min_lag, max_lag) {
        Some((lag, strength)) if lag > 0 => {
            let beat_period = lag as f64 / dsr;
            (strength.max(0.0), Some(3600.0 / beat_period))
        }
        _ => (0.0, None),
    }
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
            index: 0.0,
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

/// Ordinary least squares; returns `(slope, intercept)` of `ys` on `xs`.
fn ols(xs: &[f64], ys: &[f64]) -> Option<(f64, f64)> {
    let n = xs.len() as f64;
    if xs.len() < 2 {
        return None;
    }
    let sx: f64 = xs.iter().sum();
    let sy: f64 = ys.iter().sum();
    let sxx: f64 = xs.iter().map(|x| x * x).sum();
    let sxy: f64 = xs.iter().zip(ys).map(|(x, y)| x * y).sum();
    let denom = n * sxx - sx * sx;
    if denom.abs() < f64::EPSILON {
        return None;
    }
    let slope = (n * sxy - sx * sy) / denom;
    let intercept = (sy - slope * sx) / n;
    Some((slope, intercept))
}

/// Beat error: the difference between the two interleaved half-period intervals
/// (tick vs tock), in milliseconds, using only beats with consecutive numbers
/// so that missed beats are skipped rather than mis-paired.
fn beat_error_ms_from_beats(beats: &[Beat]) -> f64 {
    let mut even = Vec::new();
    let mut odd = Vec::new();
    for w in beats.windows(2) {
        let (a, b) = (w[0], w[1]);
        if (b.index - a.index - 1.0).abs() < 0.5 {
            let d = b.onset - a.onset;
            if (a.index as i64).rem_euclid(2) == 0 {
                even.push(d);
            } else {
                odd.push(d);
            }
        }
    }
    if even.is_empty() || odd.is_empty() {
        return 0.0;
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

/// Confidence in [0, 1] combining how many of the expected beats survived and
/// how closely the measured period matches the nominal one (a sanity check
/// that we locked onto a real, periodic tick rather than noise).
fn confidence(
    kept: &[Beat],
    beats_expected: usize,
    measured_period: f64,
    nominal: f64,
    periodicity: f64,
) -> f64 {
    if beats_expected == 0 {
        return 0.0;
    }
    let coverage = (kept.len() as f64 / beats_expected as f64).clamp(0.0, 1.0);
    let period_factor = (1.0 - (measured_period / nominal - 1.0).abs() / 0.2).clamp(0.0, 1.0);
    // Periodicity is the strongest discriminator of tick-vs-noise; gate on it.
    let periodicity_factor = (periodicity / 0.4).clamp(0.0, 1.0);
    (coverage * period_factor * periodicity_factor).clamp(0.0, 1.0)
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
    fn periodicity_high_and_bph_detected_on_clean_signal() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 10.0,
            ..Default::default()
        });
        assert!(m.periodicity > 0.8, "periodicity={}", m.periodicity);
        let bph = m.detected_bph.expect("detected bph");
        assert!(
            (bph - 28_800.0).abs() < 28_800.0 * 0.02,
            "detected_bph={bph}"
        );
    }

    #[test]
    fn detects_most_beats_on_clean_signal() {
        let m = analyze_spec(&SignalSpec {
            duration_s: 5.0,
            ..Default::default()
        });
        assert!(m.beats_detected >= 36, "beats={}", m.beats_detected);
        assert!(m.quality > 0.8, "quality={}", m.quality);
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
        assert!(m.quality > 0.8, "quality={}", m.quality);
    }

    #[test]
    fn handles_alternate_beat_rate() {
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

    #[test]
    fn rate_robust_to_louder_noise() {
        // Heavier noise (SNR ~ a few): outlier rejection + reconstructed indices
        // should still recover rate within tolerance, with decent confidence.
        let m = analyze_spec(&SignalSpec {
            duration_s: 20.0,
            rate_s_per_day: 8.0,
            amplitude_deg: 275.0,
            noise_amplitude: 0.06,
            seed: 11,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day - 8.0).abs() < 1.0,
            "rate={} quality={}",
            m.rate_s_per_day,
            m.quality
        );
        assert!(m.quality > 0.6, "quality={}", m.quality);
    }

    #[test]
    fn band_scan_finds_high_frequency_tick() {
        // An air-coupled recording (phone/laptop mic at a distance) carries the
        // tick at 8–16 kHz, far from the 3 kHz contact-mic default. The band
        // scan must still lock on.
        let m = analyze_spec(&SignalSpec {
            duration_s: 15.0,
            sample_rate: 48_000,
            rate_s_per_day: 10.0,
            carrier_hz: 12_000.0,
            noise_amplitude: 0.02,
            seed: 3,
            ..Default::default()
        });
        assert!(
            (m.rate_s_per_day - 10.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
        assert!(m.quality > 0.7, "quality={}", m.quality);
        assert!(
            m.band_center_hz >= 9_000.0,
            "expected a high band, got {} Hz",
            m.band_center_hz
        );
    }

    #[test]
    fn loud_bumps_are_masked_not_fatal() {
        // A handling bump / cough dwarfs the ticks (recordings show 30x). It
        // must be silenced rather than allowed to wreck the onset threshold,
        // and the rate must still come out right.
        let spec = SignalSpec {
            duration_s: 20.0,
            rate_s_per_day: 5.0,
            noise_amplitude: 0.02,
            seed: 9,
            ..Default::default()
        };
        let mut sig = synth_escapement(&spec);
        let sr = sig.sample_rate as usize;
        // Scale ticks down to a realistic air-mic level, then add two loud
        // broadband bursts (a bump at 2 s, a cough at 11 s).
        for s in &mut sig.samples {
            *s *= 0.02;
        }
        let mut rng = 1u64;
        let mut noise = move || {
            rng = rng.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            ((rng >> 33) as f32 / (1u64 << 31) as f32) - 0.5
        };
        for i in 2 * sr..(2 * sr + sr / 2) {
            sig.samples[i] += noise();
        }
        for i in 11 * sr..(11 * sr + sr / 4) {
            sig.samples[i] += noise();
        }

        let cfg = AnalysisConfig::new(spec.bph, spec.lift_angle_deg);
        let m = analyze(&sig.samples, sig.sample_rate, &cfg).expect("a measurement");
        assert!(m.masked_s > 0.5, "masked_s={}", m.masked_s);
        assert!(
            (m.rate_s_per_day - 5.0).abs() < 1.0,
            "rate={}",
            m.rate_s_per_day
        );
        assert!(m.quality > 0.7, "quality={}", m.quality);
    }

    #[test]
    fn candidate_bands_respect_nyquist() {
        let cfg = AnalysisConfig::new(28_800, 52.0);
        // At 48 kHz all default centres are usable.
        assert_eq!(
            candidate_band_centers(&cfg, 48_000.0).len(),
            DEFAULT_BAND_CENTERS_HZ.len()
        );
        // A 16 kHz (Bluetooth-ish) input must not scan centres above ~7.2 kHz.
        let bands = candidate_band_centers(&cfg, 16_000.0);
        assert!(bands.iter().all(|&hz| hz < 7_200.0), "bands={bands:?}");
        assert!(!bands.is_empty());
        // An empty scan list falls back to the single configured centre.
        let single = AnalysisConfig {
            band_centers_hz: Vec::new(),
            ..AnalysisConfig::new(28_800, 52.0)
        };
        assert_eq!(candidate_band_centers(&single, 48_000.0), vec![3000.0]);
    }

    #[test]
    fn pure_noise_yields_low_confidence_or_none() {
        // White-ish noise with no ticks must not masquerade as a confident
        // reading: analyze should return None or a low quality score.
        let noise: Vec<f32> = (0..44_100 * 10)
            .map(|i| {
                let r = (i as u64).wrapping_mul(2_654_435_761).wrapping_add(12345) % 2003;
                (r as f32 / 1001.0 - 1.0) * 0.5
            })
            .collect();
        let cfg = AnalysisConfig::new(28_800, 52.0);
        if let Some(m) = analyze(&noise, 44_100, &cfg) {
            assert!(m.quality < 0.5, "noise quality too high: {}", m.quality);
        }
    }
}
