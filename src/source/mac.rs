//! This Mac, .logarchive files and simulators, read through `/usr/bin/log --style ndjson`.
//!
//! The absolute path matters: zsh has a `log` builtin that shadows the binary.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow};
use async_stream::try_stream;
use chrono::{DateTime, Local};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::{Entries, simulator};
use crate::entry::{Entry, Level, basename};

const LOG: &str = "/usr/bin/log";

/// Which message levels to include beyond default/error/fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verbosity {
    Default,
    Info,
    Debug,
}

/// Whose unified log `log` reads.
#[derive(Clone, Debug)]
pub enum Target {
    Mac,
    /// A booted simulator's UDID; `log` runs inside it via `simctl spawn`.
    Simulator(String),
}

pub struct ShowQuery {
    pub target: Target,
    pub archive: Option<PathBuf>,
    pub last: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub verbosity: Verbosity,
    pub predicate: Option<String>,
}

pub fn show(q: ShowQuery) -> Result<Entries> {
    let mut args = vec!["show".to_string(), "--style".into(), "ndjson".into()];
    match q.verbosity {
        Verbosity::Default => {}
        Verbosity::Info => args.push("--info".into()),
        Verbosity::Debug => args.extend(["--info".into(), "--debug".into()]),
    }
    for (flag, value) in [
        ("--last", q.last),
        ("--start", q.start),
        ("--end", q.end),
        ("--predicate", q.predicate),
    ] {
        if let Some(v) = value {
            args.extend([flag.into(), v]);
        }
    }
    // Canonical, because `log` echoes the resolved path (e.g. /private/var/...) after the
    // trailer and `run` recognizes it by exact match.
    let archive = match q.archive {
        Some(a) => Some(
            std::fs::canonicalize(&a)
                .with_context(|| format!("cannot open archive {}", a.display()))?
                .to_string_lossy()
                .into_owned(),
        ),
        None => None,
    };
    if let Some(a) = &archive {
        args.push(a.clone());
    }
    Ok(run(q.target, args, archive))
}

pub fn stream(target: Target, verbosity: Verbosity, predicate: Option<String>) -> Entries {
    let mut args = vec!["stream".to_string(), "--style".into(), "ndjson".into()];
    match verbosity {
        Verbosity::Default => {}
        Verbosity::Info => args.extend(["--level".into(), "info".into()]),
        Verbosity::Debug => args.extend(["--level".into(), "debug".into()]),
    }
    if let Some(p) = predicate {
        args.extend(["--predicate".into(), p]);
    }
    run(target, args, None)
}

/// Runs `log` and yields its entries. `archive` is the archive path `log show` was given,
/// if any: at the end it prints a `==========` / path / `==========` block, which is
/// skipped; anything else after the end-of-output trailer is passed on to stderr.
fn run(target: Target, args: Vec<String>, archive: Option<String>) -> Entries {
    Box::pin(try_stream! {
        let (mut cmd, name) = match &target {
            Target::Mac => (Command::new(LOG), LOG.to_string()),
            Target::Simulator(udid) => {
                (simulator::log_command(udid), format!("log in simulator {udid}"))
            }
        };
        let mut child = Terminating(
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                // Own process group: a terminal Ctrl-C reaches only slog, which then stops this
                // child itself (see `main`), instead of the child dying first and looking like
                // a failure.
                .process_group(0)
                .spawn()
                .with_context(|| format!("failed to run {name}"))?,
        );
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr_task = tokio::spawn(async move {
            let mut s = String::new();
            stderr.read_to_string(&mut s).await.map(|_| s)
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut finished = false;
        while let Some(line) = lines.next_line().await? {
            // Also printed with no trailer when `log` cannot read the archive; the exit
            // status and stderr below then report why.
            if archive.is_some() && (line == "==========" || Some(&line) == archive.as_ref()) {
                continue;
            }
            if finished {
                if !line.is_empty() {
                    eprintln!("slog: {name}: {line}");
                }
                continue;
            }
            finished = is_trailer(&line);
            match parse_line(&line)? {
                Some(entry) if !is_slog_invocation(&target, &entry) => yield entry,
                _ => {}
            }
        }

        let status = child.wait().await?;
        let stderr = stderr_task.await??;
        if !status.success() {
            Err(anyhow!("{name} {} failed ({status}): {}", args[0], useful_stderr(&stderr)))?;
        }
    })
}

/// A child sent SIGTERM, not tokio's SIGKILL, if dropped while running (an interrupted or
/// bounded run). `simctl spawn` passes SIGTERM on to the `log` it started in the simulator;
/// SIGKILL would leave that `log` running.
struct Terminating(tokio::process::Child);

impl std::ops::Deref for Terminating {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Terminating {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for Terminating {
    fn drop(&mut self) {
        if let (Ok(None), Some(pid)) = (self.0.try_wait(), self.0.id()) {
            // SAFETY: sending a signal has no memory safety implications; `pid` is our
            // unreaped child, so it cannot have been reused by another process.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

/// `log` in a simulator always starts with a warning about the Mac user unknown inside it.
fn useful_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .filter(|l| !l.starts_with("getpwuid_r did not find a match for uid "))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// `{"count":N,"finished":1}`, the last ndjson line of `log show`.
fn is_trailer(line: &str) -> bool {
    line.starts_with("{\"count\":") && line.ends_with(",\"finished\":1}")
}

/// `log` records each invocation, predicate included, in the unified log. Without this,
/// every search would match the entries left by this and earlier slog runs. In a simulator
/// the parent is always `launchd_sim`, so every `simctl spawn ... log` run is dropped, as
/// are `launchd_sim`'s own ~15 entries about starting and reaping each one.
fn is_slog_invocation(target: &Target, e: &Entry) -> bool {
    let parent = match target {
        Target::Mac => " (slog), args: ",
        Target::Simulator(_) => {
            if e.process == "launchd_sim" && is_log_spawn_record(e) {
                return true;
            }
            " (launchd_sim), args: "
        }
    };
    e.process == "log"
        && e.subsystem == "com.apple.log"
        && e.message.starts_with("log run noninteractively, parent: ")
        && e.message.contains(parent)
}

/// `launchd_sim` logs a `simctl spawn ... log` run as the one-shot job
/// `com.apple.xpc.launchd.oneshot.0x<id>.log` (in the subsystem, or in the message once the
/// job is being removed) and as the process `pid/<pid> [log]`.
fn is_log_spawn_record(e: &Entry) -> bool {
    const ONESHOT: &str = "com.apple.xpc.launchd.oneshot.0x";
    let names_log_job = |s: &str| {
        s.match_indices(ONESHOT).any(|(i, _)| {
            let rest = s[i + ONESHOT.len()..].trim_start_matches(|c: char| c.is_ascii_hexdigit());
            rest.strip_prefix(".log")
                .is_some_and(|r| r.is_empty() || r.starts_with([' ', ']']))
        })
    };
    (e.subsystem.starts_with("pid/") && e.subsystem.ends_with(" [log]"))
        || names_log_job(&e.subsystem)
        || names_log_job(&e.message)
}

#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "eventType")]
    event_type: Option<String>,
    #[serde(rename = "messageType")]
    message_type: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "processImagePath")]
    process_image_path: Option<String>,
    #[serde(rename = "processID")]
    process_id: Option<u32>,
    subsystem: Option<String>,
    category: Option<String>,
    #[serde(rename = "senderImagePath")]
    sender_image_path: Option<String>,
    #[serde(rename = "threadID")]
    thread_id: Option<u64>,
    #[serde(rename = "eventMessage")]
    event_message: Option<String>,
}

/// Parses one ndjson line. Returns `None` for lines that are not log messages:
/// activity/signpost/state events, the `{"count":N,"finished":1}` trailer, and the
/// "Filtering the log data using ..." banner `log stream` prints when given a predicate.
fn parse_line(line: &str) -> Result<Option<Entry>> {
    if line.is_empty() || line.starts_with("Filtering the log data using ") {
        return Ok(None);
    }
    let raw: RawEvent = serde_json::from_str(line)
        .with_context(|| format!("unexpected output from {LOG}: {line}"))?;
    if raw.event_type.as_deref() != Some("logEvent") {
        return Ok(None);
    }
    let missing = |field: &str| anyhow!("{LOG} logEvent without {field}: {line}");

    let message_type = raw.message_type.ok_or_else(|| missing("messageType"))?;
    let level = Level::parse(&message_type)
        .ok_or_else(|| anyhow!("unknown messageType {message_type:?}"))?;
    let timestamp = raw.timestamp.ok_or_else(|| missing("timestamp"))?;
    let time = DateTime::parse_from_str(&timestamp, "%Y-%m-%d %H:%M:%S%.f%z")
        .with_context(|| format!("bad timestamp {timestamp:?}"))?
        .with_timezone(&Local);
    let process = basename(
        &raw.process_image_path
            .ok_or_else(|| missing("processImagePath"))?,
    );
    let pid = raw.process_id.ok_or_else(|| missing("processID"))?;
    let message = raw.event_message.ok_or_else(|| missing("eventMessage"))?;
    let library = basename(
        &raw.sender_image_path
            .ok_or_else(|| missing("senderImagePath"))?,
    );
    let thread = raw.thread_id.ok_or_else(|| missing("threadID"))?;

    Ok(Some(Entry {
        time,
        level,
        process,
        pid,
        subsystem: raw.subsystem.unwrap_or_default(),
        category: raw.category.unwrap_or_default(),
        library,
        thread,
        message,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{"timezoneName":"","messageType":"Info","eventType":"logEvent","source":null,"formatString":"x %{public}@","userID":0,"activityIdentifier":0,"subsystem":"com.apple.bluetooth","category":"Server.GATT","threadID":140148949,"processImagePath":"\/usr\/sbin\/bluetoothd","senderImagePath":"\/usr\/lib\/libBT.dylib","timestamp":"2026-10-09 23:29:00.007769-0400","eventMessage":"Dispatching indication","processID":642}"#;

    #[test]
    fn parses_log_event() {
        let e = parse_line(SAMPLE).unwrap().unwrap();
        assert_eq!(e.level, Level::Info);
        assert_eq!(e.process, "bluetoothd");
        assert_eq!(e.pid, 642);
        assert_eq!(e.library, "libBT.dylib");
        assert_eq!(e.subsystem, "com.apple.bluetooth");
        assert_eq!(e.category, "Server.GATT");
        assert_eq!(e.thread, 140148949);
        assert_eq!(e.message, "Dispatching indication");
        assert_eq!(e.time.timestamp_subsec_micros(), 7769);
    }

    #[test]
    fn skips_non_messages() {
        assert!(
            parse_line(r#"{"count":2979,"finished":1}"#)
                .unwrap()
                .is_none()
        );
        assert!(is_trailer(r#"{"count":2979,"finished":1}"#));
        assert!(!is_trailer(SAMPLE));
        assert!(
            parse_line(r#"{"eventType":"activityCreateEvent"}"#)
                .unwrap()
                .is_none()
        );
        assert!(
            parse_line(r#"Filtering the log data using "process == "x"""#)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_garbage_and_incomplete_events() {
        assert!(parse_line("not json").is_err());
        assert!(parse_line(r#"{"eventType":"logEvent","messageType":"Default"}"#).is_err());
        assert!(parse_line(&SAMPLE.replace("\"Info\"", "\"Loud\"")).is_err());
        assert!(parse_line(&SAMPLE.replace("\"threadID\"", "\"x\"")).is_err());
    }

    #[test]
    fn recognizes_slog_invocations_only() {
        let mut e = parse_line(SAMPLE).unwrap().unwrap();
        e.process = "log".into();
        e.subsystem = "com.apple.log".into();
        e.message =
            "log run noninteractively, parent: 51004 (slog), args: '/usr/bin/log' 'show'".into();
        assert!(is_slog_invocation(&Target::Mac, &e));
        e.message =
            "log run noninteractively, parent: 51004 (zsh), args: '/usr/bin/log' 'show'".into();
        assert!(!is_slog_invocation(&Target::Mac, &e));
        e.message =
            "log run noninteractively, parent: 14786 (launchd_sim), args: 'log' 'show'".into();
        let sim = Target::Simulator("X".into());
        assert!(is_slog_invocation(&sim, &e));
        assert!(!is_slog_invocation(&Target::Mac, &e));
    }

    #[test]
    fn recognizes_launchd_sim_records_of_log_runs_only() {
        let mut e = parse_line(SAMPLE).unwrap().unwrap();
        e.process = "launchd_sim".into();
        let sim = Target::Simulator("X".into());
        for (subsystem, message) in [
            (
                "user/501/com.apple.xpc.launchd.oneshot.0x10000027.log [31242]",
                "service state: running",
            ),
            (
                "user/501/com.apple.xpc.launchd.oneshot.0x10000028.log",
                "launching: one-shot",
            ),
            ("pid/31242 [log]", "created"),
            (
                "user/501",
                "removing service: com.apple.xpc.launchd.oneshot.0x10000027.log",
            ),
        ] {
            e.subsystem = subsystem.into();
            e.message = message.into();
            assert!(is_slog_invocation(&sim, &e), "{subsystem} {message}");
            assert!(
                !is_slog_invocation(&Target::Mac, &e),
                "{subsystem} {message}"
            );
        }
        for (subsystem, message) in [
            (
                "user/501/com.apple.xpc.launchd.oneshot.0x10000029.logger [1]",
                "service state: running",
            ),
            (
                "user/501/com.apple.xpc.launchd.oneshot.0x1000002a.MyApp [2]",
                "created",
            ),
            ("pid/31242 [MyApp]", "created"),
            ("user/501", "removing service: com.apple.example.log"),
        ] {
            e.subsystem = subsystem.into();
            e.message = message.into();
            assert!(!is_slog_invocation(&sim, &e), "{subsystem} {message}");
        }
    }

    #[test]
    fn drops_simulator_user_warning_from_errors() {
        let stderr = "getpwuid_r did not find a match for uid 501\nlog: Bad predicate\n";
        assert_eq!(useful_stderr(stderr), "log: Bad predicate");
    }
}
