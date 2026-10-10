//! Crash, hang and diagnostic reports: Console.app's "Reports" section.
//!
//! Mac reports are files in the DiagnosticReports directories; device reports come from
//! `source::device`. Both are identified by file name, which `list` prints and `show` takes.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct Report {
    /// File name; pass it to `slog reports show`.
    pub name: String,
    pub time: DateTime<Local>,
    pub size: u64,
    /// Full path on this Mac; None for device reports.
    pub path: Option<PathBuf>,
}

/// Directories Console.app lists under "Crash Reports" / "Diagnostic Reports" on a Mac.
fn mac_dirs() -> Result<Vec<PathBuf>> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(vec![
        Path::new(&home).join("Library/Logs/DiagnosticReports"),
        PathBuf::from("/Library/Logs/DiagnosticReports"),
    ])
}

/// Every report file directly in the Mac's DiagnosticReports directories, except hidden
/// files (Console.app does not list them). /Library/Logs/DiagnosticReports is readable only
/// by admins (group _analyticsusers); for other users it is skipped with a note on stderr.
pub fn list_mac() -> Result<Vec<Report>> {
    let mut reports = Vec::new();
    for dir in mac_dirs()? {
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "slog: skipping {}: permission denied (system reports need an admin account)",
                    dir.display()
                );
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("cannot list {}", dir.display())),
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("cannot list {}", dir.display()))?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            let meta = entry
                .metadata()
                .with_context(|| format!("cannot read {}", entry.path().display()))?;
            if !meta.is_file() {
                continue;
            }
            reports.push(Report {
                name: entry.file_name().to_string_lossy().into_owned(),
                time: meta.modified()?.into(),
                size: meta.len(),
                path: Some(entry.path()),
            });
        }
    }
    Ok(reports)
}

/// Resolves a name from `list` (or a path) to a Mac report file.
pub fn find_mac(name: &str) -> Result<PathBuf> {
    if name.contains('/') {
        return Ok(PathBuf::from(name));
    }
    let mut found = Vec::new();
    for dir in mac_dirs()? {
        let path = dir.join(name);
        match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => found.push(path),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }
    match found.len() {
        0 => bail!("no report named {name:?} on this Mac (see `slog reports list`)"),
        1 => Ok(found.into_iter().next().expect("one")),
        _ => bail!(
            "{name:?} exists in several directories; pass the full path: {}",
            found
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Newest first, within `since`, optionally filtered by a case-insensitive name substring.
pub fn select(
    mut reports: Vec<Report>,
    since: DateTime<Local>,
    name: Option<&str>,
    limit: usize,
) -> (Vec<Report>, usize) {
    let needle = name.map(str::to_lowercase);
    reports.retain(|r| {
        r.time >= since
            && needle
                .as_ref()
                .is_none_or(|n| r.name.to_lowercase().contains(n))
    });
    reports.sort_by(|a, b| b.time.cmp(&a.time));
    let total = reports.len();
    if limit > 0 {
        reports.truncate(limit);
    }
    (reports, total)
}

pub fn text_line(r: &Report) -> String {
    format!(
        "{}\t{}\t{}",
        r.time.format("%Y-%m-%d %H:%M:%S"),
        r.size,
        r.name
    )
}

/// The report as printed by `show`: a crash summary for crash reports (`.ips` with
/// bug_type 309) unless `full`, otherwise the file unchanged.
pub fn render(name: &str, bytes: &[u8], full: bool) -> Result<String> {
    let text = String::from_utf8_lossy(bytes).into_owned();
    if full || !name.ends_with(".ips") {
        return Ok(text);
    }
    let (header, body) = text.split_once('\n').unwrap_or((&text, ""));
    let header: Value = serde_json::from_str(header)
        .with_context(|| format!("{name}: first line is not the .ips JSON header; try --full"))?;
    if header["bug_type"] != "309" {
        return Ok(text);
    }
    let body: Value = serde_json::from_str(body)
        .with_context(|| format!("{name}: crash report body is not JSON; try --full"))?;
    crash_summary(&header, &body)
        .with_context(|| format!("{name}: unexpected crash report layout; try --full"))
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

/// Console-style summary of a crash report: identity, exception, termination, any
/// application-specific info, the last exception backtrace, and the crashed thread.
fn crash_summary(header: &Value, body: &Value) -> Result<String> {
    let mut out = String::new();
    let bundle = &body["bundleInfo"];
    let version = match (
        s(&bundle["CFBundleShortVersionString"]),
        s(&bundle["CFBundleVersion"]),
    ) {
        ("", "") => String::new(),
        (short, "") => short.to_string(),
        (short, build) => format!("{short} ({build})"),
    };
    let field = |out: &mut String, label: &str, value: &str| {
        if !value.is_empty() {
            writeln!(out, "{label:<13}{value}").expect("write to String");
        }
    };
    let process = body["procName"]
        .as_str()
        .ok_or_else(|| anyhow!("no procName"))?;
    field(
        &mut out,
        "Process:",
        &format!("{process} [{}]", body["pid"]),
    );
    field(&mut out, "Path:", s(&body["procPath"]));
    field(&mut out, "Identifier:", s(&bundle["CFBundleIdentifier"]));
    field(&mut out, "Version:", &version);
    if let Some(parent) = body["parentProc"].as_str() {
        field(
            &mut out,
            "Parent:",
            &format!("{parent} [{}]", body["parentPid"]),
        );
    }
    field(&mut out, "OS:", s(&header["os_version"]));
    field(&mut out, "Time:", s(&header["timestamp"]));
    field(&mut out, "Incident:", s(&header["incident_id"]));
    out.push('\n');

    let exc = &body["exception"];
    let signal = match s(&exc["signal"]) {
        "" => String::new(),
        sig => format!(" ({sig})"),
    };
    field(
        &mut out,
        "Exception:",
        &format!("{}{signal}", s(&exc["type"])),
    );
    field(&mut out, "Codes:", s(&exc["codes"]));
    field(&mut out, "Subtype:", s(&exc["subtype"]));
    field(&mut out, "Message:", s(&exc["message"]));
    let term = &body["termination"];
    let reasons: Vec<&str> = term["reasons"]
        .as_array()
        .map(|a| a.iter().map(s).collect())
        .unwrap_or_default();
    if term.is_object() {
        field(
            &mut out,
            "Termination:",
            format!(
                "{} {} {}",
                s(&term["namespace"]),
                term["code"],
                s(&term["indicator"])
            )
            .trim(),
        );
    }
    field(&mut out, "Reasons:", &reasons.join("; "));
    if let Some(asi) = body["asi"].as_object() {
        for (image, lines) in asi {
            let lines: Vec<&str> = lines
                .as_array()
                .map(|a| a.iter().map(s).collect())
                .unwrap_or_default();
            field(
                &mut out,
                "App info:",
                &format!("{image}: {}", lines.join(" | ")),
            );
        }
    }

    let images = body["usedImages"]
        .as_array()
        .ok_or_else(|| anyhow!("no usedImages"))?;
    if let Some(frames) = body["lastExceptionBacktrace"].as_array() {
        writeln!(out, "\nLast Exception Backtrace:")?;
        write_frames(&mut out, frames, images)?;
    }

    let threads = body["threads"]
        .as_array()
        .ok_or_else(|| anyhow!("no threads"))?;
    let faulting = body["faultingThread"]
        .as_u64()
        .ok_or_else(|| anyhow!("no faultingThread"))? as usize;
    let thread = threads
        .get(faulting)
        .ok_or_else(|| anyhow!("faultingThread {faulting} missing"))?;
    let queue = match (s(&thread["name"]), s(&thread["queue"])) {
        ("", "") => String::new(),
        (name, "") => format!(" {name}"),
        ("", queue) => format!(" Dispatch queue: {queue}"),
        (name, queue) => format!(" {name}, Dispatch queue: {queue}"),
    };
    writeln!(out, "\nThread {faulting} Crashed:{queue}")?;
    let frames = thread["frames"]
        .as_array()
        .ok_or_else(|| anyhow!("crashed thread has no frames"))?;
    write_frames(&mut out, frames, images)?;
    if threads.len() > 1 {
        writeln!(
            out,
            "\n({} other threads omitted; --full for the whole report)",
            threads.len() - 1
        )?;
    }
    Ok(out)
}

fn write_frames(out: &mut String, frames: &[Value], images: &[Value]) -> Result<()> {
    for (i, f) in frames.iter().enumerate() {
        let image = f["imageIndex"]
            .as_u64()
            .and_then(|ix| images.get(ix as usize))
            .ok_or_else(|| anyhow!("frame {i} has no valid imageIndex"))?;
        // Code outside any known binary points at a placeholder image with neither name
        // nor path; Apple prints it as "???".
        let image_name = match s(&image["name"]) {
            "" => s(&image["path"])
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or("???"),
            name => name,
        };
        let location = match s(&f["symbol"]) {
            "" => format!("{image_name} + {}", f["imageOffset"]),
            symbol => format!("{symbol} + {}", f["symbolLocation"].as_u64().unwrap_or(0)),
        };
        let source = match (s(&f["sourceFile"]), f["sourceLine"].as_u64()) {
            ("", _) => String::new(),
            (file, Some(line)) => format!(" ({file}:{line})"),
            (file, None) => format!(" ({file})"),
        };
        writeln!(out, "{i:<4}{image_name:<32} {location}{source}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn crash() -> (Value, Value) {
        let header = json!({"bug_type": "309", "os_version": "iPhone OS 27.0.1 (24A1)",
            "timestamp": "2026-10-10 08:00:00.00 -0400", "incident_id": "ABC"});
        let body = json!({
            "procName": "MyApp", "pid": 42, "procPath": "/private/var/MyApp.app/MyApp",
            "parentProc": "launchd", "parentPid": 1,
            "bundleInfo": {"CFBundleIdentifier": "com.example.MyApp",
                           "CFBundleShortVersionString": "1.2", "CFBundleVersion": "7"},
            "exception": {"type": "EXC_BREAKPOINT", "signal": "SIGTRAP", "codes": "0x1, 0x2",
                          "message": "namespc 18 reason_code 0x5"},
            "termination": {"namespace": "SIGNAL", "code": 5, "indicator": "Trace/BPT trap: 5"},
            "asi": {"libswiftCore.dylib": ["Fatal error: Index out of range"]},
            "faultingThread": 1,
            "threads": [
                {"frames": []},
                {"queue": "com.apple.main-thread", "frames": [
                    {"imageIndex": 0, "imageOffset": 100, "symbol": "Array.subscript.getter", "symbolLocation": 8},
                    {"imageIndex": 1, "imageOffset": 4096, "sourceFile": "View.swift", "sourceLine": 12},
                    {"imageIndex": 2, "imageOffset": 0}
                ]}
            ],
            "usedImages": [{"name": "libswiftCore.dylib"}, {"path": "/private/var/MyApp.app/MyApp"},
                           {"source": "A", "size": 0}]
        });
        (header, body)
    }

    #[test]
    fn crash_summary_shows_cause_and_crashed_thread() {
        let (h, b) = crash();
        let out = crash_summary(&h, &b).unwrap();
        for expected in [
            "Process:     MyApp [42]",
            "Identifier:  com.example.MyApp",
            "Version:     1.2 (7)",
            "Exception:   EXC_BREAKPOINT (SIGTRAP)",
            "Message:     namespc 18 reason_code 0x5",
            "Termination: SIGNAL 5 Trace/BPT trap: 5",
            "App info:    libswiftCore.dylib: Fatal error: Index out of range",
            "Thread 1 Crashed: Dispatch queue: com.apple.main-thread",
            "0   libswiftCore.dylib               Array.subscript.getter + 8",
            "1   MyApp                            MyApp + 4096 (View.swift:12)",
            "2   ???                              ??? + 0",
            "(1 other threads omitted",
        ] {
            assert!(out.contains(expected), "missing {expected:?} in:\n{out}");
        }
    }

    #[test]
    fn crash_summary_omits_absent_termination() {
        let (h, mut b) = crash();
        b.as_object_mut().unwrap().remove("termination");
        let out = crash_summary(&h, &b).unwrap();
        assert!(!out.contains("Termination:"), "{out}");
    }

    #[test]
    fn render_passes_through_non_crash_and_full() {
        let diag = b"Date/Time: 2026-10-10\nCommand: logd\n";
        assert_eq!(
            render("logd.diag", diag, false).unwrap(),
            String::from_utf8_lossy(diag)
        );
        let jetsam = b"{\"bug_type\":\"298\"}\n{}";
        assert_eq!(
            render("JetsamEvent.ips", jetsam, false).unwrap(),
            "{\"bug_type\":\"298\"}\n{}"
        );
        let (h, b) = crash();
        let ips = format!("{h}\n{b}");
        assert_eq!(render("MyApp.ips", ips.as_bytes(), true).unwrap(), ips);
        assert!(
            render("MyApp.ips", ips.as_bytes(), false)
                .unwrap()
                .starts_with("Process:")
        );
        assert!(render("MyApp.ips", b"not json\n{}", false).is_err());
    }

    #[test]
    fn select_filters_sorts_and_limits() {
        let at = |h| Local.with_ymd_and_hms(2026, 10, 10, h, 0, 0).unwrap();
        let r = |name: &str, h| Report {
            name: name.into(),
            time: at(h),
            size: 1,
            path: None,
        };
        let reports = vec![
            r("MyApp-a.ips", 9),
            r("Other.ips", 10),
            r("myapp-b.ips", 11),
            r("MyApp-old.ips", 1),
        ];
        let (picked, total) = select(reports, at(5), Some("MYAPP"), 1);
        assert_eq!(total, 2);
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "myapp-b.ips");
    }
}
