//! Console.app-style search tokens (`--filter`).
//!
//! One parsed filter has two backends that must agree:
//! - `to_predicate` compiles to an NSPredicate so `/usr/bin/log` filters at the source.
//! - `matches` evaluates in-process for sources with no predicate support (iOS devices).
//!
//! Semantics: positive tokens on the same named field are OR'd; bare words (`any`),
//! negated tokens and different fields are AND'd. Text comparisons are case-insensitive;
//! NSPredicate folds full Unicode, `matches` uses `to_lowercase` (e.g. "ß" vs "SS" differ).

use anyhow::{Result, bail};

use crate::entry::{Entry, Level};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Any,
    Message,
    Process,
    Pid,
    Subsystem,
    Category,
    Library,
    Level,
}

impl Field {
    const NAMES: [(&'static str, Field); 8] = [
        ("any", Field::Any),
        ("message", Field::Message),
        ("process", Field::Process),
        ("pid", Field::Pid),
        ("subsystem", Field::Subsystem),
        ("category", Field::Category),
        ("library", Field::Library),
        ("level", Field::Level),
    ];

    fn from_name(name: &str) -> Option<Field> {
        Self::NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, f)| *f)
    }

    /// NSPredicate keys searched for this field. `Any` searches several.
    fn predicate_keys(self) -> &'static [&'static str] {
        match self {
            Field::Any => &["composedMessage", "process", "subsystem", "category"],
            Field::Message => &["composedMessage"],
            Field::Process => &["process"],
            Field::Subsystem => &["subsystem"],
            Field::Category => &["category"],
            Field::Library => &["sender"],
            Field::Pid | Field::Level => &[],
        }
    }

    fn entry_values(self, e: &Entry) -> Vec<&str> {
        match self {
            Field::Any => vec![&e.message, &e.process, &e.subsystem, &e.category],
            Field::Message => vec![&e.message],
            Field::Process => vec![&e.process],
            Field::Subsystem => vec![&e.subsystem],
            Field::Category => vec![&e.category],
            Field::Library => vec![&e.library],
            Field::Pid | Field::Level => vec![],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Value {
    /// Lowercased; comparisons are case-insensitive.
    Contains(String),
    Equals(String),
    Pid(u32),
    Level(Level),
}

#[derive(Clone, Debug, PartialEq)]
struct Term {
    field: Field,
    value: Value,
    negated: bool,
}

impl Term {
    fn predicate(&self) -> String {
        let body = match &self.value {
            Value::Pid(pid) => format!("processIdentifier == {pid}"),
            Value::Level(level) => format!("logType == \"{}\"", level.as_str()),
            Value::Contains(v) | Value::Equals(v) => {
                let op = if matches!(self.value, Value::Contains(_)) {
                    "CONTAINS[c]"
                } else {
                    "==[c]"
                };
                let parts: Vec<String> = self
                    .field
                    .predicate_keys()
                    .iter()
                    .map(|k| format!("{k} {op} \"{}\"", escape(v)))
                    .collect();
                parts.join(" OR ")
            }
        };
        if self.negated {
            format!("NOT ({body})")
        } else {
            format!("({body})")
        }
    }

    fn matches(&self, e: &Entry) -> bool {
        let hit = match &self.value {
            Value::Pid(pid) => e.pid == *pid,
            Value::Level(level) => e.level == *level,
            Value::Contains(v) => self
                .field
                .entry_values(e)
                .iter()
                .any(|s| s.to_lowercase().contains(v)),
            Value::Equals(v) => self
                .field
                .entry_values(e)
                .iter()
                .any(|s| s.to_lowercase() == *v),
        };
        hit != self.negated
    }
}

fn escape(v: &str) -> String {
    v.replace('\\', "\\\\").replace('"', "\\\"")
}

/// AND of clauses; each clause is an OR of terms.
#[derive(Debug, Default)]
pub struct Filter {
    clauses: Vec<Vec<Term>>,
}

impl Filter {
    /// Parses every `--filter` argument; all of them must hold.
    pub fn parse(inputs: &[String]) -> Result<Filter> {
        let mut filter = Filter::default();
        // Index into `clauses` of the OR-group for each field's positive tokens.
        let mut groups: Vec<(Field, usize)> = Vec::new();
        for input in inputs {
            for word in split_words(input)? {
                let term = parse_term(word)?;
                if term.negated || term.field == Field::Any {
                    // Bare words narrow the search, like Console.app; never OR'd.
                    filter.clauses.push(vec![term]);
                } else if let Some((_, i)) = groups.iter().find(|(f, _)| *f == term.field) {
                    filter.clauses[*i].push(term);
                } else {
                    groups.push((term.field, filter.clauses.len()));
                    filter.clauses.push(vec![term]);
                }
            }
        }
        Ok(filter)
    }

    pub fn to_predicate(&self) -> Option<String> {
        if self.clauses.is_empty() {
            return None;
        }
        let clauses: Vec<String> = self
            .clauses
            .iter()
            .map(|c| {
                let terms: Vec<String> = c.iter().map(Term::predicate).collect();
                if terms.len() == 1 {
                    terms[0].clone()
                } else {
                    format!("({})", terms.join(" OR "))
                }
            })
            .collect();
        Some(clauses.join(" AND "))
    }

    pub fn matches(&self, e: &Entry) -> bool {
        self.clauses.iter().all(|c| c.iter().any(|t| t.matches(e)))
    }

    /// Levels a positive `level:` token asks for, so callers can include info/debug messages.
    pub fn requested_levels(&self) -> Vec<Level> {
        self.clauses
            .iter()
            .flatten()
            .filter_map(|t| match t.value {
                Value::Level(l) if !t.negated => Some(l),
                _ => None,
            })
            .collect()
    }
}

/// A run of non-whitespace text; quoted segments keep their spaces and lose special meaning.
struct Segment {
    quoted: bool,
    text: String,
}

fn split_words(input: &str) -> Result<Vec<Vec<Segment>>> {
    let mut words = Vec::new();
    let mut word: Vec<Segment> = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else if c == '"' {
            let mut text = String::new();
            loop {
                match chars.next() {
                    None => bail!("unterminated quote in --filter {input:?}"),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some(n) => text.push(n),
                        None => bail!("unterminated quote in --filter {input:?}"),
                    },
                    Some(n) => text.push(n),
                }
            }
            word.push(Segment { quoted: true, text });
        } else {
            match word.last_mut() {
                Some(s) if !s.quoted => s.text.push(c),
                _ => word.push(Segment {
                    quoted: false,
                    text: c.to_string(),
                }),
            }
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

fn parse_term(mut segs: Vec<Segment>) -> Result<Term> {
    let original: String = segs.iter().map(|s| s.text.as_str()).collect();
    let mut negated = false;
    if let Some(first) = segs.first_mut().filter(|s| !s.quoted)
        && first.text.starts_with('-')
        && (first.text.len() > 1 || segs.len() > 1)
    {
        negated = true;
        segs[0].text.remove(0);
    }

    let mut field = Field::Any;
    let mut exact = false;
    if let Some(first) = segs.first_mut().filter(|s| !s.quoted)
        && let Some(i) = first.text.find(':')
        && let Some(f) = Field::from_name(&first.text[..i])
    {
        field = f;
        let rest = &first.text[i + 1..];
        let rest = match rest.strip_prefix('=') {
            Some(r) => {
                exact = true;
                r
            }
            None => rest,
        };
        first.text = rest.to_string();
    }

    let raw: String = segs.into_iter().map(|s| s.text).collect();
    if raw.is_empty() {
        bail!("filter token {original:?} has no value");
    }
    let value = match field {
        Field::Pid => match raw.parse() {
            Ok(pid) => Value::Pid(pid),
            Err(_) => bail!("filter token {original:?}: pid must be a number"),
        },
        Field::Level => match Level::parse(&raw) {
            Some(l) => Value::Level(l),
            None => bail!(
                "filter token {original:?}: level must be one of debug, info, default, error, fault"
            ),
        },
        _ if exact => Value::Equals(raw.to_lowercase()),
        _ => Value::Contains(raw.to_lowercase()),
    };
    Ok(Term {
        field,
        value,
        negated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Local;

    fn f(s: &str) -> Filter {
        Filter::parse(&[s.to_string()]).unwrap()
    }

    fn pred(s: &str) -> String {
        f(s).to_predicate().unwrap()
    }

    fn entry() -> Entry {
        Entry {
            time: Local::now(),
            level: Level::Error,
            process: "Safari".into(),
            pid: 42,
            subsystem: "com.apple.network".into(),
            category: "connection".into(),
            library: "libnetwork.dylib".into(),
            thread: 1,
            message: "Connection Timed out after 30s".into(),
        }
    }

    #[test]
    fn empty_filter_has_no_predicate_and_matches_everything() {
        let filter = f("  ");
        assert_eq!(filter.to_predicate(), None);
        assert!(filter.matches(&entry()));
    }

    #[test]
    fn bare_word_searches_any_field() {
        assert_eq!(
            pred("timeout"),
            "(composedMessage CONTAINS[c] \"timeout\" OR process CONTAINS[c] \"timeout\" \
             OR subsystem CONTAINS[c] \"timeout\" OR category CONTAINS[c] \"timeout\")"
        );
        assert!(f("TIMED").matches(&entry()));
        assert!(f("safari").matches(&entry()));
        assert!(!f("bluetooth").matches(&entry()));
    }

    #[test]
    fn quoted_phrase_keeps_spaces() {
        assert!(f("\"timed out\"").matches(&entry()));
        assert!(f("message:\"timed out after\"").matches(&entry()));
        assert_eq!(
            pred("message:\"a \\\"b\\\"\""),
            "(composedMessage CONTAINS[c] \"a \\\"b\\\"\")"
        );
    }

    #[test]
    fn same_field_is_ored_different_fields_are_anded() {
        assert_eq!(
            pred("process:Safari process:Mail subsystem:net"),
            "((process CONTAINS[c] \"safari\") OR (process CONTAINS[c] \"mail\")) \
             AND (subsystem CONTAINS[c] \"net\")"
        );
        assert!(f("process:Mail process:Safari").matches(&entry()));
        assert!(!f("process:Safari subsystem:bluetooth").matches(&entry()));
    }

    #[test]
    fn negation_is_always_anded() {
        assert_eq!(
            pred("-process:Safari -process:Mail"),
            "NOT (process CONTAINS[c] \"safari\") AND NOT (process CONTAINS[c] \"mail\")"
        );
        assert!(!f("-process:Safari").matches(&entry()));
        assert!(f("-process:Mail").matches(&entry()));
        assert!(
            f("\"-literal\"")
                .to_predicate()
                .unwrap()
                .contains("\"-literal\"")
        );
    }

    #[test]
    fn exact_match() {
        assert_eq!(pred("process:=Safari"), "(process ==[c] \"safari\")");
        assert!(f("process:=safari").matches(&entry()));
        assert!(!f("process:=Saf").matches(&entry()));
    }

    #[test]
    fn pid_and_level() {
        assert_eq!(
            pred("pid:42 level:error"),
            "(processIdentifier == 42) AND (logType == \"error\")"
        );
        assert!(f("pid:42 level:error").matches(&entry()));
        assert!(f("level:fault level:error").matches(&entry()));
        assert!(!f("level:fault").matches(&entry()));
        assert_eq!(
            f("level:debug -level:info").requested_levels(),
            vec![Level::Debug]
        );
    }

    #[test]
    fn library_maps_to_sender() {
        assert_eq!(
            pred("library:libnetwork"),
            "(sender CONTAINS[c] \"libnetwork\")"
        );
        assert!(f("library:libnetwork").matches(&entry()));
    }

    #[test]
    fn unknown_field_prefix_is_plain_text() {
        assert!(pred("error:foo").contains("composedMessage CONTAINS[c] \"error:foo\""));
    }

    #[test]
    fn multiple_filter_args_combine() {
        let filter = Filter::parse(&["process:Safari".into(), "process:Mail".into()]).unwrap();
        assert!(filter.to_predicate().unwrap().contains(" OR "));
    }

    #[test]
    fn invalid_tokens_are_errors() {
        assert!(Filter::parse(&["pid:abc".into()]).is_err());
        assert!(Filter::parse(&["level:loud".into()]).is_err());
        assert!(Filter::parse(&["process:".into()]).is_err());
        assert!(Filter::parse(&["\"open".into()]).is_err());
    }
}
