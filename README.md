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
| Crash, hang and diagnostic reports (Mac or device) | `slog reports list` / `slog reports show NAME` |

Devices connect over USB (Wi-Fi via Finder's "Show this iPhone when on Wi-Fi" should also
work, but is untested). The device must be unlocked and trusted; no `sudo` is needed.

## Install

Requires macOS (it reads the unified log through `/usr/bin/log`).

```sh
cargo install slog-cli   # installs the `slog` command
```

The crate is named `slog-cli` because `slog` is already taken on crates.io.

## Usage

```sh
# Past entries: bounded by default (last 5 minutes, newest 1000)
slog show --last 1h --filter 'subsystem:com.example.myapp'
slog show --last 10m --info --json --fields time,level,message

# Live entries: always bound non-interactive runs with --duration
slog stream --duration 30s --filter 'process:MyApp -level:debug'

# iOS devices
slog devices
slog stream --device --duration 1m --filter 'process:MyApp'
slog show --device --last 10m --save-archive /tmp/phone.logarchive --filter 'process:MyApp'

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
