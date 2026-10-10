# slog Bug Reference

Last Updated: 2026-10-10 · Version: 0.1.0

## Fixed

### Every `--filter` search matched one spurious `log` entry

- **Symptom:** `slog show --filter nonexistent` printed a `log[pid] ... log run
  noninteractively ... args: '/usr/bin/log' 'show' ... '--predicate' '...nonexistent...'`
  entry instead of "no matching entries".
- **Root cause:** `/usr/bin/log` writes its own command line, including the predicate, to the
  unified log, so the search term always appears in at least one message. A first fix that
  dropped only the current child's PID still leaked entries from *earlier* slog runs into
  repeated searches (found in review).
- **Solution:** `source/mac.rs::is_slog_invocation` drops every `log` entry in subsystem
  `com.apple.log` whose message is `log run noninteractively, parent: <pid> (slog), ...`.
  `log` invocations from other parents are kept.
- **Prevention:** Test "no match" queries twice in a row against real output.

### Review fixes (2026-10-10)

- `show --end` without `--start` was silently overridden by the default `--last 5m`.
  `--end` now requires `--start`; `--last` conflicts with both.
- `stream --limit 0` exited immediately; 0 now means no limit, as in `show`.
- A huge duration (`999999999999999d`) overflowed and panicked; now an argument error.
- `--filter` syntax errors exited 1; they are now validated by clap and exit 2, as the help
  states.
- Device selection by name swallowed lockdown errors, reporting "no device named X" for an
  untrusted phone; the error now lists the devices whose names could not be read.
- Missing `threadID`/`senderImagePath` silently became 0/""; they are now required (present
  on every logEvent in an 86k-entry sample).
- Help text corrected: bare words are AND'd (not OR'd as "same field" suggested), `--fields`
  replaces the defaults, exit-status details, and `devices --json` keys.

### `--filter '-level:default'` rejected as an unknown flag

- **Symptom:** `error: unexpected argument '-l' found`.
- **Root cause:** clap treats values beginning with `-` as flags.
- **Solution:** `allow_hyphen_values = true` on `--filter`; covered by
  `cli::tests::filter_may_start_with_negation`.

### `show --archive` failed on every archive (found 2026-10-10)

- **Symptom:** `unexpected output from /usr/bin/log: ==========`.
- **Root cause:** after its `{"count":N,"finished":1}` trailer, `log show <archive>` prints a
  `==========` / archive path / `==========` block, which the ndjson parser rejected. Phase 1
  never ran `--archive` because no archive was available to test with.
- **Solution:** `source/mac.rs::run` skips that block (the path is canonicalized, since `log`
  prints `/private/var/...` for `/var/...`) and forwards any other post-trailer line to
  stderr.
- **Prevention:** `show --device` exercises the archive path on every run.

### Review fixes for `show --device` (2026-10-10)

- An unreadable archive reported `unexpected output from /usr/bin/log: ==========` instead
  of `log`'s error: `log` prints the `==========` / path block even when it fails, with no
  trailer before it. The block is now skipped wherever it appears, so the exit status and
  stderr (`The log archive is corrupt or incomplete`) are reported.
- `show --device` hung when tar printed more than a pipe's worth of errors (reproduced with
  `--last 7d` and `TMPDIR` on a 20 MB disk): tar's stderr was read only after the copy, so
  tar blocked writing it and stopped reading the archive. stderr is now drained during the
  copy; the run ends with tar's "No space left on device" error.
- `--last` was passed to `log` as a local time without offset, ambiguous in the hour
  repeated when DST ends; it now includes the offset (`+zzzz`, accepted by `log`).
- Help said `--last` with `--archive` counts from now; `log` counts back from the archive's
  newest entry (verified on a copied device archive). Help corrected.
- The "copying logs since" note printed only the time of day, misleading for `--last 7d`;
  it now includes the date.

## Upstream workarounds

### `idevice` 0.1.68: device timestamps lose sub-second precision

- **Root cause:** `os_trace_relay.rs` calls `DateTime::from_timestamp(seconds, microseconds)`,
  passing microseconds where chrono expects nanoseconds.
- **Workaround:** `source/device.rs::device_time` multiplies the nanosecond field by 1000 and
  treats the naive value as UTC. Out-of-range values are an error.
- **Remove when:** fixed upstream (github.com/jkcoxson/idevice); the
  `device_time_restores_microseconds` test will then fail and flag it.

## Known limitations

- `slog devices` only sees devices usbmuxd knows: USB, or Wi-Fi with Finder's "Show this
  iPhone when on Wi-Fi". A phone Xcode sees wirelessly via CoreDevice is invisible until
  Phase 2b. Check with `system_profiler SPUSBHostDataType` and `xcrun devicectl list devices`.
- Intermittent: one `--device --debug` stream (iOS 27.0.1, ~4k entries/s) ended with
  `os_trace_relay stream ended: ... peer closed connection without sending TLS close_notify`.
  Not reproduced in 7 further runs at up to ~17k entries/s. The device closed the socket;
  slog exits 1 rather than reconnecting silently. Investigate if it recurs (device lock?
  relay buffer overflow?).
- Case-insensitive matching differs for some non-ASCII text: NSPredicate (Mac) uses full
  Unicode folding, so `STRASSE` matches `Straße`; the device backend uses `to_lowercase` and
  does not. Fixing this needs a Unicode case-folding crate.
- `show --archive` without `--start` reads the whole archive; a sysdiagnose takes minutes.
- `show --device` keeps its copied archive (~300 MB+) in `$TMPDIR/slog-*` while running and
  deletes it on exit, but not if killed (Ctrl-C, SIGKILL). macOS purges `$TMPDIR`
  periodically; delete stragglers with `rm -rf "$TMPDIR"/slog-*`.
- Values logged as private print as `<private>` (same as Console.app).
- `zsh` has a `log` builtin; always invoke `/usr/bin/log` by absolute path.
- `os_trace_relay` rejects unknown level bytes with an error, ending the stream; if a future
  iOS adds a level, this surfaces as `unknown log level byte value`.
