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

/// Detect transient onsets in each contiguous run where the envelope exceeds
/// the absolute `threshold`, with accepted events kept at least `refractory_s`
/// apart. Returns onset times in seconds.
///
/// Each event is timestamped at its **leading edge**: the (interpolated)
/// instant the envelope first crosses `edge_fraction` of that run's own peak.
/// A tick is a burst of sub-pulses whose relative loudness varies from beat to
/// beat, so the peak position jumps between sub-pulses by milliseconds; the
/// height-normalised rising edge of the first pulse is far more stable, which
/// directly tightens the rate fit. `edge_fraction >= 1.0` restores peak timing.
///
/// The threshold is supplied by the caller (see [`percentile`]) so it can be
/// made robust to a few loud outliers rather than tied to the global maximum.
pub fn detect_onsets(
    env: &[f64],
    sample_rate: f64,
    threshold: f64,
    refractory_s: f64,
    edge_fraction: f64,
) -> Vec<f64> {
    if threshold <= 0.0 {
        return Vec::new();
    }
    let thr = threshold;
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
                onsets.push(edge_time(env, i, peak, thr, edge_fraction, sample_rate));
                last_peak = Some(peak);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    onsets
}

/// The interpolated time at which `env` first reaches `edge_fraction` of the
/// run's peak value, searching from the run start `i` to the `peak` index.
fn edge_time(
    env: &[f64],
    i: usize,
    peak: usize,
    threshold: f64,
    edge_fraction: f64,
    sample_rate: f64,
) -> f64 {
    if edge_fraction >= 1.0 {
        return peak as f64 / sample_rate;
    }
    let level = (edge_fraction * env[peak]).max(threshold);
    let k = (i..=peak).find(|&k| env[k] >= level).unwrap_or(peak);
    if k == 0 {
        return 0.0;
    }
    // env[k-1] < level <= env[k]: linear interpolation between the two samples.
    let (lo, hi) = (env[k - 1], env[k]);
    let frac = if hi > lo { (level - lo) / (hi - lo) } else { 1.0 };
    ((k - 1) as f64 + frac) / sample_rate
}

/// Silence windows whose peak is far above the typical (median) window peak —
/// handling bumps, coughs, dropped tools — returning the cleaned samples and
/// how many seconds were silenced.
///
/// With `window_s` longer than a beat period, a normal window's peak is the
/// tick peak, so the median tracks the tick level and a `ratio` of a few keeps
/// every tick while rejecting transients an order of magnitude louder. Short
/// fades at the mask edges avoid step discontinuities that would ring through
/// the band-pass filter. A `ratio <= 0` disables suppression.
pub fn suppress_loud_windows(
    samples: &[f32],
    sample_rate: f64,
    window_s: f64,
    ratio: f64,
) -> (Vec<f32>, f64) {
    let w = (window_s * sample_rate).round() as usize;
    // Need a clear majority of windows for the median to mean anything.
    if ratio <= 0.0 || w == 0 || samples.len() < 8 * w {
        return (samples.to_vec(), 0.0);
    }

    let peaks: Vec<f32> = samples
        .chunks(w)
        .map(|c| c.iter().fold(0.0_f32, |m, &s| m.max(s.abs())))
        .collect();
    let mut sorted = peaks.clone();
    sorted.sort_by(f32::total_cmp);
    let median = sorted[sorted.len() / 2];
    if median <= 0.0 {
        return (samples.to_vec(), 0.0);
    }

    let limit = ratio as f32 * median;
    let mut out = samples.to_vec();
    let fade = ((0.002 * sample_rate) as usize).max(1);
    let mut masked_samples = 0usize;
    for (i, &p) in peaks.iter().enumerate() {
        if p <= limit {
            continue;
        }
        let start = i * w;
        let end = ((i + 1) * w).min(out.len());
        masked_samples += end - start;
        out[start..end].fill(0.0);
        // Fade the kept neighbours toward the silence on both sides.
        for k in 0..fade {
            let g = k as f32 / fade as f32;
            if start > k {
                out[start - 1 - k] *= g;
            }
            if end + k < out.len() {
                out[end + k] *= g;
            }
        }
    }
    (out, masked_samples as f64 / sample_rate)
}

/// The `p`-quantile (0.0–1.0) of `values` via nearest-rank on a sorted copy.
/// Used to derive a detection reference level that ignores a few extreme
/// outliers (e.g. handling bumps), unlike the raw maximum.
pub fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let p = p.clamp(0.0, 1.0);
    let idx = (p * (v.len() - 1) as f64).round() as usize;
    v[idx]
}

/// Decimate by averaging non-overlapping blocks of `factor` samples. Used to
/// shrink the envelope before autocorrelation so a full lag sweep is cheap.
pub fn decimate_mean(signal: &[f64], factor: usize) -> Vec<f64> {
    if factor <= 1 {
        return signal.to_vec();
    }
    signal
        .chunks(factor)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

/// Find the lag in `[min_lag, max_lag]` with the strongest normalized
/// autocorrelation of `signal`, returning `(lag, strength)` where strength is
/// in roughly `[-1, 1]`. A strong peak means the signal is periodic at that lag
/// — the acid test for "is this a ticking watch or just noise?".
pub fn dominant_period(signal: &[f64], min_lag: usize, max_lag: usize) -> Option<(usize, f64)> {
    let n = signal.len();
    if min_lag == 0 || min_lag > max_lag || max_lag >= n {
        return None;
    }
    let mean = signal.iter().sum::<f64>() / n as f64;
    let dev: Vec<f64> = signal.iter().map(|&x| x - mean).collect();
    let den: f64 = dev.iter().map(|d| d * d).sum();
    if den <= 0.0 {
        return None;
    }

    let mut best_lag = min_lag;
    let mut best = f64::MIN;
    for lag in min_lag..=max_lag {
        let mut num = 0.0;
        for i in 0..n - lag {
            num += dev[i] * dev[i + lag];
        }
        let r = num / den;
        if r > best {
            best = r;
            best_lag = lag;
        }
    }
    Some((best_lag, best))
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
        let onsets = detect_onsets(&env, fs, 0.3, 0.003, 1.0);
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
        let onsets = detect_onsets(&env, fs, 0.3, 0.003, 1.0);
        assert_eq!(onsets.len(), 1);
    }

    #[test]
    fn onset_edge_timing_is_height_invariant() {
        // Two ramps with identical shape but different heights: the
        // half-height leading edge lands at the same point up each ramp,
        // independent of pulse height. Each pulse rises linearly over 10
        // samples, so the 50% crossing sits mid-ramp.
        let fs = 1000.0;
        let mut env = vec![0.0; 1000];
        for k in 0..=10 {
            env[100 + k] = k as f64 / 10.0; // pulse 1: peak 1.0 at 110
            env[500 + k] = 0.4 * k as f64 / 10.0; // pulse 2: peak 0.4 at 510
        }
        let onsets = detect_onsets(&env, fs, 0.05, 0.003, 0.5);
        assert_eq!(onsets.len(), 2);
        // Both cross 50% of their own peak 5 samples up the ramp.
        assert!((onsets[0] - 105.0 / fs).abs() < 1.0 / fs, "{onsets:?}");
        assert!((onsets[1] - 505.0 / fs).abs() < 1.0 / fs, "{onsets:?}");
        // edge_fraction >= 1.0 restores peak timing.
        let peaks = detect_onsets(&env, fs, 0.05, 0.003, 1.0);
        assert!((peaks[0] - 110.0 / fs).abs() < 1e-9);
    }

    #[test]
    fn dominant_period_finds_periodic_lag() {
        // A spike train every 100 samples should peak at lag 100.
        let mut s = vec![0.0; 2000];
        let mut i = 0;
        while i < 2000 {
            s[i] = 1.0;
            i += 100;
        }
        let (lag, strength) = dominant_period(&s, 50, 200).unwrap();
        assert_eq!(lag, 100);
        assert!(strength > 0.5, "strength={strength}");
    }

    #[test]
    fn dominant_period_weak_on_noise() {
        // Decorrelated pseudo-noise (SplitMix64 hash per index): no periodicity.
        let s: Vec<f64> = (0..4000u64)
            .map(|i| {
                let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                ((z ^ (z >> 31)) % 2000) as f64 / 1000.0 - 1.0
            })
            .collect();
        let (_, strength) = dominant_period(&s, 50, 400).unwrap();
        assert!(strength < 0.5, "noise strength too high: {strength}");
    }

    #[test]
    fn decimate_mean_averages_blocks() {
        assert_eq!(decimate_mean(&[1.0, 3.0, 5.0, 7.0], 2), vec![2.0, 6.0]);
    }

    #[test]
    fn suppress_loud_windows_masks_only_the_bump() {
        // 10 s at 1 kHz: a steady "tick" peak of 0.01 per window, plus a loud
        // bump (0.5) in one window. Only the bump window should be silenced.
        let sr = 1000.0; // 0.25 s windows = 250 samples; the bump fills window 8
        let mut s = vec![0.0_f32; 10_000];
        for i in (0..s.len()).step_by(125) {
            s[i] = 0.01; // ticks
        }
        for x in &mut s[2_000..2_250] {
            *x = 0.5; // bump fills window 8
        }
        let (out, masked) = suppress_loud_windows(&s, sr, 0.25, 5.0);
        assert!((masked - 0.25).abs() < 1e-9, "masked={masked}");
        assert!(out[2_000..2_250].iter().all(|&x| x == 0.0));
        // Ticks well away from the bump survive untouched.
        assert_eq!(out[5_000], 0.01);
        assert_eq!(out[125], 0.01);
        // A clean signal is untouched.
        let clean: Vec<f32> = s.iter().map(|&x| x.min(0.01)).collect();
        let (out, masked) = suppress_loud_windows(&clean, sr, 0.25, 5.0);
        assert_eq!(masked, 0.0);
        assert_eq!(out, clean);
        // Disabled by ratio <= 0.
        let (_, masked) = suppress_loud_windows(&s, sr, 0.25, 0.0);
        assert_eq!(masked, 0.0);
    }

    #[test]
    fn percentile_ignores_sparse_outliers() {
        // 100 samples near 1.0 plus one huge spike: p99 stays near 1, not 1000.
        let mut v = vec![1.0; 100];
        v.push(1000.0);
        let p99 = percentile(&v, 0.99);
        assert!((p99 - 1.0).abs() < 1e-9, "p99={p99}");
        assert_eq!(percentile(&v, 1.0), 1000.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}
