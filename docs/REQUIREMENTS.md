# TimegrapherQ — Requirements

> Living document. Status: **Draft v0.1** (requirements engineering phase).
> Last updated: 2026-06-09.

## 1. Purpose & vision

TimegrapherQ is a cross-platform, local-first desktop application that measures
the running accuracy of mechanical watches using a computer microphone, and
maintains a collection of watches with a history of tests so the owner can track
each watch's behaviour over time and conditions, and export results to share
with a watchmaker.

## 2. Stakeholders

| Stakeholder | Interest |
|---|---|
| **Primary user** (watch enthusiast / owner) | Measure watch accuracy at home; manage a collection; track and compare results; export for a watchmaker. |
| **Watchmaker** (recipient of exports) | Receives clear, trustworthy graphs/data to inform regulation. |
| **Open-source contributors** | Clear requirements, architecture, and tests to contribute against. |

## 3. Domain background (timegrapher primer)

A mechanical watch's balance wheel oscillates; the escapement locks/unlocks once
per beat, producing a short burst of sound with **2–3 distinct transients**
(unlocking, impulse, locking). By detecting these transients in an audio stream
and timing them, we derive:

- **Beat frequency (bph)** — beats per hour; design value of the movement
  (18000, 21600, 28800, 36000…). One beat = one half-oscillation. Full
  oscillation period `T = 7200 / bph` seconds.
- **Rate (s/day)** — long-term drift of beat timing versus the ideal grid,
  scaled to seconds per 24 h. Positive = fast.
- **Beat error (ms)** — difference between successive half-period intervals
  (tick vs tock); ideal ≈ 0.
- **Amplitude (°)** — peak swing angle of the balance, derived from the spacing
  of the within-beat transients and the movement's **lift angle** (typically
  52°, user-supplied).
- **Positions** — readings differ by orientation (dial up/down, crown
  up/down/left/right); a full service check uses up to 6 positions.

### Amplitude relationship (reference)

Modelling the balance as simple harmonic motion `θ(t) = A·sin(ωt)` with
`ω = 2π/T`, the two impulse transients within a beat occur as the balance crosses
the rest position, separated by the time `Δt` it takes to traverse the lift
angle `L` (from `−L/2` to `+L/2`):

```
A = L / ( 2 · sin( π · Δt / T ) )        where T = 7200 / bph
```

This is the standard DIY-timegrapher amplitude formula and will be validated
against synthetic signals in the test plan.

## 4. Scope

### In scope
- Single-microphone acoustic measurement of rate, beat error, amplitude, bph.
- Live scrolling trace **and** record-then-analyse modes.
- Environmental-noise filtering.
- Watch collection management with rich per-test metadata.
- History, trend visualisation, and comparison across tests.
- Export of graphs (PNG/PDF) and data (JSON/CSV).
- Local-only storage with a user-configurable location.

### Out of scope (for now)
- Cloud accounts / server-side sync (user may place the data folder in their own
  synced directory instead).
- Mobile apps.
- Automatic detection of *which* physical position the watch is in (user selects
  it manually).
- Hardware sensor integration for temperature (entered manually).
- Regulating/adjusting the watch (measurement only).

## 5. Decisions on record

| # | Decision | Rationale |
|---|---|---|
| D1 | **Tauri** desktop app (Rust + web UI) | Cross-platform, small, secure-by-default, native audio device control. |
| D2 | Support **both** live trace and record-then-analyse | Live for authenticity/feedback; recorded for robust, repeatable, noise-filtered results. |
| D3 | **Local-only** storage, **user-configurable path** | Privacy and simplicity; placing the folder in iCloud/Dropbox gives optional sync/backup without us running a backend. |
| D4 | **Core measurement accuracy first** (Milestone M1) | The hardest, highest-risk part; everything else depends on trustworthy numbers. |
| D5 | Data files **clearly named & documented**, with export/import | Transparency; user owns and can move/back up their data. |

## 6. Functional requirements

IDs are stable references for the backlog and test plan. Priority: **M** must,
**S** should, **C** could.

### 6.1 Audio input & devices
- **FR-A1 (M):** List all input devices exposed by the OS and let the user select one.
- **FR-A2 (M):** Display the active device's sample rate, channel count, and input level (VU meter).
- **FR-A3 (M):** Warn the user when the selected device's effective sample rate is too low for reliable measurement (e.g. Bluetooth hands-free ~16 kHz), and explain the impact.
- **FR-A4 (S):** Let the user choose the target bph (18000/21600/25200/28800/36000/custom) or auto-detect it from the signal.
- **FR-A5 (S):** Provide an input-gain / sensitivity control and a "listening" indicator confirming beats are being detected.
- **FR-A6 (C):** Remember the last-used device and settings.

### 6.2 Signal processing & measurement
- **FR-M1 (M):** Detect escapement beats from the audio stream via band-pass filtering + envelope/onset detection.
- **FR-M2 (M):** Compute **rate** in s/day.
- **FR-M3 (M):** Compute **beat error** in ms.
- **FR-M4 (M):** Compute **amplitude** in degrees using a user-supplied **lift angle** (default 52°).
- **FR-M5 (M):** Reject outliers / spurious detections caused by environmental noise and report a signal-quality / confidence indicator.
- **FR-M6 (M):** Record a fixed-duration clip (configurable, e.g. 15–60 s) and analyse it, producing a single result with the metrics above.
- **FR-M7 (S):** Provide a **live** scrolling "two-line" trace updating in real time (slope = rate, line gap = beat error, scatter = noise).
- **FR-M8 (S):** Show amplitude and beat error live alongside the trace.
- **FR-M9 (C):** Auto-detect bph when not specified.

### 6.3 Collection & test management
- **FR-C1 (M):** Create, edit, and delete **watches**, with metadata: name, brand, model, calibre/movement, reference, serial (optional), **lift angle**, **bph**, notes, optional photo.
- **FR-C2 (M):** Save a measurement as a **test** linked to a watch.
- **FR-C3 (M):** Each test stores: timestamp, all metrics, **position**, **temperature** (manual), power-reserve / wind state, microphone/device used, clip duration, signal-quality indicator, and free-text notes.
- **FR-C4 (S):** Optionally retain the recorded audio clip with the test (user-controlled, for storage reasons).
- **FR-C5 (M):** List a watch's tests and view any test's full detail.
- **FR-C6 (S):** Delete or edit metadata of past tests.

### 6.4 Visualisation & comparison
- **FR-V1 (M):** Plot a watch's metric (e.g. rate, amplitude, beat error) over time across its tests.
- **FR-V2 (S):** Filter/group trends by position, and overlay temperature.
- **FR-V3 (S):** Compare multiple tests (e.g. all six positions of one session) in one view.
- **FR-V4 (C):** Compute and show derived figures (e.g. max positional rate delta — "delta").

### 6.5 Export
- **FR-E1 (M):** Export a results graph as **PNG**.
- **FR-E2 (S):** Export as **PDF** with watch identity, conditions, and metrics — a "watchmaker report".
- **FR-E3 (S):** Export test data as **CSV** and the full collection as **JSON**.
- **FR-E4 (C):** Re-import a previously exported JSON (backup/restore / move between machines).

### 6.6 Settings & data location
- **FR-S1 (M):** Let the user choose the **data directory** (DB + optional recordings); default to a clearly-named folder in the OS app-data location.
- **FR-S2 (M):** Show the current data location and let the user open it in the file manager.
- **FR-S3 (S):** Use clearly named, documented file formats (SQLite DB + sidecar audio files) so data is portable and recognisable.
- **FR-S4 (C):** Migrate/relocate existing data when the path changes.

## 7. Non-functional requirements

- **NFR-Perf-1:** Live trace must sustain real-time analysis at ≥44.1 kHz mono without dropping audio frames on a typical laptop.
- **NFR-Perf-2:** Recorded-clip analysis of a 60 s clip completes in < 2 s.
- **NFR-Accuracy-1:** On clean synthetic signals, rate within ±1 s/day, beat error within ±0.1 ms, amplitude within ±3° of ground truth (validated in the test plan).
- **NFR-Compat-1:** Runs on macOS (primary), Windows, and Linux from a single codebase.
- **NFR-Privacy-1:** No outbound network connections in normal operation; no telemetry; no accounts. (Verified in tests.)
- **NFR-Sec-1:** Hardened per `SECURITY.md` (Tauri capability allow-list, strict CSP, scoped filesystem access, validated imports, parameterised SQL).
- **NFR-Usability-1:** A first-time user can take and save a valid measurement within 5 minutes without reading documentation.
- **NFR-Reliability-1:** App must not corrupt the database on crash/quit during a recording (atomic writes / transactions).
- **NFR-Maintainability-1:** DSP core is testable in isolation against fixture signals; CI runs unit tests and dependency audits.
- **NFR-Accessibility-1:** Readable contrast, keyboard navigation, and no reliance on colour alone for status.

## 8. Constraints

- **Bluetooth microphones (AirPods, headsets)** typically switch to a low-quality
  ~16 kHz mono hands-free profile when used as an input and apply noise
  suppression/AGC, which can degrade tick transients. The app must detect and
  warn; a wired contact/piezo mic is recommended for best results.
- macOS/Windows require explicit **microphone permission**; the app must handle
  denial gracefully.
- No assumption of internet connectivity.

## 9. Assumptions

- The user can physically couple the microphone to the watch (contact mic, or
  placing the watch against the mic in a quiet space).
- The user knows or can look up their movement's **lift angle** and **bph** (we
  provide sensible defaults and common presets).
- One watch is measured at a time.

## 10. Risks (summary; tracked with mitigations in ARCHITECTURE & BACKLOG)

| Risk | Impact | Mitigation |
|---|---|---|
| Low-quality Bluetooth mic input | Wrong/unreliable metrics | Detect sample rate, warn, recommend wired/contact mic. |
| Environmental noise false beats | Inaccurate rate/amplitude | Band-pass + envelope detection + outlier rejection + confidence score. |
| Amplitude depends on correct lift angle | Misleading amplitude | Require lift angle, default 52°, show it on every result/export. |
| DSP correctness | Loss of trust | Validate against synthetic ground-truth signals in CI. |
| User data loss | High frustration | Documented formats, export/JSON backup, atomic DB writes. |

## 11. Open questions

- Preferred default clip length for record mode? (assumed 30 s)
- Should the live trace use the classic 2-line "paper tape" look specifically, or
  a modern equivalent? (assumed classic look, configurable)
- Any preference on the PDF "watchmaker report" layout/branding?
- License confirmation (assumed MIT).
