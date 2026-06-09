# TimegrapherQ

**An open-source software timegrapher for mechanical watches.**

TimegrapherQ turns your computer plus any microphone into a watch-timing
instrument. It "listens" to the ticking of a mechanical watch's escapement and
measures how accurately it runs — the same job a hardware timegrapher does — and
it lets you keep a **collection** of watches with a **history of tests** so you
can track each watch over time, temperature, and other conditions.

> ⚠️ **Disclaimer:** TimegrapherQ is a hobbyist measurement aid, not a
> laboratory instrument. Accuracy depends heavily on your microphone and
> environment. It is intended to help you *understand* a watch's behaviour and
> to start an informed conversation with a professional watchmaker — **not** to
> replace professional regulation or calibration.

---

## What is a timegrapher?

A timegrapher measures the running quality of a mechanical watch by analysing the
sound its escapement makes on every beat of the balance wheel. From the timing of
those tick/tock sounds it derives:

| Metric | Unit | What it tells you |
|---|---|---|
| **Rate** | seconds/day (s/d) | How fast (+) or slow (−) the watch runs. The headline number. |
| **Beat error** | milliseconds (ms) | Asymmetry between the "tick" and the "tock" half-swings. Ideal ≈ 0.0 ms. |
| **Amplitude** | degrees (°) | How far the balance wheel swings. Healthy ≈ 270–310° at full wind. Requires the movement's **lift angle**. |
| **Beat frequency** | beats/hour (bph) | The watch's design frequency, e.g. 28800, 21600, 18000, 36000. |

Measurements are usually taken in several **positions** (dial up/down, crown
up/down/left/right) because rate and amplitude change with orientation.

See [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) for the full domain background.

---

## Key features (planned)

- 🎙️ **Any microphone** — works with any input device the OS exposes, including
  USB contact mics and Bluetooth (e.g. AirPods). The app warns you when the
  selected device's sample rate is too low to measure reliably.
- 📈 **Live trace + recorded analysis** — a real-time scrolling display like a
  classic timegrapher, plus a record-then-analyse mode for robust, repeatable,
  noise-filtered results.
- 🧹 **Environmental-noise filtering** — DSP front-end (band-pass + envelope /
  onset detection + outlier rejection) to separate escapement transients from
  room noise.
- 🗂️ **Watch collection** — add multiple watches, each with metadata (brand,
  model, calibre, lift angle, bph).
- 🕓 **Test history & comparison** — many tests per watch, each tagged with
  position, temperature, power-reserve state, and notes; compare trends over time.
- 🖼️ **Export** — export a results graph (PNG/PDF) and structured data
  (JSON/CSV) to share with your watchmaker or back up.
- 🔒 **Local-first & private** — all data stays on your machine. No accounts, no
  telemetry, no outbound network calls. You choose where the database lives so
  you can place it inside an iCloud/Dropbox folder for your own sync/backup.

---

## Status

🚧 **Early development — requirements & architecture phase.**

The current deliverables are the planning documents below. Implementation begins
with **Milestone M1: core measurement accuracy** (see the backlog).

## Project documents

| Document | Purpose |
|---|---|
| [`docs/REQUIREMENTS.md`](docs/REQUIREMENTS.md) | Domain background, functional & non-functional requirements. |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | Solution architecture, DSP pipeline, data model, security design. *Kept up to date as we build.* |
| [`docs/BACKLOG.md`](docs/BACKLOG.md) | Prioritised epics & user stories with acceptance criteria. |
| [`docs/TEST_PLAN.md`](docs/TEST_PLAN.md) | Test strategy and cases. *Kept up to date as we build.* |
| [`SECURITY.md`](SECURITY.md) | Threat model, hardening measures, and how to report issues. |

## Tech stack

- **App shell:** [Tauri 2](https://tauri.app) — small, secure, cross-platform
  (macOS / Windows / Linux).
- **Backend:** Rust — audio capture ([`cpal`](https://crates.io/crates/cpal)),
  DSP, storage ([SQLite via `rusqlite`](https://crates.io/crates/rusqlite)).
- **Frontend:** TypeScript + Vite; Canvas for the live trace,
  [uPlot](https://github.com/leeoniya/uPlot) for history/comparison charts.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the rationale.

## Building (placeholder)

> The application is not yet scaffolded. Once M1 begins this section will list
> the exact prerequisites and commands. The expected toolchain is:
>
> - Rust (stable) + the Tauri 2 prerequisites for your OS
> - Node.js (LTS) + a package manager (pnpm)
>
> ```sh
> pnpm install        # install frontend deps
> pnpm tauri dev      # run the app in development
> pnpm tauri build    # produce a release bundle
> ```

## Contributing

This is a public repository. Issues and pull requests are welcome. Please read
[`SECURITY.md`](SECURITY.md) before reporting anything security-related, and do
**not** commit recordings, databases, or any personal data.

## License

[MIT](LICENSE) © Ettore Ferranti. (Subject to change before first release —
open an issue if you have a preference.)
