//! Splits a converted graph into a sequence of smaller imports.
//!
//! A single Logseq import transaction has two scaling problems:
//! * it is quadratic in (property value entities × blocks), and
//! * the db-worker drops requests that run for more than ~10 minutes.
//!
//! So the graph is imported as:
//! 1. page shells, tags, property definitions and assets,
//! 2. blocks in chunks of whole pages,
//! 3. fix-ups for blocks that link to blocks of a later chunk (a reference
//!    to a block that doesn't exist yet fails validation),
//! 4. properties whose values become entities (user properties and numbered
//!    list markers), in small batches against the existing nodes.

use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::edn::Edn;

/// Maximum number of nodes receiving properties per property import.
const PROPERTY_BATCH_SIZE: usize = 200;
/// Maximum number of blocks per fix-up import.
const FIXUP_BATCH_SIZE: usize = 2000;

pub struct Step {
    pub label: String,
    pub edn: Edn,
}

fn deferred_property(key: &Edn) -> bool {
    matches!(key, Edn::Keyword(k)
        if k.starts_with("user.property/") || k == "logseq.property/order-list-type")
}

fn get<'a>(map: &'a [(Edn, Edn)], key: &str) -> Option<&'a Edn> {
    map.iter()
        .find(|(k, _)| matches!(k, Edn::Keyword(x) if x == key))
        .map(|(_, v)| v)
}

fn get_mut<'a>(map: &'a mut [(Edn, Edn)], key: &str) -> Option<&'a mut Edn> {
    map.iter_mut()
        .find(|(k, _)| matches!(k, Edn::Keyword(x) if x == key))
        .map(|(_, v)| v)
}

fn remove(map: &mut Vec<(Edn, Edn)>, key: &str) -> Option<Edn> {
    let i = map
        .iter()
        .position(|(k, _)| matches!(k, Edn::Keyword(x) if x == key))?;
    Some(map.remove(i).1)
}

/// Removes deferred entries from the map's `:build/properties`, returning them.
fn take_deferred_properties(map: &mut Vec<(Edn, Edn)>) -> Vec<(Edn, Edn)> {
    let Some(Edn::Map(props)) = get_mut(map, "build/properties") else {
        return Vec::new();
    };
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(props)
        .into_iter()
        .partition(|(k, _)| deferred_property(k));
    *props = kept;
    if props.is_empty() {
        remove(map, "build/properties");
    }
    taken
}

/// Gives a block map a fixed UUID if it has none and returns it.
fn ensure_uuid(map: &mut Vec<(Edn, Edn)>) -> Uuid {
    if let Some(Edn::Uuid(u)) = get(map, "block/uuid") {
        return *u;
    }
    let u = Uuid::new_v4();
    let at = map.len().min(1);
    map.insert(at, (Edn::kw("block/uuid"), Edn::Uuid(u)));
    map.insert(at + 1, (Edn::kw("build/keep-uuid?"), Edn::Bool(true)));
    u
}

/// Updates to apply to an existing page and its existing blocks.
#[derive(Default)]
struct Update {
    page_props: Vec<(Edn, Edn)>,
    page_updated_at: Option<Edn>,
    blocks: Vec<Edn>,
}

impl Update {
    fn size(&self) -> usize {
        usize::from(!self.page_props.is_empty()) + self.blocks.len()
    }
}

/// Splits an export into import steps with at most about `chunk_blocks`
/// blocks per block import.
pub fn split(export: &Edn, chunk_blocks: usize) -> Vec<Step> {
    let Edn::Map(mut top) = export.clone() else {
        panic!("export must be a map");
    };
    let property_defs = match get(&top, "properties") {
        Some(Edn::Map(defs)) => defs.clone(),
        _ => Vec::new(),
    };
    let mut prop_updates: Vec<(Uuid, Update)> = Vec::new();

    // Class properties stay on the class definitions: updating an existing
    // class page with property values fails validation, and there are few
    // classes so their value entities are cheap.
    let classes = match remove(&mut top, "classes") {
        Some(Edn::Map(c)) => c,
        _ => Vec::new(),
    };

    let entries = match remove(&mut top, "pages-and-blocks") {
        Some(Edn::Vector(v)) => v,
        _ => Vec::new(),
    };
    let mut shells = Vec::new();
    // (page uuid, blocks, block count)
    let mut block_pages: Vec<(Uuid, Vec<Edn>, usize)> = Vec::new();
    for entry in entries {
        let Edn::Map(mut entry) = entry else { continue };
        let Some(Edn::Map(mut page)) = remove(&mut entry, "page") else {
            continue;
        };
        let page_props = take_deferred_properties(&mut page);
        let Some(Edn::Uuid(page_uuid)) = get(&page, "block/uuid").cloned() else {
            // Pages matched by title (the Asset page) are imported whole with
            // the shells; their blocks don't reference anything.
            entry.insert(0, (Edn::kw("page"), Edn::Map(page)));
            shells.push(Edn::Map(entry));
            continue;
        };
        let mut blocks = match remove(&mut entry, "blocks") {
            Some(Edn::Vector(b)) => b,
            _ => Vec::new(),
        };
        let mut update = Update {
            page_props,
            page_updated_at: get(&page, "block/updated-at").cloned(),
            blocks: Vec::new(),
        };
        let mut count = 0;
        for block in blocks.iter_mut() {
            prepare_block(block, &mut update.blocks, &mut count);
        }
        if update.size() > 0 {
            prop_updates.push((page_uuid, update));
        }
        // Class pages are created from :classes; other pages need a shell.
        if page.len() > 1 {
            shells.push(Edn::Map(vec![(Edn::kw("page"), Edn::Map(page))]));
        }
        if !blocks.is_empty() {
            block_pages.push((page_uuid, blocks, count));
        }
    }

    let mut steps = vec![Step {
        label: "pages".to_string(),
        edn: export_map(property_defs.clone(), classes, shells),
    }];

    // Group pages into chunks of whole pages.
    let mut chunks: Vec<Vec<(Uuid, Vec<Edn>)>> = Vec::new();
    let mut size = 0;
    for (page, blocks, count) in block_pages {
        if chunks.is_empty() || (size > 0 && size + count > chunk_blocks) {
            chunks.push(Vec::new());
            size = 0;
        }
        size += count;
        chunks.last_mut().unwrap().push((page, blocks));
    }

    // Blocks that are link targets, by chunk.
    let mut target_chunk: HashMap<Uuid, usize> = HashMap::new();
    for (i, chunk) in chunks.iter().enumerate() {
        for (_, blocks) in chunk {
            for b in blocks {
                collect_block_uuids(b, i, &mut target_chunk);
            }
        }
    }

    let n_chunks = chunks.len();
    let mut fixups: Vec<(Uuid, Vec<Edn>)> = Vec::new();
    for (i, chunk) in chunks.into_iter().enumerate() {
        let mut entries = Vec::new();
        for (page, mut blocks) in chunk {
            let mut page_fixups = Vec::new();
            for b in blocks.iter_mut() {
                defer_forward_refs(b, i, &target_chunk, &mut page_fixups);
            }
            if !page_fixups.is_empty() {
                fixups.push((page, page_fixups));
            }
            entries.push(page_entry(page, Vec::new(), blocks));
        }
        let label = if n_chunks == 1 {
            "blocks".to_string()
        } else {
            format!("blocks {}/{n_chunks}", i + 1)
        };
        steps.push(Step {
            label,
            edn: export_map(Vec::new(), Vec::new(), entries),
        });
    }

    let fixup_batches = batch(fixups, FIXUP_BATCH_SIZE, |(_, b)| b.len());
    let n = fixup_batches.len();
    for (i, group) in fixup_batches.into_iter().enumerate() {
        let entries = group
            .into_iter()
            .map(|(page, blocks)| page_entry(page, Vec::new(), blocks))
            .collect();
        steps.push(Step {
            label: format!("links {}/{n}", i + 1),
            edn: export_map(Vec::new(), Vec::new(), entries),
        });
    }

    let prop_batches = batch(prop_updates, PROPERTY_BATCH_SIZE, |(_, u)| u.size());
    let n = prop_batches.len();
    for (i, group) in prop_batches.into_iter().enumerate() {
        steps.push(Step {
            label: format!("properties {}/{n}", i + 1),
            edn: property_batch(group, &property_defs),
        });
    }
    steps
}

/// Strips deferred properties from a block tree (recording updates for them)
/// and counts its blocks.
fn prepare_block(block: &mut Edn, updates: &mut Vec<Edn>, count: &mut usize) {
    let Edn::Map(map) = block else { return };
    *count += 1;
    let props = take_deferred_properties(map);
    if !props.is_empty() {
        let uuid = ensure_uuid(map);
        let mut update = vec![
            (Edn::kw("block/uuid"), Edn::Uuid(uuid)),
            (
                Edn::kw("block/title"),
                get(map, "block/title").cloned().unwrap_or(Edn::str("")),
            ),
        ];
        if let Some(t) = get(map, "block/updated-at") {
            update.push((Edn::kw("block/updated-at"), t.clone()));
        }
        update.push((Edn::kw("build/properties"), Edn::Map(props)));
        updates.push(Edn::Map(update));
    }
    if let Some(Edn::Vector(children)) = get_mut(map, "build/children") {
        for child in children.iter_mut() {
            prepare_block(child, updates, count);
        }
    }
}

fn collect_block_uuids(block: &Edn, chunk: usize, out: &mut HashMap<Uuid, usize>) {
    let Edn::Map(map) = block else { return };
    if let Some(Edn::Uuid(u)) = get(map, "block/uuid") {
        out.insert(*u, chunk);
    }
    if let Some(Edn::Vector(children)) = get(map, "build/children") {
        for c in children {
            collect_block_uuids(c, chunk, out);
        }
    }
}

/// Blocks referencing a block of a later chunk are imported with an empty
/// title and get their real title and embed link in a fix-up import.
fn defer_forward_refs(
    block: &mut Edn,
    chunk: usize,
    target_chunk: &HashMap<Uuid, usize>,
    fixups: &mut Vec<Edn>,
) {
    let Edn::Map(map) = block else { return };
    let is_forward = |u: &Uuid| target_chunk.get(u).is_some_and(|&c| c > chunk);
    let title = match get(map, "block/title") {
        Some(Edn::Str(s)) => s.clone(),
        _ => String::new(),
    };
    let link_forward = matches!(get(map, "block/link"),
        Some(Edn::Vector(v)) if matches!(v.get(1), Some(Edn::Uuid(u)) if is_forward(u)));
    if link_forward || referenced_uuids(&title).iter().any(is_forward) {
        let uuid = ensure_uuid(map);
        let mut fix = vec![
            (Edn::kw("block/uuid"), Edn::Uuid(uuid)),
            (Edn::kw("block/title"), Edn::Str(title)),
        ];
        if let Some(t) = get(map, "block/updated-at") {
            fix.push((Edn::kw("block/updated-at"), t.clone()));
        }
        if link_forward && let Some(link) = remove(map, "block/link") {
            fix.push((Edn::kw("block/link"), link));
        }
        if let Some(Edn::Str(t)) = get_mut(map, "block/title") {
            t.clear();
        }
        fixups.push(Edn::Map(fix));
    }
    if let Some(Edn::Vector(children)) = get_mut(map, "build/children") {
        for c in children.iter_mut() {
            defer_forward_refs(c, chunk, target_chunk, fixups);
        }
    }
}

/// UUIDs referenced as `[[uuid]]` in block text.
fn referenced_uuids(text: &str) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[[") {
        let after = &rest[i + 2..];
        if after.len() >= 38
            && after.as_bytes()[36] == b']'
            && let Ok(u) = Uuid::parse_str(&after[..36])
        {
            out.push(u);
        }
        rest = after;
    }
    out
}

fn batch<T>(items: Vec<T>, max: usize, size: impl Fn(&T) -> usize) -> Vec<Vec<T>> {
    let mut out: Vec<Vec<T>> = Vec::new();
    let mut current = 0;
    for item in items {
        let s = size(&item);
        if out.is_empty() || (current > 0 && current + s > max) {
            out.push(Vec::new());
            current = 0;
        }
        current += s;
        out.last_mut().unwrap().push(item);
    }
    out
}

fn page_entry(page: Uuid, page_attrs: Vec<(Edn, Edn)>, blocks: Vec<Edn>) -> Edn {
    let mut page_map = vec![(Edn::kw("block/uuid"), Edn::Uuid(page))];
    page_map.extend(page_attrs);
    let mut entry = vec![(Edn::kw("page"), Edn::Map(page_map))];
    if !blocks.is_empty() {
        entry.push((Edn::kw("blocks"), Edn::Vector(blocks)));
    }
    Edn::Map(entry)
}

fn export_map(properties: Vec<(Edn, Edn)>, classes: Vec<(Edn, Edn)>, pages: Vec<Edn>) -> Edn {
    Edn::Map(vec![
        (Edn::kw("properties"), Edn::Map(properties)),
        (Edn::kw("classes"), Edn::Map(classes)),
        (Edn::kw("pages-and-blocks"), Edn::Vector(pages)),
        (
            Edn::kw("logseq.db.sqlite.export/export-type"),
            Edn::kw("graph-human"),
        ),
    ])
}

fn property_batch(updates: Vec<(Uuid, Update)>, property_defs: &[(Edn, Edn)]) -> Edn {
    let mut used = BTreeSet::new();
    let mut pages = Vec::new();
    for (page, u) in updates {
        let mut attrs = Vec::new();
        if !u.page_props.is_empty() {
            if let Some(t) = u.page_updated_at {
                attrs.push((Edn::kw("block/updated-at"), t));
            }
            for (k, _) in &u.page_props {
                used.insert(k.to_edn_string());
            }
            attrs.push((Edn::kw("build/properties"), Edn::Map(u.page_props)));
        }
        for b in &u.blocks {
            if let Edn::Map(m) = b
                && let Some(Edn::Map(props)) = get(m, "build/properties")
            {
                for (k, _) in props {
                    used.insert(k.to_edn_string());
                }
            }
        }
        pages.push(page_entry(page, attrs, u.blocks));
    }
    let defs = property_defs
        .iter()
        .filter(|(k, _)| used.contains(&k.to_edn_string()))
        .cloned()
        .collect();
    export_map(defs, Vec::new(), pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kw(s: &str) -> Edn {
        Edn::kw(s)
    }

    fn page(uuid: Uuid, props: Vec<(Edn, Edn)>, blocks: Vec<Edn>) -> Edn {
        let mut p = vec![
            (kw("block/title"), Edn::str(uuid.to_string())),
            (kw("block/uuid"), Edn::Uuid(uuid)),
            (kw("build/keep-uuid?"), Edn::Bool(true)),
        ];
        if !props.is_empty() {
            p.push((kw("build/properties"), Edn::Map(props)));
        }
        Edn::Map(vec![
            (kw("page"), Edn::Map(p)),
            (kw("blocks"), Edn::Vector(blocks)),
        ])
    }

    fn block(title: &str, extra: Vec<(Edn, Edn)>) -> Edn {
        let mut m = vec![(kw("block/title"), Edn::str(title))];
        m.extend(extra);
        Edn::Map(m)
    }

    fn export(pages: Vec<Edn>) -> Edn {
        Edn::Map(vec![
            (
                kw("properties"),
                Edn::Map(vec![(kw("user.property/x"), Edn::Map(vec![]))]),
            ),
            (kw("pages-and-blocks"), Edn::Vector(pages)),
        ])
    }

    #[test]
    fn moves_value_properties_to_batches() {
        let p = Uuid::new_v4();
        let steps = split(
            &export(vec![page(
                p,
                vec![(kw("user.property/x"), Edn::str("v"))],
                vec![block(
                    "one",
                    vec![(
                        kw("build/properties"),
                        Edn::Map(vec![
                            (kw("logseq.property/order-list-type"), Edn::str("number")),
                            (kw("logseq.property/heading"), Edn::Int(2)),
                        ]),
                    )],
                )],
            )]),
            1000,
        );
        let labels: Vec<_> = steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, vec!["pages", "blocks", "properties 1/1"]);
        let pages = steps[0].edn.to_edn_string();
        assert!(pages.contains(":block/title"));
        assert!(!pages.contains(":user.property/x \"v\""));
        let blocks = steps[1].edn.to_edn_string();
        assert!(!blocks.contains("order-list-type"));
        assert!(blocks.contains(":logseq.property/heading 2"));
        assert!(blocks.contains(":build/keep-uuid? true"));
        let props = steps[2].edn.to_edn_string();
        assert!(props.contains(&p.to_string()));
        assert!(props.contains(":user.property/x \"v\""));
        assert!(props.contains("order-list-type"));
        assert!(!props.contains("keep-uuid"));
    }

    #[test]
    fn defers_forward_block_refs_across_chunks() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let target = Uuid::new_v4();
        let steps = split(
            &export(vec![
                page(
                    a,
                    vec![],
                    vec![
                        block(&format!("see [[{target}]] and [[{b}]]"), vec![]),
                        block("", vec![(kw("block/link"), Edn::uuid_ref(target))]),
                        block(&format!("page ref only [[{b}]]"), vec![]),
                    ],
                ),
                page(
                    b,
                    vec![],
                    vec![block(
                        "target",
                        vec![
                            (kw("block/uuid"), Edn::Uuid(target)),
                            (kw("build/keep-uuid?"), Edn::Bool(true)),
                        ],
                    )],
                ),
            ]),
            1,
        );
        let labels: Vec<_> = steps.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["pages", "blocks 1/2", "blocks 2/2", "links 1/1"]
        );
        let first = steps[1].edn.to_edn_string();
        assert!(!first.contains(&target.to_string()), "{first}");
        assert!(first.contains("page ref only"));
        let links = steps[3].edn.to_edn_string();
        assert!(links.contains(&format!("see [[{target}]]")));
        assert!(links.contains(":block/link [:block/uuid"));
    }
}
