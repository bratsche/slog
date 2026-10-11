//! Command-line definition. Help text here is the primary interface for LLM callers,
//! so every option explains its default, its bounds, and an example.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

const ABOUT: &str =
    "Console.app for the command line: print macOS and iOS unified log entries to stdout.";

const LONG_ABOUT: &str = "\
Console.app for the command line: print macOS and iOS unified log entries to stdout,
one entry per line.

Sources:
  this Mac             default for `show` and `stream`
  a .logarchive file   `slog show --archive PATH`
  an iPhone or iPad    `slog show|stream --device [UDID|NAME]` (USB, or Wi-Fi; trusted)
  a booted simulator   `slog show|stream --simulator [UDID|NAME]`

Crash, hang and diagnostic reports (Console.app's Reports): `slog reports`. Apps crashing
in a simulator are reported on this Mac, so `slog reports` lists them too.

`show` is bounded by default (last 5 minutes, newest 1000 entries). `stream` runs until
Ctrl-C unless --duration or --limit is given; non-interactive callers should always pass
--duration.

Run `slog <command> --help` for every option, the --filter syntax, and examples.";

const FILTER_HELP: &str = "\
Filter syntax (--filter):
  Space-separated tokens, like Console.app's search field. Matching is case-insensitive
  (full Unicode folding, except simple lowercasing for `stream --device`).
    word               any of message, process, subsystem, category contains `word`
    \"two words\"        quoted phrase; quotes keep spaces and disable token syntax
    FIELD:value        FIELD contains value
    FIELD:=value       FIELD equals value exactly
    -TOKEN             exclude entries matching TOKEN
  Fields: message, process, pid, subsystem, category, library, level, any
    pid:1234           process ID (always exact)
    level:error        one of debug, info, default, error, fault (always exact).
                       level:info / level:debug automatically include those levels.
  Combining: positive tokens on the same named field are OR'd. Everything else is AND'd:
  bare words (and any:), negated tokens, and tokens on different fields.
    process:Safari process:Mail level:error   (Safari OR Mail) AND error
    connection timeout                        connection AND timeout
    subsystem:com.apple.network -category:boringssl \"timed out\"

Raw predicates (--predicate):
  An NSPredicate passed to /usr/bin/log, AND'd with --filter. Use it for fields --filter
  lacks (see `log help predicates`). Not available for `stream --device`.
    --predicate 'senderImagePath ENDSWITH \"MyFramework\"'

Output:
  Text (default), one line per entry, newlines in messages shown as \\n:
    2026-10-09 23:29:00.007 error Safari[42] [com.apple.network:connection] Timed out
  i.e. time level process[pid] [subsystem:category] message. Empty subsystem/category are
  omitted. --fields replaces the defaults; the library and thread fields print as
  (library) before the brackets and tid=0x... before the message.
  JSON (--json): one object per line with keys
    time (RFC 3339, microseconds), level, process, pid, library, subsystem, category,
    thread, message
  --fields selects which of these appear, e.g. --fields time,level,process,message.

Exit status: 0 success (including no matches); 2 invalid arguments, including --filter
syntax; 1 any other error, including a --predicate or --start/--end that /usr/bin/log
rejects (its message is shown). Ctrl-C, SIGTERM and SIGHUP stop slog cleanly (temp files
removed, child processes stopped) and it then dies by that signal, so a shell reports 130,
143 or 129. A second signal, or slog still running 2 s later (e.g. blocked writing to a
pipe nobody reads), stops it immediately. Signals ignored by the parent (nohup) stay
ignored.
Notes on stderr (truncation, no matches) never appear on stdout.

Private data: values logged as private appear as <private>, same as Console.app.";

const SHOW_EXAMPLES: &str = "\
Examples:
  slog show                                         newest 1000 entries from the last 5m
  slog show --last 1h --filter 'process:MyApp level:error'
  slog show --last 10m --info --filter 'subsystem:com.example.myapp' --json
  slog show --start '2026-10-09 14:00:00' --end '2026-10-09 14:05:00' --limit 0
  slog show --archive ~/Desktop/sysdiagnose.logarchive --filter 'process:SpringBoard'
  slog show --device --last 10m --filter 'process:MyApp level:error level:fault'
  slog show --device --last 1h --save-archive /tmp/phone.logarchive --filter 'process:MyApp'
    then repeat queries with the `slog show --archive ... --start ...` command it prints
  slog show --simulator --last 10m --filter 'process:MyApp'";

const STREAM_EXAMPLES: &str = "\
Examples:
  slog stream --duration 30s --filter 'process:MyApp'
  slog stream --device --duration 1m --filter 'process:MyApp -level:debug'
  slog stream --device 00008110-001A2B3C4D5E801E --limit 200 --json
  slog stream --simulator 'iPhone 17 Pro' --duration 30s --filter 'subsystem:com.example.myapp'
  slog stream --debug --duration 10s --filter 'subsystem:com.example.myapp'";

#[derive(Parser, Debug)]
#[command(name = "slog", version, about = ABOUT, long_about = LONG_ABOUT)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print past log entries (bounded: last 5m, newest 1000 by default)
    #[command(after_long_help = format!("{FILTER_HELP}\n\n{SHOW_EXAMPLES}"))]
    Show(ShowArgs),

    /// Print live log entries as they are logged, from this Mac or an iOS device
    #[command(after_long_help = format!("{FILTER_HELP}\n\n{STREAM_EXAMPLES}"))]
    Stream(StreamArgs),

    /// List reachable iOS devices (USB, Wi-Fi) and their UDIDs, for `--device`
    #[command(long_about = "\
List the iOS devices slog can reach, for `--device`:
  usb       connected by USB
  network   over Wi-Fi with Finder's \"Show this iPhone when on Wi-Fi\"
  wifi      over Wi-Fi through slog's own pairing (`slog pair`, once, over USB); found
            over Bonjour, so it must be awake, unlocked and on this Mac's network
Use the UDID or name with `slog show --device` or `slog stream --device`.

Text: tab-separated UDID, connection, name, model, OS version.
JSON (--json): one object per line with keys udid, connection, name, model, os_version,
error. Devices that are not trusted show an error instead of details (name, model and
os_version are null); unlock the device and tap Trust, then retry. os_version is null for
wifi devices (slog does not connect to list them).")]
    Devices(ListArgs),

    /// Pair a USB-connected iPhone or iPad with slog, to reach it over Wi-Fi later
    #[command(long_about = "\
Pair a USB-connected iPhone or iPad with slog, so `--device` reaches it over Wi-Fi later
without the cable and without Finder's \"Show this iPhone when on Wi-Fi\". Uses the same
remote pairing Xcode uses for wireless devices. The device must already trust this Mac
(unlock it and tap Trust when first connected); the USB connection then counts as consent,
so pairing shows no prompt. Pairing again replaces the old pairing.

The pairing is stored in ~/Library/Application Support/slog/pairing/<UDID>.plist (mode
0600: it lets its holder use the device's developer services). Delete it to forget the
device here. slog cannot remove the pairing from the device; see the README (\"Managing
pairings\") for resetting the device's trusted computers.

Over Wi-Fi, slog finds the device over Bonjour, so it must be awake, unlocked and on this
Mac's network. Connecting needs macOS's Local Network permission for the app slog runs in
(your terminal, editor or AI agent host): without it, connecting fails with \"No route to
host\"; allow the app in System Settings > Privacy & Security > Local Network and retry.

Examples:
  slog pair                 the only USB-connected device
  slog pair --device 00008160-00123C380100000A")]
    Pair(PairArgs),

    /// List booted simulators and their UDIDs, for `--simulator`
    #[command(long_about = "\
List booted simulators (iOS, watchOS, tvOS, visionOS), as reported by `xcrun simctl`.
Use the UDID or name with `slog show --simulator` or `slog stream --simulator`. Shut-down
simulators have no running log; boot one in Xcode or with `xcrun simctl boot NAME`.

Text: tab-separated UDID, name, OS (e.g. iOS 26.5).
JSON (--json): one object per line with keys udid, name, os.")]
    Simulators(ListArgs),

    /// Install or print the Claude skill that teaches Claude how to use slog
    #[command(
        subcommand,
        long_about = "\
Install or print the Claude skill that teaches Claude how to use slog. The skill is built
into this binary, so it always matches this slog version; after upgrading slog, re-run
`slog skill install --force` with the same --user or --project to replace the old version."
    )]
    Skill(SkillCommand),

    /// List and print crash, hang and diagnostic reports from this Mac or an iOS device
    #[command(
        subcommand,
        long_about = "\
List and print crash, hang and diagnostic reports, like Console.app's Reports section.

Mac reports come from ~/Library/Logs/DiagnosticReports and /Library/Logs/DiagnosticReports;
device reports from the device's crash report store (top level only, as with Mac). Reports
are named by file, e.g. MyApp-2026-10-10-084952.ips: the process, then the local time.

Typical use: `slog reports list --name MyApp`, then `slog reports show <name>`."
    )]
    Reports(ReportsCommand),
}

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Write SKILL.md to ~/.claude/skills/slog/ (--user) or ./.claude/skills/slog/ (--project)
    #[command(long_about = "\
Write the skill's SKILL.md for Claude Code.
  --user      ~/.claude/skills/slog/SKILL.md (all projects)
  --project   ./.claude/skills/slog/SKILL.md, relative to the current directory (run it from
              the repository root; commit the file to share it)
An existing file that already matches is left alone. One with different contents (an
older version or local edits) is only replaced with --force.")]
    Install(SkillInstallArgs),

    /// Print SKILL.md to stdout, e.g. to install it somewhere else
    Print,
}

#[derive(Args, Debug)]
#[command(group(ArgGroup::new("location").required(true).args(["user", "project"])))]
pub struct SkillInstallArgs {
    /// Install for all projects: ~/.claude/skills/slog/SKILL.md
    #[arg(long)]
    pub user: bool,

    /// Install for this project: ./.claude/skills/slog/SKILL.md
    #[arg(long)]
    pub project: bool,

    /// Overwrite an existing SKILL.md whose contents differ
    #[arg(long)]
    pub force: bool,
}

#[derive(Subcommand, Debug)]
pub enum ReportsCommand {
    /// List reports, newest first (last 7 days, at most 50 by default)
    #[command(after_long_help = "\
Output:
  Text: tab-separated time (local), size in bytes, name.
  JSON (--json): one object per line with keys name, time (RFC 3339), size, path (the
  full path on this Mac; null for device reports).
  A note on stderr says when --limit dropped reports.

Examples:
  slog reports list --name MyApp
  slog reports list --last 1h
  slog reports list --device --name MyApp --json")]
    List(ReportsListArgs),

    /// Print one report; crash reports are summarized unless --full
    #[command(after_long_help = "\
Crash reports (.ips files with bug_type 309) print as a summary: process and version,
exception, termination reason, application-specific info (e.g. Swift fatal error
messages), the last exception backtrace if any, and the crashed thread's frames. Other
threads, register state and binary images are omitted; --full prints the original file.
Every other report prints unchanged. Some (.diag, JetsamEvent) are large. An .ips file
without a JSON header line is an error unless --full is given.

Put NAME before --device: `--device NAME` would read NAME as the device.

Examples:
  slog reports show MyApp-2026-10-10-084952.ips
  slog reports show MyApp-2026-10-10-084952.ips --device
  slog reports show ~/Library/Logs/DiagnosticReports/MyApp-2026-10-10-084952.ips --full")]
    Show(ReportsShowArgs),
}

#[derive(Args, Debug)]
pub struct ReportsListArgs {
    /// Only reports whose file name contains this (case-insensitive), e.g. your app's name
    #[arg(long, value_name = "SUBSTR")]
    pub name: Option<String>,

    /// Only reports written within this long, e.g. 1h, 2d
    #[arg(long, value_name = "DURATION", default_value = "7d")]
    pub last: Span,

    /// Print at most N reports, newest first; 0 = no limit
    #[arg(long, value_name = "N", default_value_t = 50)]
    pub limit: usize,

    /// Print one JSON object per line instead of text
    #[arg(long)]
    pub json: bool,

    /// List an iOS device's reports instead of this Mac's (UDID or name; no value = the
    /// only connected device). Takes a few seconds.
    #[arg(long, value_name = "UDID|NAME", num_args = 0..=1, default_missing_value = "")]
    pub device: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReportsShowArgs {
    /// Report name from `slog reports list`, or a path containing '/' (e.g. ./x.ips)
    pub name: String,

    /// Print the whole original file instead of a crash summary
    #[arg(long)]
    pub full: bool,

    /// Read the report from an iOS device (UDID or name; no value = the only connected
    /// device)
    #[arg(long, value_name = "UDID|NAME", num_args = 0..=1, default_missing_value = "")]
    pub device: Option<String>,
}

#[derive(Args, Debug)]
pub struct QueryArgs {
    /// Console-style search tokens; repeatable, all must match
    ///
    /// Example: --filter 'process:MyApp level:error -category:network'.
    /// See "Filter syntax" below.
    #[arg(short, long, value_name = "TOKENS", allow_hyphen_values = true, value_parser = parse_filter)]
    pub filter: Vec<String>,

    /// Raw NSPredicate for /usr/bin/log, AND'd with --filter (not for `stream --device`)
    ///
    /// Example: --predicate 'processImagePath BEGINSWITH "/Applications"'.
    /// Field list: `log help predicates`.
    #[arg(long, value_name = "NSPREDICATE")]
    pub predicate: Option<String>,

    /// Include info-level messages (hidden by default, as in Console.app)
    #[arg(long)]
    pub info: bool,

    /// Include debug- and info-level messages (hidden by default, as in Console.app)
    #[arg(long)]
    pub debug: bool,

    /// Print one JSON object per line instead of text
    #[arg(long)]
    pub json: bool,

    /// Comma-separated fields to print
    ///
    /// Defaults: text = time,level,process,pid,subsystem,category,message;
    /// JSON = all fields. Fields always print in the canonical order.
    #[arg(long, value_enum, value_delimiter = ',', value_name = "FIELDS")]
    pub fields: Vec<OutputField>,
}

#[derive(Args, Debug)]
pub struct ShowArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// How far back to look, e.g. 30s, 5m, 2h, 1d [default: 5m unless --start or --archive]
    ///
    /// Counts back from now, or from the archive's newest entry with --archive.
    #[arg(long, value_name = "DURATION", conflicts_with_all = ["start", "end"])]
    pub last: Option<Span>,

    /// Earliest entry time, local: 'YYYY-MM-DD', 'YYYY-MM-DD HH:MM:SS'
    #[arg(long, value_name = "TIME")]
    pub start: Option<String>,

    /// Latest entry time, same format as --start; requires --start
    #[arg(long, value_name = "TIME", requires = "start")]
    pub end: Option<String>,

    /// Read a .logarchive (from `log collect` or a sysdiagnose) instead of this Mac
    ///
    /// Without --start or --last, every entry in the archive is read before the newest
    /// --limit are printed; for a large archive (e.g. a sysdiagnose) that takes minutes.
    /// Pass --start and --end to read only a window. With --archive, --last counts back
    /// from the archive's newest entry, not from now.
    #[arg(long, value_name = "PATH", conflicts_with = "save_archive")]
    pub archive: Option<PathBuf>,

    /// Read past entries from an iOS device instead of this Mac
    ///
    /// Value is a UDID or device name from `slog devices`. With no value, uses the only
    /// connected device (an error if there are zero or several); USB devices come first,
    /// then devices paired for Wi-Fi with `slog pair`. Copies the device's log archive for
    /// the requested window first: expect ~5-10 s over USB (~30-40 s over Wi-Fi) and ~300 MB
    /// of temporary disk (kept with --save-archive) for the default --last 5m, more for
    /// longer windows.
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "archive"
    )]
    pub device: Option<String>,

    /// Read past entries from a booted simulator instead of this Mac
    ///
    /// Value is a UDID or name from `slog simulators`. With no value, uses the only booted
    /// simulator (an error if there are zero or several). A simulator keeps its own log,
    /// separate from this Mac's; your app's messages logged in the simulator are only here.
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["archive", "device", "save_archive"]
    )]
    pub simulator: Option<String>,

    /// Keep the copied device archive at PATH (needs --device)
    ///
    /// PATH must end in .logarchive and not exist yet; its parent must exist. Stderr then
    /// prints a `slog show --archive PATH --start ...` command that re-queries exactly the
    /// same window without re-copying; add your own --filter etc. An incomplete copy is
    /// deleted; a complete one is yours to delete (`rm -rf PATH`).
    #[arg(long, value_name = "PATH", requires = "device", value_parser = parse_new_logarchive)]
    pub save_archive: Option<PathBuf>,

    /// Print at most N entries, keeping the newest; 0 = no limit
    ///
    /// When entries are dropped, a note on stderr says how many matched in total.
    #[arg(long, value_name = "N", default_value_t = 1000)]
    pub limit: usize,
}

#[derive(Args, Debug)]
pub struct StreamArgs {
    #[command(flatten)]
    pub query: QueryArgs,

    /// Stop after this long, e.g. 30s, 5m [default: run until Ctrl-C]
    #[arg(long, value_name = "DURATION")]
    pub duration: Option<Span>,

    /// Stop after printing N entries; 0 = no limit [default: no limit]
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,

    /// Stream from an iOS device instead of this Mac
    ///
    /// Value is a UDID or device name from `slog devices`. With no value, uses the only
    /// connected device (an error if there are zero or several); USB devices come first,
    /// then devices paired for Wi-Fi with `slog pair`.
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "predicate"
    )]
    pub device: Option<String>,

    /// Stream from a booted simulator instead of this Mac
    ///
    /// Value is a UDID or name from `slog simulators`. With no value, uses the only booted
    /// simulator (an error if there are zero or several). --predicate works here.
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "device"
    )]
    pub simulator: Option<String>,
}

#[derive(Args, Debug)]
pub struct PairArgs {
    /// The device to pair (UDID or name); with no value, the only USB-connected device
    #[arg(long, value_name = "UDID|NAME", num_args = 0..=1, default_missing_value = "")]
    pub device: Option<String>,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    /// Print one JSON object per line instead of text
    #[arg(long)]
    pub json: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputField {
    Time,
    Level,
    Process,
    Pid,
    Library,
    Subsystem,
    Category,
    Thread,
    Message,
}

/// Validates --save-archive: `log show` only reads directories named *.logarchive, and an
/// existing archive is never overwritten.
fn parse_new_logarchive(s: &str) -> Result<PathBuf, String> {
    if s.starts_with('~') {
        return Err(format!(
            "{s:?}: ~ is not expanded here; use an absolute path"
        ));
    }
    let path = PathBuf::from(s);
    if !path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().ends_with(".logarchive"))
    {
        return Err(format!("{s:?} must end in .logarchive"));
    }
    if path.symlink_metadata().is_ok() {
        return Err(format!("{s:?} already exists"));
    }
    Ok(path)
}

/// Validates one --filter so syntax errors are reported as argument errors (exit 2).
fn parse_filter(s: &str) -> Result<String, String> {
    crate::filter::Filter::parse(&[s.to_string()]).map_err(|e| e.to_string())?;
    Ok(s.to_string())
}

/// Parses --start/--end in the formats `log` accepts, as local time unless an offset is
/// given. Used where slog needs the instant itself (device archive collection).
pub fn parse_local_time(s: &str) -> anyhow::Result<DateTime<Local>> {
    if let Ok(t) = DateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%z") {
        return Ok(t.with_timezone(&Local));
    }
    let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").or_else(|_| {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").map(|d| d.and_hms_opt(0, 0, 0).expect("midnight"))
    });
    let Ok(naive) = naive else {
        anyhow::bail!("{s:?} is not a time; use 'YYYY-MM-DD' or 'YYYY-MM-DD HH:MM:SS'");
    };
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(t) => Ok(t),
        _ => anyhow::bail!(
            "{s:?} is ambiguous or skipped in local time (DST change); add an offset, e.g. '{s}-0400'"
        ),
    }
}

/// A duration written as a number and unit: 30s, 5m, 2h, 1d.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span(pub Duration);

impl Span {
    /// Format accepted by `log show --last`.
    pub fn to_log_arg(self) -> String {
        format!("{}s", self.0.as_secs())
    }
}

impl FromStr for Span {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err =
            || format!("{s:?} is not a duration; use a number and unit, e.g. 30s, 5m, 2h, 1d");
        let unit_at = s.find(|c: char| !c.is_ascii_digit()).ok_or_else(err)?;
        let n: u64 = s[..unit_at].parse().map_err(|_| err())?;
        let unit = match &s[unit_at..] {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(err()),
        };
        let secs = n.checked_mul(unit).ok_or_else(err)?;
        if secs == 0 {
            return Err(format!("{s:?} must be greater than zero"));
        }
        Ok(Span(Duration::from_secs(secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn span_parsing() {
        assert_eq!("30s".parse::<Span>().unwrap().0, Duration::from_secs(30));
        assert_eq!("5m".parse::<Span>().unwrap().to_log_arg(), "300s");
        assert_eq!("1d".parse::<Span>().unwrap().0, Duration::from_secs(86400));
        for bad in ["", "5", "m", "5x", "0s", "-5m", "1.5h", "999999999999999d"] {
            assert!(bad.parse::<Span>().is_err(), "{bad}");
        }
    }

    #[test]
    fn local_time_parsing() {
        let t = parse_local_time("2026-10-09 14:05:06").unwrap();
        assert_eq!(
            t.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-10-09 14:05:06"
        );
        let d = parse_local_time("2026-10-09").unwrap();
        assert_eq!(d.format("%H:%M:%S").to_string(), "00:00:00");
        let z = parse_local_time("2026-10-09 14:05:06+0000").unwrap();
        assert_eq!(z.timestamp(), 1_791_554_706);
        assert!(parse_local_time("yesterday").is_err());
        assert!(parse_local_time("2026-10-09T14:05:06").is_err());
    }

    #[test]
    fn device_flag_without_value_means_only_device() {
        let cli = Cli::try_parse_from(["slog", "stream", "--device", "--duration", "5s"]).unwrap();
        let Command::Stream(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.device.as_deref(), Some(""));
    }

    #[test]
    fn filter_may_start_with_negation() {
        let cli = Cli::try_parse_from(["slog", "show", "--filter", "-level:default"]).unwrap();
        let Command::Show(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.query.filter, ["-level:default"]);
    }

    #[test]
    fn save_archive_needs_device_and_a_new_logarchive_path() {
        let dir = std::env::temp_dir();
        let new = dir.join(format!("slog-cli-test-{}.logarchive", std::process::id()));
        let new = new.to_str().unwrap();
        assert!(Cli::try_parse_from(["slog", "show", "--device", "--save-archive", new]).is_ok());
        assert!(Cli::try_parse_from(["slog", "show", "--save-archive", new]).is_err());
        assert!(
            Cli::try_parse_from(["slog", "show", "--device", "--save-archive", "x.tar"]).is_err()
        );
        assert!(parse_new_logarchive(&format!("{new}/")).is_ok());
        assert!(parse_new_logarchive("~/phone.logarchive").is_err());
        let existing = dir.join(format!(
            "slog-cli-existing-{}.logarchive",
            std::process::id()
        ));
        std::fs::create_dir(&existing).unwrap();
        let result = parse_new_logarchive(existing.to_str().unwrap());
        std::fs::remove_dir(&existing).unwrap();
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn show_device_allows_predicate_but_not_archive() {
        assert!(Cli::try_parse_from(["slog", "show", "--device", "--predicate", "a == 1"]).is_ok());
        assert!(Cli::try_parse_from(["slog", "show", "--device", "--archive", "x"]).is_err());
    }

    #[test]
    fn skill_install_needs_exactly_one_location() {
        assert!(Cli::try_parse_from(["slog", "skill", "install"]).is_err());
        assert!(Cli::try_parse_from(["slog", "skill", "install", "--user", "--project"]).is_err());
        assert!(Cli::try_parse_from(["slog", "skill", "install", "--project", "--force"]).is_ok());
    }

    #[test]
    fn reports_show_takes_name_then_device() {
        let cli = Cli::try_parse_from(["slog", "reports", "show", "A.ips", "--device"]).unwrap();
        let Command::Reports(ReportsCommand::Show(args)) = cli.command else {
            panic!()
        };
        assert_eq!(
            (args.name.as_str(), args.device.as_deref()),
            ("A.ips", Some(""))
        );
    }

    #[test]
    fn simulator_excludes_device_and_archive() {
        assert!(
            Cli::try_parse_from(["slog", "show", "--simulator", "--predicate", "a == 1"]).is_ok()
        );
        assert!(Cli::try_parse_from(["slog", "show", "--simulator", "--device"]).is_err());
        assert!(Cli::try_parse_from(["slog", "show", "--simulator", "--archive", "x"]).is_err());
        let new = std::env::temp_dir().join(format!("slog-sim-{}.logarchive", std::process::id()));
        let new = new.to_str().unwrap();
        assert!(
            Cli::try_parse_from(["slog", "show", "--simulator", "--save-archive", new]).is_err()
        );
        assert!(
            Cli::try_parse_from(["slog", "show", "--archive", "x", "--save-archive", new]).is_err()
        );
        assert!(Cli::try_parse_from(["slog", "stream", "--simulator", "--device"]).is_err());
        let cli =
            Cli::try_parse_from(["slog", "stream", "--simulator", "--duration", "5s"]).unwrap();
        let Command::Stream(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.simulator.as_deref(), Some(""));
    }

    #[test]
    fn predicate_conflicts_with_device() {
        assert!(
            Cli::try_parse_from(["slog", "stream", "--device", "x", "--predicate", "a == 1"])
                .is_err()
        );
    }
}
