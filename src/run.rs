//! Drains an entry stream to the printer, enforcing --limit and --duration.

use std::collections::VecDeque;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use tokio::time::{Instant, sleep_until};

use crate::output::Printer;
use crate::source::Entries;

/// Prints the newest `limit` entries (0 = all) once the source is exhausted.
pub async fn show(mut entries: Entries, limit: usize, printer: &mut Printer) -> Result<()> {
    let mut total: u64 = 0;
    let mut newest = VecDeque::new();
    while let Some(entry) = entries.next().await {
        let entry = entry?;
        total += 1;
        if limit == 0 {
            printer.print(&entry)?;
            continue;
        }
        if newest.len() == limit {
            newest.pop_front();
        }
        newest.push_back(entry);
    }
    for entry in &newest {
        printer.print(entry)?;
    }
    if total == 0 {
        eprintln!("slog: no matching entries");
    } else if limit > 0 && total > limit as u64 {
        eprintln!(
            "slog: showed the newest {limit} of {total} matching entries; \
             narrow --last/--filter or raise --limit (0 = no limit)"
        );
    }
    Ok(())
}

/// Prints entries as they arrive until the source ends, `limit` entries, or `duration`.
pub async fn stream(
    mut entries: Entries,
    limit: Option<usize>,
    duration: Option<Duration>,
    printer: &mut Printer,
) -> Result<()> {
    // A deadline far enough away to never fire stands in for "no --duration".
    let deadline = Instant::now() + duration.unwrap_or(Duration::from_secs(100 * 365 * 86400));
    let mut printed = 0;
    loop {
        if limit.is_some_and(|l| l > 0 && printed >= l) {
            break;
        }
        tokio::select! {
            entry = entries.next() => match entry {
                Some(entry) => {
                    printer.print(&entry?)?;
                    printed += 1;
                }
                None => break,
            },
            _ = sleep_until(deadline) => break,
        }
    }
    if printed == 0 {
        eprintln!("slog: no matching entries");
    }
    Ok(())
}
