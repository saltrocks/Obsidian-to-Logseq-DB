//! Transit-JSON encoding of [`Edn`] values, the wire format of Logseq's
//! db-worker `/v1/invoke` API.

use std::fmt::Write as _;

use crate::edn::Edn;

/// Largest integer transit-js reads as a plain JSON number.
const MAX_SAFE_INT: i64 = (1 << 53) - 1;

pub fn to_transit_json(value: &Edn) -> String {
    let mut out = String::new();
    write(&mut out, value);
    out
}

fn write(out: &mut String, value: &Edn) {
    match value {
        Edn::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Edn::Int(i) if i.abs() <= MAX_SAFE_INT => {
            let _ = write!(out, "{i}");
        }
        Edn::Int(i) => write_json_str(out, &format!("~i{i}")),
        Edn::Float(f) if f.is_finite() => {
            let _ = write!(out, "{f:?}");
        }
        Edn::Float(_) => out.push_str("null"),
        Edn::Str(s) => {
            if s.starts_with(['~', '^', '`']) {
                write_json_str(out, &format!("~{s}"));
            } else {
                write_json_str(out, s);
            }
        }
        Edn::Keyword(k) => write_json_str(out, &format!("~:{k}")),
        Edn::Uuid(u) => write_json_str(out, &format!("~u{u}")),
        Edn::Vector(items) => write_array(out, items.iter()),
        Edn::Set(items) => {
            out.push_str("[\"~#set\",");
            write_array(out, items.iter());
            out.push(']');
        }
        Edn::Map(entries) => {
            out.push_str("[\"^ \"");
            for (k, v) in entries {
                out.push(',');
                write(out, k);
                out.push(',');
                write(out, v);
            }
            out.push(']');
        }
    }
}

fn write_array<'a>(out: &mut String, items: impl Iterator<Item = &'a Edn>) {
    out.push('[');
    for (i, item) in items.enumerate() {
        if i > 0 {
            out.push(',');
        }
        write(out, item);
    }
    out.push(']');
}

fn write_json_str(out: &mut String, s: &str) {
    out.push_str(&serde_json::to_string(s).expect("strings always serialize"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn encodes_values() {
        let v = Edn::Vector(vec![
            Edn::str("repo"),
            Edn::Map(vec![
                (Edn::kw("block/title"), Edn::str("~tilde \"q\"")),
                (Edn::kw("tags"), Edn::Set(vec![Edn::kw("a/b")])),
                (Edn::kw("id"), Edn::Uuid(Uuid::nil())),
                (Edn::kw("n"), Edn::Int(1_791_815_030_438)),
                (Edn::kw("big"), Edn::Int(i64::MAX)),
                (Edn::kw("f"), Edn::Float(1.5)),
                (Edn::kw("ok"), Edn::Bool(true)),
            ]),
        ]);
        assert_eq!(
            to_transit_json(&v),
            "[\"repo\",[\"^ \",\"~:block/title\",\"~~tilde \\\"q\\\"\",\"~:tags\",[\"~#set\",[\"~:a/b\"]],\
             \"~:id\",\"~u00000000-0000-0000-0000-000000000000\",\"~:n\",1791815030438,\
             \"~:big\",\"~i9223372036854775807\",\"~:f\",1.5,\"~:ok\",true]]"
        );
    }
}
