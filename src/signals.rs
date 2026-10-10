//! Clean shutdown on SIGINT, SIGTERM and SIGHUP.
//!
//! The first signal makes `main` drop the running command, which deletes temp dirs (the
//! `show --device` archive copy) and kills child processes, before slog dies by that
//! signal. Dying by the signal, rather than exiting 128 + N, matters: a shell only stops a
//! script on Ctrl-C if its child was killed by SIGINT.
//!
//! The watcher runs on a worker thread, so it still works while the main thread is blocked,
//! e.g. writing to a pipe nobody reads. A second signal, or slog still running
//! `FORCE_AFTER` later, ends slog immediately; the watcher first deletes the temp dirs
//! registered with `remove_on_force`, since a blocked main thread never drops them.
//!
//! Signals the parent set to be ignored (`nohup`, background jobs in scripts) stay ignored.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use futures::future::select_all;
use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::oneshot;

const FORCE_AFTER: Duration = Duration::from_secs(2);

static FORCE_CLEANUP: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Installs the handlers immediately and returns a receiver for the first signal's number.
pub fn watch() -> oneshot::Receiver<i32> {
    let mut signals: Vec<(i32, Signal)> = [
        (libc::SIGINT, SignalKind::interrupt()),
        (libc::SIGTERM, SignalKind::terminate()),
        (libc::SIGHUP, SignalKind::hangup()),
    ]
    .into_iter()
    .filter(|(sig, _)| !ignored(*sig))
    .map(|(sig, kind)| (sig, signal(kind).expect("installing a signal handler")))
    .collect();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let first = next(&mut signals).await;
        let _ = tx.send(first);
        tokio::select! {
            _ = next(&mut signals) => {}
            _ = tokio::time::sleep(FORCE_AFTER) => {}
        }
        // `main` is blocked or slow; it may never drop its temp dirs.
        if let Ok(paths) = FORCE_CLEANUP.try_lock() {
            for path in paths.iter() {
                let _ = std::fs::remove_dir_all(path);
            }
        }
        die_by(first);
    });
    rx
}

/// Registers a temp dir to delete if slog has to exit without unwinding. Its normal owner
/// (e.g. a `TempDir`) still deletes it on the regular path.
pub fn remove_on_force(path: PathBuf) {
    FORCE_CLEANUP.lock().expect("cleanup list lock").push(path);
}

/// Whether `sig` is currently ignored (inherited SIG_IGN, e.g. from `nohup`).
fn ignored(sig: i32) -> bool {
    // SAFETY: querying the current action with a null new-action pointer changes nothing;
    // `old` is a valid, writable sigaction.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, std::ptr::null(), &mut old) == 0 && old.sa_sigaction == libc::SIG_IGN
    }
}

async fn next(signals: &mut [(i32, Signal)]) -> i32 {
    if signals.is_empty() {
        return std::future::pending().await;
    }
    let waits = signals.iter_mut().map(|(sig, s)| {
        Box::pin(async move {
            s.recv().await;
            *sig
        })
    });
    select_all(waits).await.0
}

/// Ends the process by `sig`'s default action (terminate), as if never handled.
pub fn die_by(sig: i32) -> ! {
    // SAFETY: resetting a signal to its default disposition and raising it has no memory
    // safety implications; the default action of these signals terminates the process.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
    std::process::exit(128 + sig)
}
