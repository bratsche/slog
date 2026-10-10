# slog Version Log

Last Updated: 2026-10-10 · Version: 0.1.0

## Unreleased

- `slog skill install --user|--project [--force]` and `slog skill print`: the Claude skill
  for slog, embedded in the binary.
- `slog show --device [UDID|NAME]`: past entries from an iOS device, copied with
  `os_trace_relay` `CreateArchive` (no `sudo`); supports `--predicate`.
- Fix: `show --archive` failed on every archive (post-trailer banner).
- `slog reports list|show [--device]`: crash, hang and diagnostic reports from the Mac or an
  iOS device, with a compact crash summary (`--full` for the original file).

- `slog show --device --save-archive PATH`: keep the device archive copy for reuse with
  `--archive`.
- Packaging: crate `slog-cli` (installs `slog`), README, LICENSE, `rust-version = 1.89`.

## 0.1.0 — 2026-10-09 (unreleased)

- Initial project: `show`, `stream`, `devices` commands.
- Console-style `--filter` compiled to NSPredicate (Mac) or evaluated in-process (devices);
  raw `--predicate` passthrough.
- Compact text and `--json` output, `--fields`, `--info`, `--debug`.
- `show` bounded by default (`--last 5m`, `--limit 1000`, newest kept).
- iOS live streaming via `idevice` `os_trace_relay`, verified on iPhone (iOS 27.0.1, USB).
- Fixes from code review: see [BUG_REFERENCE.md](BUG_REFERENCE.md) "Review fixes".
