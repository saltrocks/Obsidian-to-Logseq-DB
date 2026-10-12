//! Minimal EDN value model and serializer, sufficient for Logseq's
//! `sqlite.build` / `:graph-human` import format.

use std::fmt::Write as _;

use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum Edn {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    /// Keyword without the leading colon, e.g. `block/title`.
    Keyword(String),
    Uuid(Uuid),
    Vector(Vec<Edn>),
    /// Callers must ensure elements are unique; EDN readers reject duplicates.
    Set(Vec<Edn>),
    /// Callers must ensure keys are unique; EDN readers reject duplicates.
    Map(Vec<(Edn, Edn)>),
}

impl Edn {
    pub fn kw(s: impl Into<String>) -> Edn {
        Edn::Keyword(s.into())
    }

    pub fn str(s: impl Into<String>) -> Edn {
        Edn::Str(s.into())
    }

    /// `[:block/uuid #uuid "..."]` lookup ref.
    pub fn uuid_ref(id: Uuid) -> Edn {
        Edn::Vector(vec![Edn::kw("block/uuid"), Edn::Uuid(id)])
    }

    pub fn to_edn_string(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    /// Writes the value at nesting `depth`. Map entries and items of
    /// collections of maps go on their own lines, indented one space per
    /// level, which keeps large exports compact but diffable.
    fn write(&self, out: &mut String, depth: usize) {
        match self {
            Edn::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Edn::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Edn::Float(f) => {
                if f.is_finite() {
                    let _ = write!(out, "{f:?}");
                } else {
                    out.push_str("nil");
                }
            }
            Edn::Str(s) => write_string(out, s),
            Edn::Keyword(k) => {
                out.push(':');
                out.push_str(k);
            }
            Edn::Uuid(u) => {
                let _ = write!(out, "#uuid \"{u}\"");
            }
            Edn::Vector(items) => write_seq(out, "[", "]", items, depth),
            Edn::Set(items) => write_seq(out, "#{", "}", items, depth),
            Edn::Map(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push('\n');
                        push_indent(out, depth + 1);
                    }
                    k.write(out, depth + 1);
                    out.push(' ');
                    v.write(out, depth + 1);
                }
                out.push('}');
            }
        }
    }
}

/// Collections of maps are written one item per line; others on one line.
fn write_seq(out: &mut String, open: &str, close: &str, items: &[Edn], depth: usize) {
    out.push_str(open);
    let multiline = items
        .iter()
        .any(|i| matches!(i, Edn::Map(m) if !m.is_empty()));
    let inner = depth + 1;
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            if multiline {
                out.push('\n');
                push_indent(out, inner);
            } else {
                out.push(' ');
            }
        }
        item.write(out, inner);
    }
    out.push_str(close);
}

fn push_indent(out: &mut String, n: usize) {
    out.extend(std::iter::repeat_n(' ', n));
}

fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_strings() {
        let s = Edn::str("a\"b\\c\nd\u{1}").to_edn_string();
        assert_eq!(s, "\"a\\\"b\\\\c\\nd\\u0001\"\n");
    }

    #[test]
    fn writes_nested_structures() {
        let id = Uuid::nil();
        let m = Edn::Map(vec![
            (Edn::kw("block/title"), Edn::str("x")),
            (Edn::kw("tags"), Edn::Set(vec![Edn::kw("a/b")])),
            (Edn::kw("ref"), Edn::uuid_ref(id)),
            (Edn::kw("n"), Edn::Float(1.0)),
        ]);
        let s = m.to_edn_string();
        assert!(s.contains(":block/title \"x\""));
        assert!(s.contains("#{:a/b}"));
        assert!(s.contains("[:block/uuid #uuid \"00000000-0000-0000-0000-000000000000\"]"));
        assert!(s.contains(":n 1.0"));
    }
}
