# slog Version Log

Last Updated: 2026-10-10 · Version: 0.1.0

## 0.1.0 — 2026-10-09 (unreleased)

- Initial project: `show`, `stream`, `devices` commands.
- Console-style `--filter` compiled to NSPredicate (Mac) or evaluated in-process (devices);
  raw `--predicate` passthrough.
- Compact text and `--json` output, `--fields`, `--info`, `--debug`.
- `show` bounded by default (`--last 5m`, `--limit 1000`, newest kept).
- iOS live streaming via `idevice` `os_trace_relay`, verified on iPhone (iOS 27.0.1, USB).
- Fixes from code review: see [BUG_REFERENCE.md](BUG_REFERENCE.md) "Review fixes".
