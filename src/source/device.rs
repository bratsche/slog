//! iOS devices over usbmuxd (USB, or Wi-Fi via Finder's setting), or over Wi-Fi through
//! slog's own pairing and tunnel (see `remote`), via the `idevice` crate.
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
use idevice::provider::{IdeviceProvider, RsdProvider, UsbmuxdProvider};
use idevice::rsd::RsdHandshake;
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection, UsbmuxdDevice};
use idevice::{Idevice, IdeviceError, IdeviceService, RsdService};
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{Entries, remote};
use crate::entry::{Entry, Level, basename};
use crate::reports::Report;

const LABEL: &str = "slog";

#[derive(Debug, Serialize)]
pub struct DeviceInfo {
    pub udid: String,
    /// "usb" or "network" (usbmuxd), or "wifi" (slog's own pairing; see `slog pair`).
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

/// usbmuxd's devices, then the devices paired with `slog pair` that are on the network.
pub async fn list() -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    let usbmux = connected().await?;
    for d in &usbmux {
        let mut info = DeviceInfo {
            udid: d.udid.clone(),
            connection: connection_name(&d.connection_type),
            name: None,
            model: None,
            os_version: None,
            error: None,
        };
        match lockdown_values(&provider(d)).await {
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
    let paired: Vec<remote::Paired> = remote::paired()?
        .into_iter()
        .filter(|p| !usbmux.iter().any(|u| u.udid.eq_ignore_ascii_case(&p.udid)))
        .collect();
    out.extend(remote::find(paired).await?.into_iter().map(|f| DeviceInfo {
        udid: f.paired.udid,
        connection: "wifi".into(),
        name: Some(f.paired.name),
        model: Some(f.paired.model),
        // Known only once connected; `devices` does not connect.
        os_version: None,
        error: None,
    }));
    Ok(out)
}

/// A selected device and the route to it.
struct Connected {
    udid: String,
    via: Via,
}

enum Via {
    Usbmux(UsbmuxdDevice),
    Remote(Box<remote::Tunnel>),
}

impl Connected {
    async fn os_trace(&mut self) -> Result<OsTraceRelayClient> {
        match &mut self.via {
            Via::Usbmux(d) => OsTraceRelayClient::connect(&provider(d)).await,
            Via::Remote(t) => {
                OsTraceRelayClient::connect_rsd(&mut t.handle, &mut t.handshake).await
            }
        }
        .with_context(|| format!("cannot start os_trace_relay on {}", self.udid))
    }

    /// The device's crash report store, after asking it to move pending reports there
    /// (`crashreportmover`, ~1-2 s), so reports written moments ago are included.
    async fn crash_store(&mut self) -> Result<AfcClient> {
        let udid = self.udid.clone();
        let client = match &mut self.via {
            Via::Usbmux(d) => {
                let p = provider(d);
                flush_reports(&p)
                    .await
                    .with_context(|| format!("cannot flush crash reports on {udid}"))?;
                CrashReportCopyMobileClient::connect(&p).await
            }
            Via::Remote(t) => {
                flush_reports_rsd(&mut t.handle, &t.handshake)
                    .await
                    .with_context(|| format!("cannot flush crash reports on {udid}"))?;
                CrashReportCopyMobileClient::connect_rsd(&mut t.handle, &mut t.handshake).await
            }
        }
        .with_context(|| format!("cannot open crash reports on {udid}"))?;
        Ok(client.to_afc_client())
    }
}

/// `flush_reports` for RSD, where the crate has none: the shim service checks in, then
/// answers "ping" once pending reports are moved.
async fn flush_reports_rsd(
    provider: &mut impl RsdProvider,
    handshake: &RsdHandshake,
) -> Result<()> {
    const SERVICE: &str = "com.apple.crashreportmover.shim.remote";
    let port = handshake
        .services
        .get(SERVICE)
        .with_context(|| format!("the device offers no {SERVICE}"))?
        .port;
    let mut idevice = Idevice::new(provider.connect_to_service_port(port).await?, LABEL);
    idevice.rsd_checkin().await?;
    let reply = idevice.read_raw(4).await?;
    if reply != b"ping" {
        bail!("crashreportmover answered {reply:02X?} instead of \"ping\"");
    }
    Ok(())
}

/// Resolves `--device` (UDID, name, or empty for "the only device"): usbmuxd's devices
/// first, as they are faster to reach; then devices paired with `slog pair`, over Wi-Fi.
async fn select(selector: &str) -> Result<Connected> {
    let devices = connected().await?;
    let unreadable = match select_usbmux(&devices, selector).await? {
        Usbmux::Found(d) => {
            return Ok(Connected {
                udid: d.udid.clone(),
                via: Via::Usbmux(d),
            });
        }
        Usbmux::NotFound { unreadable } => unreadable,
    };
    let found = select_remote(selector, &unreadable).await?;
    let udid = found.paired.udid.clone();
    Ok(Connected {
        udid,
        via: Via::Remote(Box::new(remote::connect(found).await?)),
    })
}

/// The device paired with `slog pair` that `selector` names, found on the network; an
/// empty selector means the only paired device on the network. `unreadable` lists usbmuxd
/// devices whose names could not be read, for the error when nothing matches.
async fn select_remote(selector: &str, unreadable: &[String]) -> Result<remote::Found> {
    let paired: Vec<remote::Paired> = remote::paired()?
        .into_iter()
        .filter(|p| {
            selector.is_empty()
                || p.udid.eq_ignore_ascii_case(selector)
                || p.name.eq_ignore_ascii_case(selector)
        })
        .collect();
    if paired.is_empty() {
        if selector.is_empty() {
            bail!(
                "no iOS devices connected; connect one by USB and tap Trust (run `slog pair` \
                 once while it is connected to use it over Wi-Fi later)"
            );
        }
        if !unreadable.is_empty() {
            bail!(
                "no connected device has UDID or name {selector:?}; names of some devices \
                 could not be read: {}",
                unreadable.join("; ")
            );
        }
        bail!("no connected device has UDID or name {selector:?} (see `slog devices`)");
    }
    let names: Vec<String> = paired
        .iter()
        .map(|p| format!("{} ({})", p.name, p.udid))
        .collect();
    if !selector.is_empty() && paired.len() > 1 {
        bail!(
            "several paired devices are named {selector:?}; pass the UDID instead: {}",
            names.join(", ")
        );
    }
    let mut found = remote::find(paired).await?;
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => bail!(
            "no paired device is on the network ({}); check that it is awake, unlocked and on \
             the same Wi-Fi as this Mac",
            names.join(", ")
        ),
        n => bail!(
            "{n} paired devices are on the network; pass --device UDID|NAME: {}",
            found
                .iter()
                .map(|f| format!("{} ({})", f.paired.name, f.paired.udid))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Pairs the USB-connected device `selector` names (empty: the only one) for later use
/// over Wi-Fi.
pub async fn pair(selector: &str) -> Result<remote::Paired> {
    let usb: Vec<UsbmuxdDevice> = connected()
        .await?
        .into_iter()
        .filter(|d| d.connection_type == Connection::Usb)
        .collect();
    if usb.is_empty() {
        bail!("no device connected by USB; `slog pair` needs a USB connection");
    }
    let d = match select_usbmux(&usb, selector).await? {
        Usbmux::Found(d) => d,
        Usbmux::NotFound { unreadable } if unreadable.is_empty() => {
            bail!("no USB-connected device has UDID or name {selector:?} (see `slog devices`)")
        }
        Usbmux::NotFound { unreadable } => bail!(
            "no USB-connected device has UDID or name {selector:?}; names of some devices \
             could not be read: {}",
            unreadable.join("; ")
        ),
    };
    remote::pair(&provider(&d), &d.udid).await
}

enum Usbmux {
    Found(UsbmuxdDevice),
    /// With the devices whose names could not be read.
    NotFound {
        unreadable: Vec<String>,
    },
}

/// The device among `devices` that `selector` names; an empty selector means the only one
/// (none if there are none, an error if there are several).
async fn select_usbmux(devices: &[UsbmuxdDevice], selector: &str) -> Result<Usbmux> {
    if selector.is_empty() {
        return match devices {
            [] => Ok(Usbmux::NotFound {
                unreadable: Vec::new(),
            }),
            [one] => Ok(Usbmux::Found(one.clone())),
            several => bail!(
                "{} devices connected; pass --device UDID|NAME (see `slog devices`)",
                several.len()
            ),
        };
    }
    if let Some(d) = devices
        .iter()
        .find(|d| d.udid.eq_ignore_ascii_case(selector))
    {
        return Ok(Usbmux::Found(d.clone()));
    }
    let mut by_name = Vec::new();
    let mut unreadable = Vec::new();
    for d in devices {
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
        0 => Ok(Usbmux::NotFound { unreadable }),
        1 => Ok(Usbmux::Found(by_name.remove(0))),
        _ => bail!("several devices are named {selector:?}; pass the UDID instead"),
    }
}

/// Copies the device's unified log from `start` onward into `dest`, a new .logarchive
/// directory, for `log show`. The relay sends a tar stream, piped straight into tar.
///
/// `start` matters: without it the relay sends the whole store (~3 GB, ~20 s on an
/// iPhone); with it, only the files covering that window (~300 MB, ~5 s).
pub async fn collect_archive(selector: &str, start: DateTime<Local>, dest: &Path) -> Result<()> {
    let mut device = select(selector).await?;
    let mut client = device.os_trace().await?;
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

/// The selected device and its crash report store (see `Connected::crash_store`).
async fn crash_reports(selector: &str) -> Result<(Connected, AfcClient)> {
    let mut device = select(selector).await?;
    let afc = device.crash_store().await?;
    Ok((device, afc))
}

/// Report files in the device's crash report store.
///
/// For the device itself: the top level only; subdirectories (Retired, Assistant,
/// ProxiedDevice-*) are not included. With `watch`: see `watch_dirs`. Watch reports carry
/// their path in the store, e.g. `ProxiedDevice-<id>/Retired/x.ips`, which `pull_report`
/// accepts when a name is in more than one Watch directory.
async fn stored_reports(
    afc: &mut AfcClient,
    device: &Connected,
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
async fn watch_dirs(afc: &mut AfcClient, device: &Connected) -> Result<Vec<String>> {
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
    device: &Connected,
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
async fn list_dir(afc: &mut AfcClient, device: &Connected, dir: &str) -> Result<Vec<String>> {
    let mut names = afc
        .list_dir(format!("/{dir}"))
        .await
        .with_context(|| format!("cannot list crash reports in /{dir} on {}", device.udid))?;
    names.retain(|n| n != "." && n != "..");
    Ok(names)
}

/// Whether `path` in the crash report store is a directory; false if it does not exist.
async fn is_dir(afc: &mut AfcClient, device: &Connected, path: &str) -> Result<bool> {
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
        let mut device = select(&selector).await?;
        let client = device.os_trace().await?;
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
