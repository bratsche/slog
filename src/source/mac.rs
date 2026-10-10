//! This Mac and .logarchive files, read through `/usr/bin/log --style ndjson`.
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

use super::Entries;
use crate::entry::{Entry, Level, basename};

const LOG: &str = "/usr/bin/log";

/// Which message levels to include beyond default/error/fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verbosity {
    Default,
    Info,
    Debug,
}

pub struct ShowQuery {
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
    Ok(run(args, archive))
}

pub fn stream(verbosity: Verbosity, predicate: Option<String>) -> Entries {
    let mut args = vec!["stream".to_string(), "--style".into(), "ndjson".into()];
    match verbosity {
        Verbosity::Default => {}
        Verbosity::Info => args.extend(["--level".into(), "info".into()]),
        Verbosity::Debug => args.extend(["--level".into(), "debug".into()]),
    }
    if let Some(p) = predicate {
        args.extend(["--predicate".into(), p]);
    }
    run(args, None)
}

/// Runs `log` and yields its entries. `archive` is the archive path `log show` was given,
/// if any: after the end-of-output trailer it prints a `==========` / path / `==========`
/// block, which is skipped; anything else after the trailer is passed on to stderr.
fn run(args: Vec<String>, archive: Option<String>) -> Entries {
    Box::pin(try_stream! {
        let mut child = Command::new(LOG)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to run {LOG}"))?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr_task = tokio::spawn(async move {
            let mut s = String::new();
            stderr.read_to_string(&mut s).await.map(|_| s)
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut finished = false;
        while let Some(line) = lines.next_line().await? {
            if finished {
                if !(line.is_empty() || line == "==========" || Some(&line) == archive.as_ref()) {
                    eprintln!("slog: {LOG}: {line}");
                }
                continue;
            }
            finished = is_trailer(&line);
            match parse_line(&line)? {
                Some(entry) if !is_slog_invocation(&entry) => yield entry,
                _ => {}
            }
        }

        let status = child.wait().await?;
        let stderr = stderr_task.await??;
        if !status.success() {
            Err(anyhow!("{LOG} {} failed ({status}): {}", args[0], stderr.trim()))?;
        }
    })
}

/// `{"count":N,"finished":1}`, the last ndjson line of `log show`.
fn is_trailer(line: &str) -> bool {
    line.starts_with("{\"count\":") && line.ends_with(",\"finished\":1}")
}

/// `log` records each invocation, predicate included, in the unified log. Without this,
/// every search would match the entries left by this and earlier slog runs.
fn is_slog_invocation(e: &Entry) -> bool {
    e.process == "log"
        && e.subsystem == "com.apple.log"
        && e.message.starts_with("log run noninteractively, parent: ")
        && e.message.contains(" (slog), args: ")
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
        assert!(is_slog_invocation(&e));
        e.message =
            "log run noninteractively, parent: 51004 (zsh), args: '/usr/bin/log' 'show'".into();
        assert!(!is_slog_invocation(&e));
    }
}
