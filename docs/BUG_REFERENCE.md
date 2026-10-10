# slog Bug Reference

Last Updated: 2026-10-10 · Version: 0.1.0

## Fixed

### Stopping `simctl spawn` with SIGKILL left `log` running in the simulator (found 2026-10-10)

- **Symptom (found while building `--simulator`, before it shipped):** after a bounded
  `stream --simulator` (`--duration`, `--limit`) or Ctrl-C, a `log stream` process kept
  running in the simulator, visible on the Mac with `pgrep -fl 'log stream'`.
- **Root cause:** tokio's `kill_on_drop` sends SIGKILL. `simctl spawn` forwards SIGTERM to
  the process it started in the simulator, but a SIGKILLed `simctl` forwards nothing, and
  the simulator's `log` is not its child on the Mac, so it is orphaned.
- **Solution:** `mac::Terminating` wraps every `log` child and sends SIGTERM on drop if it
  is still running. Same for Mac `log`, which exits on SIGTERM too.
- **Verified:** `--duration`, `--limit`, SIGINT, and the forced exit with stdout blocked
  all leave no `log stream` behind.

### Interrupted `show --device` left ~300 MB in `$TMPDIR` (2026-10-10)

- **Symptom:** Ctrl-C, or a `timeout` (SIGTERM), during `show --device` left a
  `$TMPDIR/slog-*` directory holding the partial or complete device archive.
- **Root cause:** the default signal action ends the process at once, so the `TempDir`
  destructor never ran. Children (`tar`, `log`) shared the terminal's process group, so a
  Ctrl-C also killed them directly.
- **Solution:** `src/signals.rs` watches SIGINT/SIGTERM/SIGHUP on a worker thread. On the
  first signal `main` drops the command (deleting temp dirs; `kill_on_drop` stops children),
  then resets the signal to its default and re-raises it, so slog dies *by* the signal.
  `log` and `tar` run in their own process group so a Ctrl-C reaches only slog; otherwise
  `log` could die first and surface as a spurious "log failed" error.
- **Review catches (before merge):** exiting 128 + N instead of dying by the signal made
  bash keep running a script after Ctrl-C (bash only aborts when the child was killed by
  SIGINT); a watcher on the main thread never ran while slog was blocked writing to a full
  pipe, so Ctrl-C was ignored; the forced exit (second signal or 2 s) then skipped temp-dir
  deletion, because a blocked main thread never drops its `TempDir`; and installing tokio
  handlers overrode signals the parent ignored, breaking `nohup`. Hence the re-raise, the
  worker-thread watcher, `remove_on_force` (the watcher deletes registered temp dirs before
  a forced exit), and skipping signals whose inherited disposition is SIG_IGN.
- **Verified:** SIGINT to the process group, SIGTERM and SIGHUP during the copy, SIGINT
  during `log show` and Mac `stream`: killed by the signal, no temp dirs, no children. A bash
  loop stops at the first Ctrl-C; slog blocked on an unread pipe exits 2 s after SIGINT.

### `slog skill` review fixes (2026-10-10)

- `HOME= slog skill install --user` installed into `./.claude/skills/slog/` (the project
  location) and reported success. `PathBuf::join` on an empty or relative `HOME` yields a
  path relative to the current directory; `user_path` now rejects a non-absolute `HOME`.
- `slog skill print | head` (reader gone before the write) panicked with exit 101 because
  `print!` panics on `EPIPE`. Output now goes through `write!` so `main`'s broken-pipe
  handling applies, as for the other commands.
- The skill and `slog skill --help` said to re-run `slog skill install` after upgrading,
  but the new version always differs from the installed file, so that fails without
  `--force`. Both now say `--force`. The skill also said "same-field tokens are OR'd",
  which is wrong for exclusions (`-process:a -process:b` excludes both); now "positive
  tokens".

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

### `slog reports show` review fixes (2026-10-10)

- **Blank image name in crash frames.** Every real `.ips` has a placeholder `usedImages` entry
  (`source: "A"`, `size: 0`, no name or path) for addresses outside any binary. A frame
  pointing at it printed an empty image column and `" + <offset>"`, because
  `"".rsplit('/').next()` is `Some("")`, so the `"???"` default never applied. Now `???`,
  as in Apple's reports; covered by `crash_summary_shows_cause_and_crashed_thread`.
- **Exception message dropped.** `exception.message` (e.g. EXC_GUARD details in an
  ExcUserFault report) was not in the summary; now printed as `Message:`.
- **Permission errors reported as "no report named X".** `find_mac` used `Path::is_file`,
  which returns false on any error, so an unreadable DiagnosticReports directory looked like a
  missing report. It now uses `fs::metadata` and surfaces errors other than NotFound, as
  `list` already does.

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
- `show --archive` without `--start` or `--last` reads the whole archive; a sysdiagnose takes
  minutes. With `--archive`, `--last` counts back from the archive's newest entry.
- `show --device` keeps its copied archive (~300 MB+) in `$TMPDIR/slog-*` while running and
  deletes it on exit, including Ctrl-C, SIGTERM, SIGHUP and the forced exit 2 s later when
  output is blocked, but not after `kill -9`, and possibly only partly if a second signal
  lands during the ~0.1 s deletion. macOS purges `$TMPDIR`
  periodically; delete stragglers with `rm -rf "$TMPDIR"/slog-*`.
- `log` runs in its own process group, so after `kill -9` of slog (or a forced exit) a
  quiet `log stream` (narrow predicate) keeps running until its next write fails; clean up
  with `pkill -f 'log stream --style ndjson'`. Same for `stream --simulator` after `kill -9`:
  `simctl spawn` and the simulator's `log stream` (both visible on the Mac) stay running.
  Ctrl-Z stops only slog.
- Non-admin users cannot read `/Library/Logs/DiagnosticReports` (`root:_analyticsusers 0770`);
  `slog reports list` skips it with a stderr note and lists only `~/Library` reports.
- Values logged as private print as `<private>` (same as Console.app).
- In a simulator, `log` records its own invocations with parent `launchd_sim` whoever ran
  it, so `--simulator` hides every `simctl spawn ... log` invocation entry, not only slog's,
  along with `launchd_sim`'s records of starting and reaping those one-shot `log` jobs.
- `zsh` has a `log` builtin; always invoke `/usr/bin/log` by absolute path.
- `os_trace_relay` rejects unknown level bytes with an error, ending the stream; if a future
  iOS adds a level, this surfaces as `unknown log level byte value`.
