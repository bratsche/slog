//! Command-line definition. Help text here is the primary interface for LLM callers,
//! so every option explains its default, its bounds, and an example.

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use clap::{Args, Parser, Subcommand, ValueEnum};

const ABOUT: &str =
    "Console.app for the command line: print macOS and iOS unified log entries to stdout.";

const LONG_ABOUT: &str = "\
Console.app for the command line: print macOS and iOS unified log entries to stdout,
one entry per line.

Sources:
  this Mac             default for `show` and `stream`
  a .logarchive file   `slog show --archive PATH`
  an iPhone or iPad    `slog show|stream --device [UDID|NAME]` (USB or Wi-Fi; trusted)

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
rejects (its message is shown).
Notes on stderr (truncation, no matches) never appear on stdout.

Private data: values logged as private appear as <private>, same as Console.app.";

const SHOW_EXAMPLES: &str = "\
Examples:
  slog show                                         newest 1000 entries from the last 5m
  slog show --last 1h --filter 'process:MyApp level:error'
  slog show --last 10m --info --filter 'subsystem:com.example.myapp' --json
  slog show --start '2026-10-09 14:00:00' --end '2026-10-09 14:05:00' --limit 0
  slog show --archive ~/Desktop/sysdiagnose.logarchive --filter 'process:SpringBoard'
  slog show --device --last 10m --filter 'process:MyApp level:error level:fault'";

const STREAM_EXAMPLES: &str = "\
Examples:
  slog stream --duration 30s --filter 'process:MyApp'
  slog stream --device --duration 1m --filter 'process:MyApp -level:debug'
  slog stream --device 00008110-001A2B3C4D5E801E --limit 200 --json
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

    /// List connected iOS devices and their UDIDs, for `--device`
    #[command(long_about = "\
List every iOS device visible to usbmuxd (USB, or Wi-Fi if
\"Show this iPhone when on Wi-Fi\" is enabled in Finder). Use the UDID or name with
`slog show --device` or `slog stream --device`.

Text: tab-separated UDID, connection (usb|network), name, model, OS version.
JSON (--json): one object per line with keys udid, connection, name, model, os_version,
error. Devices that are not trusted show an error instead of details (name, model and
os_version are null); unlock the device and tap Trust, then retry.")]
    Devices(DevicesArgs),
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
    #[arg(long, value_name = "PATH")]
    pub archive: Option<PathBuf>,

    /// Read past entries from an iOS device instead of this Mac
    ///
    /// Value is a UDID or device name from `slog devices`. With no value, uses the only
    /// connected device (an error if there are zero or several). Copies the device's log
    /// archive for the requested window first: expect ~5-10 s and ~300 MB of temporary
    /// disk for the default --last 5m, more for longer windows.
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "archive"
    )]
    pub device: Option<String>,

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
    /// connected device (an error if there are zero or several).
    #[arg(
        long,
        value_name = "UDID|NAME",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "predicate"
    )]
    pub device: Option<String>,
}

#[derive(Args, Debug)]
pub struct DevicesArgs {
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
    fn show_device_allows_predicate_but_not_archive() {
        assert!(Cli::try_parse_from(["slog", "show", "--device", "--predicate", "a == 1"]).is_ok());
        assert!(Cli::try_parse_from(["slog", "show", "--device", "--archive", "x"]).is_err());
    }

    #[test]
    fn predicate_conflicts_with_device() {
        assert!(
            Cli::try_parse_from(["slog", "stream", "--device", "x", "--predicate", "a == 1"])
                .is_err()
        );
    }
}
