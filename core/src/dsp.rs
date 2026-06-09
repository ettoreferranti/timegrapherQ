//! Low-level DSP primitives for the measurement front-end: band-pass
//! filtering, envelope extraction, and onset (transient) detection.
//!
//! These are pure functions over sample slices so they can be unit-tested in
//! isolation and reused by both the record-then-analyse and live paths.

use std::f64::consts::PI;

/// A second-order IIR section (biquad), transposed Direct-Form II.
#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// Band-pass section with constant 0 dB peak gain (RBJ audio-EQ cookbook).
    ///
    /// `center_hz` is the geometric centre of the pass-band and `q` controls
    /// its width (bandwidth ≈ `center_hz / q`).
    pub fn bandpass(center_hz: f64, q: f64, sample_rate: f64) -> Self {
        let w0 = 2.0 * PI * center_hz / sample_rate;
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * cos_w0 / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Process one sample, advancing the filter state.
    pub fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Band-pass filter a signal, emphasising escapement transient energy and
/// rejecting low-frequency room rumble and mains hum.
pub fn bandpass(samples: &[f32], sample_rate: f64, center_hz: f64, q: f64) -> Vec<f64> {
    let mut bq = Biquad::bandpass(center_hz, q, sample_rate);
    samples.iter().map(|&x| bq.process(f64::from(x))).collect()
}

/// Amplitude envelope: full-wave rectification followed by a one-pole low-pass
/// smoother with time constant `tau_s`. The smoothing introduces a small,
/// *consistent* group delay, which cancels out of rate, beat-error and
/// amplitude (all derived from relative timing).
pub fn envelope(signal: &[f64], sample_rate: f64, tau_s: f64) -> Vec<f64> {
    let alpha = 1.0 - (-1.0 / (tau_s * sample_rate)).exp();
    let mut e = 0.0;
    signal
        .iter()
        .map(|&x| {
            e += alpha * (x.abs() - e);
            e
        })
        .collect()
}

/// Detect transient onsets as the argmax of each contiguous run where the
/// envelope exceeds `threshold_ratio * max_envelope`, with accepted peaks kept
/// at least `refractory_s` apart. Returns onset times in seconds.
pub fn detect_onsets(
    env: &[f64],
    sample_rate: f64,
    threshold_ratio: f64,
    refractory_s: f64,
) -> Vec<f64> {
    let max_env = env.iter().copied().fold(0.0_f64, f64::max);
    if max_env <= 0.0 {
        return Vec::new();
    }
    let thr = threshold_ratio * max_env;
    let refractory_n = (refractory_s * sample_rate).round() as usize;

    let mut onsets = Vec::new();
    let mut last_peak: Option<usize> = None;
    let mut i = 0;
    let n = env.len();
    while i < n {
        if env[i] > thr {
            // Walk to the end of this above-threshold run, tracking its peak.
            let mut j = i;
            let mut peak = i;
            while j < n && env[j] > thr {
                if env[j] > env[peak] {
                    peak = j;
                }
                j += 1;
            }
            let accept = match last_peak {
                None => true,
                Some(p) => peak.saturating_sub(p) >= refractory_n,
            };
            if accept {
                onsets.push(peak as f64 / sample_rate);
                last_peak = Some(peak);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    onsets
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz tone band-passed at 1 kHz should survive; a 50 Hz tone should be
    /// strongly attenuated.
    #[test]
    fn bandpass_passes_center_rejects_low() {
        let fs = 44_100.0;
        let make_tone = |hz: f64| -> Vec<f32> {
            (0..44_100)
                .map(|i| (2.0 * PI * hz * i as f64 / fs).sin() as f32)
                .collect()
        };
        let rms = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();

        let pass = bandpass(&make_tone(1000.0), fs, 1000.0, 0.7);
        let reject = bandpass(&make_tone(50.0), fs, 1000.0, 0.7);
        // Ignore filter settling at the very start.
        assert!(rms(&pass[2000..]) > 0.5);
        assert!(rms(&reject[2000..]) < 0.1);
    }

    #[test]
    fn envelope_is_nonnegative_and_tracks_bursts() {
        let fs = 44_100.0;
        let mut sig = vec![0.0; 4410];
        // A short burst in the middle.
        for (i, s) in sig.iter_mut().enumerate().take(2300).skip(2200) {
            *s = (2.0 * PI * 3000.0 * i as f64 / fs).sin();
        }
        let env = envelope(&sig, fs, 0.0008);
        assert!(env.iter().all(|&e| e >= 0.0));
        let burst_peak = env[2200..2400].iter().copied().fold(0.0, f64::max);
        assert!(burst_peak > env[0..2000].iter().copied().fold(0.0, f64::max));
    }

    #[test]
    fn detects_two_separated_pulses() {
        let fs = 48_000.0;
        let mut env = vec![0.0; 4800];
        env[1000] = 1.0; // pulse 1
        env[3000] = 0.8; // pulse 2, 2000 samples later
        let onsets = detect_onsets(&env, fs, 0.3, 0.003);
        assert_eq!(onsets.len(), 2);
        assert!((onsets[0] - 1000.0 / fs).abs() < 1e-9);
        assert!((onsets[1] - 3000.0 / fs).abs() < 1e-9);
    }

    #[test]
    fn refractory_suppresses_close_second_peak() {
        let fs = 48_000.0;
        let mut env = vec![0.0; 1000];
        env[100] = 1.0;
        env[150] = 0.9; // ~1 ms later, inside a 3 ms refractory window
        let onsets = detect_onsets(&env, fs, 0.3, 0.003);
        assert_eq!(onsets.len(), 1);
    }
}
