//! Pure DSP / measurement core for TimegrapherQ.
//!
//! This crate intentionally has **no I/O and no UI**: it takes numbers (and,
//! later, audio sample buffers) and returns measurement results. Keeping it
//! pure makes the horology maths unit-testable against known ground truth,
//! which is the cornerstone of the project's test strategy (see
//! `docs/TEST_PLAN.md`).

/// Crate version, surfaced to the app for a health check.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Full balance oscillation period in seconds for a given beat frequency.
///
/// One beat is a half-oscillation, so a full oscillation spans two beats:
/// `T = 7200 / bph` seconds.
pub fn full_period_seconds(bph: u32) -> f64 {
    debug_assert!(bph > 0, "bph must be positive");
    7200.0 / f64::from(bph)
}

/// Balance-wheel amplitude in degrees.
///
/// Modelling the balance as simple harmonic motion `θ(t) = A·sin(ωt)` with
/// `ω = 2π/T`, the two impulse transients within a beat straddle the rest
/// position, separated by the time `delta_t` (seconds) it takes the balance to
/// traverse the `lift_angle` (degrees):
///
/// ```text
/// A = lift_angle / ( 2 · sin( π · delta_t / T ) ),   T = 7200 / bph
/// ```
///
/// Returns `None` if the inputs are non-physical (would put the `sin` argument
/// outside the valid range or divide by zero).
pub fn amplitude_degrees(delta_t: f64, lift_angle: f64, bph: u32) -> Option<f64> {
    if !delta_t.is_finite()
        || delta_t <= 0.0
        || !lift_angle.is_finite()
        || lift_angle <= 0.0
        || bph == 0
    {
        return None;
    }
    let t = full_period_seconds(bph);
    let s = (std::f64::consts::PI * delta_t / t).sin();
    if s <= 0.0 {
        return None;
    }
    Some(lift_angle / (2.0 * s))
}

/// Inverse of [`amplitude_degrees`]: the impulse-transient spacing `delta_t`
/// (seconds) that a given `amplitude` (degrees) would produce. Used by the
/// synthetic-signal generator in tests to create signals with a known answer.
pub fn impulse_spacing_seconds(amplitude_deg: f64, lift_angle: f64, bph: u32) -> Option<f64> {
    if !amplitude_deg.is_finite()
        || amplitude_deg <= 0.0
        || !lift_angle.is_finite()
        || lift_angle <= 0.0
        || bph == 0
    {
        return None;
    }
    let ratio = lift_angle / (2.0 * amplitude_deg);
    if !(-1.0..=1.0).contains(&ratio) {
        return None; // amplitude too small for this lift angle
    }
    let t = full_period_seconds(bph);
    Some(t * ratio.asin() / std::f64::consts::PI)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_period_matches_known_frequencies() {
        // 28800 bph = 4 Hz balance => full period 0.25 s.
        assert!((full_period_seconds(28800) - 0.25).abs() < 1e-12);
        // 21600 bph => 7200/21600 = 1/3 s.
        assert!((full_period_seconds(21600) - 1.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn amplitude_round_trips_against_its_inverse() {
        // For a sweep of realistic amplitudes, going amplitude -> delta_t ->
        // amplitude must recover the original within tight tolerance.
        let lift = 52.0;
        let bph = 28800;
        for amp in [180.0, 220.0, 270.0, 300.0, 315.0_f64] {
            let dt = impulse_spacing_seconds(amp, lift, bph).expect("valid dt");
            let back = amplitude_degrees(dt, lift, bph).expect("valid amplitude");
            assert!(
                (back - amp).abs() < 1e-9,
                "amplitude round-trip failed: {amp} -> {dt} -> {back}"
            );
        }
    }

    #[test]
    fn rejects_non_physical_inputs() {
        assert_eq!(amplitude_degrees(0.0, 52.0, 28800), None);
        assert_eq!(amplitude_degrees(0.01, 0.0, 28800), None);
        assert_eq!(amplitude_degrees(0.01, 52.0, 0), None);
        // Amplitude smaller than half the lift angle is impossible.
        assert_eq!(impulse_spacing_seconds(20.0, 52.0, 28800), None);
    }
}
