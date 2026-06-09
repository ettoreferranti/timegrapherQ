# Security Policy

TimegrapherQ is a **local-first desktop application**. It is designed to keep
your data on your machine and to make no network connections in normal use. This
document describes the threat model, the hardening measures, and how to report
security issues.

## Reporting a vulnerability

Please **do not** open a public issue for security vulnerabilities. Instead,
report privately via GitHub's *Security → Report a vulnerability* (private
advisory) on this repository. Include reproduction steps and impact. We aim to
acknowledge within a reasonable time for a hobbyist project.

When reporting, **never include personal data, recordings, or your watch
collection database**.

## Scope & data handling

- **No accounts, no telemetry, no analytics.** The app does not phone home.
- **No outbound network connections** are required or expected during normal
  operation. This is treated as a verifiable property (see test plan
  `TC-NFR-Privacy`).
- **Your data stays local.** The watch collection (SQLite) and any optional
  audio recordings live in a directory you control. If you place that directory
  in iCloud/Dropbox for backup, your data is then subject to *that* provider's
  security and privacy terms — that is your choice, not something the app does
  for you.

## Threat model (summary)

| Asset | Threats | Mitigations |
|---|---|---|
| User data (collection, recordings) | Accidental loss/corruption; leakage via misconfig | Transactional SQLite writes; documented formats; export/backup; fs scoped to data dir |
| The app binary | Tampering / supply-chain compromise | Dependency audits, lockfiles, minimal deps; (future) code-signing & notarisation |
| Webview UI | XSS / data exfiltration via injected content | Strict CSP, local-only assets, no remote scripts/CDNs, `connect-src` locked down |
| Import surface (JSON/audio) | Malicious/oversized files, parser exploits | Size limits, schema validation, defensive parsing, reject malformed input |
| Local IPC (Tauri commands) | Over-broad capabilities | Minimal capability allow-list; only required commands/plugins enabled |
| Filesystem access | Path traversal / writing outside intended dirs | Access scoped to the data dir and explicit user-picked targets via native dialogs |
| SQL layer | Injection | Parameterised statements only |

## Hardening measures (implemented as the app is built)

- **Tauri capability allow-list:** enable only the audio, dialog (file/folder
  pick), scoped-fs, and "reveal in file manager" capabilities actually used. No
  arbitrary shell execution, no remote URL loading.
- **Content Security Policy:** restrict the webview to bundled local assets;
  forbid remote script/style sources and outbound `connect-src`.
- **Dependency hygiene:** committed `Cargo.lock` and lockfile for the frontend;
  `cargo audit` and `pnpm audit` run in CI; Dependabot enabled; dependencies kept
  minimal and reviewed.
- **Input validation:** all imports (JSON collection, audio files) are bounded in
  size and validated against an expected schema/format before use.
- **Database:** parameterised queries; migrations are versioned and reviewed.
- **No secrets in the repo:** `.gitignore` blocks `.env`, keys, certificates, and
  user data (databases, recordings).
- **Least privilege at runtime:** the app requests only the **microphone**
  permission and filesystem access to the chosen data directory.

## Privacy notes for users

- The microphone is used **only** while you are actively measuring or previewing
  input. Recordings are saved **only** if you opt in per test.
- You can see and open the exact data directory from Settings, and delete or
  export your data at any time.
- Sharing an export (PNG/PDF/CSV/JSON) is entirely under your control; exports may
  contain watch identifiers and notes you entered — review before sharing.

## Out of scope

- The security of third-party cloud-sync folders you choose to store data in.
- Physical access to an unlocked machine.

*This policy evolves with the codebase; see `docs/ARCHITECTURE.md` §7 for the
corresponding design details.*
