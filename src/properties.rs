//! Frontmatter handling: special keys and property type inference.

use std::collections::{BTreeSet, HashMap};

use chrono::NaiveDate;
use yaml_serde::Value;

/// Frontmatter keys with Obsidian-specific meaning (compared lowercase).
pub const TAG_KEYS: &[&str] = &["tags", "tag"];
pub const ALIAS_KEYS: &[&str] = &["aliases", "alias"];
pub const DROPPED_KEYS: &[&str] = &["cssclasses", "cssclass", "position"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PropType {
    Default,
    Number,
    Checkbox,
    Url,
    Date,
    Node,
}

impl PropType {
    pub fn keyword(self) -> &'static str {
        match self {
            PropType::Default => "default",
            PropType::Number => "number",
            PropType::Checkbox => "checkbox",
            PropType::Url => "url",
            PropType::Date => "date",
            PropType::Node => "node",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PropSchema {
    pub title: String,
    pub ident: String,
    pub ptype: PropType,
    pub many: bool,
}

/// Classifies a scalar YAML value; `None` for null/empty values.
pub fn classify(value: &Value) -> Option<PropType> {
    match value {
        Value::Null => None,
        Value::Bool(_) => Some(PropType::Checkbox),
        Value::Number(_) => Some(PropType::Number),
        Value::String(s) => classify_str(s),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => Some(PropType::Default),
    }
}

fn classify_str(s: &str) -> Option<PropType> {
    let s = s.trim();
    if s.is_empty() {
        None
    } else if single_wikilink(s).is_some() {
        Some(PropType::Node)
    } else if is_url(s) {
        Some(PropType::Url)
    } else if parse_iso_date(s).is_some() {
        Some(PropType::Date)
    } else {
        Some(PropType::Default)
    }
}

/// Returns the inner text of a value that is exactly one `[[wikilink]]`.
pub fn single_wikilink(s: &str) -> Option<&str> {
    let inner = s.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    (!inner.contains("[[") && !inner.contains("]]")).then_some(inner)
}

pub fn is_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !s.chars().any(char::is_whitespace)
}

/// Parses `YYYY-MM-DD`, optionally followed by a time part (`T10:00`).
pub fn parse_iso_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    if s.len() < 10 || !s.is_char_boundary(10) {
        return None;
    }
    let (date, rest) = s.split_at(10);
    if !(rest.is_empty() || rest.starts_with('T') || rest.starts_with(' ')) {
        return None;
    }
    if rest.len() > 1 && !rest[1..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()
}

/// Accumulates every value seen for each frontmatter key across the vault and
/// infers one schema per key.
#[derive(Debug, Default)]
pub struct SchemaBuilder {
    order: Vec<String>,
    keys: HashMap<String, KeyStats>,
}

#[derive(Debug, Default)]
struct KeyStats {
    title: String,
    types: BTreeSet<PropType>,
    many: bool,
}

impl SchemaBuilder {
    pub fn observe(&mut self, key: &str, value: &Value) {
        let lower = key.to_lowercase();
        let stats = self.keys.entry(lower.clone()).or_insert_with(|| {
            self.order.push(lower.clone());
            KeyStats {
                title: key.to_string(),
                ..Default::default()
            }
        });
        match value {
            Value::Sequence(items) => {
                stats.many = true;
                for item in items {
                    match item {
                        Value::Sequence(_) | Value::Mapping(_) => {
                            stats.types.insert(PropType::Default);
                        }
                        other => {
                            if let Some(t) = classify(other) {
                                stats.types.insert(t);
                            }
                        }
                    }
                }
            }
            other => {
                if let Some(t) = classify(other) {
                    stats.types.insert(t);
                }
            }
        }
    }

    /// Returns schemas keyed by lowercase key, with unique `user.property/*` idents.
    pub fn build(self) -> HashMap<String, PropSchema> {
        let mut used_idents = BTreeSet::new();
        let mut out = HashMap::new();
        for key in self.order {
            let stats = &self.keys[&key];
            let ptype = match stats.types.len() {
                0 => PropType::Default,
                1 => *stats.types.iter().next().unwrap(),
                _ => PropType::Default,
            };
            // Checkboxes can't hold several values.
            let ptype = if ptype == PropType::Checkbox && stats.many {
                PropType::Default
            } else {
                ptype
            };
            let ident = unique_ident("user.property", &stats.title, &mut used_idents);
            out.insert(
                key,
                PropSchema {
                    title: stats.title.clone(),
                    ident,
                    ptype,
                    many: stats.many,
                },
            );
        }
        out
    }
}

/// Builds a valid, unique keyword like `user.property/my-key` from a name.
pub fn unique_ident(namespace: &str, name: &str, used: &mut BTreeSet<String>) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        slug = "x".to_string();
    }
    if slug.starts_with(|c: char| c.is_ascii_digit()) {
        slug = format!("n{slug}");
    }
    let mut candidate = format!("{namespace}/{slug}");
    let mut n = 2;
    while used.contains(&candidate) {
        candidate = format!("{namespace}/{slug}-{n}");
        n += 1;
    }
    used.insert(candidate.clone());
    candidate
}

/// Extracts tag names from a `tags` frontmatter value. Accepts lists,
/// comma/space separated strings and leading `#`.
pub fn tag_values(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        for part in s.split([',', ' ']) {
            let t = part.trim().trim_start_matches('#').trim();
            if !t.is_empty() {
                out.push(t.to_string());
            }
        }
    };
    match value {
        Value::Sequence(items) => {
            for item in items {
                if let Some(s) = scalar_string(item) {
                    push(&s);
                }
            }
        }
        other => {
            if let Some(s) = scalar_string(other) {
                push(&s);
            }
        }
    }
    out
}

/// Extracts alias names; unlike tags, aliases may contain spaces.
pub fn alias_values(value: &Value) -> Vec<String> {
    let items: Vec<String> = match value {
        Value::Sequence(items) => items.iter().filter_map(scalar_string).collect(),
        Value::String(s) => s.split(',').map(str::to_string).collect(),
        other => scalar_string(other).into_iter().collect(),
    };
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// String form of any YAML value, used for `:default` properties.
pub fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Tagged(t) => value_to_text(&t.value),
        Value::Sequence(_) | Value::Mapping(_) => serde_json::to_string(value).ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml(s: &str) -> Value {
        yaml_serde::from_str(s).unwrap()
    }

    #[test]
    fn infers_types() {
        let mut b = SchemaBuilder::default();
        b.observe("Rating", &yaml("5"));
        b.observe("rating", &yaml("4.5"));
        b.observe("done", &yaml("true"));
        b.observe("due", &yaml("2024-01-02"));
        b.observe("site", &yaml("https://example.com"));
        b.observe("related", &yaml("[\"[[A]]\", \"[[B]]\"]"));
        b.observe("mixed", &yaml("1"));
        b.observe("mixed", &yaml("text"));
        let s = b.build();
        assert_eq!(s["rating"].ptype, PropType::Number);
        assert_eq!(s["rating"].title, "Rating");
        assert_eq!(s["rating"].ident, "user.property/rating");
        assert_eq!(s["done"].ptype, PropType::Checkbox);
        assert_eq!(s["due"].ptype, PropType::Date);
        assert_eq!(s["site"].ptype, PropType::Url);
        assert_eq!(s["related"].ptype, PropType::Node);
        assert!(s["related"].many);
        assert_eq!(s["mixed"].ptype, PropType::Default);
    }

    #[test]
    fn idents_are_valid_and_unique() {
        let mut used = BTreeSet::new();
        assert_eq!(
            unique_ident("user.property", "My Key!", &mut used),
            "user.property/my-key"
        );
        assert_eq!(
            unique_ident("user.property", "my key", &mut used),
            "user.property/my-key-2"
        );
        assert_eq!(
            unique_ident("user.property", "2nd", &mut used),
            "user.property/n2nd"
        );
        assert_eq!(
            unique_ident("user.class", "日本", &mut used),
            "user.class/x"
        );
    }

    #[test]
    fn tags_and_aliases() {
        assert_eq!(tag_values(&yaml("[a, '#b/c']")), vec!["a", "b/c"]);
        assert_eq!(tag_values(&yaml("\"x, y z\"")), vec!["x", "y", "z"]);
        assert_eq!(
            alias_values(&yaml("[Start here, Other]")),
            vec!["Start here", "Other"]
        );
        assert_eq!(alias_values(&yaml("Just one")), vec!["Just one"]);
    }
}
