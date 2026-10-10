mod cli;
mod entry;
mod filter;
mod output;
mod reports;
mod run;
mod signals;
mod skill;
mod source;

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use futures::StreamExt;

use cli::{
    Cli, Command, DevicesArgs, QueryArgs, ReportsCommand, ShowArgs, SkillCommand, StreamArgs,
};
use entry::Level;
use filter::Filter;
use output::Printer;
use source::{device, mac};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let signal = signals::watch();
    let command = async {
        match cli.command {
            Command::Show(args) => show(args).await,
            Command::Stream(args) => stream(args).await,
            Command::Devices(args) => devices(args).await,
            Command::Skill(cmd) => skill(cmd),
            Command::Reports(cmd) => reports_cmd(cmd).await,
        }
    };
    // On a signal, the command is dropped when this select! completes (deleting temp dirs,
    // killing children); only then does slog die by the signal. See `signals`.
    let outcome = tokio::select! {
        result = command => Ok(result),
        Ok(sig) = signal => Err(sig),
    };
    let result = match outcome {
        Ok(result) => result,
        Err(sig) => {
            let _ = io::stdout().flush();
            signals::die_by(sig);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // A closed pipe (e.g. `slog ... | head`) is a normal way to stop reading.
        Err(e)
            if e.downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
        {
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("slog: error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Parsed --filter plus the info/debug inclusion it implies.
struct Query {
    filter: Filter,
    verbosity: mac::Verbosity,
}

impl Query {
    fn new(args: &QueryArgs) -> Result<Query> {
        let filter = Filter::parse(&args.filter)?;
        let wants = filter.requested_levels();
        let verbosity = if args.debug || wants.contains(&Level::Debug) {
            mac::Verbosity::Debug
        } else if args.info || wants.contains(&Level::Info) {
            mac::Verbosity::Info
        } else {
            mac::Verbosity::Default
        };
        Ok(Query { filter, verbosity })
    }

    /// --filter compiled to NSPredicate, AND'd with the raw --predicate.
    fn predicate(&self, raw: &Option<String>) -> Option<String> {
        match (self.filter.to_predicate(), raw) {
            (None, None) => None,
            (Some(f), None) => Some(f),
            (None, Some(r)) => Some(r.clone()),
            (Some(f), Some(r)) => Some(format!("{f} AND ({r})")),
        }
    }
}

async fn show(args: ShowArgs) -> Result<()> {
    let query = Query::new(&args.query)?;
    // Holds a device's copied archive until printing is done; deleted on drop.
    let mut _device_archive = None;
    let (archive, last, start) = match &args.device {
        Some(selector) => {
            // An explicit start for both the copy and `log show`, so `log` never has to
            // interpret --last relative to an archive.
            let start = match (&args.last, &args.start) {
                (Some(span), _) => Local::now() - span.0,
                (None, Some(s)) => cli::parse_local_time(s)?,
                (None, None) => Local::now() - Duration::from_secs(300),
            };
            let dir = tempfile::Builder::new().prefix("slog-").tempdir()?;
            signals::remove_on_force(dir.path().to_path_buf());
            let path = dir.path().join("device.logarchive");
            device::collect_archive(selector, start, &path).await?;
            _device_archive = Some(dir);
            // With the offset: without it, a time in the hour repeated when DST ends is
            // ambiguous.
            let start = args
                .start
                .clone()
                .unwrap_or_else(|| start.format("%Y-%m-%d %H:%M:%S%z").to_string());
            (Some(path), None, Some(start))
        }
        None => {
            let last = match (args.last, &args.start, &args.archive) {
                (Some(span), _, _) => Some(span.to_log_arg()),
                (None, None, None) => Some("5m".to_string()),
                (None, _, _) => None,
            };
            (args.archive.clone(), last, args.start.clone())
        }
    };
    let entries = mac::show(mac::ShowQuery {
        archive,
        last,
        start,
        end: args.end,
        verbosity: query.verbosity,
        predicate: query.predicate(&args.query.predicate),
    })?;
    let mut printer = Printer::new(args.query.json, &args.query.fields);
    run::show(entries, args.limit, &mut printer).await
}

async fn stream(args: StreamArgs) -> Result<()> {
    let query = Query::new(&args.query)?;
    let entries = match args.device {
        Some(selector) => {
            let include_info = query.verbosity != mac::Verbosity::Default;
            let include_debug = query.verbosity == mac::Verbosity::Debug;
            let filter = query.filter;
            device::stream(selector, include_info, include_debug)
                .filter(move |e| std::future::ready(e.as_ref().map_or(true, |e| filter.matches(e))))
                .boxed()
        }
        None => mac::stream(query.verbosity, query.predicate(&args.query.predicate)),
    };
    let mut printer = Printer::new(args.query.json, &args.query.fields);
    run::stream(
        entries,
        args.limit,
        args.duration.map(|s| s.0),
        &mut printer,
    )
    .await
}

async fn devices(args: DevicesArgs) -> Result<()> {
    let devices = device::list().await?;
    if devices.is_empty() {
        eprintln!("slog: no iOS devices connected; connect one by USB and tap Trust");
    }
    let mut out = io::stdout().lock();
    if args.json {
        for d in &devices {
            writeln!(out, "{}", serde_json::to_string(d)?)?;
        }
    } else {
        for d in &devices {
            match &d.error {
                Some(err) => writeln!(out, "{}\t{}\terror: {err}", d.udid, d.connection)?,
                None => writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}",
                    d.udid,
                    d.connection,
                    d.name.as_deref().unwrap_or(""),
                    d.model.as_deref().unwrap_or(""),
                    d.os_version.as_deref().unwrap_or(""),
                )?,
            }
        }
    }
    Ok(())
}

fn skill(cmd: SkillCommand) -> Result<()> {
    match cmd {
        SkillCommand::Print => write!(io::stdout(), "{}", skill::contents())?,
        SkillCommand::Install(args) => {
            let path = if args.user {
                skill::user_path()?
            } else {
                skill::project_path()
            };
            let what = match skill::install(&path, args.force)? {
                skill::Installed::Created => "installed",
                skill::Installed::Updated => "updated",
                skill::Installed::Unchanged => "already up to date",
            };
            writeln!(io::stdout(), "{what}: {}", path.display())?;
        }
    }
    Ok(())
}

async fn reports_cmd(cmd: ReportsCommand) -> Result<()> {
    let mut out = io::stdout().lock();
    match cmd {
        ReportsCommand::List(args) => {
            let all = match &args.device {
                Some(selector) => device::list_reports(selector).await?,
                None => reports::list_mac()?,
            };
            let since = Local::now() - args.last.0;
            let (picked, total) = reports::select(all, since, args.name.as_deref(), args.limit);
            for r in &picked {
                if args.json {
                    writeln!(out, "{}", serde_json::to_string(r)?)?;
                } else {
                    writeln!(out, "{}", reports::text_line(r))?;
                }
            }
            if total == 0 {
                eprintln!("slog: no matching reports");
            } else if picked.len() < total {
                eprintln!(
                    "slog: showed the newest {} of {total} matching reports; narrow --last/--name or raise --limit (0 = no limit)",
                    picked.len()
                );
            }
        }
        ReportsCommand::Show(args) => {
            let bytes = match &args.device {
                Some(selector) => device::pull_report(selector, &args.name).await?,
                None => {
                    let path = reports::find_mac(&args.name)?;
                    std::fs::read(&path)
                        .with_context(|| format!("cannot read {}", path.display()))?
                }
            };
            let name = args.name.rsplit('/').next().unwrap_or(&args.name);
            let text = reports::render(name, &bytes, args.full)?;
            write!(out, "{text}")?;
            if !text.ends_with('\n') {
                writeln!(out)?;
            }
        }
    }
    Ok(())
}
