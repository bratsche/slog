# slog Data Flow

Last Updated: 2026-10-10 · Version: 0.1.0

## Pipeline

```text
CLI args (cli.rs)
   │
   ├─ --filter ──► Filter::parse (filter.rs)
   │                 ├─ to_predicate() ──┐  Mac / archive: filtered by /usr/bin/log
   │                 └─ matches(&Entry) ─┼─ device: filtered in-process
   │                                     │
   ▼                                     ▼
Source (source/*)  ──►  Stream<Result<Entry>>  ──►  run.rs  ──►  Printer (output.rs)  ──► stdout
   mac.rs     /usr/bin/log show|stream --style ndjson     show: keep newest N, then print
   device.rs  usbmuxd → lockdown → os_trace_relay         stream: print until limit/duration
                                                           notes ──► stderr
```

## Modules

| File | Responsibility |
|---|---|
| `src/cli.rs` | clap definitions, all help text, `Span` duration parsing |
| `src/entry.rs` | `Entry` and `Level`: the source-independent record |
| `src/filter.rs` | Console token parser; NSPredicate and in-process backends |
| `src/source/mac.rs` | Spawns `/usr/bin/log`, parses ndjson into `Entry` |
| `src/source/device.rs` | Lists devices, resolves `--device`, streams `os_trace_relay`, copies archives |
| `src/run.rs` | `--limit` / `--duration` enforcement, stderr notes |
| `src/output.rs` | Text and JSON rendering, `--fields` |
| `src/main.rs` | Wiring and error/exit-code handling |

## Filter semantics

Both backends must agree; unit tests in `filter.rs` cover each rule for both.

- Positive tokens on the same named field are OR'd; bare words (`any`), negated tokens and
  different fields are AND'd.
- Text comparisons are case-insensitive (`CONTAINS[c]`, `==[c]` / lowercase compare). Some
  non-ASCII folding differs; see [BUG_REFERENCE.md](BUG_REFERENCE.md).
- Bare words search message, process, subsystem and category (Console's "Any").
- `level:info` / `level:debug` turn on `--info` / `--debug` so the filter can match.

Field mapping:

| Token field | NSPredicate key | `Entry` field |
|---|---|---|
| `message` | `composedMessage` | `message` |
| `process` | `process` | `process` |
| `pid` | `processIdentifier` | `pid` |
| `subsystem` | `subsystem` | `subsystem` |
| `category` | `category` | `category` |
| `library` | `sender` | `library` |
| `level` | `logType` | `level` |

## `/usr/bin/log` ndjson notes

- Only `eventType == "logEvent"` lines become entries; activity, signpost and state events
  are skipped.
- `log show` ends with `{"count":N,"finished":1}`; skipped. For archives, `log` then prints
  `==========`, the canonical archive path, `==========`; skipped. Any other line after the
  trailer is passed to stderr. Archive paths are canonicalized so the path line matches.
- `log stream --predicate` prints a plain-text `Filtering the log data using ...` banner
  first; skipped explicitly. Any other non-JSON line is an error.
- `log` logs its own invocation (including the predicate) to the unified log, so entries
  recording slog-spawned invocations (from this run or earlier ones) are dropped.
- Non-zero exit (e.g. 64 for a bad predicate) becomes an error carrying `log`'s stderr.

## Device notes

- `show --device` copies a log archive with `os_trace_relay` `CreateArchive` (a tar stream,
  piped into `/usr/bin/tar -x` in a temp dir), then reads it like `--archive`. `StartTime`
  (unix seconds) is always sent: without it the device sends its whole store (~3 GB, ~20 s
  on an iPhone); with it, the files covering the window (~300 MB minimum, ~5 s). `--last`
  is converted to an explicit `--start` so `log` never interprets it relative to an archive.

- usbmuxd may list a device twice (USB and Wi-Fi); the USB entry wins.
- `os_trace_relay` has no predicate support and streams every level; info/debug are dropped
  in-process unless requested.
