//! Converts a scanned vault into Logseq's `:graph-human` EDN import format.
//!
//! Every page, tag (class), journal, referenced block and asset gets an
//! explicit UUID with `:build/keep-uuid? true`, and all links in block text
//! are rewritten to `[[uuid]]` so Logseq derives `:block/refs` on import.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yaml_serde::Value;

use crate::edn::Edn;
use crate::markdown::{BlockKind, MdBlock, ParseOptions, code_span_len, parse_note};
use crate::properties::{
    self, ALIAS_KEYS, DROPPED_KEYS, PropSchema, PropType, SchemaBuilder, TAG_KEYS,
};
use crate::vault::{Target, Vault};

/// Titles of the pages in a freshly created Logseq DB graph, used when the
/// target graph can't be queried.
pub fn bundled_reserved_titles() -> HashSet<String> {
    include_str!("data/reserved_titles.txt")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Title of Logseq's built-in page that holds `#Asset` blocks.
const ASSET_PAGE_TITLE: &str = "Asset";

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub nest_under_headings: bool,
    pub keep_comments: bool,
    pub tags_as_pages: bool,
    pub journals: bool,
    /// Lowercased titles of pages that already exist in the target graph.
    /// Imported pages with these titles would be merged into the existing
    /// entity by Logseq, so they are renamed instead.
    pub reserved_titles: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct AssetFile {
    pub source: PathBuf,
    /// File name inside the graph's `assets/` directory: `<uuid>.<ext>`.
    pub file_name: String,
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub notes: usize,
    pub pages: usize,
    pub journals: usize,
    pub placeholder_pages: usize,
    pub alias_pages: usize,
    pub classes: usize,
    pub properties: usize,
    pub blocks: usize,
    pub assets: usize,
    pub unreferenced_attachments: usize,
}

pub struct Conversion {
    pub edn: Edn,
    pub assets: Vec<AssetFile>,
    pub stats: Stats,
    pub warnings: Vec<String>,
    /// UUIDs of a few converted note pages, used to verify an import.
    pub sample_page_uuids: Vec<Uuid>,
}

#[derive(Debug, Default)]
struct Entity {
    uuid: Uuid,
    title: String,
    notes: Vec<usize>,
    journal: Option<NaiveDate>,
    class_ident: Option<String>,
    alias_of: Option<usize>,
    aliases: Vec<usize>,
    blocks: Vec<Edn>,
    props: Vec<(String, Edn)>,
    tags: Vec<usize>,
}

struct Asset {
    uuid: Uuid,
    attachment: usize,
}

struct Converter<'a> {
    vault: &'a Vault,
    opts: &'a ConvertOptions,
    entities: Vec<Entity>,
    /// Lowercased Obsidian names (note stems, aliases, tags, link targets).
    by_name: HashMap<String, usize>,
    titles_taken: HashSet<String>,
    note_entity: Vec<usize>,
    journal_entity: HashMap<NaiveDate, usize>,
    block_ids: Vec<HashMap<String, Uuid>>,
    headings: Vec<Vec<(String, Uuid)>>,
    assets: Vec<Asset>,
    asset_by_attachment: HashMap<usize, usize>,
    class_idents: BTreeSet<String>,
    schemas: HashMap<String, PropSchema>,
    used_props: BTreeSet<String>,
    warnings: Vec<String>,
    block_count: usize,
}

struct ParsedFile {
    frontmatter: Vec<(String, Value)>,
    blocks: Vec<MdBlock>,
}

pub fn convert(vault: &Vault, opts: &ConvertOptions) -> Result<Conversion> {
    let mut conv = Converter {
        vault,
        opts,
        entities: Vec::new(),
        by_name: HashMap::new(),
        titles_taken: HashSet::new(),
        note_entity: vec![usize::MAX; vault.notes.len()],
        journal_entity: HashMap::new(),
        block_ids: vec![HashMap::new(); vault.notes.len()],
        headings: vec![Vec::new(); vault.notes.len()],
        assets: Vec::new(),
        asset_by_attachment: HashMap::new(),
        class_idents: BTreeSet::new(),
        schemas: HashMap::new(),
        used_props: BTreeSet::new(),
        warnings: Vec::new(),
        block_count: 0,
    };
    let mut parsed = conv.parse_notes()?;
    for (i, file) in parsed.iter_mut().enumerate() {
        conv.assign_block_uuids(i, &mut file.blocks);
    }
    conv.build_schemas(&parsed);
    conv.create_note_entities();
    conv.create_aliases(&parsed);
    for (i, file) in parsed.iter().enumerate() {
        conv.render_note(i, file);
    }
    Ok(conv.finish())
}

impl<'a> Converter<'a> {
    // ---------------------------------------------------------------------
    // Parsing and setup
    // ---------------------------------------------------------------------

    fn parse_notes(&mut self) -> Result<Vec<ParsedFile>> {
        let parse_opts = ParseOptions {
            nest_under_headings: self.opts.nest_under_headings,
            keep_comments: self.opts.keep_comments,
        };
        let mut out = Vec::with_capacity(self.vault.notes.len());
        for note in &self.vault.notes {
            let bytes = fs::read(&note.abs_path)?;
            let source = String::from_utf8_lossy(&bytes);
            let parsed = parse_note(&source, parse_opts);
            let mut frontmatter = Vec::new();
            if let Some(yaml) = parsed
                .frontmatter
                .as_deref()
                .filter(|y| !y.trim().is_empty())
            {
                match yaml_serde::from_str::<Value>(yaml) {
                    Ok(Value::Mapping(map)) => {
                        for (k, v) in map {
                            if let Some(key) = properties::scalar_string(&k) {
                                frontmatter.push((key, v));
                            }
                        }
                    }
                    Ok(Value::Null) => {}
                    Ok(_) => self.warnings.push(format!(
                        "{}: frontmatter is not a mapping, ignored",
                        note.rel_path
                    )),
                    Err(e) => self.warnings.push(format!(
                        "{}: invalid frontmatter ignored ({e})",
                        note.rel_path
                    )),
                }
            }
            out.push(ParsedFile {
                frontmatter,
                blocks: parsed.blocks,
            });
        }
        Ok(out)
    }

    fn assign_block_uuids(&mut self, note: usize, blocks: &mut [MdBlock]) {
        for block in blocks {
            if let Some(id) = &block.block_id {
                let uuid = Uuid::new_v4();
                block.uuid = Some(uuid);
                self.block_ids[note].insert(id.to_lowercase(), uuid);
            }
            if let BlockKind::Heading(_) = block.kind {
                let uuid = *block.uuid.get_or_insert_with(Uuid::new_v4);
                self.headings[note].push((normalize_heading(&block.text), uuid));
            }
            self.assign_block_uuids(note, &mut block.children);
        }
    }

    fn build_schemas(&mut self, parsed: &[ParsedFile]) {
        let mut builder = SchemaBuilder::default();
        for file in parsed {
            for (key, value) in &file.frontmatter {
                let lower = key.to_lowercase();
                if ALIAS_KEYS.contains(&lower.as_str()) || DROPPED_KEYS.contains(&lower.as_str()) {
                    continue;
                }
                if TAG_KEYS.contains(&lower.as_str()) {
                    if self.opts.tags_as_pages {
                        builder.observe(
                            "tags",
                            &Value::Sequence(vec![Value::String("[[x]]".into())]),
                        );
                    }
                    continue;
                }
                builder.observe(key, value);
            }
        }
        self.schemas = builder.build();
    }

    fn create_note_entities(&mut self) {
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, note) in self.vault.notes.iter().enumerate() {
            if self.opts.journals
                && let Some(date) = self.vault.note_journal_date(note)
            {
                let e = self.journal_for(date);
                self.entities[e].notes.push(i);
                self.note_entity[i] = e;
                continue;
            }
            groups.entry(note.stem.to_lowercase()).or_default().push(i);
        }
        let mut keys: Vec<_> = groups.keys().cloned().collect();
        keys.sort();
        for key in keys {
            let mut members = groups.remove(&key).unwrap();
            members.sort_by_key(|&i| {
                let p = &self.vault.notes[i].rel_path;
                (p.matches('/').count(), p.clone())
            });
            for (rank, &i) in members.iter().enumerate() {
                let note = &self.vault.notes[i];
                let base = if rank == 0 || note.folder.is_empty() {
                    note.stem.clone()
                } else {
                    format!("{} ({})", note.stem, note.folder)
                };
                let title = self.claim_title(&base, "Obsidian");
                let e = self.new_entity(title);
                self.entities[e].notes.push(i);
                self.note_entity[i] = e;
                if rank == 0 {
                    self.by_name.insert(key.clone(), e);
                }
            }
        }
    }

    fn create_aliases(&mut self, parsed: &[ParsedFile]) {
        for (i, file) in parsed.iter().enumerate() {
            let owner = self.note_entity[i];
            for (key, value) in &file.frontmatter {
                if !ALIAS_KEYS.contains(&key.to_lowercase().as_str()) {
                    continue;
                }
                for alias in properties::alias_values(value) {
                    let lower = alias.to_lowercase();
                    if let Some(&existing) = self.by_name.get(&lower) {
                        if existing != owner {
                            self.warnings.push(format!(
                                "{}: alias {alias:?} skipped, the name is already used",
                                self.vault.notes[i].rel_path
                            ));
                        }
                        continue;
                    }
                    if self.title_unavailable(&alias) {
                        self.warnings.push(format!(
                            "{}: alias {alias:?} skipped, the title already exists in Logseq",
                            self.vault.notes[i].rel_path
                        ));
                        continue;
                    }
                    self.titles_taken.insert(lower.clone());
                    let e = self.new_entity(alias);
                    self.entities[e].alias_of = Some(owner);
                    self.entities[owner].aliases.push(e);
                    self.by_name.insert(lower, e);
                }
            }
        }
    }

    // ---------------------------------------------------------------------
    // Entity registry
    // ---------------------------------------------------------------------

    fn new_entity(&mut self, title: String) -> usize {
        self.entities.push(Entity {
            uuid: Uuid::new_v4(),
            title,
            ..Default::default()
        });
        self.entities.len() - 1
    }

    fn title_unavailable(&self, title: &str) -> bool {
        let lower = title.to_lowercase();
        self.titles_taken.contains(&lower) || self.opts.reserved_titles.contains(&lower)
    }

    /// Returns `base` or the first free `base (suffix)` / `base (suffix N)`.
    fn claim_title(&mut self, base: &str, suffix: &str) -> String {
        let mut candidate = base.to_string();
        let mut n = 1;
        while self.title_unavailable(&candidate) {
            candidate = if n == 1 {
                format!("{base} ({suffix})")
            } else {
                format!("{base} ({suffix} {n})")
            };
            n += 1;
        }
        self.titles_taken.insert(candidate.to_lowercase());
        candidate
    }

    fn journal_for(&mut self, date: NaiveDate) -> usize {
        if let Some(&e) = self.journal_entity.get(&date) {
            return e;
        }
        self.entities.push(Entity {
            uuid: journal_uuid(date),
            journal: Some(date),
            ..Default::default()
        });
        let e = self.entities.len() - 1;
        self.journal_entity.insert(date, e);
        e
    }

    /// Page for an unresolved link target; created on first use like Obsidian
    /// does when clicking a dangling link.
    fn page_for_name(&mut self, name: &str) -> usize {
        let name = name.trim();
        let name = name.strip_suffix(".md").unwrap_or(name);
        let lower = name.to_lowercase();
        if let Some(&e) = self.by_name.get(&lower) {
            return self.entities[e].alias_of.unwrap_or(e);
        }
        if self.opts.journals
            && let Some(date) = self.vault.parse_journal_link(name)
        {
            return self.journal_for(date);
        }
        let title = self.claim_title(name, "Obsidian");
        let e = self.new_entity(title);
        self.by_name.insert(lower, e);
        e
    }

    /// Class entity for an Obsidian tag. Notes or dangling pages with the same
    /// name become the tag's page, matching Logseq where tags are pages.
    fn class_for_tag(&mut self, tag: &str) -> usize {
        let lower = tag.to_lowercase();
        if let Some(&e) = self.by_name.get(&lower) {
            let ent = &self.entities[e];
            if ent.class_ident.is_some() {
                return e;
            }
            if ent.journal.is_none() && ent.alias_of.is_none() {
                let ident =
                    properties::unique_ident("user.class", &ent.title, &mut self.class_idents);
                self.entities[e].class_ident = Some(ident);
                return e;
            }
            let title = self.claim_title(tag, "tag");
            return self.new_class(title);
        }
        let title = self.claim_title(tag, "tag");
        let e = self.new_class(title);
        self.by_name.insert(lower, e);
        e
    }

    fn new_class(&mut self, title: String) -> usize {
        let ident = properties::unique_ident("user.class", &title, &mut self.class_idents);
        let e = self.new_entity(title);
        self.entities[e].class_ident = Some(ident);
        e
    }

    fn asset_for(&mut self, attachment: usize) -> Option<Uuid> {
        if let Some(&a) = self.asset_by_attachment.get(&attachment) {
            return Some(self.assets[a].uuid);
        }
        if self.vault.attachments[attachment].ext.is_empty() {
            return None;
        }
        let uuid = Uuid::new_v4();
        self.assets.push(Asset { uuid, attachment });
        self.asset_by_attachment
            .insert(attachment, self.assets.len() - 1);
        Some(uuid)
    }

    // ---------------------------------------------------------------------
    // Link resolution
    // ---------------------------------------------------------------------

    /// Resolves an Obsidian link to the UUID of a page, block or asset.
    /// Returns the UUID and a display label to use when the link text differs
    /// from the target's title (aliases).
    fn resolve_link(
        &mut self,
        target: &str,
        sub: Option<&str>,
        note: usize,
        create_missing: bool,
    ) -> Option<(Uuid, Option<String>)> {
        let target = target.trim();
        let (entity, target_note, label) = if target.is_empty() {
            (self.note_entity[note], Some(note), None)
        } else {
            match self.vault.resolve(target, Some(note)) {
                Some(Target::Note(j)) => (self.note_entity[j], Some(j), None),
                Some(Target::Attachment(a)) => return self.asset_for(a).map(|u| (u, None)),
                None => {
                    let lower = target.to_lowercase();
                    let alias_target = self
                        .by_name
                        .get(&lower)
                        .and_then(|&e| self.entities[e].alias_of);
                    if let Some(owner) = alias_target {
                        let owner_note = self.entities[owner].notes.first().copied();
                        (owner, owner_note, Some(target.to_string()))
                    } else if create_missing {
                        (self.page_for_name(target), None, None)
                    } else {
                        return None;
                    }
                }
            }
        };
        if let (Some(sub), Some(n)) = (sub.map(str::trim).filter(|s| !s.is_empty()), target_note) {
            let found = if let Some(id) = sub.strip_prefix('^') {
                self.block_ids[n].get(&id.to_lowercase()).copied()
            } else {
                let wanted = normalize_heading(sub.rsplit('#').next().unwrap_or(sub));
                self.headings[n]
                    .iter()
                    .find(|(h, _)| *h == wanted)
                    .map(|(_, u)| *u)
            };
            if let Some(uuid) = found {
                return Some((uuid, label));
            }
        }
        Some((self.entities[entity].uuid, label))
    }

    // ---------------------------------------------------------------------
    // Rendering
    // ---------------------------------------------------------------------

    fn render_note(&mut self, note: usize, file: &ParsedFile) {
        let e = self.note_entity[note];
        self.render_frontmatter(note, e, &file.frontmatter);
        let (created, modified) = {
            let n = &self.vault.notes[note];
            (n.created_ms, n.modified_ms)
        };
        let blocks: Vec<Edn> = file
            .blocks
            .iter()
            .filter_map(|b| self.render_block(b, note, (created, modified)))
            .collect();
        self.entities[e].blocks.extend(blocks);
    }

    fn render_frontmatter(&mut self, note: usize, e: usize, fm: &[(String, Value)]) {
        for (key, value) in fm {
            let lower = key.to_lowercase();
            if ALIAS_KEYS.contains(&lower.as_str()) || DROPPED_KEYS.contains(&lower.as_str()) {
                continue;
            }
            if TAG_KEYS.contains(&lower.as_str()) {
                let tags = properties::tag_values(value);
                if self.opts.tags_as_pages {
                    let mut refs = Vec::new();
                    for tag in &tags {
                        let page = self.page_for_name(tag);
                        refs.push(Edn::uuid_ref(self.entities[page].uuid));
                    }
                    if !refs.is_empty() {
                        self.push_prop(e, "tags", dedupe(refs), true);
                    }
                } else {
                    for tag in tags {
                        let c = self.class_for_tag(&tag);
                        if c != e && !self.entities[e].tags.contains(&c) {
                            self.entities[e].tags.push(c);
                        }
                    }
                }
                continue;
            }
            let Some(schema) = self.schemas.get(&lower).cloned() else {
                continue;
            };
            let items: Vec<&Value> = match value {
                Value::Sequence(items) => items.iter().collect(),
                other => vec![other],
            };
            let values: Vec<Edn> = items
                .into_iter()
                .filter_map(|v| self.property_value(&schema, v, note))
                .collect();
            if values.is_empty() {
                continue;
            }
            let values = dedupe(values);
            self.push_prop(e, &lower, values, schema.many);
        }
    }

    fn push_prop(&mut self, e: usize, key: &str, values: Vec<Edn>, many: bool) {
        let ident = self.schemas[key].ident.clone();
        self.used_props.insert(key.to_string());
        let value = if many {
            Edn::Set(values)
        } else {
            values.into_iter().next().expect("caller checks non-empty")
        };
        let props = &mut self.entities[e].props;
        if let Some(existing) = props.iter_mut().find(|(k, _)| *k == ident) {
            // Two daily notes merged into one journal page: last one wins.
            existing.1 = value;
        } else {
            props.push((ident, value));
        }
    }

    fn property_value(&mut self, schema: &PropSchema, value: &Value, note: usize) -> Option<Edn> {
        match schema.ptype {
            PropType::Default => {
                let text = properties::value_to_text(value)?;
                let text = self.render_inline(&text, note, &mut Vec::new(), false);
                (!text.trim().is_empty()).then_some(Edn::Str(text))
            }
            PropType::Number => match value {
                Value::Number(n) => n
                    .as_i64()
                    .map(Edn::Int)
                    .or_else(|| n.as_f64().map(Edn::Float)),
                _ => None,
            },
            PropType::Checkbox => match value {
                Value::Bool(b) => Some(Edn::Bool(*b)),
                _ => None,
            },
            PropType::Url => {
                properties::scalar_string(value).map(|s| Edn::Str(s.trim().to_string()))
            }
            PropType::Date => {
                let s = properties::scalar_string(value)?;
                let date = properties::parse_iso_date(&s)?;
                let j = self.journal_for(date);
                Some(Edn::uuid_ref(self.entities[j].uuid))
            }
            PropType::Node => {
                let s = properties::scalar_string(value)?;
                let inner = properties::single_wikilink(&s).unwrap_or(s.trim());
                let (target, sub, _) = split_wikilink(inner);
                let (uuid, _) = self.resolve_link(&target, sub.as_deref(), note, true)?;
                Some(Edn::uuid_ref(uuid))
            }
        }
    }

    fn render_block(&mut self, block: &MdBlock, note: usize, ts: (i64, i64)) -> Option<Edn> {
        let mut tags: Vec<usize> = Vec::new();
        let mut builtin_tags: Vec<&str> = Vec::new();
        let mut props: Vec<(Edn, Edn)> = Vec::new();
        let mut extra: Vec<(Edn, Edn)> = Vec::new();
        let title = match &block.kind {
            BlockKind::Text => {
                if let Some((uuid, _)) = self.standalone_embed(&block.text, note) {
                    extra.push((Edn::kw("block/link"), Edn::uuid_ref(uuid)));
                    String::new()
                } else {
                    let mut text = self.render_inline(&block.text, note, &mut tags, true);
                    if let Some(c) = block.task {
                        let meta = extract_task_meta(&text);
                        text = meta.text;
                        builtin_tags.push("logseq.class/Task");
                        props.push((Edn::kw("logseq.property/status"), Edn::kw(task_status(c))));
                        if let Some(p) = meta.priority {
                            props.push((Edn::kw("logseq.property/priority"), Edn::kw(p)));
                        }
                        if let Some(d) = meta.deadline {
                            props.push((
                                Edn::kw("logseq.property/deadline"),
                                Edn::Int(local_midnight_ms(d)),
                            ));
                        }
                        if let Some(d) = meta.scheduled {
                            props.push((
                                Edn::kw("logseq.property/scheduled"),
                                Edn::Int(local_midnight_ms(d)),
                            ));
                        }
                    }
                    text
                }
            }
            BlockKind::Heading(level) => {
                props.push((Edn::kw("logseq.property/heading"), Edn::Int(*level as i64)));
                self.render_inline(&block.text, note, &mut tags, true)
            }
            BlockKind::Code { lang } => {
                builtin_tags.push("logseq.class/Code-block");
                props.push((
                    Edn::kw("logseq.property.node/display-type"),
                    Edn::kw("code"),
                ));
                if let Some(lang) = lang {
                    props.push((Edn::kw("logseq.property.code/lang"), Edn::str(lang.clone())));
                }
                block.text.clone()
            }
            BlockKind::Quote => {
                builtin_tags.push("logseq.class/Quote-block");
                props.push((
                    Edn::kw("logseq.property.node/display-type"),
                    Edn::kw("quote"),
                ));
                self.render_inline(&block.text, note, &mut tags, true)
            }
            BlockKind::Math => {
                builtin_tags.push("logseq.class/Math-block");
                props.push((
                    Edn::kw("logseq.property.node/display-type"),
                    Edn::kw("math"),
                ));
                block.text.clone()
            }
            // Logseq DB renders `#+BEGIN_QUOTE` as a deprecation warning, so
            // quote callouts become quote blocks with the title on top.
            BlockKind::Callout { kind, title } if matches!(kind.as_str(), "quote" | "cite") => {
                builtin_tags.push("logseq.class/Quote-block");
                props.push((
                    Edn::kw("logseq.property.node/display-type"),
                    Edn::kw("quote"),
                ));
                let body = self.render_inline(&block.text, note, &mut tags, true);
                if title.is_empty() {
                    body
                } else {
                    let title = self.render_inline(title, note, &mut tags, true);
                    if body.is_empty() {
                        format!("**{title}**")
                    } else {
                        format!("**{title}**\n{body}")
                    }
                }
            }
            BlockKind::Callout { kind, title } => {
                let (admonition, default_title) = callout_admonition(kind);
                let title = if title.is_empty() {
                    default_title.map(str::to_string).unwrap_or_default()
                } else {
                    self.render_inline(title, note, &mut tags, true)
                };
                let body = self.render_inline(&block.text, note, &mut tags, true);
                let mut s = format!("#+BEGIN_{admonition}\n");
                if !title.is_empty() {
                    s.push_str(&format!("**{title}**\n"));
                }
                if !body.is_empty() {
                    s.push_str(&body);
                    s.push('\n');
                }
                s.push_str(&format!("#+END_{admonition}"));
                s
            }
        };
        if block.ordered {
            props.push((
                Edn::kw("logseq.property/order-list-type"),
                Edn::str("number"),
            ));
        }
        let children: Vec<Edn> = block
            .children
            .iter()
            .filter_map(|c| self.render_block(c, note, ts))
            .collect();
        let has_link = !extra.is_empty();
        if title.trim().is_empty() && children.is_empty() && !has_link && block.task.is_none() {
            return None;
        }
        self.block_count += 1;

        let mut m: Vec<(Edn, Edn)> = vec![(Edn::kw("block/title"), Edn::Str(title))];
        if let Some(uuid) = block.uuid {
            m.push((Edn::kw("block/uuid"), Edn::Uuid(uuid)));
            m.push((Edn::kw("build/keep-uuid?"), Edn::Bool(true)));
        }
        m.push((Edn::kw("block/created-at"), Edn::Int(ts.0)));
        m.push((Edn::kw("block/updated-at"), Edn::Int(ts.1)));
        let mut tag_kws: Vec<Edn> = builtin_tags.into_iter().map(Edn::kw).collect();
        for t in tags {
            if let Some(ident) = &self.entities[t].class_ident {
                tag_kws.push(Edn::kw(ident.clone()));
            }
        }
        let tag_kws = dedupe(tag_kws);
        if !tag_kws.is_empty() {
            m.push((Edn::kw("build/tags"), Edn::Vector(tag_kws)));
        }
        if !props.is_empty() {
            m.push((Edn::kw("build/properties"), Edn::Map(props)));
        }
        m.extend(extra);
        if !children.is_empty() {
            m.push((Edn::kw("build/children"), Edn::Vector(children)));
        }
        Some(Edn::Map(m))
    }

    /// A paragraph consisting only of `![[note]]` becomes an embed block.
    fn standalone_embed(&mut self, text: &str, note: usize) -> Option<(Uuid, Option<String>)> {
        let inner = text.trim().strip_prefix("![[")?.strip_suffix("]]")?;
        if inner.contains("[[") || inner.contains("]]") {
            return None;
        }
        let (target, sub, _) = split_wikilink(inner);
        if let Some(Target::Attachment(_)) = self.vault.resolve(&target, Some(note)) {
            return None;
        }
        self.resolve_link(&target, sub.as_deref(), note, true)
    }

    /// Rewrites Obsidian inline syntax into Logseq DB syntax. Tags found are
    /// pushed to `tags` (as class entities) when `allow_tags` is set.
    fn render_inline(
        &mut self,
        text: &str,
        note: usize,
        tags: &mut Vec<usize>,
        allow_tags: bool,
    ) -> String {
        let mut out = String::with_capacity(text.len() + 16);
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            let c = rest.chars().next().unwrap();
            match c {
                '\\' => {
                    let len = 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
                    out.push_str(&rest[..len]);
                    i += len;
                }
                '`' => {
                    let len = code_span_len(rest);
                    out.push_str(&rest[..len]);
                    i += len;
                }
                '!' if rest.starts_with("![[") => match rest[3..].find("]]") {
                    Some(end) => {
                        let inner = &rest[3..3 + end];
                        out.push_str(&self.render_wikilink(inner, note, true));
                        i += 3 + end + 2;
                    }
                    None => {
                        out.push('!');
                        i += 1;
                    }
                },
                '[' if rest.starts_with("[[") => match rest[2..].find("]]") {
                    Some(end) => {
                        let inner = &rest[2..2 + end];
                        out.push_str(&self.render_wikilink(inner, note, false));
                        i += 2 + end + 2;
                    }
                    None => {
                        out.push('[');
                        i += 1;
                    }
                },
                '!' | '[' => {
                    let image = c == '!';
                    let start = if image { 1 } else { 0 };
                    match parse_md_link(&rest[start..]) {
                        Some(link) if rest[start..].starts_with('[') => {
                            let replacement = self.render_md_link(&link, image, note);
                            out.push_str(&replacement);
                            i += start + link.len;
                        }
                        _ => {
                            out.push(c);
                            i += 1;
                        }
                    }
                }
                '=' if rest.starts_with("==") => {
                    let close = rest[2..].find("==").map(|p| p + 2);
                    match close {
                        Some(end)
                            if end > 2
                                && !rest[2..end].starts_with(' ')
                                && !rest[2..end].ends_with(' ')
                                && !rest[2..end].contains('\n') =>
                        {
                            let inner = self.render_inline(&rest[2..end], note, tags, allow_tags);
                            out.push_str("^^");
                            out.push_str(&inner);
                            out.push_str("^^");
                            i += end + 2;
                        }
                        _ => {
                            out.push_str("==");
                            i += 2;
                        }
                    }
                }
                '#' if allow_tags
                    && (i == 0 || is_tag_boundary(text[..i].chars().next_back().unwrap())) =>
                {
                    let len = tag_len(&rest[1..]);
                    let tag = rest[1..1 + len].trim_end_matches('/');
                    if !tag.is_empty() && tag.chars().any(|c| !c.is_ascii_digit()) {
                        out.push_str(&self.render_tag(tag, tags));
                        i += 1 + tag.len();
                    } else {
                        out.push('#');
                        i += 1;
                    }
                }
                _ => {
                    out.push(c);
                    i += c.len_utf8();
                }
            }
        }
        out
    }

    fn render_tag(&mut self, tag: &str, tags: &mut Vec<usize>) -> String {
        if self.opts.tags_as_pages {
            let e = self.page_for_name(tag);
            return format!("[[{}]]", self.entities[e].uuid);
        }
        let e = self.class_for_tag(tag);
        if !tags.contains(&e) {
            tags.push(e);
        }
        format!("#[[{}]]", self.entities[e].uuid)
    }

    fn render_wikilink(&mut self, inner: &str, note: usize, embed: bool) -> String {
        let (target, sub, label) = split_wikilink(inner);
        // In embeds the part after `|` is a size (`![[img.png|300]]`).
        let label = if embed { None } else { label };
        match self.resolve_link(&target, sub.as_deref(), note, true) {
            Some((uuid, alias_label)) => match label.or(alias_label) {
                Some(label) => format!("[{}]([[{uuid}]])", escape_label(&label)),
                None => format!("[[{uuid}]]"),
            },
            None => {
                let prefix = if embed { "!" } else { "" };
                format!("{prefix}[[{inner}]]")
            }
        }
    }

    fn render_md_link(&mut self, link: &MdLink, image: bool, note: usize) -> String {
        let original = || format!("{}{}", if image { "!" } else { "" }, link.raw);
        let url = link.url.trim();
        if image && let Some(embed) = web_embed(url) {
            return embed;
        }
        if url.is_empty() || has_scheme(url) || url.starts_with('#') {
            return original();
        }
        let decoded = percent_encoding::percent_decode_str(url)
            .decode_utf8_lossy()
            .to_string();
        let (path, fragment) = match decoded.split_once('#') {
            Some((p, f)) => (p.to_string(), Some(f.to_string())),
            None => (decoded, None),
        };
        match self.vault.resolve(&path, Some(note)) {
            Some(Target::Attachment(a)) => match self.asset_for(a) {
                Some(uuid) if image => format!("[[{uuid}]]"),
                Some(uuid) => format!("[{}]([[{uuid}]])", escape_label(&link.label)),
                None => original(),
            },
            Some(Target::Note(_)) => {
                match self.resolve_link(&path, fragment.as_deref(), note, false) {
                    Some((uuid, _)) if link.label.trim().is_empty() || image => {
                        format!("[[{uuid}]]")
                    }
                    Some((uuid, _)) => format!("[{}]([[{uuid}]])", escape_label(&link.label)),
                    None => original(),
                }
            }
            None => original(),
        }
    }

    // ---------------------------------------------------------------------
    // Output
    // ---------------------------------------------------------------------

    fn finish(mut self) -> Conversion {
        let mut stats = Stats {
            notes: self.vault.notes.len(),
            blocks: self.block_count,
            ..Default::default()
        };
        let mut pages = Vec::new();
        let mut classes = Vec::new();
        let mut sample_page_uuids = Vec::new();
        let entities = std::mem::take(&mut self.entities);
        for ent in &entities {
            let (created, modified) = self.entity_times(ent);
            let tags: Vec<Edn> = dedupe(
                ent.tags
                    .iter()
                    .filter_map(|&t| entities[t].class_ident.clone())
                    .map(Edn::kw)
                    .collect(),
            );
            let props: Vec<(Edn, Edn)> = ent
                .props
                .iter()
                .map(|(k, v)| (Edn::kw(k.clone()), v.clone()))
                .collect();
            if let Some(ident) = &ent.class_ident {
                stats.classes += 1;
                let mut c = vec![
                    (Edn::kw("block/title"), Edn::str(ent.title.clone())),
                    (Edn::kw("block/uuid"), Edn::Uuid(ent.uuid)),
                    (Edn::kw("build/keep-uuid?"), Edn::Bool(true)),
                ];
                if let (Some(c0), Some(m0)) = (created, modified) {
                    c.push((Edn::kw("block/created-at"), Edn::Int(c0)));
                    c.push((Edn::kw("block/updated-at"), Edn::Int(m0)));
                }
                if !props.is_empty() {
                    c.push((Edn::kw("build/properties"), Edn::Map(props)));
                }
                if !ent.tags.is_empty() || !ent.aliases.is_empty() {
                    self.warnings.push(format!(
                        "{:?} is both a note and a tag; its own tags/aliases were dropped",
                        ent.title
                    ));
                }
                classes.push((Edn::kw(ident.clone()), Edn::Map(c)));
                if !ent.blocks.is_empty() {
                    pages.push(Edn::Map(vec![
                        (
                            Edn::kw("page"),
                            Edn::Map(vec![(Edn::kw("block/uuid"), Edn::Uuid(ent.uuid))]),
                        ),
                        (Edn::kw("blocks"), Edn::Vector(ent.blocks.clone())),
                    ]));
                }
                continue;
            }
            let mut page: Vec<(Edn, Edn)> = Vec::new();
            if let Some(date) = ent.journal {
                stats.journals += 1;
                let day =
                    date.year() as i64 * 10000 + date.month() as i64 * 100 + date.day() as i64;
                page.push((Edn::kw("build/journal"), Edn::Int(day)));
            } else {
                page.push((Edn::kw("block/title"), Edn::str(ent.title.clone())));
                if ent.alias_of.is_some() {
                    stats.alias_pages += 1;
                } else if ent.notes.is_empty() {
                    stats.placeholder_pages += 1;
                } else {
                    stats.pages += 1;
                    if sample_page_uuids.len() < 3 {
                        sample_page_uuids.push(ent.uuid);
                    }
                }
            }
            page.push((Edn::kw("block/uuid"), Edn::Uuid(ent.uuid)));
            page.push((Edn::kw("build/keep-uuid?"), Edn::Bool(true)));
            if let (Some(c0), Some(m0)) = (created, modified) {
                page.push((Edn::kw("block/created-at"), Edn::Int(c0)));
                page.push((Edn::kw("block/updated-at"), Edn::Int(m0)));
            }
            if !tags.is_empty() {
                let mut tags = tags;
                if ent.journal.is_some() {
                    // Explicit so user tags don't replace the #Journal tag.
                    tags.insert(0, Edn::kw("logseq.class/Journal"));
                }
                page.push((Edn::kw("build/tags"), Edn::Vector(tags)));
            }
            if !props.is_empty() {
                page.push((Edn::kw("build/properties"), Edn::Map(props)));
            }
            if !ent.aliases.is_empty() {
                let aliases = ent
                    .aliases
                    .iter()
                    .map(|&a| Edn::uuid_ref(entities[a].uuid))
                    .collect();
                page.push((Edn::kw("block/alias"), Edn::Set(aliases)));
            }
            let mut entry = vec![(Edn::kw("page"), Edn::Map(page))];
            if !ent.blocks.is_empty() {
                entry.push((Edn::kw("blocks"), Edn::Vector(ent.blocks.clone())));
            }
            pages.push(Edn::Map(entry));
        }

        let mut asset_files = Vec::new();
        let mut asset_blocks = Vec::new();
        for asset in &self.assets {
            let att = &self.vault.attachments[asset.attachment];
            let data = match fs::read(&att.abs_path) {
                Ok(d) => d,
                Err(e) => {
                    self.warnings
                        .push(format!("{}: cannot read attachment ({e})", att.rel_path));
                    Vec::new()
                }
            };
            let checksum = hex(&Sha256::digest(&data));
            asset_blocks.push(Edn::Map(vec![
                (Edn::kw("block/title"), Edn::str(att.stem.clone())),
                (Edn::kw("block/uuid"), Edn::Uuid(asset.uuid)),
                (Edn::kw("build/keep-uuid?"), Edn::Bool(true)),
                (Edn::kw("block/created-at"), Edn::Int(att.created_ms)),
                (Edn::kw("block/updated-at"), Edn::Int(att.modified_ms)),
                (
                    Edn::kw("build/tags"),
                    Edn::Vector(vec![Edn::kw("logseq.class/Asset")]),
                ),
                (
                    Edn::kw("build/properties"),
                    Edn::Map(vec![
                        (
                            Edn::kw("logseq.property.asset/type"),
                            Edn::str(att.ext.clone()),
                        ),
                        (
                            Edn::kw("logseq.property.asset/checksum"),
                            Edn::Str(checksum),
                        ),
                        (
                            Edn::kw("logseq.property.asset/size"),
                            Edn::Int(data.len() as i64),
                        ),
                    ]),
                ),
            ]));
            asset_files.push(AssetFile {
                source: att.abs_path.clone(),
                file_name: format!("{}.{}", asset.uuid, att.ext),
            });
        }
        if !asset_blocks.is_empty() {
            // The page is matched by title to Logseq's existing Asset class page.
            pages.push(Edn::Map(vec![
                (
                    Edn::kw("page"),
                    Edn::Map(vec![(Edn::kw("block/title"), Edn::str(ASSET_PAGE_TITLE))]),
                ),
                (Edn::kw("blocks"), Edn::Vector(asset_blocks)),
            ]));
        }
        stats.assets = asset_files.len();
        stats.unreferenced_attachments = self.vault.attachments.len() - asset_files.len();

        let mut prop_defs = Vec::new();
        for key in &self.used_props {
            let s = &self.schemas[key];
            prop_defs.push((
                Edn::kw(s.ident.clone()),
                Edn::Map(vec![
                    (Edn::kw("logseq.property/type"), Edn::kw(s.ptype.keyword())),
                    (
                        Edn::kw("db/cardinality"),
                        Edn::kw(if s.many {
                            "db.cardinality/many"
                        } else {
                            "db.cardinality/one"
                        }),
                    ),
                    (Edn::kw("block/title"), Edn::str(s.title.clone())),
                    // A known UUID lets later property batches reference the
                    // property entity (see phases::property_batch).
                    (Edn::kw("block/uuid"), Edn::Uuid(Uuid::new_v4())),
                    (Edn::kw("build/keep-uuid?"), Edn::Bool(true)),
                ]),
            ));
        }
        stats.properties = prop_defs.len();

        let edn = Edn::Map(vec![
            (Edn::kw("properties"), Edn::Map(prop_defs)),
            (Edn::kw("classes"), Edn::Map(classes)),
            (Edn::kw("pages-and-blocks"), Edn::Vector(pages)),
            (
                Edn::kw("logseq.db.sqlite.export/export-type"),
                Edn::kw("graph-human"),
            ),
        ]);
        Conversion {
            edn,
            assets: asset_files,
            stats,
            warnings: self.warnings,
            sample_page_uuids,
        }
    }

    fn entity_times(&self, ent: &Entity) -> (Option<i64>, Option<i64>) {
        let notes = ent.notes.iter().map(|&n| &self.vault.notes[n]);
        let created = notes.clone().map(|n| n.created_ms).min();
        let modified = notes.map(|n| n.modified_ms).max();
        (created, modified)
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

/// Logseq's deterministic journal page UUID: `00000001-YYYY-MMDD-0000-000000000000`.
pub fn journal_uuid(date: NaiveDate) -> Uuid {
    let s = format!(
        "00000001-{:04}-{:02}{:02}-0000-000000000000",
        date.year(),
        date.month(),
        date.day()
    );
    Uuid::parse_str(&s).expect("valid journal uuid")
}

/// Splits `target#sub|label` (also accepting `\|` used inside tables).
fn split_wikilink(inner: &str) -> (String, Option<String>, Option<String>) {
    let inner = inner.replace("\\|", "|");
    let (link, label) = match inner.split_once('|') {
        Some((l, a)) => (
            l.to_string(),
            Some(a.trim().to_string()).filter(|a| !a.is_empty()),
        ),
        None => (inner.clone(), None),
    };
    let (target, sub) = match link.split_once('#') {
        Some((t, s)) => (t.trim().to_string(), Some(s.to_string())),
        None => (link.trim().to_string(), None),
    };
    (target, sub, label)
}

fn normalize_heading(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                c,
                '[' | ']' | '*' | '_' | '`' | '#' | '|' | '^' | ':' | '=' | '~'
            )
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn escape_label(label: &str) -> String {
    label.replace('[', "\\[").replace(']', "\\]")
}

/// Obsidian embeds YouTube videos and tweets with image syntax
/// (`![](https://youtu.be/…)`); Logseq uses `{{video …}}` / `{{tweet …}}`.
fn web_embed(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host
        .strip_prefix("www.")
        .or_else(|| host.strip_prefix("m."))
        .unwrap_or(host);
    match host {
        "youtube.com" | "youtu.be" | "youtube-nocookie.com" => Some(format!("{{{{video {url}}}}}")),
        "twitter.com" | "x.com" if path.contains("/status/") => {
            Some(format!("{{{{tweet {url}}}}}"))
        }
        _ => None,
    }
}

fn has_scheme(url: &str) -> bool {
    match url.find(':') {
        Some(i) if i > 1 => url[..i]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')),
        _ => false,
    }
}

fn is_tag_boundary(prev: char) -> bool {
    prev.is_whitespace() || matches!(prev, '(' | ',' | ';')
}

/// Length in bytes of the tag name starting at `s` (after the `#`).
fn tag_len(s: &str) -> usize {
    s.char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || matches!(c, '_' | '-' | '/')))
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn dedupe(items: Vec<Edn>) -> Vec<Edn> {
    let mut out: Vec<Edn> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn task_status(c: char) -> &'static str {
    match c {
        'x' | 'X' => "logseq.property/status.done",
        '/' => "logseq.property/status.doing",
        '-' => "logseq.property/status.canceled",
        _ => "logseq.property/status.todo",
    }
}

fn local_midnight_ms(date: NaiveDate) -> i64 {
    let dt = date.and_hms_opt(0, 0, 0).expect("midnight exists");
    Local
        .from_local_datetime(&dt)
        .earliest()
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|| dt.and_utc().timestamp_millis())
}

/// Maps an Obsidian callout type to a Logseq admonition and, for types
/// without a direct equivalent, a title that keeps the original meaning.
fn callout_admonition(kind: &str) -> (&'static str, Option<&'static str>) {
    match kind {
        "note" => ("NOTE", None),
        "tip" => ("TIP", None),
        "hint" => ("TIP", Some("Hint")),
        "success" | "check" | "done" => ("TIP", Some("Success")),
        "important" => ("IMPORTANT", None),
        "warning" => ("WARNING", None),
        "attention" => ("WARNING", Some("Attention")),
        "caution" => ("CAUTION", None),
        "danger" | "error" => ("CAUTION", Some("Danger")),
        "failure" | "fail" | "missing" => ("CAUTION", Some("Failure")),
        "bug" => ("CAUTION", Some("Bug")),
        "abstract" | "summary" | "tldr" => ("NOTE", Some("Summary")),
        "info" => ("NOTE", Some("Info")),
        "todo" => ("NOTE", Some("Todo")),
        "question" | "help" | "faq" => ("NOTE", Some("Question")),
        "example" => ("NOTE", Some("Example")),
        _ => ("NOTE", None),
    }
}

struct TaskMeta {
    text: String,
    priority: Option<&'static str>,
    deadline: Option<NaiveDate>,
    scheduled: Option<NaiveDate>,
}

/// Extracts Obsidian Tasks plugin metadata (`📅 2024-01-01`, `⏫`, ...).
fn extract_task_meta(text: &str) -> TaskMeta {
    let mut meta = TaskMeta {
        text: text.to_string(),
        priority: None,
        deadline: None,
        scheduled: None,
    };
    for (emoji, prio) in [
        ("🔺", "logseq.property/priority.urgent"),
        ("⏫", "logseq.property/priority.high"),
        ("🔼", "logseq.property/priority.medium"),
        ("🔽", "logseq.property/priority.low"),
        ("⏬", "logseq.property/priority.low"),
    ] {
        if meta.text.contains(emoji) {
            meta.priority.get_or_insert(prio);
            meta.text = meta.text.replace(emoji, "");
        }
    }
    for (emoji, is_deadline) in [("📅", true), ("⏳", false)] {
        if let Some(pos) = meta.text.find(emoji) {
            let after = meta.text[pos + emoji.len()..].trim_start();
            if let Some(date) = after.get(..10).and_then(properties::parse_iso_date) {
                let consumed = meta.text.len() - after.len() + 10;
                meta.text = format!("{}{}", &meta.text[..pos], &meta.text[consumed..]);
                if is_deadline {
                    meta.deadline = Some(date);
                } else {
                    meta.scheduled = Some(date);
                }
            }
        }
    }
    meta.text = meta.text.split_whitespace().collect::<Vec<_>>().join(" ");
    meta
}

struct MdLink {
    label: String,
    url: String,
    raw: String,
    /// Bytes consumed starting at `[`.
    len: usize,
}

/// Parses `[label](url "title")` at the start of `s`.
fn parse_md_link(s: &str) -> Option<MdLink> {
    if !s.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    let mut label_end = None;
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    label_end = Some(i);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let label_end = label_end?;
    let after = &s[label_end + 1..];
    if !after.starts_with('(') {
        return None;
    }
    let mut depth = 0;
    let mut url_end = None;
    let mut in_angle = false;
    for (i, c) in after.char_indices() {
        match c {
            '<' if i == 1 => in_angle = true,
            '>' if in_angle => in_angle = false,
            '(' if !in_angle => depth += 1,
            ')' if !in_angle => {
                depth -= 1;
                if depth == 0 {
                    url_end = Some(i);
                    break;
                }
            }
            '\n' => return None,
            _ => {}
        }
    }
    let url_end = url_end?;
    let dest = after[1..url_end].trim();
    let url = if let Some(stripped) = dest.strip_prefix('<') {
        stripped.split('>').next().unwrap_or("").to_string()
    } else {
        dest.split_whitespace().next().unwrap_or("").to_string()
    };
    let len = label_end + 1 + url_end + 1;
    Some(MdLink {
        label: s[1..label_end].to_string(),
        url,
        raw: s[..len].to_string(),
        len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_uuid_format() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
        assert_eq!(
            journal_uuid(d).to_string(),
            "00000001-2026-1012-0000-000000000000"
        );
    }

    #[test]
    fn wikilink_parts() {
        assert_eq!(
            split_wikilink("Note#Head|Label"),
            ("Note".into(), Some("Head".into()), Some("Label".into()))
        );
        assert_eq!(
            split_wikilink("#^abc"),
            ("".into(), Some("^abc".into()), None)
        );
        assert_eq!(
            split_wikilink("A\\|B"),
            ("A".into(), None, Some("B".into()))
        );
    }

    #[test]
    fn md_links() {
        let l = parse_md_link("[a [b]](some%20file.md \"t\") rest").unwrap();
        assert_eq!(l.label, "a [b]");
        assert_eq!(l.url, "some%20file.md");
        assert_eq!(l.raw, "[a [b]](some%20file.md \"t\")");
        let l = parse_md_link("[x](<a b.png>)").unwrap();
        assert_eq!(l.url, "a b.png");
        assert!(parse_md_link("[x] (y)").is_none());
    }

    #[test]
    fn task_meta() {
        let m = extract_task_meta("Pay bills ⏫ 📅 2024-02-01 ⏳ 2024-01-30");
        assert_eq!(m.text, "Pay bills");
        assert_eq!(m.priority, Some("logseq.property/priority.high"));
        assert_eq!(m.deadline, NaiveDate::from_ymd_opt(2024, 2, 1));
        assert_eq!(m.scheduled, NaiveDate::from_ymd_opt(2024, 1, 30));
    }

    #[test]
    fn web_embeds() {
        let yt = "https://www.youtube.com/watch?v=1adNxqAtYMQ";
        assert_eq!(web_embed(yt), Some(format!("{{{{video {yt}}}}}")));
        assert_eq!(
            web_embed("https://youtu.be/1adNxqAtYMQ").as_deref(),
            Some("{{video https://youtu.be/1adNxqAtYMQ}}")
        );
        assert_eq!(
            web_embed("https://x.com/obsdmd/status/123").as_deref(),
            Some("{{tweet https://x.com/obsdmd/status/123}}")
        );
        assert_eq!(web_embed("https://x.com/obsdmd"), None);
        assert_eq!(web_embed("https://example.com/a.png"), None);
        assert_eq!(web_embed("attachments/a.png"), None);
    }

    #[test]
    fn schemes() {
        assert!(has_scheme("https://x"));
        assert!(has_scheme("obsidian://open"));
        assert!(has_scheme("mailto:a@b"));
        assert!(!has_scheme("C:/x"));
        assert!(!has_scheme("folder/note.md"));
    }
}
