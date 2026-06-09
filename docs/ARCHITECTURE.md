# TimegrapherQ — Architecture

> Living document. **Keep this up to date as the implementation evolves.**
> Status: **Draft v0.1**. Last updated: 2026-06-09.

## 1. Overview

TimegrapherQ is a single-user, local-first desktop application built on **Tauri 2**.
A **Rust backend** owns audio capture, digital signal processing (DSP), and
persistence; a **web frontend** (TypeScript + Vite) renders the UI, the live
trace, and the history/comparison charts. The two communicate over Tauri's
command/event IPC. No server, no network dependency.

```
            ┌──────────────────────────────────────────────────────┐
            │                    Tauri application                   │
            │                                                        │
  Mic ──►   │  ┌──────────────┐   audio   ┌────────────────────┐    │
            │  │ Audio Capture │ ────────► │   DSP / Analysis    │   │
            │  │  (cpal)       │  frames   │  (Rust core)        │   │
            │  └──────────────┘           └─────────┬──────────┘    │
            │                                       │ results        │
            │  ┌──────────────┐   SQL      ┌────────▼──────────┐    │
            │  │  Storage      │ ◄────────► │   App services /   │   │
            │  │ (rusqlite)    │            │   Tauri commands   │   │
            │  └──────────────┘            └────────┬──────────┘    │
            │        ▲ files                        │ IPC (events    │
            │        │                              │  + commands)   │
            │  ┌─────┴───────┐             ┌────────▼──────────┐     │
            │  │ Data dir     │             │   Web frontend     │    │
            │  │ (user-chosen)│             │ (TS, Canvas, uPlot)│    │
            │  └─────────────┘             └───────────────────┘     │
            └──────────────────────────────────────────────────────┘
```

## 2. Technology choices & rationale

| Layer | Choice | Why | Alternatives considered |
|---|---|---|---|
| Shell | Tauri 2 | Small binaries, strong security model, native APIs, cross-platform | Electron (heavier, larger attack surface), pure web (limited device control) |
| Backend lang | Rust | Performance for real-time DSP, memory safety, great audio crates | Python (easier DSP but harder packaging) |
| Audio I/O | `cpal` | Cross-platform device enumeration + capture | OS-specific APIs |
| DSP | Rust (`rustfft`, `biquad`/hand-rolled IIR, custom onset detection) | In-process, fast, testable | offload to WASM |
| Storage | SQLite via `rusqlite` | Single-file, robust, portable, transactional | flat JSON (no querying), embedded KV |
| Frontend | TypeScript + Vite | Fast dev, typed, good charting ecosystem | framework-heavy SPAs |
| Live trace | HTML Canvas (hand-drawn) | Full control, high frame rate | charting libs too slow for per-beat dots |
| History charts | uPlot | Extremely fast, tiny, time-series friendly | Chart.js (heavier), D3 (more code) |

## 3. Components

### 3.1 Audio Capture (Rust, `cpal`)
- Enumerates input devices; exposes name, supported sample rates, channels.
- Opens a stream at the highest reasonable mono sample rate (target 44.1/48 kHz).
- Pushes fixed-size frames into a ring buffer consumed by the DSP stage.
- Emits an input-level (RMS/peak) event for the VU meter.
- Flags "degraded" devices (effective rate ≤ ~16 kHz, e.g. Bluetooth HFP).

### 3.2 DSP / Analysis core (Rust) — *the heart of M1*
Pipeline (record-then-analyse and live share this core):

1. **Pre-filter** — DC removal + band-pass IIR to emphasise escapement
   transient energy and suppress low-frequency room noise and hum.
2. **Envelope / onset detection** — compute an energy envelope; detect onset
   peaks (candidate transients) with adaptive thresholding.
3. **Beat segmentation** — group transients into beats; estimate the beat period
   from the chosen bph (or autocorrelation when auto-detecting).
4. **Phase tracking → rate** — track the deviation of each beat's onset from the
   ideal grid; linear-regress accumulated phase over the capture window; scale to
   **s/day**.
5. **Beat error** — compare consecutive half-period intervals (tick vs tock);
   report the difference in **ms**.
6. **Amplitude** — measure within-beat transient spacing `Δt`; apply
   `A = L / (2·sin(π·Δt/T))`, `T = 7200/bph`, `L` = lift angle.
7. **Quality / outlier rejection** — robust statistics (e.g. median/MAD),
   discard beats failing consistency checks, output a **confidence score** and
   the count of accepted vs rejected beats.

The core is a **pure library crate** (no Tauri, no I/O) so it can be unit-tested
against synthetic and recorded fixtures (see TEST_PLAN).

### 3.3 App services / Tauri commands (Rust)
- Exposes typed commands: list devices, start/stop live, record-and-analyse,
  CRUD watches/tests, export, get/set settings, choose data dir.
- Streams live results and VU levels to the UI via Tauri **events**.
- Owns transactions and enforces validation before persistence.

### 3.4 Storage (Rust, SQLite + files)
- One SQLite database file in the user-chosen data directory.
- Optional recorded audio stored as sidecar **WAV/FLAC** files referenced by path.
- Schema migrations versioned and applied on startup.

### 3.5 Frontend (TypeScript)
- Views: **Measure** (device picker, VU, live trace, record button, live
  metrics), **Collection** (watch list/detail), **Test detail**, **History &
  compare**, **Settings**.
- Live trace drawn on Canvas from streamed beat events.
- History/compare charts via uPlot.
- Export rendering (PNG via canvas/SVG; PDF via a print-to-PDF or a small PDF lib).

## 4. Data model (initial)

```sql
-- Schema version tracked in a `meta` table.

CREATE TABLE watch (
  id            TEXT PRIMARY KEY,          -- UUID
  name          TEXT NOT NULL,
  brand         TEXT,
  model         TEXT,
  calibre       TEXT,
  reference     TEXT,
  serial        TEXT,
  lift_angle    REAL NOT NULL DEFAULT 52.0,
  bph           INTEGER NOT NULL DEFAULT 28800,
  photo_path    TEXT,
  notes         TEXT,
  created_at    TEXT NOT NULL,             -- ISO-8601 UTC
  updated_at    TEXT NOT NULL
);

CREATE TABLE test (
  id              TEXT PRIMARY KEY,        -- UUID
  watch_id        TEXT NOT NULL REFERENCES watch(id) ON DELETE CASCADE,
  measured_at     TEXT NOT NULL,           -- ISO-8601 UTC
  position        TEXT,                    -- e.g. 'DU','DD','CU','CD','CL','CR'
  rate_s_per_day  REAL,
  beat_error_ms   REAL,
  amplitude_deg   REAL,
  bph_used        INTEGER NOT NULL,
  lift_angle_used REAL NOT NULL,
  temperature_c   REAL,                    -- manual
  power_state     TEXT,                    -- e.g. 'full wind','24h', notes
  device_name     TEXT,
  sample_rate_hz  INTEGER,
  clip_seconds    REAL,
  quality_score   REAL,                    -- 0..1 confidence
  beats_accepted  INTEGER,
  beats_rejected  INTEGER,
  audio_path      TEXT,                    -- nullable; sidecar recording
  notes           TEXT,
  created_at      TEXT NOT NULL
);

CREATE INDEX idx_test_watch ON test(watch_id, measured_at);
```

Settings (data dir, default clip length, last device, units) are stored in a
small JSON config in the OS config location — *not* in the movable data dir, so
the app can find the data dir on launch.

## 5. Data location strategy (D3/D5)

- **Default:** a clearly-named folder, e.g.
  `~/Library/Application Support/TimegrapherQ/` (macOS),
  `%APPDATA%\TimegrapherQ\` (Windows), `~/.local/share/TimegrapherQ/` (Linux),
  containing `timegrapherq.sqlite` and a `recordings/` subfolder.
- **User-configurable:** the Settings screen lets the user pick any directory —
  e.g. one inside iCloud Drive or Dropbox — to get sync/backup for free.
- The chosen path is stored in the OS config file (above). The data folder
  includes a small `README.txt` describing the contents so it's self-describing
  if found later.
- **Caution documented:** cloud-synced SQLite can corrupt if the app runs on two
  machines simultaneously against the same synced file; the app writes
  transactionally and we warn users not to run two instances on one folder at once.

## 6. Concurrency & real-time model

- Audio capture runs on `cpal`'s callback thread → lock-free ring buffer.
- A dedicated analysis thread consumes frames; for **live** mode it emits
  incremental results; for **record** mode it buffers N seconds then analyses.
- Tauri commands are async; long work runs off the UI thread. The UI never blocks
  on DSP.

## 7. Security architecture

(See `SECURITY.md` for the full threat model. Summary of design measures:)

- **Tauri capabilities:** minimal allow-list — only the commands/plugins we need
  (audio, dialog for folder/file pick, fs scoped to the data dir, shell *open*
  for "reveal in finder" only). No arbitrary shell, no remote URLs.
- **Strict CSP:** the webview loads only local assets; `connect-src 'none'` (or
  self) so the UI cannot exfiltrate data. No remote scripts/CDNs — dependencies
  are bundled.
- **No network:** the app makes no outbound requests in normal operation;
  absence verified by test (NFR-Privacy-1).
- **Filesystem scope:** fs access limited to the configured data directory plus
  explicit user-picked export targets (via native dialogs).
- **Input validation:** imported JSON and audio files are size-bounded,
  schema-validated, and parsed defensively (reject malformed/oversized input).
- **SQL:** parameterised statements only; no string-built queries.
- **Supply chain:** committed lockfiles; CI runs `cargo audit` and `npm/pnpm
  audit`; Dependabot for updates; minimal dependencies.
- **No secrets:** the app needs none; `.gitignore` blocks env/key files.
- **Distribution (future):** code-signing + notarisation (macOS) and signed
  installers; documented reproducible build.

## 8. Build, CI/CD (planned)

- `pnpm` workspace for frontend; Cargo workspace for Rust (`core` lib + `app`).
- CI: lint (clippy, eslint), unit tests (Rust core + TS), DSP fixture tests,
  dependency audits. Release builds per-OS via Tauri action.

## 9. Architecture Decision Records (ADRs)

- **ADR-001 Tauri over Electron** — smaller footprint, stronger default security,
  native audio. *Accepted.*
- **ADR-002 DSP in Rust as a pure library** — testability and performance; UI/IO
  kept out of the core. *Accepted.*
- **ADR-003 SQLite for storage** — queryable, transactional, single portable
  file; fits the movable-data-dir requirement. *Accepted.*
- **ADR-004 Local-only, user-chosen data dir instead of built-in cloud sync** —
  avoids backend, auth, and privacy burden; user leverages existing cloud
  folders. *Accepted.*
- **ADR-005 Canvas for live trace, uPlot for history** — performance for
  per-beat rendering; lightweight time-series charts. *Accepted.*

> Add new ADRs here as decisions are made; never silently change an accepted one.

## 10. Repository layout (as implemented — M0)

```
timegrapherq/
├── Cargo.toml              # Rust workspace (core + src-tauri)
├── package.json            # frontend + Tauri scripts (pnpm)
├── pnpm-workspace.yaml      # pnpm build-script allow-list (supply-chain safety)
├── index.html / vite.config.ts / tsconfig.json
├── eslint.config.js / .prettierrc.json / .prettierignore / vitest.config.ts
├── src/                    # TypeScript frontend
│   ├── main.ts             #   calls the `health` Tauri command
│   ├── format.ts           #   pure helpers (unit-tested)
│   └── format.test.ts
├── core/                   # timegrapherq-core: pure DSP/measurement lib
│   └── src/
│       ├── lib.rs          #   amplitude formula + period helpers
│       ├── synth.rs        #   synthetic escapement generator (ground truth)
│       ├── dsp.rs          #   band-pass biquad, envelope, onset detection
│       └── measure.rs      #   analyze(): onsets → rate / beat error / amplitude
├── src-tauri/              # timegrapherq: Tauri app shell
│   ├── Cargo.toml          #   depends on timegrapherq-core
│   ├── tauri.conf.json     #   hardened CSP, withGlobalTauri off
│   ├── capabilities/default.json
│   └── src/{main.rs,lib.rs}#   `health` command (core integration point)
├── .github/workflows/ci.yml + .github/dependabot.yml
└── docs/                   # REQUIREMENTS / ARCHITECTURE / BACKLOG / TEST_PLAN
```

**Toolchain:** Rust (stable) + Tauri 2; Node + pnpm for the frontend. Verified on
macOS: full workspace compiles, `cargo clippy -D warnings` clean, `cargo test`
(core 3/3) green, frontend `lint`/`typecheck`/`test`/`build` green. The GUI
(`pnpm tauri dev`) has not been launched in the build sandbox; runtime IPC will
be exercised when the Measure UI lands in M1.
