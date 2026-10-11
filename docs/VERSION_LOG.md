# slog Version Log

Last Updated: 2026-10-10 · Version: 0.1.0

## 0.1.0 — 2026-10-10

First release. Crate `slog-cli` (installs the `slog` command); Homebrew
`bratsche/tap/slog`.

### Sources

- `show` and `stream` for this Mac via `/usr/bin/log --style ndjson`; `show --archive` for
  `.logarchive` files.
- iOS devices over usbmuxd (USB, or Wi-Fi via Finder's setting):
  `stream --device` (live, `os_trace_relay`), `show --device` (past entries, copied with
  `os_trace_relay` `CreateArchive`, no `sudo`; supports `--predicate`), and
  `--save-archive PATH` to keep that copy for reuse with `--archive`.
- `devices` lists connected iOS devices.
- `pair`: pairs a USB-connected device with slog so `--device` reaches it over Wi-Fi
  afterwards without Finder's setting (own remote pairing and in-process tunnel).
- Booted simulators: `simulators` lists them; `show --simulator` and `stream --simulator`
  run `log` inside one via `xcrun simctl spawn` (`--predicate` supported).
- `reports list|show [--device]`: crash, hang and diagnostic reports from the Mac or a
  device, with a compact crash summary (`--full` for the original file).

### Querying and output

- Console-style `--filter` compiled to NSPredicate for `show` and Mac `stream`, evaluated
  in-process for `stream --device`; raw `--predicate` passthrough.
- Compact text and `--json` output, `--fields`, `--info`, `--debug`.
- `show` bounded by default (`--last 5m`, `--limit 1000`, newest kept) with stderr notes on
  truncation and empty results.

### Tooling

- `skill install --user|--project [--force]` and `skill print`: the Claude skill for slog,
  embedded in the binary.
- Ctrl-C, SIGTERM and SIGHUP clean up temp archives and child processes, then slog dies by
  the signal; signals ignored by the parent (`nohup`) stay ignored.
- Packaging (`slog-cli`, MSRV 1.89, macOS only), README, LICENSE; CI on macOS with smoke
  runs against the runner's unified log.

Fixes found in review along the way are listed in [BUG_REFERENCE.md](BUG_REFERENCE.md).
