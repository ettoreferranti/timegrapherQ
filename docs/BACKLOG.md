# TimegrapherQ — Backlog

> Living document. Status: **Draft v0.1**. Last updated: 2026-06-09.
> Priority follows decision **D4: core measurement accuracy first**.
> Story IDs are stable; check items off as completed.

## Legend
- Status: `TODO` / `WIP` / `DONE` / `BLOCKED`
- Each story lists **acceptance criteria** and the requirement IDs it satisfies.

---

## Milestone M0 — Project setup ✅ (done 2026-06-09)
*Goal: a runnable, hardened skeleton.*

- [x] **M0-1** Scaffold Tauri 2 app (Rust `src-tauri` app + `core` lib crate, TS/Vite frontend).
  - Cargo workspace (`core` + `src-tauri`); `health` command wires the core to the UI. Full workspace compiles; frontend builds; `core` unit tests pass. (GUI `tauri dev` launch not run in the build sandbox; verified by full compile instead.)
- [x] **M0-2** Cargo + pnpm workspaces; linting (clippy + eslint) and formatting (rustfmt + prettier) wired with scripts.
- [x] **M0-3** Baseline Tauri security config: strict CSP, `withGlobalTauri` off, minimal capability allow-list, no network. *(NFR-Sec-1)*
- [x] **M0-4** CI pipeline (`.github/workflows/ci.yml`): format/lint/typecheck/test/build + advisory `cargo audit` / `pnpm audit`.
- [x] **M0-5** LICENSE (MIT), SECURITY.md, and Dependabot (`cargo`, `npm`, `github-actions`) in place.

> First `core` unit tests already validate the amplitude formula via round-trip
> against its inverse — the foundation for the M1 ground-truth strategy.

## Milestone M1 — Core measurement accuracy *(highest priority)*
*Goal: trustworthy rate / beat error / amplitude from a recorded clip, with noise filtering. Proven against ground truth before any UI polish.*

- [x] **M1-1** Synthetic signal generator (`core::synth`): escapement-like audio with known bph, rate, beat error, amplitude, and seeded noise; returns ground-truth beat onsets + impulse spacing. 7 unit tests covering sample count, beat count, rate scaling, beat-error alternation, amplitude spacing, transient energy, and deterministic noise. *(supports TEST_PLAN; done 2026-06-09)*
- [ ] **M1-2** Audio capture via `cpal`: enumerate devices, open mono stream, ring buffer. *(FR-A1)*
- [ ] **M1-3** DSP pre-filter (DC removal + band-pass) and energy envelope. *(FR-M1)*
- [ ] **M1-4** Onset/transient detection with adaptive threshold. *(FR-M1)*
- [ ] **M1-5** Beat segmentation + period estimate from supplied bph. *(FR-M1, FR-A4)*
- [ ] **M1-6** Rate computation (phase regression → s/day). *(FR-M2)*
  - AC: within ±1 s/day on clean synthetic signal.
- [ ] **M1-7** Beat-error computation (ms). *(FR-M3)*
  - AC: within ±0.1 ms on clean synthetic signal.
- [ ] **M1-8** Amplitude computation from Δt, lift angle, bph. *(FR-M4)*
  - AC: within ±3° on clean synthetic signal.
- [ ] **M1-9** Outlier rejection + quality/confidence score; report accepted/rejected beats. *(FR-M5)*
  - AC: maintains accuracy targets at a defined SNR; degrades gracefully and lowers confidence below it.
- [ ] **M1-10** Record-then-analyse command: capture N seconds, return a result object. *(FR-M6)*
- [ ] **M1-11** Low-sample-rate / degraded-device warning. *(FR-A3)*
- [ ] **M1-12** Minimal Measure UI: device picker, VU meter, bph + lift-angle input, record button, result display. *(FR-A1, FR-A2, FR-A5)*

## Milestone M2 — Collection & persistence
*Goal: save and revisit measurements per watch.*

- [ ] **M2-1** SQLite storage + migrations; data-dir bootstrap with self-describing README.txt. *(FR-S1, FR-S3)*
- [ ] **M2-2** Watch CRUD + metadata (incl. lift angle, bph, photo). *(FR-C1)*
- [ ] **M2-3** Save measurement as a test linked to a watch, with full metadata (position, temperature, power state, device, quality). *(FR-C2, FR-C3)*
- [ ] **M2-4** List & view tests per watch. *(FR-C5)*
- [ ] **M2-5** Edit/delete tests and watches. *(FR-C6)*
- [ ] **M2-6** Settings: choose/show/open data directory; default clip length. *(FR-S1, FR-S2)*
- [ ] **M2-7** Optionally store the recorded clip with a test. *(FR-C4)*

## Milestone M3 — Live trace
*Goal: real-time classic timegrapher experience.*

- [ ] **M3-1** Stream incremental beat events from the analysis thread via Tauri events. *(FR-M7)*
- [ ] **M3-2** Canvas scrolling two-line trace (slope=rate, gap=beat error, scatter=noise). *(FR-M7)*
- [ ] **M3-3** Live numeric rate / beat error / amplitude readouts. *(FR-M8)*
- [ ] **M3-4** bph auto-detect. *(FR-M9, FR-A4)*

## Milestone M4 — Visualisation, comparison & export
*Goal: insight over time and shareable output.*

- [ ] **M4-1** Per-watch metric-over-time chart (uPlot). *(FR-V1)*
- [ ] **M4-2** Filter/group by position; temperature overlay. *(FR-V2)*
- [ ] **M4-3** Multi-test comparison view (e.g. 6 positions). *(FR-V3)*
- [ ] **M4-4** Export graph as PNG. *(FR-E1)*
- [ ] **M4-5** PDF "watchmaker report" (identity + conditions + metrics + graph). *(FR-E2)*
- [ ] **M4-6** CSV (tests) and JSON (collection) export. *(FR-E3)*
- [ ] **M4-7** JSON import / restore with validation. *(FR-E4, security)*
- [ ] **M4-8** Positional delta and derived figures. *(FR-V4)*

## Milestone M5 — Polish & release
- [ ] **M5-1** Microphone-permission handling & onboarding/help. *(constraints)*
- [ ] **M5-2** Accessibility pass (contrast, keyboard nav, non-colour status). *(NFR-Accessibility-1)*
- [ ] **M5-3** Remember last device/settings. *(FR-A6)*
- [ ] **M5-4** Data-dir migration when path changes. *(FR-S4)*
- [ ] **M5-5** Code-signing/notarisation & signed installers; release docs.
- [ ] **M5-6** User guide (how to mic a watch, choosing lift angle/bph, interpreting results).

---

## Cross-cutting / ongoing
- [ ] Keep `ARCHITECTURE.md` and `TEST_PLAN.md` updated with every functional change.
- [ ] Maintain dependency audits green in CI.
- [ ] Track risks from REQUIREMENTS §10 until mitigated.

## Icebox (out of current scope)
- Cloud accounts / hosted sync.
- Mobile companion app.
- Automatic position detection.
- Hardware temperature-sensor integration.
- Multi-language UI.
