//! iOS devices over usbmuxd (USB or Wi-Fi), via the `idevice` crate.
//!
//! Live logs come from `com.apple.os_trace_relay`, the service Console.app uses. It has no
//! predicate support, so `--filter` is applied in-process by the caller.

use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Timelike, Utc};
use idevice::IdeviceService;
use idevice::lockdown::LockdownClient;
use idevice::os_trace_relay::{LogLevel, OsTraceLog, OsTraceRelayClient};
use idevice::provider::{IdeviceProvider, UsbmuxdProvider};
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice};
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::Entries;
use crate::entry::{Entry, Level, basename};

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
