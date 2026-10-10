//! iOS devices over usbmuxd (USB or Wi-Fi), via the `idevice` crate.
//!
//! Live logs come from `com.apple.os_trace_relay`, the service Console.app uses. It has no
//! predicate support, so `--filter` is applied in-process by the caller.

use anyhow::{Context, Result, bail};
use async_stream::try_stream;
use chrono::{Local, NaiveDateTime, TimeZone, Timelike, Utc};
use idevice::IdeviceService;
use idevice::crashreportcopymobile::{CrashReportCopyMobileClient, flush_reports};
use idevice::lockdown::LockdownClient;
use idevice::os_trace_relay::{LogLevel, OsTraceLog, OsTraceRelayClient};
use idevice::provider::{IdeviceProvider, UsbmuxdProvider};
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice};
use serde::Serialize;

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

/// Connects to the device's crash report store after asking it to move pending reports
/// there (`crashreportmover`, ~2 s), so reports written moments ago are included.
async fn crash_reports(selector: &str) -> Result<(UsbmuxdDevice, CrashReportCopyMobileClient)> {
    let device = select(selector).await?;
    let p = provider(&device);
    flush_reports(&p)
        .await
        .with_context(|| format!("cannot flush crash reports on {}", device.udid))?;
    let client = CrashReportCopyMobileClient::connect(&p)
        .await
        .with_context(|| format!("cannot open crash reports on {}", device.udid))?;
    Ok((device, client))
}

/// Report files at the top of the device's crash report store. Subdirectories
/// (Retired, Assistant, ProxiedDevice-* for a paired Apple Watch) are not included.
pub async fn list_reports(selector: &str) -> Result<Vec<Report>> {
    let (device, mut client) = crash_reports(selector).await?;
    let names = client
        .ls(None)
        .await
        .with_context(|| format!("cannot list crash reports on {}", device.udid))?;
    let mut afc = client.to_afc_client();
    let mut reports = Vec::new();
    for name in names {
        let info = afc
            .get_file_info(format!("/{name}"))
            .await
            .with_context(|| format!("cannot stat {name} on {}", device.udid))?;
        if info.st_ifmt != "S_IFREG" {
            continue;
        }
        reports.push(Report {
            // AFC reports st_mtime as a UTC naive time.
            time: Utc.from_utc_datetime(&info.modified).with_timezone(&Local),
            size: info.size as u64,
            name,
            path: None,
        });
    }
    Ok(reports)
}

pub async fn pull_report(selector: &str, name: &str) -> Result<Vec<u8>> {
    if name.contains('/') {
        bail!("device report names have no '/'; use a name from `slog reports list --device`");
    }
    let (device, mut client) = crash_reports(selector).await?;
    client.pull(name).await.with_context(|| {
        format!(
            "cannot read report {name:?} on {} (see `slog reports list --device`)",
            device.udid
        )
    })
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
