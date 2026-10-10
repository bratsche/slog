mod cli;
mod entry;
mod filter;
mod output;
mod run;
mod skill;
mod source;

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;
use futures::StreamExt;

use cli::{Cli, Command, DevicesArgs, QueryArgs, ShowArgs, SkillCommand, StreamArgs};
use entry::Level;
use filter::Filter;
use output::Printer;
use source::{device, mac};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Show(args) => show(args).await,
        Command::Stream(args) => stream(args).await,
        Command::Devices(args) => devices(args).await,
        Command::Skill(cmd) => skill(cmd),
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
    let last = match (args.last, &args.start, &args.archive) {
        (Some(span), _, _) => Some(span.to_log_arg()),
        (None, None, None) => Some("5m".to_string()),
        (None, _, _) => None,
    };
    let entries = mac::show(mac::ShowQuery {
        archive: args.archive,
        last,
        start: args.start,
        end: args.end,
        verbosity: query.verbosity,
        predicate: query.predicate(&args.query.predicate),
    });
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
