//! Bonjour (DNS-SD) lookups through the system's mDNSResponder, via `dns_sd.h` in
//! libSystem: browse for service instances, resolve one to host, port and TXT record, and
//! look up a host's addresses. Each call blocks its thread until it has answers or its
//! deadline passes, so callers run it with `spawn_blocking`.

use std::ffi::{CStr, CString, c_char, c_void};
use std::net::{IpAddr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::ptr;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

type DnsServiceRef = *mut c_void;
type Flags = u32;
type ErrorType = i32;

const FLAG_MORE_COMING: Flags = 0x1;
const FLAG_ADD: Flags = 0x2;
const PROTOCOL_IPV4: u32 = 0x01;
const PROTOCOL_IPV6: u32 = 0x02;
const TICK: Duration = Duration::from_millis(50);

type BrowseReply = extern "C" fn(
    DnsServiceRef,
    Flags,
    u32,
    ErrorType,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = extern "C" fn(
    DnsServiceRef,
    Flags,
    u32,
    ErrorType,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
type AddrInfoReply = extern "C" fn(
    DnsServiceRef,
    Flags,
    u32,
    ErrorType,
    *const c_char,
    *const libc::sockaddr,
    u32,
    *mut c_void,
);

unsafe extern "C" {
    fn DNSServiceBrowse(
        sd_ref: *mut DnsServiceRef,
        flags: Flags,
        interface_index: u32,
        regtype: *const c_char,
        domain: *const c_char,
        callback: BrowseReply,
        context: *mut c_void,
    ) -> ErrorType;
    fn DNSServiceResolve(
        sd_ref: *mut DnsServiceRef,
        flags: Flags,
        interface_index: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        callback: ResolveReply,
        context: *mut c_void,
    ) -> ErrorType;
    fn DNSServiceGetAddrInfo(
        sd_ref: *mut DnsServiceRef,
        flags: Flags,
        interface_index: u32,
        protocol: u32,
        hostname: *const c_char,
        callback: AddrInfoReply,
        context: *mut c_void,
    ) -> ErrorType;
    fn DNSServiceRefSockFD(sd_ref: DnsServiceRef) -> i32;
    fn DNSServiceProcessResult(sd_ref: DnsServiceRef) -> ErrorType;
    fn DNSServiceRefDeallocate(sd_ref: DnsServiceRef);
}

/// A browsed service instance and the interface it was seen on.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub name: String,
    pub interface: u32,
}

/// A resolved service instance.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub host: String,
    pub port: u16,
    /// Raw TXT record: length-prefixed `key=value` strings.
    pub txt: Vec<u8>,
}

/// Owns a DNSServiceRef; deallocated on drop, which ends the operation.
struct Operation(DnsServiceRef);

impl Drop for Operation {
    fn drop(&mut self) {
        // SAFETY: the ref came from a successful DNSService* call and is deallocated once,
        // here, on the thread that processes its results.
        unsafe { DNSServiceRefDeallocate(self.0) }
    }
}

impl Operation {
    /// Processes results until `done` says so or `deadline` passes.
    fn run(&self, deadline: Instant, mut done: impl FnMut() -> bool) -> Result<()> {
        // SAFETY: the ref is valid for the lifetime of `self`.
        let fd = unsafe { DNSServiceRefSockFD(self.0) };
        if fd < 0 {
            bail!("mDNSResponder gave no socket");
        }
        while !done() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // Short slices, so conditions that depend on time alone (`settle`) are noticed
            // without another event arriving.
            let ms = left.min(TICK).as_millis() as i32;
            // SAFETY: `pfd` is a valid pollfd for the duration of the call.
            let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
            if ready < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if ready == 0 {
                continue;
            }
            // SAFETY: the socket is readable, so this does not block; it calls our callback
            // with the context we registered, which outlives this operation.
            let err = unsafe { DNSServiceProcessResult(self.0) };
            if err != 0 {
                bail!("mDNSResponder error {err}");
            }
        }
        Ok(())
    }
}

fn check(err: ErrorType, what: &str) -> Result<()> {
    if err != 0 {
        bail!("{what} failed with mDNSResponder error {err}");
    }
    Ok(())
}

fn string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: mDNSResponder passes NUL-terminated strings valid during the callback.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

struct BrowseState {
    found: Vec<Instance>,
    last_add: Option<Instant>,
    more_coming: bool,
    error: ErrorType,
}

/// mDNSResponder's error for a lookup macOS's Local Network privacy forbids.
const ERR_POLICY_DENIED: ErrorType = -65570;

/// An error a callback reported.
fn callback_error(err: ErrorType, what: &str) -> anyhow::Error {
    if err == ERR_POLICY_DENIED {
        anyhow::anyhow!(
            "{what}: denied by macOS; allow your terminal app in System Settings > Privacy & \
             Security > Local Network"
        )
    } else {
        anyhow::anyhow!("{what}: mDNSResponder error {err}")
    }
}

extern "C" fn on_browse(
    _: DnsServiceRef,
    flags: Flags,
    interface: u32,
    err: ErrorType,
    name: *const c_char,
    _: *const c_char,
    _: *const c_char,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `BrowseState` `browse` registered and still owns.
    let state = unsafe { &mut *(context as *mut BrowseState) };
    state.more_coming = flags & FLAG_MORE_COMING != 0;
    if err != 0 {
        state.error = err;
        return;
    }
    let instance = Instance {
        name: string(name),
        interface,
    };
    if flags & FLAG_ADD != 0 {
        if !state.found.contains(&instance) {
            state.found.push(instance);
        }
        state.last_add = Some(Instant::now());
    } else {
        state.found.retain(|i| *i != instance);
    }
}

/// Instances of `regtype` (e.g. `_remotepairing._tcp`) in `local.`. Returns `settle` after
/// the last answer, or at `wait` if there is none.
pub fn browse(regtype: &str, wait: Duration, settle: Duration) -> Result<Vec<Instance>> {
    let regtype = CString::new(regtype)?;
    let domain = CString::new("local.")?;
    let mut state = BrowseState {
        found: Vec::new(),
        last_add: None,
        more_coming: false,
        error: 0,
    };
    // One pointer for the callbacks and the checks between them; `state` outlives the
    // operation, which is deallocated (ending callbacks) before `state` is read again.
    let state_ptr = &mut state as *mut BrowseState;
    let mut sd_ref = ptr::null_mut();
    // SAFETY: valid out-pointer and C strings; see `state_ptr`.
    check(
        unsafe {
            DNSServiceBrowse(
                &mut sd_ref,
                0,
                0,
                regtype.as_ptr(),
                domain.as_ptr(),
                on_browse,
                state_ptr as *mut c_void,
            )
        },
        "Bonjour browse",
    )?;
    let op = Operation(sd_ref);
    op.run(Instant::now() + wait, || {
        // SAFETY: read between callbacks, on this thread only.
        let s = unsafe { &*state_ptr };
        !s.more_coming && s.last_add.is_some_and(|t| t.elapsed() >= settle)
    })?;
    drop(op);
    if state.found.is_empty() && state.error != 0 {
        return Err(callback_error(state.error, "Bonjour browse"));
    }
    Ok(state.found)
}

extern "C" fn on_resolve(
    _: DnsServiceRef,
    _: Flags,
    _: u32,
    err: ErrorType,
    _: *const c_char,
    host: *const c_char,
    port: u16,
    txt_len: u16,
    txt: *const u8,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `Option<Resolved>` `resolve` registered and still owns.
    let out = unsafe { &mut *(context as *mut Option<Resolved>) };
    if err != 0 || out.is_some() {
        return;
    }
    let txt = if txt.is_null() {
        Vec::new()
    } else {
        // SAFETY: mDNSResponder passes `txt_len` readable bytes.
        unsafe { std::slice::from_raw_parts(txt, txt_len as usize) }.to_vec()
    };
    *out = Some(Resolved {
        host: string(host),
        port: u16::from_be(port),
        txt,
    });
}

/// Host, port and TXT record of one browsed instance.
pub fn resolve(instance: &Instance, regtype: &str, wait: Duration) -> Result<Resolved> {
    let name = CString::new(instance.name.as_str())?;
    let regtype_c = CString::new(regtype)?;
    let domain = CString::new("local.")?;
    let mut out: Option<Resolved> = None;
    let out_ptr = &mut out as *mut Option<Resolved>;
    let mut sd_ref = ptr::null_mut();
    // SAFETY: as in `browse`, with `out_ptr`.
    check(
        unsafe {
            DNSServiceResolve(
                &mut sd_ref,
                0,
                instance.interface,
                name.as_ptr(),
                regtype_c.as_ptr(),
                domain.as_ptr(),
                on_resolve,
                out_ptr as *mut c_void,
            )
        },
        "Bonjour resolve",
    )?;
    let op = Operation(sd_ref);
    // SAFETY: read between callbacks, on this thread only.
    op.run(Instant::now() + wait, || unsafe { (*out_ptr).is_some() })?;
    drop(op);
    match out {
        Some(r) => Ok(r),
        None => bail!(
            "{} did not resolve within {}s",
            instance.name,
            wait.as_secs()
        ),
    }
}

struct AddrState {
    found: Vec<SocketAddr>,
    more_coming: bool,
    error: ErrorType,
}

extern "C" fn on_addr(
    _: DnsServiceRef,
    flags: Flags,
    interface: u32,
    err: ErrorType,
    _: *const c_char,
    address: *const libc::sockaddr,
    _: u32,
    context: *mut c_void,
) {
    // SAFETY: `context` is the `AddrState` `addresses` registered and still owns.
    let state = unsafe { &mut *(context as *mut AddrState) };
    state.more_coming = flags & FLAG_MORE_COMING != 0;
    if err != 0 {
        state.error = err;
        return;
    }
    if address.is_null() || flags & FLAG_ADD == 0 {
        return;
    }
    // SAFETY: mDNSResponder passes a sockaddr_in or sockaddr_in6 per `sa_family`; read
    // unaligned, as nothing promises the buffer's alignment.
    let addr = unsafe {
        match ptr::read_unaligned(address).sa_family as i32 {
            libc::AF_INET => {
                let a = ptr::read_unaligned(address as *const libc::sockaddr_in);
                SocketAddr::V4(SocketAddrV4::new(u32::from_be(a.sin_addr.s_addr).into(), 0))
            }
            libc::AF_INET6 => {
                let a = ptr::read_unaligned(address as *const libc::sockaddr_in6);
                let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
                // Link-local addresses only work with the interface they were found on.
                let scope = if ip.is_unicast_link_local() {
                    interface
                } else {
                    0
                };
                SocketAddr::V6(SocketAddrV6::new(ip, 0, 0, scope))
            }
            _ => return,
        }
    };
    if !state.found.contains(&addr) {
        state.found.push(addr);
    }
}

/// Addresses of `host` (e.g. `Codys-iPhone.local.`), IPv4 first, port 0.
pub fn addresses(host: &str, interface: u32, wait: Duration) -> Result<Vec<SocketAddr>> {
    let host_c = CString::new(host)?;
    let mut state = AddrState {
        found: Vec::new(),
        more_coming: false,
        error: 0,
    };
    let state_ptr = &mut state as *mut AddrState;
    let mut sd_ref = ptr::null_mut();
    // SAFETY: as in `browse`, with `state_ptr`.
    check(
        unsafe {
            DNSServiceGetAddrInfo(
                &mut sd_ref,
                0,
                interface,
                PROTOCOL_IPV4 | PROTOCOL_IPV6,
                host_c.as_ptr(),
                on_addr,
                state_ptr as *mut c_void,
            )
        },
        "Bonjour address lookup",
    )?;
    let op = Operation(sd_ref);
    op.run(Instant::now() + wait, || {
        // SAFETY: read between callbacks, on this thread only.
        let s = unsafe { &*state_ptr };
        !s.found.is_empty() && !s.more_coming
    })?;
    drop(op);
    let mut found = state.found;
    found.sort_by_key(|a| match a.ip() {
        IpAddr::V4(_) => 0,
        IpAddr::V6(ip) if ip.is_unicast_link_local() => 1,
        IpAddr::V6(_) => 2,
    });
    if found.is_empty() {
        if state.error != 0 {
            return Err(callback_error(
                state.error,
                &format!("address lookup for {host}"),
            ));
        }
        bail!("{host} has no addresses");
    }
    Ok(found)
}

/// The value of `key` in a raw TXT record (`key=value` strings, each prefixed by its
/// length byte).
pub fn txt_value<'a>(txt: &'a [u8], key: &str) -> Option<&'a [u8]> {
    let mut rest = txt;
    while let Some((&len, tail)) = rest.split_first() {
        let len = len as usize;
        if tail.len() < len {
            return None;
        }
        let (entry, next) = tail.split_at(len);
        rest = next;
        let (k, v) = match entry.iter().position(|&b| b == b'=') {
            Some(i) => (&entry[..i], &entry[i + 1..]),
            None => (entry, &entry[entry.len()..]),
        };
        if k.eq_ignore_ascii_case(key.as_bytes()) {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_txt_values() {
        let mut txt = Vec::new();
        for entry in ["identifier=6BF4", "authTag=9OYSuZwC", "flags"] {
            txt.push(entry.len() as u8);
            txt.extend_from_slice(entry.as_bytes());
        }
        assert_eq!(txt_value(&txt, "identifier"), Some(&b"6BF4"[..]));
        assert_eq!(txt_value(&txt, "authtag"), Some(&b"9OYSuZwC"[..]));
        assert_eq!(txt_value(&txt, "flags"), Some(&b""[..]));
        assert_eq!(txt_value(&txt, "ver"), None);
        assert_eq!(txt_value(&[9, b'a'], "a"), None);
    }
}
