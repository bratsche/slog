# slog

Console.app for the command line. `slog` prints macOS and iOS unified log entries — and crash
and diagnostic reports — to stdout, one entry per line, with Console-style search.

It is built to be driven by LLMs as well as people: `show` is bounded by default (last 5
minutes, newest 1000 entries), `stream` stops after `--duration` or `--limit`, `--help` is the
complete reference, and it ships a [Claude](https://claude.com/claude-code) skill.

```console
$ slog show --last 10m --filter 'process:MyApp level:error level:fault' --limit 2
2026-10-10 12:05:15.366 error MyApp[4242] [com.example.myapp:network] Request failed: timed out
2026-10-10 12:05:17.012 fault MyApp[4242] [com.example.myapp:sync] Store migration failed
slog: showed the newest 2 of 37 matching entries; narrow --last/--filter or raise --limit (0 = no limit)
```

## Sources

| Source | Command |
|---|---|
| This Mac, past entries | `slog show` |
| This Mac, live | `slog stream` |
| A `.logarchive` (e.g. from a sysdiagnose) | `slog show --archive PATH` |
| A connected iPhone or iPad, past entries | `slog show --device` |
| A connected iPhone or iPad, live | `slog stream --device` |
| A booted simulator, past entries or live | `slog show --simulator` / `slog stream --simulator` |
| Crash, hang and diagnostic reports (Mac, simulator or device) | `slog reports list` / `slog reports show NAME` |

Devices connect over USB or Wi-Fi, and simulators must be booted; see [Setup](#setup).
No `sudo` is needed for anything.

## Install

Requires macOS (it reads the unified log through `/usr/bin/log`).

```sh
brew install bratsche/tap/slog
```

or with Cargo:

```sh
cargo install --locked slog-cli   # installs the `slog` command
```

The crate is named `slog-cli` because `slog` is already taken on crates.io. Both build from
source; Homebrew brings its own Rust, Cargo needs Rust 1.89 or newer.

## Setup

Logs and reports from this Mac work right after installing. Devices and simulators need a
little setup first.

### iPhone or iPad over USB

1. Connect the device by USB, unlock it, and tap **Trust** when it asks to trust this Mac
   (enter its passcode if asked). Finder or Xcode may already have done this.
2. Check with `slog devices`. A line like this means it is ready:

   ```text
   00008160-00123C380100000A	usb	My iPhone	iPhone19,2	27.0.1
   ```

   An `error:` column instead means the device is locked or does not trust this Mac yet:
   unlock it, tap Trust, and run `slog devices` again.

### iPhone or iPad over Wi-Fi

The device must be awake, unlocked and on the same network as the Mac. There are two ways;
use either.

- **`slog pair` (recommended).** With the device connected and trusted over USB, run
  `slog pair` once. From then on `--device` also finds it over Wi-Fi, and `slog devices`
  lists it as `wifi`. This is slog's own pairing, made with the protocol Xcode uses for
  wireless devices; the device shows no prompt, as the USB connection counts as consent.
- **Finder's "Show this iPhone when on Wi-Fi".** With the device connected by USB, select
  it in Finder's sidebar, and on the General tab tick **Show this iPhone when on Wi-Fi**
  under Options, then **Apply**. `slog devices` then lists it as `network`.

Both need macOS's **Local Network** permission for the app slog runs in (your terminal,
editor, or the app hosting an AI agent), the first time slog connects to a device over
Wi-Fi:

1. Run a device command, e.g. `slog stream --device --duration 10s`. Without the permission
   it fails with `No route to host`, and the app should then appear in the Local Network
   list.
2. Open **System Settings > Privacy & Security > Local Network** and turn the app on.
3. Run the command again.

USB connections never need this permission, and neither does finding devices over Wi-Fi:
`slog devices` can list a `wifi` device that connecting to then fails with
`No route to host`.

Over Wi-Fi, `show --device` copies the device's log archive over the network: about
30-40 s, versus 5-10 s over USB. For several queries over the same period, copy once with
`--save-archive` (see `slog show --help`).

**Managing pairings.** `slog pair` stores one file per device in
`~/Library/Application Support/slog/pairing/<UDID>.plist`, readable only by you: it holds
the key that lets slog use the device's developer services, so do not share it. Running
`slog pair` again replaces it. Delete the file to forget the device on this Mac. slog cannot
remove the pairing from the device itself. Apple's way to reset the computers a device
trusts is Settings > General > Transfer or Reset iPhone > Reset > Reset Location & Privacy,
which also resets Finder's and Xcode's trust and every app's location and privacy
permissions; whether it also removes slog's pairing has not been verified.

### Simulators

Simulators need Xcode. Boot one (run your app in it from Xcode, or
`xcrun simctl boot "iPhone 17 Pro"`), then check with `slog simulators`. A simulator keeps
its own log, separate from the Mac's: use `slog show --simulator` or
`slog stream --simulator` for messages logged in it. Apps that crash in a simulator are
reported on the Mac, so plain `slog reports list` includes them.

### Claude Code and other AI agents

Install the skill (see [Claude skill](#claude-skill)) so the agent knows how to use slog.
The agent runs slog itself, so the Local Network permission above must be granted to the
app the agent runs in. In testing, commands run by Claude Code inside iTerm were refused
with `No route to host` even though iTerm was allowed, while another terminal (Ghostty)
worked as soon as it was allowed; turning the app's permission off and on again, or
restarting the app, may help. USB devices are not affected.

## Usage

```sh
# Past entries: bounded by default (last 5 minutes, newest 1000)
slog show --last 1h --filter 'subsystem:com.example.myapp'
slog show --last 10m --info --json --fields time,level,message

# Live entries: always bound non-interactive runs with --duration
slog stream --duration 30s --filter 'process:MyApp -level:debug'

# iOS devices
slog devices
slog pair                       # once, over USB: reach the device over Wi-Fi afterwards
slog stream --device --duration 1m --filter 'process:MyApp'
slog show --device --last 10m --save-archive /tmp/phone.logarchive --filter 'process:MyApp'

# Simulators
slog simulators
slog stream --simulator --duration 30s --filter 'process:MyApp'

# Crash reports
slog reports list --name MyApp
slog reports show MyApp-2026-10-10-084952.ips
```

### Filter syntax

`--filter` takes Console.app-style search tokens; matching is case-insensitive.

| Token | Meaning |
|---|---|
| `word`, `"a phrase"` | message, process, subsystem or category contains it |
| `process:MyApp` | field contains value |
| `process:=MyApp` | field equals value |
| `-category:network` | exclude matches |
| `pid:1234`, `level:error` | exact process ID / level (`debug`, `info`, `default`, `error`, `fault`) |

Positive tokens on the same field are OR'd; everything else is AND'd. For anything `--filter`
can't express, `--predicate` passes an NSPredicate straight to `/usr/bin/log` (not available
for `stream --device`).

Run `slog <command> --help` for every option, the output formats and exit codes.

## Troubleshooting

| Symptom | Fix |
|---|---|
| `slog devices` prints `no iOS devices found` | Connect the device by USB, unlock it, tap Trust. For Wi-Fi, see [Setup](#iphone-or-ipad-over-wi-fi). |
| An `error:` column in `slog devices` | Unlock the device and tap Trust. |
| `No route to host` over Wi-Fi | Allow the app slog runs in under System Settings > Privacy & Security > Local Network. |
| `no paired device is on the network` | Wake and unlock the device; check it is on the Mac's Wi-Fi network. |
| `... no longer accepts slog's pairing` | The device's trusted computers were cleared: connect it by USB and run `slog pair` again. |
| `no booted simulators` | Boot one in Xcode or with `xcrun simctl boot NAME`. |
| Your app's simulator logs are missing from `slog show` | Add `--simulator`: a simulator has its own log. |
| `slog: skipping /Library/Logs/DiagnosticReports: permission denied` | System reports need an admin account; reports in `~/Library` are still listed. |
| Messages show `<private>` | The app logged them as private; neither slog nor Console.app can show them. |
| `show` prints nothing but `no matching entries` | Info and debug messages are hidden by default, as in Console.app: add `--info` or `--debug`, or widen `--last`. |

## Claude skill

```sh
slog skill install --user      # ~/.claude/skills/slog/SKILL.md, for all projects
slog skill install --project   # ./.claude/skills/slog/SKILL.md, commit it to share
```

The skill is embedded in the binary, so it always matches the installed version. After
upgrading slog, re-run the install with `--force`.

## Documentation

- [Roadmap](docs/ROADMAP.md) — sources, design decisions, what's next
- [Data flow](docs/DATA_FLOW.md) — architecture and module map
- [Bug reference](docs/BUG_REFERENCE.md) — fixed bugs and known limitations
- [Version log](docs/VERSION_LOG.md)

## License

[MIT](LICENSE)
