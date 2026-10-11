//! iPhones and iPads over Wi-Fi without Finder's "Show this iPhone when on Wi-Fi", through
//! slog's own remote pairing, the protocol Xcode uses for wireless devices.
//!
//! `slog pair` creates the pairing once, over USB (`remotepairingdeviced`; a USB connection
//! counts as consent, so the device shows no prompt) and stores it in `store_dir()`. Over
//! Wi-Fi, the device advertises `_remotepairing._tcp` over Bonjour with an `authTag` that
//! only a paired host can check. slog verifies the pairing there, asks the device for a
//! tunnel (TLS with the pairing's key, carrying IPv6 packets), and runs TCP over it in
//! process, so no root or network interface is needed. The tunnel handshake names the
//! device's RemoteServiceDiscovery (RSD) port; RSD lists every other service.

use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use idevice::IdeviceError;
use idevice::IdeviceService;
use idevice::provider::{RsdProvider, UsbmuxdProvider};
use idevice::remote_pairing::errors::RemotePairingError;
use idevice::remote_pairing::{
    PeerDevice, RemotePairingClient, RemotePairingLockdownService, RpPairingFile, RpPairingSocket,
    connect_tls_psk_tunnel_native,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::adapter::Adapter;
use idevice::tcp::handle::AdapterHandle;
use tokio::net::TcpStream;

use super::dnssd;

/// How slog names itself to the device.
const HOST_NAME: &str = "slog";
const SERVICE: &str = "_remotepairing._tcp";
/// The PIN a USB pairing uses: the cable is the consent.
const USB_PIN: &str = "000000";
const BROWSE_FOR: Duration = Duration::from_secs(3);
/// Bonjour answers arrive within milliseconds of each other; stop this long after the last.
const BROWSE_SETTLE: Duration = Duration::from_millis(300);
const RESOLVE_FOR: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Pair-verify, tunnel and RSD take ~0.5 s; a device that locks or leaves the network
/// mid-way would otherwise hang the command.
const SETUP_TIMEOUT: Duration = Duration::from_secs(20);

/// A device slog has paired with.
pub struct Paired {
    pub udid: String,
    pub name: String,
    pub model: String,
    pub path: PathBuf,
    pairing: RpPairingFile,
}

/// A device slog found on the network, matched to its pairing.
pub struct Found {
    pub paired: Paired,
    host: String,
    port: u16,
    interface: u32,
}

/// An open tunnel: TCP to the device's services goes through `handle`.
pub struct Tunnel {
    pub handle: AdapterHandle,
    pub handshake: RsdHandshake,
    /// The pairing connection the tunnel was requested on; kept open for the tunnel's
    /// lifetime, as Xcode does.
    _control: RemotePairingClient<RpPairingSocket<TcpStream>>,
}

/// `~/Library/Application Support/slog/pairing`, one `<UDID>.plist` per device.
pub fn store_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        bail!("HOME is not an absolute path");
    }
    Ok(home.join("Library/Application Support/slog/pairing"))
}

/// Seeds the pairing identifier the device stores slog under: per Mac and user, so pairing
/// from another Mac or account adds a pairing instead of replacing this one.
fn pairing_seed() -> Result<String> {
    let mut uuid = [0u8; 16];
    let wait = libc::timespec {
        tv_sec: 5,
        tv_nsec: 0,
    };
    // SAFETY: `uuid` is the 16 writable bytes gethostuuid expects; `wait` is valid.
    if unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &wait) } != 0 {
        return Err(std::io::Error::last_os_error()).context("cannot read this Mac's host UUID");
    }
    let hex: String = uuid.iter().map(|b| format!("{b:02x}")).collect();
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    Ok(format!("{HOST_NAME}-{hex}-{uid}"))
}

/// Pairs with the USB-connected device behind `provider` and stores the pairing,
/// replacing an earlier one.
pub async fn pair(provider: &UsbmuxdProvider, udid: &str) -> Result<Paired> {
    let service = RemotePairingLockdownService::connect(provider)
        .await
        .with_context(|| format!("cannot start remotepairingdeviced on {udid}"))?;
    let mut client = service.into_client(HOST_NAME)?;
    let mut pairing = RpPairingFile::generate(&pairing_seed()?);
    // A fresh pairing never verifies, so this always goes on to pair.
    client
        .connect(&mut pairing, || async { USB_PIN.to_string() })
        .await
        .with_context(|| format!("pairing with {udid} failed"))?;
    let peer = client.paired_peer_device()?;
    if !peer.remotepairing_udid.eq_ignore_ascii_case(udid) {
        bail!(
            "{udid} paired as {}; refusing to store a pairing under the wrong UDID",
            peer.remotepairing_udid
        );
    }
    let paired = Paired {
        udid: udid.to_string(),
        name: peer.name.clone(),
        model: peer.model.clone(),
        path: store_dir()?.join(format!("{udid}.plist")),
        pairing,
    };
    save(&paired)?;
    Ok(paired)
}

/// The stored pairing: a plist with name, model and the pairing file's own plist. Written
/// 0600 in a 0700 directory, as the pairing's private key lets its holder use the device's
/// developer services.
fn save(p: &Paired) -> Result<()> {
    let dir = p.path.parent().expect("store path has a parent");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("cannot create {}", dir.display()))?;
    // The mode above only applies to directories it creates.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("cannot restrict {}", dir.display()))?;
    let mut dict = plist::Dictionary::new();
    dict.insert("name".into(), p.name.clone().into());
    dict.insert("model".into(), p.model.clone().into());
    dict.insert("pairing".into(), plist::Value::Data(p.pairing.to_bytes()));
    let mut bytes = Vec::new();
    plist::Value::Dictionary(dict).to_writer_xml(&mut bytes)?;
    // Written next to the target, then renamed over it, so a failed write never leaves a
    // truncated pairing behind; removed if anything fails.
    let tmp = p.path.with_extension("plist.tmp");
    let written = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &p.path)?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("cannot write {}", p.path.display()));
    }
    Ok(())
}

fn load(path: &Path) -> Result<Paired> {
    let value = plist::Value::from_file(path)?;
    let dict = value.as_dictionary().context("not a dictionary")?;
    let text = |k: &str| -> Result<String> {
        Ok(dict
            .get(k)
            .and_then(|v| v.as_string())
            .with_context(|| format!("missing {k}"))?
            .to_string())
    };
    let data = dict
        .get("pairing")
        .and_then(|v| v.as_data())
        .context("missing pairing")?;
    let udid = path
        .file_stem()
        .and_then(|s| s.to_str())
        .context("file name is not a UDID")?
        .to_string();
    Ok(Paired {
        udid,
        name: text("name")?,
        model: text("model")?,
        path: path.to_path_buf(),
        pairing: RpPairingFile::from_bytes(data)?,
    })
}

/// Every stored pairing. A missing store means none; an unreadable file is skipped with a
/// note on stderr, so it cannot block the other devices.
pub fn paired() -> Result<Vec<Paired>> {
    let dir = store_dir()?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "plist") {
            match load(&path) {
                Ok(p) => out.push(p),
                Err(e) => eprintln!(
                    "slog: skipping unreadable pairing {} ({e:#}); pair again with `slog pair`",
                    path.display()
                ),
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The paired devices advertising themselves on the network now.
pub async fn find(paired: Vec<Paired>) -> Result<Vec<Found>> {
    if paired.is_empty() {
        return Ok(Vec::new());
    }
    tokio::task::spawn_blocking(move || find_blocking(paired)).await?
}

fn find_blocking(paired: Vec<Paired>) -> Result<Vec<Found>> {
    let instances = dnssd::browse(SERVICE, BROWSE_FOR, BROWSE_SETTLE)
        .context("Bonjour browse for _remotepairing._tcp failed")?;
    let mut remaining: Vec<Paired> = paired;
    let mut found = Vec::new();
    for instance in instances {
        if remaining.is_empty() {
            break;
        }
        let Ok(r) = dnssd::resolve(&instance, SERVICE, RESOLVE_FOR) else {
            continue;
        };
        let identifier = dnssd::txt_value(&r.txt, "identifier").map(String::from_utf8_lossy);
        let tag = dnssd::txt_value(&r.txt, "authTag").map(String::from_utf8_lossy);
        let (Some(identifier), Some(tag)) = (identifier, tag) else {
            continue;
        };
        let matched = remaining.iter().position(|p| {
            p.pairing
                .alt_irk()
                .is_some_and(|irk| PeerDevice::validate_auth_tag(irk, &identifier, &tag))
        });
        if let Some(i) = matched {
            found.push(Found {
                paired: remaining.remove(i),
                host: r.host,
                port: r.port,
                interface: instance.interface,
            });
        }
    }
    Ok(found)
}

/// Verifies the pairing with a found device and opens a tunnel to it (~0.5 s).
pub async fn connect(found: Found) -> Result<Tunnel> {
    let udid = found.paired.udid.clone();
    tokio::time::timeout(SETUP_TIMEOUT, connect_inner(found))
        .await
        .map_err(|_| {
            anyhow!(
                "{udid} did not finish setting up a Wi-Fi connection within {}s; check that it \
                 is awake, unlocked and on the same Wi-Fi as this Mac",
                SETUP_TIMEOUT.as_secs()
            )
        })?
}

async fn connect_inner(found: Found) -> Result<Tunnel> {
    let Found {
        mut paired,
        host,
        port,
        interface,
    } = found;
    let udid = paired.udid.clone();
    let addrs =
        tokio::task::spawn_blocking(move || dnssd::addresses(&host, interface, RESOLVE_FOR))
            .await??;
    let (socket, ip) = connect_any(&addrs, port)
        .await
        .with_context(|| format!("cannot reach {udid} over Wi-Fi"))?;
    let mut client = RemotePairingClient::new(RpPairingSocket::new(socket), HOST_NAME);
    client.attempt_pair_verify().await?;
    // No fallback to pairing anew: that needs USB.
    match client.validate_pairing(&mut paired.pairing).await {
        Ok(()) => {}
        Err(IdeviceError::RemotePairing(RemotePairingError::PairVerifyFailed)) => bail!(
            "{udid} no longer accepts slog's pairing; connect it by USB and run `slog pair` again"
        ),
        Err(e) => return Err(e).with_context(|| format!("pair-verify with {udid} failed")),
    }
    let tunnel_port = client
        .create_tcp_listener()
        .await
        .with_context(|| format!("{udid} refused to open a tunnel"))?;
    let (tunnel_socket, _) = connect_any(&[ip], tunnel_port)
        .await
        .with_context(|| format!("cannot reach {udid}'s tunnel"))?;
    let tunnel = connect_tls_psk_tunnel_native(tunnel_socket, client.encryption_key())
        .await
        .with_context(|| format!("tunnel handshake with {udid} failed"))?;
    let info = tunnel.info.clone();
    let mut adapter = Adapter::new(
        Box::new(tunnel.into_inner()),
        info.client_address.parse()?,
        info.server_address.parse()?,
    );
    // The tunnel's MTU minus IPv6 (40) and TCP (20) headers.
    adapter.set_mss((info.mtu as usize).saturating_sub(60));
    let mut handle = adapter.to_async_handle();
    let rsd = handle
        .connect_to_service_port(info.server_rsd_port)
        .await
        .with_context(|| format!("cannot reach RemoteServiceDiscovery on {udid}"))?;
    let handshake = RsdHandshake::new(rsd)
        .await
        .with_context(|| format!("RemoteServiceDiscovery handshake with {udid} failed"))?;
    Ok(Tunnel {
        handle,
        handshake,
        _control: client,
    })
}

/// Connects to the first of `addrs` that answers on `port`.
async fn connect_any(addrs: &[SocketAddr], port: u16) -> Result<(TcpStream, SocketAddr)> {
    let mut errors = Vec::new();
    for addr in addrs {
        let mut addr = *addr;
        addr.set_port(port);
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(s)) => return Ok((s, addr)),
            Ok(Err(e)) if e.raw_os_error() == Some(libc::EHOSTUNREACH) => errors.push(format!(
                "{addr}: {e} (if the device is on this network, allow your terminal app in \
                 System Settings > Privacy & Security > Local Network)"
            )),
            Ok(Err(e)) => errors.push(format!("{addr}: {e}")),
            Err(_) => errors.push(format!("{addr}: timed out")),
        }
    }
    Err(anyhow!("{}", errors.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_round_trips_through_the_store_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pairing/00008160-00123C380100000A.plist");
        let pairing = RpPairingFile::generate("slog-test");
        let identifier = pairing.identifier().to_string();
        save(&Paired {
            udid: "00008160-00123C380100000A".into(),
            name: "Cody’s iPhone".into(),
            model: "iPhone19,2".into(),
            path: path.clone(),
            pairing,
        })
        .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.udid, "00008160-00123C380100000A");
        assert_eq!(loaded.name, "Cody’s iPhone");
        assert_eq!(loaded.pairing.identifier(), identifier);
        assert!(!path.with_extension("plist.tmp").exists());
    }

    #[test]
    fn pairing_seed_is_per_mac_and_user() {
        let seed = pairing_seed().unwrap();
        assert!(seed.starts_with("slog-"));
        assert_eq!(seed, pairing_seed().unwrap());
        assert_ne!(seed, HOST_NAME);
    }
}
