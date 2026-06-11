//! Synthetic escapement-signal generator.
//!
//! Produces audio that mimics a ticking mechanical watch, with **known**
//! rate, beat error, amplitude and noise. This is the ground-truth source for
//! validating the measurement pipeline (see `docs/TEST_PLAN.md`): a test builds
//! a signal with chosen parameters, runs the analyzer, and asserts the analyzer
//! recovers those parameters within tolerance.
//!
//! The model is intentionally simple but physically consistent with the
//! amplitude relationship used by the analyzer:
//!
//! * Each **beat** is a half-oscillation; the nominal beat period is
//!   `3600 / bph` seconds.
//! * **Rate** scales every interval by `86400 / (86400 + rate_s_per_day)`
//!   (a watch that gains time has shorter intervals).
//! * **Beat error** alternates successive intervals by `±Δ` so that a full
//!   oscillation (two beats) keeps a constant period — i.e. beat error does not
//!   leak into rate.
//! * **Amplitude** is encoded as a second transient within each beat, offset by
//!   `Δt = impulse_spacing(amplitude, lift_angle, bph)` from the beat onset.
//! * **Noise** is additive white Gaussian noise from a seeded PRNG, so signals
//!   are fully reproducible.

use crate::impulse_spacing_seconds;
use std::f64::consts::PI;

/// Seconds in a day; used to convert a rate in s/day into an interval scale.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// Default carrier frequency of a single escapement "click" (Hz).
const TRANSIENT_CARRIER_HZ: f64 = 3_500.0;

/// Exponential decay time constant of a click (seconds).
const TRANSIENT_DECAY_S: f64 = 0.0015;

/// Parameters describing a synthetic escapement recording.
#[derive(Debug, Clone)]
pub struct SignalSpec {
    /// Output sample rate (Hz).
    pub sample_rate: u32,
    /// Total duration (seconds).
    pub duration_s: f64,
    /// Design beat frequency (beats/hour).
    pub bph: u32,
    /// Rate deviation (seconds/day); positive = fast.
    pub rate_s_per_day: f64,
    /// Beat error (milliseconds); must be >= 0.
    pub beat_error_ms: f64,
    /// Balance amplitude (degrees).
    pub amplitude_deg: f64,
    /// Movement lift angle (degrees).
    pub lift_angle_deg: f64,
    /// Standard deviation of additive white Gaussian noise (0.0 = clean).
    /// Transient peaks are ~1.0, so this is roughly the inverse linear SNR.
    pub noise_amplitude: f64,
    /// PRNG seed for reproducible noise.
    pub seed: u64,
    /// Carrier frequency of each click (Hz). ~3.5 kHz models a contact mic;
    /// air-coupled recordings carry the tick much higher (8–16 kHz).
    pub carrier_hz: f64,
}

impl Default for SignalSpec {
    fn default() -> Self {
        Self {
            sample_rate: 44_100,
            duration_s: 10.0,
            bph: 28_800,
            rate_s_per_day: 0.0,
            beat_error_ms: 0.0,
            amplitude_deg: 270.0,
            lift_angle_deg: 52.0,
            noise_amplitude: 0.0,
            seed: 0x5EED,
            carrier_hz: TRANSIENT_CARRIER_HZ,
        }
    }
}

/// A generated signal together with its ground truth.
#[derive(Debug, Clone)]
pub struct SynthSignal {
    /// Mono PCM samples in roughly [-1, 1] (noise may exceed slightly).
    pub samples: Vec<f32>,
    /// Sample rate (Hz).
    pub sample_rate: u32,
    /// True onset time of each beat's first transient (seconds).
    pub beat_onsets_s: Vec<f64>,
    /// Within-beat transient spacing used to encode amplitude (seconds).
    pub impulse_spacing_s: f64,
}

/// Deterministic SplitMix64 PRNG — small, dependency-free, good enough for
/// reproducible test noise.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        // Use the top 53 bits for a double in [0, 1).
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal sample via the Box-Muller transform.
    fn next_gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-12); // avoid ln(0)
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

/// Value of one escapement click at `dt` seconds after its onset.
/// A damped sinusoid; zero before onset and effectively zero after a few `tau`.
fn transient(dt: f64, carrier_hz: f64) -> f64 {
    if dt < 0.0 {
        return 0.0;
    }
    (-dt / TRANSIENT_DECAY_S).exp() * (2.0 * PI * carrier_hz * dt).sin()
}

/// Generate a synthetic escapement recording from `spec`.
///
/// Returns the samples plus the ground-truth beat onsets and impulse spacing.
/// Beat-error and rate are applied to the inter-beat intervals; amplitude is
/// rendered as a second click per beat.
pub fn synth_escapement(spec: &SignalSpec) -> SynthSignal {
    let sr = f64::from(spec.sample_rate);
    let n_samples = (spec.duration_s * sr).round() as usize;
    let mut samples = vec![0.0f64; n_samples];

    let nominal_period = 3600.0 / f64::from(spec.bph); // seconds per beat
    let rate_scale = SECONDS_PER_DAY / (SECONDS_PER_DAY + spec.rate_s_per_day);
    let period = nominal_period * rate_scale;
    let half_be = (spec.beat_error_ms / 1000.0) / 2.0;

    // Amplitude -> within-beat transient spacing. If the amplitude is
    // non-physical for the lift angle, fall back to a single transient.
    let spacing =
        impulse_spacing_seconds(spec.amplitude_deg, spec.lift_angle_deg, spec.bph).unwrap_or(0.0);

    // Build beat onset times across the duration. Alternate +Δ / -Δ on the
    // intervals so each full oscillation (two beats) keeps period 2*`period`.
    let mut onsets = Vec::new();
    let mut t = 0.0;
    let mut k = 0usize;
    while t < spec.duration_s {
        onsets.push(t);
        let delta = if k.is_multiple_of(2) {
            half_be
        } else {
            -half_be
        };
        t += period + delta;
        k += 1;
    }

    // Render transients. Each click is short, so we only touch a small window.
    let window_s = 8.0 * TRANSIENT_DECAY_S;
    let window_n = (window_s * sr).ceil() as usize;
    for &onset in &onsets {
        for click_time in [onset, onset + spacing] {
            if spacing == 0.0 && click_time != onset {
                continue; // single transient when amplitude is non-physical
            }
            let start = (click_time * sr).floor() as isize;
            for i in 0..window_n as isize {
                let idx = start + i;
                if idx < 0 || idx as usize >= n_samples {
                    continue;
                }
                let sample_t = idx as f64 / sr;
                samples[idx as usize] += transient(sample_t - click_time, spec.carrier_hz);
            }
        }
    }

    // Additive white Gaussian noise.
    if spec.noise_amplitude > 0.0 {
        let mut rng = SplitMix64::new(spec.seed);
        for s in &mut samples {
            *s += spec.noise_amplitude * rng.next_gaussian();
        }
    }

    SynthSignal {
        samples: samples.into_iter().map(|s| s as f32).collect(),
        sample_rate: spec.sample_rate,
        beat_onsets_s: onsets,
        impulse_spacing_s: spacing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_expected_sample_count() {
        let spec = SignalSpec {
            sample_rate: 48_000,
            duration_s: 2.0,
            ..Default::default()
        };
        let sig = synth_escapement(&spec);
        assert_eq!(sig.samples.len(), 96_000);
        assert_eq!(sig.sample_rate, 48_000);
    }

    #[test]
    fn beat_count_matches_frequency() {
        // 28800 bph = 8 beats/s; over 3 s expect ~24 beats.
        let spec = SignalSpec {
            duration_s: 3.0,
            bph: 28_800,
            ..Default::default()
        };
        let sig = synth_escapement(&spec);
        assert!(
            (23..=25).contains(&sig.beat_onsets_s.len()),
            "unexpected beat count: {}",
            sig.beat_onsets_s.len()
        );
    }

    #[test]
    fn rate_shortens_intervals_when_fast() {
        let nominal = 3600.0 / 28_800.0;
        let fast = synth_escapement(&SignalSpec {
            rate_s_per_day: 86_400.0, // double speed: scale = 1/2
            beat_error_ms: 0.0,
            ..Default::default()
        });
        let interval = fast.beat_onsets_s[1] - fast.beat_onsets_s[0];
        assert!(
            (interval - nominal / 2.0).abs() < 1e-9,
            "interval={interval}"
        );
    }

    #[test]
    fn beat_error_alternates_intervals() {
        let be_ms = 1.0;
        let sig = synth_escapement(&SignalSpec {
            rate_s_per_day: 0.0,
            beat_error_ms: be_ms,
            ..Default::default()
        });
        let i0 = sig.beat_onsets_s[1] - sig.beat_onsets_s[0];
        let i1 = sig.beat_onsets_s[2] - sig.beat_onsets_s[1];
        // The two successive intervals differ by exactly the beat error.
        assert!(((i0 - i1).abs() - be_ms / 1000.0).abs() < 1e-9);
        // ...but two beats sum to a constant full-oscillation period (rate clean).
        let full = sig.beat_onsets_s[2] - sig.beat_onsets_s[0];
        assert!((full - 2.0 * 3600.0 / 28_800.0).abs() < 1e-9);
    }

    #[test]
    fn impulse_spacing_matches_amplitude_formula() {
        let spec = SignalSpec {
            amplitude_deg: 270.0,
            lift_angle_deg: 52.0,
            bph: 28_800,
            ..Default::default()
        };
        let sig = synth_escapement(&spec);
        let expected = impulse_spacing_seconds(270.0, 52.0, 28_800).unwrap();
        assert!((sig.impulse_spacing_s - expected).abs() < 1e-12);
        assert!(sig.impulse_spacing_s > 0.0);
    }

    #[test]
    fn clean_signal_has_energy_near_onsets() {
        let spec = SignalSpec {
            duration_s: 1.0,
            noise_amplitude: 0.0,
            ..Default::default()
        };
        let sig = synth_escapement(&spec);
        // The sample at the first onset region should be well above the global
        // mean magnitude (it's a transient against a silent background).
        let peak = sig.samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.5, "expected a clear transient peak, got {peak}");
    }

    #[test]
    fn noise_is_deterministic_for_a_seed() {
        let spec = SignalSpec {
            duration_s: 0.5,
            noise_amplitude: 0.1,
            seed: 42,
            ..Default::default()
        };
        let a = synth_escapement(&spec);
        let b = synth_escapement(&spec);
        assert_eq!(a.samples, b.samples);

        let c = synth_escapement(&SignalSpec { seed: 43, ..spec });
        assert_ne!(a.samples, c.samples);
    }
}
