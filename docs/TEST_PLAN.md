# TimegrapherQ — Test Plan

> Living document. **Keep this up to date as the implementation evolves.**
> Status: **Draft v0.1**. Last updated: 2026-06-09.

## 1. Strategy

Because the project's value rests entirely on **measurement trustworthiness**,
the test strategy is centred on validating the DSP core against **known ground
truth** before and alongside UI work. We use a layered approach:

| Level | What | Tooling |
|---|---|---|
| Unit | Pure functions in the DSP `core` crate and TS utilities | `cargo test`, `vitest` |
| Property/fixture | DSP against synthetic + recorded signals with known answers | `cargo test` + fixtures |
| Integration | Tauri commands, storage, migrations, export/import | Rust integration tests |
| End-to-end | Full user flows in the running app | WebDriver/`tauri-driver` or manual scripts |
| Manual | Real watches, real mics (incl. AirPods) in real rooms | Documented procedures |
| Non-functional | Performance, privacy/no-network, security | dedicated checks in CI |

**Ground-truth principle:** every measurement metric (rate, beat error,
amplitude) has automated tests using a **synthetic escapement signal generator**
(backlog M1-1) where the true values are known by construction.

## 2. Test data & fixtures

- **Synthetic signals:** generated with configurable bph, rate offset, beat
  error, amplitude (→ Δt via the amplitude formula), waveform of the
  tick/tock transients, and additive noise at controlled SNR.
- **Reference recordings:** a small set of real clips (clean and noisy) stored in
  `tests/fixtures/` (allow-listed in `.gitignore`), each with documented expected
  ranges. No personal data; small files only.
- **Edge fixtures:** silence, pure noise, clipped/over-driven input, 16 kHz
  (Bluetooth-like) downsampled clips, wrong-bph inputs.

## 3. Acceptance thresholds (tie to NFRs)

| Metric | Clean-signal tolerance | Reference NFR |
|---|---|---|
| Rate | ±1 s/day | NFR-Accuracy-1 |
| Beat error | ±0.1 ms | NFR-Accuracy-1 |
| Amplitude | ±3° | NFR-Accuracy-1 |
| 60 s clip analysis time | < 2 s | NFR-Perf-2 |
| Live mode | no dropped frames @44.1 kHz | NFR-Perf-1 |

Define a **minimum SNR** at which thresholds still hold; below it, confidence
score must drop and the UI must indicate low reliability (not silently mislead).

## 4. Test cases by requirement

### Audio input (FR-A*)
- **TC-A1** Enumerate devices returns ≥1 entry on a machine with a mic. *(FR-A1)*
- **TC-A2** Selected device's sample rate/channels/level surfaced correctly. *(FR-A2)*
- **TC-A3** A ≤16 kHz device triggers the degraded-input warning. *(FR-A3)*
- **TC-A4** bph presets selectable; custom value accepted/validated. *(FR-A4)*
- **TC-A5** VU meter responds to input; "listening"/beat-detected indicator works. *(FR-A5)*

### Measurement (FR-M*) — automated against synthetic ground truth
- **TC-M1** Beats detected count matches expected for a clean signal. *(FR-M1)*
- **TC-M2** Rate within ±1 s/day across a sweep of true rates (−60…+60 s/d). *(FR-M2)*
- **TC-M3** Beat error within ±0.1 ms across a sweep (0…3 ms). *(FR-M3)*
- **TC-M4** Amplitude within ±3° across a sweep (180…320°) at lift 52°, and for
  alternate lift angles (51/52/53°). *(FR-M4)*
- **TC-M5** With added noise at the defined SNR, thresholds still met; below it,
  confidence drops and rejected-beat count rises. *(FR-M5)*
- **TC-M6** Record-then-analyse returns a complete result for a fixed clip. *(FR-M6)*
- **TC-M7** Wrong/auto bph: auto-detect recovers correct bph for common rates. *(FR-M9)*
- **TC-M8** Degenerate inputs (silence, pure noise, clipping) yield low/zero
  confidence and never crash. *(FR-M5)*

### Live trace (FR-M7/8)
- **TC-M9** Live trace renders beats; slope reflects injected rate; line gap
  reflects beat error (validated by feeding synthetic stream). *(FR-M7)*
- **TC-M10** Sustained live run shows no audio-frame drops over 5 min. *(NFR-Perf-1)*

### Collection & persistence (FR-C*, FR-S*)
- **TC-C1** Watch CRUD round-trips all metadata incl. lift angle/bph. *(FR-C1)*
- **TC-C2** Saving a test persists all fields and links to its watch. *(FR-C2/3)*
- **TC-C3** Listing/viewing tests returns correct ordered history. *(FR-C5)*
- **TC-C4** Editing/deleting tests & watches behaves (cascade delete). *(FR-C6)*
- **TC-C5** Migrations apply cleanly from empty and from prior schema versions.
- **TC-C6** Changing data dir creates self-describing folder; app reopens it. *(FR-S1/2/3)*
- **TC-C7** Crash/quit mid-record leaves the DB uncorrupted. *(NFR-Reliability-1)*

### Visualisation & export (FR-V*, FR-E*)
- **TC-E1** Metric-over-time chart matches stored values. *(FR-V1)*
- **TC-E2** Position filter / temperature overlay correct. *(FR-V2)*
- **TC-E3** PNG export produces a valid image of the current graph. *(FR-E1)*
- **TC-E4** PDF report contains watch identity, conditions, metrics, graph. *(FR-E2)*
- **TC-E5** CSV/JSON export schema valid; JSON re-import round-trips. *(FR-E3/4)*
- **TC-E6** Import rejects malformed/oversized/hostile JSON safely. *(security)*

## 5. Non-functional tests

- **TC-NFR-Privacy** With a network monitor / sandbox, the app makes **no**
  outbound connections during launch, measure, save, and export. *(NFR-Privacy-1)*
- **TC-NFR-Sec-1** Tauri capability allow-list contains only required entries;
  CSP forbids remote/`connect-src`. *(NFR-Sec-1)*
- **TC-NFR-Sec-2** All SQL is parameterised (lint/grep + review). *(NFR-Sec-1)*
- **TC-NFR-Sec-3** `cargo audit` and `pnpm audit` are clean (or triaged) in CI.
- **TC-NFR-Sec-4** fs access cannot escape the data dir / picked targets. *(NFR-Sec-1)*
- **TC-NFR-Perf** Meets §3 timing budgets on a reference laptop.
- **TC-NFR-A11y** Keyboard navigation and contrast checks pass; status not
  conveyed by colour alone. *(NFR-Accessibility-1)*

## 6. Manual / field test procedures

1. **Quiet-room baseline:** measure a known-good watch with a wired contact mic;
   record rate/amplitude/beat error in 6 positions; compare against a hardware
   timegrapher if available; log deltas.
2. **AirPods test:** repeat with AirPods as input; confirm the degraded-input
   warning appears and document the accuracy difference (expected: worse).
3. **Noisy-room test:** introduce controlled background noise; verify confidence
   drops and the app does not report falsely precise numbers.
4. **Cloud-folder test:** set data dir inside iCloud/Dropbox; verify data syncs
   and reopens on a second machine (not run simultaneously); confirm warning.

> Record manual results in an appendix table here as runs are performed (date,
> watch, mic, room, position, metrics, reference, notes).

## 7. Exit criteria per milestone

- **M1:** all TC-M automated tests green within tolerances at the defined SNR;
  degenerate inputs safe.
- **M2:** all TC-C green; no DB corruption case reproduces.
- **M3:** TC-M9/M10 green.
- **M4:** all TC-E green incl. hostile-import rejection.
- **Release:** all non-functional tests green; manual field procedures executed
  and documented.

## 8. Tooling & CI

- Rust: `cargo test`, `cargo clippy`, `cargo audit`.
- TS: `vitest`, `eslint`, `pnpm audit`.
- E2E: `tauri-driver` + WebDriver (where feasible) or scripted manual checks.
- CI runs unit/fixture/integration tests and audits on every PR.

## 9. Appendix — manual run log
*(to be filled during testing)*

| Date | Watch | Mic | Room | Position | Rate | Beat err | Amp | Ref/notes |
|---|---|---|---|---|---|---|---|---|
| | | | | | | | | |
