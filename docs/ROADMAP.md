# slog Roadmap

Last Updated: 2026-10-10 · Version: 0.1.0

## Overview

`slog` is Console.app for the command line: it prints macOS and iOS unified log entries to
stdout, one entry per line, with Console-style search (`--filter`). It is designed to be
driven by LLMs as well as people, so:

- every command is bounded by default or documents how to bound it (`--duration`, `--limit`)
- `--help` is the complete reference: syntax, defaults, output format, exit codes, examples
- output is compact text by default and stable JSON lines with `--json`
- notes (truncation, no matches) go to stderr, never stdout

See [DATA_FLOW.md](DATA_FLOW.md) for the architecture.

## Sources

| Source | Command | Mechanism | Status |
|---|---|---|---|
| This Mac, history | `slog show` | `/usr/bin/log show --style ndjson` | Done |
| This Mac, live | `slog stream` | `/usr/bin/log stream --style ndjson` | Done |
| `.logarchive` | `slog show --archive` | `/usr/bin/log show <archive>` | Done |
| iPhone/iPad, live (USB, usbmuxd Wi-Fi) | `slog stream --device` | `idevice` crate, `os_trace_relay` | Done |
| iPhone/iPad over Wi-Fi without Finder's setting | `slog pair` once, then `--device` | `idevice` `remote_pairing` + TLS-PSK tunnel + `jktcp` + RSD | Done |
| iPhone/iPad, history | `slog show --device` | `os_trace_relay` `CreateArchive`, then `log show` | Done |
| Crash/spin/diagnostic reports (Mac) | `slog reports list\|show` | Read `~/Library/Logs/DiagnosticReports`, `/Library/Logs/DiagnosticReports` | Done |
| Crash reports (device) | `slog reports list\|show --device` | `crashreportmover` flush + `crashreportcopymobile` | Done |
| Simulators (booted) | `slog show\|stream --simulator` | `xcrun simctl spawn <id> log ...` | Done |

### Why these mechanisms

- **`/usr/bin/log` for local data.** Apple's public `OSLogStore` cannot stream live, so
  Console.app uses private frameworks. `log` is Apple-supported, emits ndjson, and filters
  server-side with NSPredicate. Always call it by absolute path: zsh has a `log` builtin.
- **`idevice` (pure Rust) for devices.** Keeps `cargo install` and Homebrew to a single
  binary with no C or Python dependencies. `os_trace_relay` returns structured entries
  (subsystem, category, level), unlike the plain-text `syslog_relay`. Alternatives that were
  considered: libimobiledevice (C dependency, lags new iOS), pymobiledevice3 (Python
  runtime), Apple private frameworks (undocumented, most fragile).
- **`os_trace_relay` `CreateArchive` for device history** instead of `log collect --device`,
  because it does not require `sudo`.

## Phases

### Phase 1 — v0.1 (current)

- `show`, `stream`, `devices` commands
- `--filter` (Console tokens) compiled to NSPredicate and evaluated in-process for devices
- `--predicate` raw passthrough (Mac/archive only)
- `--json`, `--fields`, `--info`, `--debug`
- Bounded `show` (`--last 5m`, `--limit 1000`, newest kept) with stderr truncation note
- `stream --device` verified on iPhone19,2 / iOS 27.0.1 over USB (up to ~17k entries/s with
  `--debug`)

### Phase 2

- Done: `slog show --device` via `CreateArchive` (~5-10 s, ~300 MB temporary for the
  default 5m window). Supports `--predicate`, unlike `stream --device`.
- Done: `show --device --save-archive PATH` keeps the copy so repeated queries reuse it via
  `--archive` (~1 s) instead of re-copying (~5-10 s) each time.
- Done: `slog reports list|show [--device]`. Crash reports (`.ips`, bug_type 309) print as a
  summary (exception, termination, app-specific info, last exception backtrace, crashed
  thread); `--full` and every other report type print the file unchanged.
- Not included: `Retired/` and other subdirectories; Console's "Log Reports" (text logs in
  `~/Library/Logs`, `/var/log`); Mac Analytics Data.
- Done: `slog skill install --user|--project [--force]` and `slog skill print`. The skill
  (`skill/SKILL.md`, `{{VERSION}}` filled in at runtime) is embedded in the binary so it
  always matches the installed version. Existing files with different contents are only
  replaced with `--force`, to protect local edits.

### Phase 2b

- Done: Wi-Fi devices without Finder's "Show this iPhone when on Wi-Fi", through slog's
  own remote pairing (the protocol Xcode uses for wireless devices). `slog pair` pairs once
  over USB (`remotepairingdeviced`; PIN 000000, no prompt) and stores the pairing in
  `~/Library/Application Support/slog/pairing/<UDID>.plist` (0600). Over Wi-Fi: Bonjour
  `_remotepairing._tcp`, matched by `authTag` (SipHash of the pairing's `alt_irk`),
  pair-verify, a TLS-PSK tunnel carrying IPv6, TCP in process (`jktcp`, no root, no utun),
  RSD at the port the tunnel names. Measured over Wi-Fi (release build): stream starts in
  ~1 s, `reports list` ~4.7 s, `show --device` ~36 s (network-bound ~300 MB copy).
- Rejected: reusing the tunnel macOS keeps for Xcode (`devicectl`). It works, but its RSD
  port is unpublished and `netstat` hides other processes' sockets from non-shell callers;
  scanning for the port crashed device daemons (see BUG_REFERENCE).

### Phase 3

- Done: simulators. `slog simulators` lists booted ones; `show|stream --simulator
  [UDID|NAME]` runs `log` inside the simulator (`simctl spawn`), which keeps its own unified
  log, so `--predicate` and NSPredicate filtering work as on the Mac. Simulator app crashes
  are written to this Mac's DiagnosticReports, so `slog reports` needs no flag for them.
- Distribution:
  - Done: packaged as `slog-cli` (binary `slog`; `slog` is taken on crates.io), README,
    LICENSE, MSRV 1.89, `cargo publish --dry-run` clean. Builds only on macOS
    (`compile_error!` elsewhere).
  - Done: CI (GitHub Actions on macOS: fmt, clippy, tests, smoke runs, MSRV).
  - On hold until more features land: release `v0.1.0` (git tag and GitHub release,
    Homebrew tap `bratsche/homebrew-tap` with a from-source formula, crates.io `slog-cli`
    published by the maintainer). The formula is drafted and passed `brew test`/`audit`.

## Not planned

Console.app features that do not fit a stdout CLI: the Activities tree, pause/resume of a
live view, and the click-to-inspect detail pane (`--json` carries the detail fields).
