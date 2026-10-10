//! iOS devices over usbmuxd (USB or Wi-Fi), via the `idevice` crate.
//!
//! Live logs come from `com.apple.os_trace_relay`, the service Console.app uses. It has no
//! predicate support, so `--filter` is applied in-process by the caller.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Timelike, Utc};
use idevice::afc::AfcClient;
use idevice::afc::errors::AfcError;
use idevice::afc::opcode::AfcFopenMode;
use idevice::crashreportcopymobile::{CrashReportCopyMobileClient, flush_reports};
use idevice::lockdown::LockdownClient;
use idevice::os_trace_relay::{LogLevel, OsTraceLog, OsTraceRelayClient};
use idevice::provider::{IdeviceProvider, UsbmuxdProvider};
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice};
use idevice::{IdeviceError, IdeviceService};
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::Entries;
use crate::entry::{Entry, Level, basename};
use crate::reports::Report;

const LABEL: &str = "slog";

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub udid: String,
    /// "usb" or "network".
    pub connection: String,
    pub name: Option<String>,
    /// Hardware model, e.g. iPhone16,2.
    pub model: Option<String>,
    pub os_version: Option<String>,
    /// Why details could not be read, typically an untrusted or locked device.
    pub error: Option<String>,
}

/// Every device usbmuxd knows about, one per UDID (USB preferred over Wi-Fi).
async fn connected() -> Result<Vec<UsbmuxdDevice>> {
    let mut mux = UsbmuxdConnection::default()
        .await
        .context("cannot reach usbmuxd at /var/run/usbmuxd")?;
    let mut devices: Vec<UsbmuxdDevice> = Vec::new();
    for d in mux
        .get_devices()
        .await
        .context("usbmuxd device list failed")?
    {
        match devices.iter_mut().find(|x| x.udid == d.udid) {
            Some(existing) if d.connection_type == Connection::Usb => *existing = d,
            Some(_) => {}
            None => devices.push(d),
        }
    }
    Ok(devices)
}

fn provider(d: &UsbmuxdDevice) -> UsbmuxdProvider {
    d.to_provider(UsbmuxdAddr::default(), LABEL)
}

fn connection_name(c: &Connection) -> String {
    match c {
        Connection::Usb => "usb".into(),
        Connection::Network(_) => "network".into(),
        Connection::Unknown(s) => s.clone(),
    }
}

async fn lockdown_values(p: &UsbmuxdProvider) -> Result<plist::Dictionary> {
    let mut lockdown = LockdownClient::connect(p)
        .await
        .context("lockdown connect failed")?;
    let pairing = p
        .get_pairing_file()
        .await
        .context("no pairing record; is the device trusted?")?;
    lockdown
        .start_session(&pairing)
        .await
        .context("lockdown session failed")?;
    let values = lockdown
        .get_value(None, None)
        .await
        .context("lockdown GetValue failed")?;
    values
        .into_dictionary()
        .context("lockdown GetValue returned a non-dictionary")
}

pub async fn list() -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    for d in connected().await? {
        let mut info = DeviceInfo {
            udid: d.udid.clone(),
            connection: connection_name(&d.connection_type),
            name: None,
            model: None,
            os_version: None,
            error: None,
        };
        match lockdown_values(&provider(&d)).await {
            Ok(v) => {
                let get = |k: &str| v.get(k).and_then(|x| x.as_string()).map(str::to_string);
                info.name = get("DeviceName");
                info.model = get("ProductType");
                info.os_version = get("ProductVersion");
            }
            Err(e) => info.error = Some(format!("{e:#}")),
        }
        out.push(info);
    }
    Ok(out)
}

/// Resolves `--device` (UDID, name, or empty for "the only device").
async fn select(selector: &str) -> Result<UsbmuxdDevice> {
    let devices = connected().await?;
    if selector.is_empty() {
        return match devices.len() {
            0 => bail!("no iOS devices connected; connect one by USB and tap Trust"),
            1 => Ok(devices.into_iter().next().expect("one device")),
            n => bail!("{n} devices connected; pass --device UDID|NAME (see `slog devices`)"),
        };
    }
    if let Some(d) = devices
        .iter()
        .find(|d| d.udid.eq_ignore_ascii_case(selector))
    {
        return Ok(d.clone());
    }
    let mut by_name = Vec::new();
    let mut unreadable = Vec::new();
    for d in &devices {
        match lockdown_values(&provider(d)).await {
            Ok(v) => {
                if v.get("DeviceName")
                    .and_then(|x| x.as_string())
                    .is_some_and(|n| n.eq_ignore_ascii_case(selector))
                {
                    by_name.push(d.clone());
                }
            }
            Err(e) => unreadable.push(format!("{}: {e:#}", d.udid)),
        }
    }
    match by_name.len() {
        0 if !unreadable.is_empty() => bail!(
            "no connected device has UDID or name {selector:?}; names of some devices could not be read: {}",
            unreadable.join("; ")
        ),
        0 => bail!("no connected device has UDID or name {selector:?} (see `slog devices`)"),
        1 => Ok(by_name.remove(0)),
        _ => bail!("several devices are named {selector:?}; pass the UDID instead"),
    }
}

/// Copies the device's unified log from `start` onward into `dest`, a new .logarchive
/// directory, for `log show`. The relay sends a tar stream, piped straight into tar.
///
/// `start` matters: without it the relay sends the whole store (~3 GB, ~20 s on an
/// iPhone); with it, only the files covering that window (~300 MB, ~5 s).
pub async fn collect_archive(selector: &str, start: DateTime<Local>, dest: &Path) -> Result<()> {
    let device = select(selector).await?;
    let mut client = OsTraceRelayClient::connect(&provider(&device))
        .await
        .with_context(|| format!("cannot start os_trace_relay on {}", device.udid))?;
    eprintln!(
        "slog: copying logs since {} from {}...",
        start.format("%Y-%m-%d %H:%M:%S"),
        device.udid
    );
    let start_secs = u64::try_from(start.timestamp()).context("start time is before 1970")?;
    std::fs::create_dir_all(dest).with_context(|| format!("cannot create {}", dest.display()))?;

    let mut tar = Command::new("/usr/bin/tar")
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(dest)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Own process group: a terminal Ctrl-C reaches only slog, which then stops this
        // child itself (see `main`), instead of the child dying first and looking like a failure.
        .process_group(0)
        .spawn()
        .context("failed to run /usr/bin/tar")?;
    let mut stdin = tar.stdin.take().expect("stdin is piped");
    let mut stderr = tar.stderr.take().expect("stderr is piped");
    // stderr is read during the copy: tar prints a line per file it cannot write (e.g. disk
    // full), and once the pipe is full tar stops reading stdin, which stalls the copy.
    let mut tar_errors = String::new();
    let (copied, read) = tokio::join!(
        async move {
            let copied = client
                .create_archive(&mut stdin, None, None, Some(start_secs))
                .await;
            drop(stdin);
            copied
        },
        stderr.read_to_string(&mut tar_errors),
    );
    let status = tar.wait().await?;
    read?;
    copied.with_context(|| format!("copying the log archive from {} failed", device.udid))?;
    if !status.success() {
        bail!(
            "/usr/bin/tar could not unpack the device's log archive ({status}): {}",
            first_line_and_count(&tar_errors)
        );
    }
    Ok(())
}

/// tar prints a line per failed file (thousands when the disk fills); keep errors short.
fn first_line_and_count(text: &str) -> String {
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("(no output)").trim().to_string();
    match lines.count() {
        0 => first,
        n => format!("{first} (and {n} more lines)"),
    }
}

/// Connects to the device's crash report store after asking it to move pending reports
/// there (`crashreportmover`, ~1-2 s), so reports written moments ago are included.
async fn crash_reports(selector: &str) -> Result<(UsbmuxdDevice, AfcClient)> {
    let device = select(selector).await?;
    let p = provider(&device);
    flush_reports(&p)
        .await
        .with_context(|| format!("cannot flush crash reports on {}", device.udid))?;
    let client = CrashReportCopyMobileClient::connect(&p)
        .await
        .with_context(|| format!("cannot open crash reports on {}", device.udid))?;
    Ok((device, client.to_afc_client()))
}

/// Report files in the device's crash report store.
///
/// For the device itself: the top level only; subdirectories (Retired, Assistant,
/// ProxiedDevice-*) are not included. With `watch`: see `watch_dirs`. Watch reports carry
/// their path in the store, e.g. `ProxiedDevice-<id>/Retired/x.ips`, which `pull_report`
/// accepts when a name is in more than one Watch directory.
async fn stored_reports(
    afc: &mut AfcClient,
    device: &UsbmuxdDevice,
    watch: bool,
) -> Result<Vec<Report>> {
    let dirs = if watch {
        watch_dirs(afc, device).await?
    } else {
        vec![String::new()]
    };
    let mut reports = Vec::new();
    for dir in dirs {
        for name in report_names(afc, device, &dir, watch).await? {
            let path = store_path(&dir, &name);
            let info = afc
                .get_file_info(&path)
                .await
                .with_context(|| format!("cannot stat {path} on {}", device.udid))?;
            if info.st_ifmt != "S_IFREG" {
                continue;
            }
            reports.push(Report {
                // AFC reports st_mtime as a UTC naive time.
                time: Utc.from_utc_datetime(&info.modified).with_timezone(&Local),
                size: info.size as u64,
                name,
                path: watch.then(|| path.trim_start_matches('/').into()),
            });
        }
    }
    Ok(reports)
}

/// The directories holding paired Apple Watches' reports: `ProxiedDevice-<id>/` for each
/// Watch, and its `Retired/`, where the iPhone moves Watch reports once processed (usually
/// within hours), so the top level mostly holds the iPhone's bookkeeping files.
async fn watch_dirs(afc: &mut AfcClient, device: &UsbmuxdDevice) -> Result<Vec<String>> {
    let mut dirs = Vec::new();
    for name in list_dir(afc, device, "").await? {
        if name.starts_with("ProxiedDevice-") && is_dir(afc, device, &name).await? {
            let retired = format!("{name}/Retired");
            if is_dir(afc, device, &retired).await? {
                dirs.push(retired);
            }
            dirs.push(name);
        }
    }
    if dirs.is_empty() {
        bail!(
            "no Apple Watch is paired with {} (its crash report store has no ProxiedDevice-* folder)",
            device.udid
        );
    }
    Ok(dirs)
}

/// Candidate report names in `dir`: every entry, minus the `.proxy` bookkeeping files the
/// iPhone keeps in Watch directories.
async fn report_names(
    afc: &mut AfcClient,
    device: &UsbmuxdDevice,
    dir: &str,
    watch: bool,
) -> Result<Vec<String>> {
    let mut names = list_dir(afc, device, dir).await?;
    if watch {
        names.retain(|n| !n.ends_with(".proxy"));
    }
    Ok(names)
}

/// Absolute AFC path of `name` in `dir` ("" = top level) of the crash report store.
fn store_path(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        format!("/{name}")
    } else {
        format!("/{dir}/{name}")
    }
}

/// Entry names in `dir` of the crash report store ("" = top level), without `.` and `..`.
async fn list_dir(afc: &mut AfcClient, device: &UsbmuxdDevice, dir: &str) -> Result<Vec<String>> {
    let mut names = afc
        .list_dir(format!("/{dir}"))
        .await
        .with_context(|| format!("cannot list crash reports in /{dir} on {}", device.udid))?;
    names.retain(|n| n != "." && n != "..");
    Ok(names)
}

/// Whether `path` in the crash report store is a directory; false if it does not exist.
async fn is_dir(afc: &mut AfcClient, device: &UsbmuxdDevice, path: &str) -> Result<bool> {
    match afc.get_file_info(format!("/{path}")).await {
        Ok(info) => Ok(info.st_ifmt == "S_IFDIR"),
        Err(IdeviceError::Afc(AfcError::ObjectNotFound)) => Ok(false),
        Err(e) => Err(e).with_context(|| format!("cannot stat /{path} on {}", device.udid)),
    }
}

/// See `stored_reports`.
pub async fn list_reports(selector: &str, watch: bool) -> Result<Vec<Report>> {
    let (device, mut afc) = crash_reports(selector).await?;
    stored_reports(&mut afc, &device, watch).await
}

/// `name` is a file name from `slog reports list`; with `watch`, also a Watch report's path
/// in the store (`ProxiedDevice-<id>/[Retired/]name`).
pub async fn pull_report(selector: &str, name: &str, watch: bool) -> Result<Vec<u8>> {
    let list = if watch {
        "slog reports list --watch"
    } else {
        "slog reports list --device"
    };
    if name.contains('/') && !(watch && is_watch_path(name)) {
        bail!("{name:?} is not a report name; use a name from `{list}`");
    }
    let (device, mut afc) = crash_reports(selector).await?;
    let path = if !watch || name.contains('/') {
        store_path("", name)
    } else {
        let mut found = Vec::new();
        for dir in watch_dirs(&mut afc, &device).await? {
            if report_names(&mut afc, &device, &dir, true)
                .await?
                .iter()
                .any(|n| n == name)
            {
                found.push(format!("{dir}/{name}"));
            }
        }
        match found.as_slice() {
            [one] => store_path("", one),
            [] => bail!(
                "no Apple Watch report {name:?} on {} (see `{list}`)",
                device.udid
            ),
            several => bail!(
                "{name:?} is in several Watch report folders; pass one of these paths instead: {}",
                several.join(", ")
            ),
        }
    };
    let mut file = afc
        .open(&path, AfcFopenMode::RdOnly)
        .await
        .with_context(|| {
            format!(
                "cannot read report {:?} on {} (see `{list}`)",
                path.trim_start_matches('/'),
                device.udid
            )
        })?;
    let data = file.read_entire().await;
    // Closed even if the read failed, so the device-side handle is not leaked.
    file.close().await?;
    data.with_context(|| format!("cannot read report {path} on {}", device.udid))
}

/// `ProxiedDevice-<id>/name` or `ProxiedDevice-<id>/Retired/name`, as `list --watch --json`
/// prints in `path`.
fn is_watch_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let file_ok = |f: &str| !f.is_empty() && f != "." && f != "..";
    match parts.as_slice() {
        [watch, file] => watch.starts_with("ProxiedDevice-") && file_ok(file),
        [watch, "Retired", file] => watch.starts_with("ProxiedDevice-") && file_ok(file),
        _ => false,
    }
}

pub fn stream(selector: String, include_info: bool, include_debug: bool) -> Entries {
    Box::pin(try_stream! {
        let device = select(&selector).await?;
        let client = OsTraceRelayClient::connect(&provider(&device))
            .await
            .with_context(|| format!("cannot start os_trace_relay on {}", device.udid))?;
        let mut relay = client.start_trace(None).await.context("os_trace_relay StartActivity failed")?;
        loop {
            let log = relay.next().await.context("os_trace_relay stream ended")?;
            let entry = to_entry(log)?;
            let wanted = match entry.level {
                Level::Info => include_info,
                Level::Debug => include_debug,
                _ => true,
            };
            if wanted {
                yield entry;
            }
        }
    })
}

fn to_entry(log: OsTraceLog) -> Result<Entry> {
    let (subsystem, category) = match log.label {
        Some(l) => (l.subsystem, l.category),
        None => (String::new(), String::new()),
    };
    Ok(Entry {
        time: device_time(log.timestamp)?,
        level: match log.level {
            LogLevel::Notice => Level::Default,
            LogLevel::Info => Level::Info,
            LogLevel::Debug => Level::Debug,
            LogLevel::Error => Level::Error,
            LogLevel::Fault => Level::Fault,
        },
        process: basename(&log.filename),
        pid: log.pid,
        subsystem,
        category,
        library: basename(&log.image_name),
        thread: log.thread_id,
        message: log.message,
    })
}

/// idevice 0.1.68 builds the timestamp as UTC with the wire's microseconds passed as
/// nanoseconds; scale them back. See docs/BUG_REFERENCE.md.
fn device_time(ts: NaiveDateTime) -> Result<chrono::DateTime<Local>> {
    let micros = ts.nanosecond();
    if micros >= 1_000_000 {
        bail!("device log timestamp has out-of-range microseconds: {micros}");
    }
    let fixed = ts.with_nanosecond(micros * 1000).expect("below one second");
    Ok(Utc.from_utc_datetime(&fixed).with_timezone(&Local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_paths() {
        assert!(is_watch_path("ProxiedDevice-8cbc/Retired/x.ips"));
        assert!(is_watch_path("ProxiedDevice-8cbc/ota_patch.txt"));
        for bad in [
            "x.ips",
            "Retired/x.ips",
            "ProxiedDevice-8cbc/DiagnosticLogs/x.txt",
            "ProxiedDevice-8cbc/..",
            "ProxiedDevice-8cbc/Retired/",
            "/ProxiedDevice-8cbc/x.ips",
        ] {
            assert!(!is_watch_path(bad), "{bad}");
        }
        assert_eq!(store_path("", "a.ips"), "/a.ips");
        assert_eq!(
            store_path("ProxiedDevice-1/Retired", "a.ips"),
            "/ProxiedDevice-1/Retired/a.ips"
        );
    }
    use chrono::DateTime;

    #[test]
    fn tar_errors_are_shortened() {
        assert_eq!(
            first_line_and_count("tar: a: No space left\n"),
            "tar: a: No space left"
        );
        assert_eq!(
            first_line_and_count("tar: a: No space\ntar: b: No space\n\ntar: c: No space\n"),
            "tar: a: No space (and 2 more lines)"
        );
        assert_eq!(first_line_and_count(""), "(no output)");
    }

    #[test]
    fn device_time_restores_microseconds() {
        // What idevice produces for seconds=1_760_000_000, microseconds=123_456.
        let raw = DateTime::from_timestamp(1_760_000_000, 123_456)
            .unwrap()
            .naive_utc();
        let t = device_time(raw).unwrap();
        assert_eq!(t.timestamp(), 1_760_000_000);
        assert_eq!(t.timestamp_subsec_micros(), 123_456);
    }
}
