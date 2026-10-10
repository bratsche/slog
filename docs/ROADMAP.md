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
| iPhone/iPad seen only by Xcode/CoreDevice (Wi-Fi) | `--device` | `idevice` `rsd` + `tunnel` + `remote_pairing` | Phase 2b |
| iPhone/iPad, history | `slog show --device` | `os_trace_relay` `CreateArchive`, then `log show` | Phase 2 |
| Crash/spin/diagnostic reports (Mac) | `slog reports` | Read `~/Library/Logs/DiagnosticReports`, `/Library/Logs/...` | Phase 2 |
| Crash reports (device) | `slog reports --device` | `crashreportcopymobile` service | Phase 2 |
| Simulators | `--simulator` | `xcrun simctl spawn <id> log ...` | Phase 3 |
| Apple Watch | `--device` | `companion_proxy` through the paired iPhone | Phase 4 |

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

- `slog show --device` via `CreateArchive`
- `slog reports` for Mac and device crash/spin/diagnostic reports (`.ips` JSON)
- `slog skill install --user|--project` and `slog skill print`: the Claude skill is embedded
  in the binary so it always matches the installed version
  (`~/.claude/skills/slog/SKILL.md` or `./.claude/skills/slog/SKILL.md`)

### Phase 2b

- Devices reachable only over CoreDevice (what `xcrun devicectl list devices` shows as
  "available (paired)" without USB or Finder's "Show this iPhone when on Wi-Fi"). Needs the
  RemoteXPC tunnel path in `idevice` (`rsd`, `tunnel`, `remote_pairing`). Also the likely
  route to Apple Watch.

### Phase 3

- Simulators (`--simulator [UDID|NAME]`)
- Distribution: crates.io (`cargo install slog-cli` or similar; `slog` is taken on
  crates.io) and a Homebrew tap

### Phase 4

- Apple Watch through the paired iPhone

## Not planned

Console.app features that do not fit a stdout CLI: the Activities tree, pause/resume of a
live view, and the click-to-inspect detail pane (`--json` carries the detail fields).
