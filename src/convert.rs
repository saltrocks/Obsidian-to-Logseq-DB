//! Converts a scanned vault into Logseq's `:graph-human` EDN import format.
//!
//! Every page, tag (class), journal, referenced block and asset gets an
//! explicit UUID with `:build/keep-uuid? true`, and all links in block text
//! are rewritten to `[[uuid]]` so Logseq derives `:block/refs` on import.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, NaiveDateTime, TimeZone};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use yaml_serde::Value;

use crate::edn::Edn;
use crate::markdown::{BlockKind, MdBlock, ParseOptions, code_span_len, parse_note};
use crate::properties::{
    self, ALIAS_KEYS, DROPPED_KEYS, PropSchema, PropType, SchemaBuilder, TAG_KEYS,
};
use crate::vault::{Bookmark, Target, Vault};

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
/// Title of Logseq's hidden page whose blocks link to the favorites.
const FAVORITES_PAGE_TITLE: &str = "$$$favorites";

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
    pub templates: usize,
    pub favorites: usize,
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
    /// Parent tag of a nested tag (`#parent/child`).
    class_extends: Option<usize>,
    alias_of: Option<usize>,
    aliases: Vec<usize>,
    blocks: Vec<Edn>,
    props: Vec<(String, Edn)>,
    tags: Vec<usize>,
}

struct Asset {
    uuid: Uuid,
    attachment: usize,
    /// Display width from an embed like `![[img.png|300]]`.
    width: Option<i64>,
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
    template_count: usize,
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
        template_count: 0,
    };
    let mut parsed = conv.parse_notes()?;
    for (i, file) in parsed.iter_mut().enumerate() {
        attach_footnotes(&mut file.blocks);
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
            let source = if self.vault.is_template(note) {
                std::borrow::Cow::Owned(template_variables(&source))
            } else {
                source
            };
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
        for file in parsed {
            observe_task_dates(&file.blocks, &mut builder);
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
        let tag = tag.trim_matches('/');
        let lower = tag.to_lowercase();
        if let Some((parent, leaf)) = tag.rsplit_once('/')
            && !parent.is_empty()
            && !leaf.is_empty()
        {
            return self.nested_class(tag, parent, leaf);
        }
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

    /// Logseq has no `/` namespaces for tags: `#area/work` becomes the tag
    /// `work` extending the tag `area`, as Logseq's own importer does. Tags
    /// may share a title when their parents differ, so the leaf title isn't
    /// claimed.
    fn nested_class(&mut self, tag: &str, parent: &str, leaf: &str) -> usize {
        let lower = tag.to_lowercase();
        if let Some(&e) = self.by_name.get(&lower)
            && self.entities[e].class_ident.is_some()
        {
            return e;
        }
        let parent = self.class_for_tag(parent);
        let ident = properties::unique_ident("user.class", tag, &mut self.class_idents);
        let e = self.new_entity(leaf.to_string());
        self.entities[e].class_ident = Some(ident);
        self.entities[e].class_extends = Some(parent);
        self.by_name.entry(lower).or_insert(e);
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
        self.assets.push(Asset {
            uuid,
            attachment,
            width: None,
        });
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
        if self.vault.is_template(&self.vault.notes[note]) && !blocks.is_empty() {
            // A Logseq template is a #Template block whose children are
            // inserted; its title is the template's name.
            self.template_count += 1;
            self.block_count += 1;
            let template = Edn::Map(vec![
                (
                    Edn::kw("block/title"),
                    Edn::str(self.vault.notes[note].stem.clone()),
                ),
                (Edn::kw("block/created-at"), Edn::Int(created)),
                (Edn::kw("block/updated-at"), Edn::Int(modified)),
                (
                    Edn::kw("build/tags"),
                    Edn::Vector(vec![Edn::kw("logseq.class/Template")]),
                ),
                (Edn::kw("build/children"), Edn::Vector(blocks)),
            ]);
            self.entities[e].blocks.push(template);
        } else {
            self.entities[e].blocks.extend(blocks);
        }
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
            PropType::DateTime => {
                let s = properties::scalar_string(value)?;
                let dt = properties::parse_iso_datetime(&s).or_else(|| {
                    properties::parse_iso_date(&s).and_then(|d| d.and_hms_opt(0, 0, 0))
                })?;
                Some(Edn::Int(local_ms(dt)))
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
        let mut created_at = ts.0;
        // Body of a folded callout, imported as a collapsed child block.
        let mut folded_body: Option<String> = None;
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
                        if let Some(r) = &meta.recurrence {
                            let temporal = if meta.deadline.is_some() {
                                "logseq.property/deadline"
                            } else {
                                "logseq.property/scheduled"
                            };
                            let repeat_type = if r.when_done {
                                "logseq.property.repeat/repeat-type.dotted-plus"
                            } else {
                                "logseq.property.repeat/repeat-type.plus"
                            };
                            props.extend([
                                (Edn::kw("logseq.property.repeat/repeated?"), Edn::Bool(true)),
                                (
                                    Edn::kw("logseq.property.repeat/temporal-property"),
                                    Edn::kw(temporal),
                                ),
                                (
                                    Edn::kw("logseq.property.repeat/repeat-type"),
                                    Edn::kw(repeat_type),
                                ),
                                (
                                    Edn::kw("logseq.property.repeat/recur-frequency"),
                                    Edn::Int(r.frequency),
                                ),
                                (
                                    Edn::kw("logseq.property.repeat/recur-unit"),
                                    Edn::kw(r.unit),
                                ),
                            ]);
                        }
                        if let Some(d) = meta.created {
                            created_at = local_midnight_ms(d);
                        }
                        for (i, (_, prop)) in TASK_DATE_PROPS.iter().enumerate() {
                            if let Some(d) = meta.other_dates[i]
                                && let Some(p) = self.task_date_property(prop, d)
                            {
                                props.push(p);
                            }
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
            BlockKind::Callout {
                kind,
                title,
                folded,
            } if matches!(kind.as_str(), "quote" | "cite") => {
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
                    } else if *folded {
                        folded_body = Some(body);
                        format!("**{title}**")
                    } else {
                        format!("**{title}**\n{body}")
                    }
                }
            }
            BlockKind::Callout {
                kind,
                title,
                folded,
            } => {
                let (admonition, default_title) = callout_admonition(kind);
                let mut body = self.render_inline(&block.text, note, &mut tags, true);
                if *folded && !body.is_empty() {
                    folded_body = Some(std::mem::take(&mut body));
                }
                let title = if !title.is_empty() {
                    self.render_inline(title, note, &mut tags, true)
                } else if let Some(t) = default_title {
                    t
                } else if folded_body.is_some() {
                    // A folded callout shows only its title.
                    capitalize(kind)
                } else {
                    String::new()
                };
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
        let mut children: Vec<Edn> = Vec::new();
        let collapsed = folded_body.is_some();
        if let Some(body) = folded_body {
            self.block_count += 1;
            children.push(Edn::Map(vec![
                (Edn::kw("block/title"), Edn::Str(body)),
                (Edn::kw("block/created-at"), Edn::Int(ts.0)),
                (Edn::kw("block/updated-at"), Edn::Int(ts.1)),
            ]));
        }
        children.extend(
            block
                .children
                .iter()
                .filter_map(|c| self.render_block(c, note, ts)),
        );
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
        m.push((Edn::kw("block/created-at"), Edn::Int(created_at)));
        m.push((Edn::kw("block/updated-at"), Edn::Int(ts.1.max(created_at))));
        if collapsed {
            m.push((Edn::kw("block/collapsed?"), Edn::Bool(true)));
        }
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

    /// A Tasks plugin date stored in the user property titled `prop`.
    fn task_date_property(&mut self, prop: &str, date: NaiveDate) -> Option<(Edn, Edn)> {
        let key = prop.to_lowercase();
        let schema = self.schemas.get(&key)?.clone();
        let value = match schema.ptype {
            PropType::DateTime => Edn::Int(local_midnight_ms(date)),
            PropType::Date => {
                let j = self.journal_for(date);
                Edn::uuid_ref(self.entities[j].uuid)
            }
            PropType::Default => Edn::Str(date.format("%Y-%m-%d").to_string()),
            _ => return None,
        };
        self.used_props.insert(key);
        let value = if schema.many {
            Edn::Set(vec![value])
        } else {
            value
        };
        Some((Edn::kw(schema.ident), value))
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
        if embed && let Some(width) = label.as_deref().and_then(embed_width) {
            self.set_asset_width(&target, note, width);
        }
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

    /// Records an embed width on the asset. An asset is one block shared by
    /// all its embeds, so the first width wins.
    fn set_asset_width(&mut self, target: &str, note: usize, width: i64) {
        if let Some(Target::Attachment(a)) = self.vault.resolve(target, Some(note))
            && self.asset_for(a).is_some()
        {
            let asset = &mut self.assets[self.asset_by_attachment[&a]];
            asset.width.get_or_insert(width);
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
        // Obsidian sizes markdown images with `![alt|300](img.png)`.
        if image
            && let Some((_, size)) = link.label.rsplit_once('|')
            && let Some(width) = embed_width(size)
        {
            self.set_asset_width(&path, note, width);
        }
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
            templates: self.template_count,
            ..Default::default()
        };
        let favorites = self.favorites();
        stats.favorites = favorites.len();
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
                if let Some(parent) = ent.class_extends
                    && let Some(parent_ident) = &entities[parent].class_ident
                {
                    c.push((
                        Edn::kw("build/class-extends"),
                        Edn::Vector(vec![Edn::kw(parent_ident.clone())]),
                    ));
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
            let mut asset_props = vec![
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
            ];
            if let Some(width) = asset.width {
                asset_props.push((
                    Edn::kw("logseq.property.asset/resize-metadata"),
                    Edn::Map(vec![(Edn::kw("width"), Edn::Int(width))]),
                ));
            }
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
                (Edn::kw("build/properties"), Edn::Map(asset_props)),
            ]));
            asset_files.push(AssetFile {
                source: att.abs_path.clone(),
                file_name: format!("{}.{}", asset.uuid, att.ext),
            });
        }
        if !favorites.is_empty() {
            // Like the Asset page, matched by title to Logseq's own page.
            let blocks = favorites
                .into_iter()
                .map(|e| {
                    Edn::Map(vec![
                        (Edn::kw("block/title"), Edn::str("")),
                        (Edn::kw("block/link"), Edn::uuid_ref(entities[e].uuid)),
                    ])
                })
                .collect();
            pages.push(Edn::Map(vec![
                (
                    Edn::kw("page"),
                    Edn::Map(vec![(
                        Edn::kw("block/title"),
                        Edn::str(FAVORITES_PAGE_TITLE),
                    )]),
                ),
                (Edn::kw("blocks"), Edn::Vector(blocks)),
            ]));
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

    /// Entities of the notes bookmarked in Obsidian, in bookmark order.
    /// Headings and blocks are bookmarked as their note, since Logseq's
    /// favorites are pages.
    fn favorites(&mut self) -> Vec<usize> {
        let mut out = Vec::new();
        for bookmark in &self.vault.bookmarks {
            match bookmark {
                Bookmark::File { path } => match self.vault.resolve(path, None) {
                    Some(Target::Note(n)) => {
                        let e = self.note_entity[n];
                        if !out.contains(&e) {
                            out.push(e);
                        }
                    }
                    _ => self
                        .warnings
                        .push(format!("bookmark {path:?} skipped, no such note")),
                },
                Bookmark::Other { kind, title } => self.warnings.push(format!(
                    "{kind} bookmark {title:?} skipped, Logseq favorites are pages"
                )),
            }
        }
        out
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

// -------------------------------------------------------------------------
// Footnotes
// -------------------------------------------------------------------------

/// Logseq resolves footnotes within a block, while Obsidian collects the
/// definitions at the end of the note. Each referenced definition is moved
/// into the blocks that reference it, and inline footnotes (`^[text]`) are
/// numbered like Obsidian renders them.
fn attach_footnotes(blocks: &mut Vec<MdBlock>) {
    let mut labels = Vec::new();
    walk_texts(blocks, &mut |text| labels.extend(footnote_refs(text)));
    let mut next = labels
        .iter()
        .filter_map(|l| l.parse::<u32>().ok())
        .max()
        .unwrap_or(0)
        + 1;
    let mut defs: HashMap<String, String> = HashMap::new();
    walk_texts_mut(blocks, &mut |text| {
        if text.contains("^[") {
            *text = number_inline_footnotes(text, &mut next, &mut defs);
        }
    });
    collect_footnote_defs(blocks, &mut defs);
    if defs.is_empty() {
        return;
    }
    let mut referenced = BTreeSet::new();
    walk_texts(blocks, &mut |text| {
        if footnote_def_label(text).is_none() {
            referenced.extend(footnote_refs(text));
        }
    });
    remove_footnote_defs(blocks, &referenced);
    walk_texts_mut(blocks, &mut |text| {
        let mut appended = BTreeSet::new();
        for label in footnote_refs(text) {
            if let Some(def) = defs.get(&label)
                && appended.insert(label)
            {
                text.push_str("\n\n");
                text.push_str(def);
            }
        }
    });
}

fn has_inline_text(block: &MdBlock) -> bool {
    !matches!(block.kind, BlockKind::Code { .. } | BlockKind::Math)
}

fn walk_texts(blocks: &[MdBlock], f: &mut impl FnMut(&str)) {
    for b in blocks {
        if has_inline_text(b) {
            f(&b.text);
        }
        walk_texts(&b.children, f);
    }
}

fn walk_texts_mut(blocks: &mut [MdBlock], f: &mut impl FnMut(&mut String)) {
    for b in blocks {
        if has_inline_text(b) {
            f(&mut b.text);
        }
        walk_texts_mut(&mut b.children, f);
    }
}

/// Label of a footnote definition block (`[^label]: text`).
fn footnote_def_label(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("[^")?;
    let end = rest.find("]:")?;
    let label = &rest[..end];
    (!label.is_empty() && !label.contains(char::is_whitespace)).then_some(label)
}

fn collect_footnote_defs(blocks: &[MdBlock], defs: &mut HashMap<String, String>) {
    for b in blocks {
        if b.kind == BlockKind::Text
            && b.task.is_none()
            && b.children.is_empty()
            && let Some(label) = footnote_def_label(&b.text)
        {
            defs.entry(label.to_string())
                .or_insert_with(|| b.text.clone());
        }
        collect_footnote_defs(&b.children, defs);
    }
}

fn remove_footnote_defs(blocks: &mut Vec<MdBlock>, referenced: &BTreeSet<String>) {
    blocks.retain(|b| {
        !(b.kind == BlockKind::Text
            && b.task.is_none()
            && b.children.is_empty()
            && footnote_def_label(&b.text).is_some_and(|l| referenced.contains(l)))
    });
    for b in blocks {
        remove_footnote_defs(&mut b.children, referenced);
    }
}

/// Labels of footnote references (`[^label]`) outside code spans.
fn footnote_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('`') {
            i += code_span_len(rest);
        } else if let Some(after) = rest.strip_prefix("[^")
            && let Some(end) = after.find(']')
            && end > 0
            && !after[..end].contains(char::is_whitespace)
            && !after[end + 1..].starts_with(':')
        {
            out.push(after[..end].to_string());
            i += 2 + end + 1;
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// Replaces inline footnotes `^[text]` with numbered references and records
/// their definitions.
fn number_inline_footnotes(
    text: &str,
    next: &mut u32,
    defs: &mut HashMap<String, String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with('`') {
            let len = code_span_len(rest);
            out.push_str(&rest[..len]);
            i += len;
            continue;
        }
        if rest.starts_with("^[") {
            let mut depth = 0;
            let close = rest[1..].char_indices().find_map(|(j, c)| {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                None
            });
            if let Some(end) = close {
                let label = next.to_string();
                *next += 1;
                defs.insert(label.clone(), format!("[^{label}]: {}", &rest[2..end]));
                out.push_str(&format!("[^{label}]"));
                i += end + 1;
                continue;
            }
        }
        let c = rest.chars().next().expect("non-empty");
        out.push(c);
        i += c.len_utf8();
    }
    out
}

fn observe_task_dates(blocks: &[MdBlock], builder: &mut SchemaBuilder) {
    for block in blocks {
        if block.task.is_some() {
            let meta = extract_task_meta(&block.text);
            for (i, (_, prop)) in TASK_DATE_PROPS.iter().enumerate() {
                if meta.other_dates[i].is_some() {
                    builder.observe_type(prop, PropType::DateTime);
                }
            }
        }
        observe_task_dates(&block.children, builder);
    }
}

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

/// Rewrites Obsidian template variables to Logseq's: `{{date}}` and
/// `{{date:FORMAT}}` become today's journal page, `{{time}}` the current time
/// and `{{title}}` the page the template is inserted into.
fn template_variables(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            rest = &rest[start..];
            break;
        };
        let inner = &after[..end];
        let name = inner.split(':').next().unwrap_or("").trim().to_lowercase();
        match name.as_str() {
            "date" => out.push_str("<% today %>"),
            "time" => out.push_str("<% time %>"),
            "title" => out.push_str("<% current page %>"),
            _ => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
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

/// Width of an embed size like `300` or `300x200`.
fn embed_width(size: &str) -> Option<i64> {
    let size = size.trim();
    let width = size.split_once('x').map_or(size, |(w, _)| w);
    width.parse().ok().filter(|&w| w > 0)
}

fn escape_label(label: &str) -> String {
    label.replace('[', "\\[").replace(']', "\\]")
}

/// Obsidian embeds YouTube videos and tweets with image syntax
/// (`![](https://youtu.be/…)`); Logseq uses `{{youtube …}}` / `{{tweet …}}`.
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
        "youtube.com" | "youtu.be" | "youtube-nocookie.com" => {
            Some(format!("{{{{youtube {url}}}}}"))
        }
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

fn local_ms(dt: NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(&dt)
        .earliest()
        .map(|d| d.timestamp_millis())
        .unwrap_or_else(|| dt.and_utc().timestamp_millis())
}

fn local_midnight_ms(date: NaiveDate) -> i64 {
    local_ms(date.and_hms_opt(0, 0, 0).expect("midnight exists"))
}

/// Maps an Obsidian callout type to a Logseq admonition and, for types
/// without a direct equivalent, a title that keeps the original meaning.
fn callout_admonition(kind: &str) -> (&'static str, Option<String>) {
    let (admonition, title) = match kind {
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
        // Obsidian titles custom callouts with their type.
        _ => return ("NOTE", Some(capitalize(kind))),
    };
    (admonition, title.map(str::to_string))
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Tasks plugin dates without a Logseq built-in property, stored in user
/// properties with these titles.
const TASK_DATE_PROPS: [(&str, &str); 3] = [
    ("🛫", "Start date"),
    ("✅", "Done date"),
    ("❌", "Cancelled date"),
];

/// Tasks plugin signifiers; a recurrence rule runs until the next one.
const TASK_SIGNIFIERS: [&str; 15] = [
    "📅", "⏳", "🛫", "✅", "➕", "❌", "🔁", "🔺", "⏫", "🔼", "🔽", "⏬", "🆔", "⛔", "🏁",
];

#[derive(Debug, PartialEq)]
struct Recurrence {
    frequency: i64,
    unit: &'static str,
    when_done: bool,
}

#[derive(Debug, Default)]
struct TaskMeta {
    text: String,
    priority: Option<&'static str>,
    deadline: Option<NaiveDate>,
    scheduled: Option<NaiveDate>,
    created: Option<NaiveDate>,
    /// Values for [`TASK_DATE_PROPS`], by index.
    other_dates: [Option<NaiveDate>; 3],
    recurrence: Option<Recurrence>,
}

/// Extracts Obsidian Tasks plugin metadata (`📅 2024-01-01`, `⏫`, ...).
fn extract_task_meta(text: &str) -> TaskMeta {
    let mut meta = TaskMeta {
        text: text.to_string(),
        ..Default::default()
    };
    // Recurrences need a date to repeat from; otherwise the rule stays text.
    if (text.contains("📅") || text.contains("⏳"))
        && let Some(pos) = meta.text.find("🔁")
    {
        let after = &meta.text[pos + "🔁".len()..];
        let end = TASK_SIGNIFIERS
            .iter()
            .filter_map(|s| after.find(s))
            .min()
            .unwrap_or(after.len());
        if let Some(r) = parse_recurrence(&after[..end]) {
            meta.recurrence = Some(r);
            meta.text = format!("{}{}", &meta.text[..pos], &after[end..]);
        }
    }
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
    let mut take_date = |emoji: &str| -> Option<NaiveDate> {
        let pos = meta.text.find(emoji)?;
        let after = meta.text[pos + emoji.len()..].trim_start();
        let date = after.get(..10).and_then(properties::parse_iso_date)?;
        let consumed = meta.text.len() - after.len() + 10;
        meta.text = format!("{}{}", &meta.text[..pos], &meta.text[consumed..]);
        Some(date)
    };
    let deadline = take_date("📅");
    let scheduled = take_date("⏳");
    let created = take_date("➕");
    let other_dates = TASK_DATE_PROPS.map(|(emoji, _)| take_date(emoji));
    meta.deadline = deadline;
    meta.scheduled = scheduled;
    meta.created = created;
    meta.other_dates = other_dates;
    meta.text = meta.text.split_whitespace().collect::<Vec<_>>().join(" ");
    meta
}

/// Parses simple Tasks plugin rules: `every day`, `every 2 weeks`,
/// `every month when done`. Rules naming weekdays or dates have no Logseq
/// equivalent and return `None`.
fn parse_recurrence(rule: &str) -> Option<Recurrence> {
    let rule = rule.trim().to_lowercase();
    let rule = rule.strip_prefix("every ")?;
    let (rule, when_done) = match rule.strip_suffix(" when done") {
        Some(r) => (r.trim(), true),
        None => (rule.trim(), false),
    };
    let words: Vec<&str> = rule.split_whitespace().collect();
    let (frequency, unit) = match words.as_slice() {
        [unit] => (1, *unit),
        [n, unit] => (n.parse().ok().filter(|&n: &i64| n > 0)?, *unit),
        _ => return None,
    };
    let unit = match unit.trim_end_matches('s') {
        "day" => "logseq.property.repeat/recur-unit.day",
        "week" => "logseq.property.repeat/recur-unit.week",
        "month" => "logseq.property.repeat/recur-unit.month",
        "year" => "logseq.property.repeat/recur-unit.year",
        _ => return None,
    };
    Some(Recurrence {
        frequency,
        unit,
        when_done,
    })
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
    fn footnotes_join_their_references() {
        let mut blocks = parse_note(
            "Text[^1] and `[^x]` and inline^[a [[link]]].\n\nOther[^n2]\n\n[^1]: First.\n\n[^n2]: Second.\n\n[^unused]: Kept.\n",
            ParseOptions::default(),
        )
        .blocks;
        attach_footnotes(&mut blocks);
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "Text[^1] and `[^x]` and inline[^2].\n\n[^1]: First.\n\n[^2]: a [[link]]",
                "Other[^n2]\n\n[^n2]: Second.",
                "[^unused]: Kept.",
            ]
        );
    }

    #[test]
    fn template_vars() {
        assert_eq!(
            template_variables("# {{title}}\n{{date:YYYY-MM-DD}} {{ time }} {{other}} {{"),
            "# <% current page %>\n<% today %> <% time %> {{other}} {{"
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

        let m = extract_task_meta(
            "Water plants 🔁 every 2 weeks when done ➕ 2024-01-01 🛫 2024-01-02 📅 2024-01-05 ✅ 2024-01-04",
        );
        assert_eq!(m.text, "Water plants");
        assert_eq!(
            m.recurrence,
            Some(Recurrence {
                frequency: 2,
                unit: "logseq.property.repeat/recur-unit.week",
                when_done: true
            })
        );
        assert_eq!(m.created, NaiveDate::from_ymd_opt(2024, 1, 1));
        assert_eq!(m.other_dates[0], NaiveDate::from_ymd_opt(2024, 1, 2));
        assert_eq!(m.other_dates[1], NaiveDate::from_ymd_opt(2024, 1, 4));

        // Without a date, or with a rule Logseq can't express, it stays text.
        let m = extract_task_meta("Stretch 🔁 every day");
        assert_eq!(m.text, "Stretch 🔁 every day");
        assert_eq!(m.recurrence, None);
        let m = extract_task_meta("Gym 🔁 every week on Monday 📅 2024-01-01");
        assert_eq!(m.text, "Gym 🔁 every week on Monday");
        assert_eq!(m.recurrence, None);
        assert_eq!(parse_recurrence(" every month").unwrap().frequency, 1);
    }

    #[test]
    fn web_embeds() {
        let yt = "https://www.youtube.com/watch?v=1adNxqAtYMQ";
        assert_eq!(web_embed(yt), Some(format!("{{{{youtube {yt}}}}}")));
        assert_eq!(
            web_embed("https://youtu.be/1adNxqAtYMQ").as_deref(),
            Some("{{youtube https://youtu.be/1adNxqAtYMQ}}")
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
