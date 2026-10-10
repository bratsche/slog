//! Renders entries as compact text or JSON lines.

use std::io::{self, Write};

use chrono::SecondsFormat;
use serde_json::{Map, Value, json};

use crate::cli::OutputField as F;
use crate::entry::Entry;

const DEFAULT_TEXT: &[F] = &[
    F::Time,
    F::Level,
    F::Process,
    F::Pid,
    F::Subsystem,
    F::Category,
    F::Message,
];
const ALL: &[F] = &[
    F::Time,
    F::Level,
    F::Process,
    F::Pid,
    F::Library,
    F::Subsystem,
    F::Category,
    F::Thread,
    F::Message,
];

pub struct Printer {
    json: bool,
    fields: Vec<F>,
    out: io::StdoutLock<'static>,
}

impl Printer {
    pub fn new(json: bool, fields: &[F]) -> Printer {
        let mut fields = match (fields.is_empty(), json) {
            (false, _) => fields.to_vec(),
            (true, true) => ALL.to_vec(),
            (true, false) => DEFAULT_TEXT.to_vec(),
        };
        fields.sort();
        fields.dedup();
        Printer {
            json,
            fields,
            out: io::stdout().lock(),
        }
    }

    pub fn print(&mut self, e: &Entry) -> io::Result<()> {
        let line = if self.json {
            json_line(e, &self.fields)
        } else {
            text_line(e, &self.fields)
        };
        writeln!(self.out, "{line}")
    }
}

fn json_line(e: &Entry, fields: &[F]) -> String {
    let mut m = Map::new();
    for f in fields {
        let (k, v): (&str, Value) = match f {
            F::Time => (
                "time",
                json!(e.time.to_rfc3339_opts(SecondsFormat::Micros, false)),
            ),
            F::Level => ("level", json!(e.level.as_str())),
            F::Process => ("process", json!(e.process)),
            F::Pid => ("pid", json!(e.pid)),
            F::Library => ("library", json!(e.library)),
            F::Subsystem => ("subsystem", json!(e.subsystem)),
            F::Category => ("category", json!(e.category)),
            F::Thread => ("thread", json!(e.thread)),
            F::Message => ("message", json!(e.message)),
        };
        m.insert(k.into(), v);
    }
    Value::Object(m).to_string()
}

fn text_line(e: &Entry, fields: &[F]) -> String {
    let has = |f| fields.contains(&f);
    let mut parts: Vec<String> = Vec::new();
    if has(F::Time) {
        parts.push(e.time.format("%Y-%m-%d %H:%M:%S%.3f").to_string());
    }
    if has(F::Level) {
        parts.push(e.level.as_str().into());
    }
    match (has(F::Process), has(F::Pid)) {
        (true, true) => parts.push(format!("{}[{}]", e.process, e.pid)),
        (true, false) => parts.push(e.process.clone()),
        (false, true) => parts.push(format!("[{}]", e.pid)),
        (false, false) => {}
    }
    if has(F::Library) && !e.library.is_empty() {
        parts.push(format!("({})", e.library));
    }
    let subsystem = has(F::Subsystem) && !e.subsystem.is_empty();
    let category = has(F::Category) && !e.category.is_empty();
    match (subsystem, category) {
        (true, true) => parts.push(format!("[{}:{}]", e.subsystem, e.category)),
        (true, false) => parts.push(format!("[{}]", e.subsystem)),
        (false, true) => parts.push(format!("[:{}]", e.category)),
        (false, false) => {}
    }
    if has(F::Thread) {
        parts.push(format!("tid=0x{:x}", e.thread));
    }
    if has(F::Message) {
        parts.push(e.message.replace('\r', "\\r").replace('\n', "\\n"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::Level;
    use chrono::{Local, TimeZone};

    fn entry() -> Entry {
        Entry {
            time: Local.with_ymd_and_hms(2026, 10, 9, 23, 29, 0).unwrap(),
            level: Level::Error,
            process: "Safari".into(),
            pid: 42,
            subsystem: "com.apple.network".into(),
            category: "connection".into(),
            library: "libnetwork.dylib".into(),
            thread: 0x1a2b,
            message: "line one\nline two".into(),
        }
    }

    #[test]
    fn default_text() {
        assert_eq!(
            text_line(&entry(), DEFAULT_TEXT),
            "2026-10-09 23:29:00.000 error Safari[42] [com.apple.network:connection] line one\\nline two"
        );
    }

    #[test]
    fn all_text_fields_and_empty_subsystem() {
        let mut e = entry();
        e.subsystem.clear();
        e.category.clear();
        assert_eq!(
            text_line(&e, ALL),
            "2026-10-09 23:29:00.000 error Safari[42] (libnetwork.dylib) tid=0x1a2b line one\\nline two"
        );
    }

    #[test]
    fn json_keys_in_canonical_order() {
        let line = json_line(&entry(), &[F::Level, F::Message]);
        assert_eq!(line, r#"{"level":"error","message":"line one\nline two"}"#);
        let all = json_line(&entry(), ALL);
        assert!(
            all.starts_with(r#"{"time":"2026-10-09T23:29:00.000000"#),
            "{all}"
        );
    }
}
