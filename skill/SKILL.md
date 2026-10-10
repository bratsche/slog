---
name: slog
version: {{VERSION}}
description: Read macOS and iOS unified logs (what Console.app shows) from the command line with `slog`. Use when debugging a Mac or iOS app or system behavior and you need log output, when the user mentions Console.app, os_log/Logger output, subsystems, or "check the logs", or when you need logs from a connected iPhone/iPad.
---

# slog: Console.app for the command line

`slog` prints unified log entries to stdout, one per line. Sources: this Mac (default), a
`.logarchive` (`show --archive`), or a connected iPhone/iPad (`show`/`stream --device`).
`slog <command> --help` is the complete reference (filter syntax, output format, exit
codes); read it before using an option you are unsure of.

## Rules

- **Always bound the output.** The unified log produces thousands of entries per second.
  - `slog show` is bounded by default (last 5m, newest 1000). Narrow it further rather
    than raising `--limit`.
  - `slog stream` runs until interrupted unless given `--duration`. **Always pass
    `--duration`** (and usually `--limit`).
- **Filter at the source** with `--filter` instead of piping to `grep`; it is faster and
  the counts on stderr stay meaningful.
- **Read stderr.** It says when output was truncated (`showed the newest N of M`) or
  nothing matched (`no matching entries`). Exit 0 with no matches is not an error.
- Info and debug messages are hidden by default, as in Console.app. Add `--info` or
  `--debug` (or a `level:info`/`level:debug` token) when looking for app logging that may
  use those levels.

## Filter cheatsheet

```
word / "a phrase"     message, process, subsystem or category contains it
process:MyApp         field contains value      process:=MyApp   exact match
-category:network     exclude                    pid:1234         level:error
```
Fields: message, process, pid, subsystem, category, library, level. Positive tokens on the
same field are OR'd (`level:error level:fault`); everything else (bare words, exclusions,
different fields) is AND'd.

## Workflow

1. Start narrow: your app's process or subsystem, errors first.
   `slog show --last 10m --filter 'process:MyApp level:error level:fault'`
2. Widen only as needed: drop the level filter, add `--info`/`--debug`, extend `--last`.
3. Reproduce live:
   `slog stream --duration 30s --limit 500 --filter 'subsystem:com.example.myapp'`
   Start the stream in the background (or ask the user to trigger the action) while it runs.
4. Use `--json` when you need to process fields; `--fields time,level,message` to save
   tokens on long results.

## iOS devices

- `slog devices` lists connected devices (USB, or Wi-Fi with Finder's "Show this iPhone
  when on Wi-Fi"). An `error:` column means the device is locked or untrusted: ask the
  user to unlock it and tap Trust.
- `--device` with no value uses the only connected device; pass a UDID or name when
  several are connected.
- Past entries: `slog show --device --last 10m --filter 'process:MyApp level:error'`.
  Each run copies the device's logs first (~5-10 s, ~300 MB temporary disk), so keep
  `--last` short and prefer one well-filtered query over many small ones.
- Live entries: `slog stream --device --duration 30s --filter 'process:MyApp'`.
- Devices produce far more output than Macs with `--debug` (10k+ entries/s); always filter
  by process or subsystem.
- `--predicate` works with `show --device` but not `stream --device`; use `--filter`.

## Crash and diagnostic reports

- `slog reports list --name MyApp` lists reports newest first (last 7 days, 50 max), as
  `time  size  name`. Add `--device` for a connected iPhone/iPad (takes a few seconds).
- `slog reports show <name>` (add `--device` *after* the name for device reports) prints a
  crash report as a short summary: exception, termination reason, app-specific info such as
  Swift `Fatal error:` messages, and the crashed thread's frames. Start there; use `--full`
  only if you need other threads or binary images.
- Other report types (`.diag`, JetsamEvent, ...) print unchanged and can be hundreds of KB;
  check `size` in the list before showing one.
- When debugging a crash, pair the report with the logs just before it:
  `slog show --start '<crash time minus 1m>' --end '<crash time>' --filter 'process:MyApp'`.

## Limits

- Values logged as private appear as `<private>`; slog cannot reveal them.
- A device that Xcode sees only wirelessly (`xcrun devicectl list devices`) but that is not
  in `slog devices` needs a USB cable.

This skill was written for slog {{VERSION}}. If `slog --version` differs, update it with
`slog skill install --force` plus `--user` or `--project`, matching where it was installed
(`--force` is needed because the contents changed; it also replaces any local edits).
