use std::path::Path;
use std::process::Command;

use obsidian_to_logseq::convert::{self, Conversion, ConvertOptions};
use obsidian_to_logseq::phases;
use obsidian_to_logseq::vault::Vault;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample-vault");

fn convert_fixture(opts: impl FnOnce(&mut ConvertOptions)) -> (Conversion, String) {
    let mut vault = Vault::scan(Path::new(FIXTURE), &[]).unwrap();
    vault.load_daily_config(None, None).unwrap();
    let mut options = ConvertOptions {
        nest_under_headings: true,
        keep_comments: false,
        tags_as_pages: false,
        journals: true,
        reserved_titles: convert::bundled_reserved_titles()
            .into_iter()
            .map(|t| t.to_lowercase())
            .collect(),
    };
    opts(&mut options);
    let conversion = convert::convert(&vault, &options).unwrap();
    let edn = conversion.edn.to_edn_string();
    (conversion, edn)
}

#[test]
fn converts_sample_vault() {
    let (conv, edn) = convert_fixture(|_| {});
    let s = &conv.stats;
    assert_eq!(s.notes, 10);
    assert_eq!(s.journals, 2, "daily note plus the date property target");
    assert_eq!(s.assets, 2);
    assert_eq!(s.placeholder_pages, 1, "[[Missing Note]]");
    assert_eq!(s.alias_pages, 1, "Start here");
    assert_eq!(s.templates, 1);
    assert_eq!(s.favorites, 2, "Home and the note of a bookmarked heading");
    assert_eq!(conv.warnings.len(), 2, "{:?}", conv.warnings);
    assert!(
        conv.warnings
            .iter()
            .any(|w| w.starts_with("search bookmark")),
        "{:?}",
        conv.warnings
    );

    // Pages, renamed collisions and journals.
    assert!(edn.contains(":block/title \"Task (Obsidian)\""));
    assert!(edn.contains(":block/title \"Index (Projects)\""));
    assert!(edn.contains(":build/journal 20240115"));
    assert!(edn.contains("#uuid \"00000001-2024-0115-0000-000000000000\""));

    // Links are rewritten to UUID refs, code is left alone.
    assert!(!edn.contains("[[Project Alpha"));
    assert!(edn.contains("[the alpha project]([["));
    assert!(edn.contains("`inline [[not a link]] #notag`"));
    assert!(edn.contains("fn main() { println!(\\\"#not-a-tag [[nope]]\\\"); }"));
    assert!(edn.contains("Issue #42 is not a tag."));
    assert!(!edn.contains("hidden comment"));
    assert!(edn.contains("^^highlight^^"));
    assert!(!edn.contains("#+BEGIN_QUOTE"));

    // Logseq node types.
    for needle in [
        ":logseq.property/status :logseq.property/status.todo",
        ":logseq.property/status :logseq.property/status.done",
        ":logseq.property/status :logseq.property/status.doing",
        ":logseq.property/status :logseq.property/status.canceled",
        ":logseq.property/priority :logseq.property/priority.high",
        ":logseq.property/heading 2",
        ":logseq.property.code/lang \"rust\"",
        ":logseq.property.node/display-type :quote",
        ":logseq.property.node/display-type :math",
        "#+BEGIN_WARNING\\n**Be careful**",
        "\"**Ada Lovelace**\\nQuoted callout body\"",
        "#+BEGIN_NOTE\\n**Recipe**\\nCustom callout body",
        "#+BEGIN_NOTE\\n**Folded info**\\n#+END_NOTE",
        ":block/collapsed? true",
        ":logseq.property.asset/resize-metadata {:width 300}",
        // Tasks plugin repeat and dates.
        ":logseq.property.repeat/temporal-property :logseq.property/deadline",
        ":logseq.property.repeat/repeat-type :logseq.property.repeat/repeat-type.dotted-plus",
        ":logseq.property.repeat/recur-frequency 2",
        ":logseq.property.repeat/recur-unit :logseq.property.repeat/recur-unit.week",
        ":user.property/start-date {:logseq.property/type :datetime",
        ":user.property/done-date {:logseq.property/type :datetime",
        // Footnote definitions join the block that references them.
        "Claim with a source[^1] and an aside[^2].\\n\\n[^1]: The cited source.\\n\\n[^2]: Inline footnote text\"",
        // Templates and favorites.
        ":block/title \"Meeting\"",
        ":build/tags [:logseq.class/Template]",
        ":block/title \"<% current page %>\"",
        "Created <% today %> at <% time %>",
        ":block/title \"$$$favorites\"",
        ":block/link [:block/uuid",
        ":logseq.property.asset/type \"png\"",
        ":logseq.property.asset/type \"pdf\"",
        ":block/alias #{[:block/uuid",
        "{{video https://www.youtube.com/watch?v=1adNxqAtYMQ}}",
        "![logo](https://example.com/logo.png)",
    ] {
        assert!(edn.contains(needle), "missing {needle}");
    }

    // Frontmatter properties with inferred types.
    for (ident, ty) in [
        ("rating", "number"),
        ("done", "checkbox"),
        ("website", "url"),
        ("related", "node"),
        ("due", "date"),
        ("meeting", "datetime"),
        ("status", "default"),
    ] {
        assert!(
            edn.contains(&format!(
                ":user.property/{ident} {{:logseq.property/type :{ty}"
            )) || edn.contains(&format!(
                ":user.property/{ident} {{:logseq.property/type :{ty}\n"
            )),
            "property {ident} should be {ty}"
        );
    }
    assert!(!edn.contains("cssclasses"));

    // Tags become classes; the note named like a tag becomes the tag's page.
    assert!(edn.contains(":user.class/hub {:block/title \"hub\""));
    // Nested tags become a tag hierarchy.
    assert!(edn.contains(":user.class/area-work {:block/title \"work\""));
    assert!(edn.contains(":build/class-extends [:user.class/area]"));
    assert!(edn.contains(":user.class/area {:block/title \"area\""));
}

#[test]
fn tags_as_pages_and_flat_layout() {
    let (_, edn) = convert_fixture(|o| {
        o.tags_as_pages = true;
        o.nest_under_headings = false;
    });
    assert!(!edn.contains(":user.class/"));
    assert!(edn.contains(":user.property/tags {:logseq.property/type :node"));
    assert!(!edn.contains(":build/children [{:block/title \"Intro paragraph"));
}

#[test]
fn phases_cover_all_deferred_properties() {
    let (conv, _) = convert_fixture(|_| {});
    let steps = phases::split(&conv.edn, 15_000);
    let text = |prefix: &str| -> String {
        steps
            .iter()
            .filter(|s| s.label.starts_with(prefix))
            .map(|s| s.edn.to_edn_string())
            .collect()
    };
    let (pages, blocks, props) = (text("pages"), text("blocks"), text("properties"));
    assert!(pages.contains(":block/title \"Home\""));
    assert!(!pages.contains(":user.property/rating 5"));
    assert!(blocks.contains("Intro paragraph"));
    assert!(!blocks.contains("order-list-type"));
    assert!(props.contains(":user.property/rating 5"));
    assert!(props.contains(":logseq.property/order-list-type \"number\""));
    assert!(props.contains(":user.property/mood \"good\""));
}

/// Full import through the Logseq CLI. Run with
/// `O2L_E2E=1 cargo test -- --ignored` when `logseq` is installed.
#[test]
#[ignore]
fn imports_into_logseq() {
    if std::env::var_os("O2L_E2E").is_none() {
        return;
    }
    let graph = "obsidian-to-logseq-e2e";
    let status = Command::new(env!("CARGO_BIN_EXE_obsidian-to-logseq"))
        .args([FIXTURE, "--graph", graph, "--replace"])
        .status()
        .unwrap();
    assert!(status.success());
    let out = Command::new("logseq")
        .args(["graph", "validate", "--graph", graph, "--output", "json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["status"], "ok", "{json}");
    assert!(json["data"]["result"]["errors"].is_null(), "{json}");
    let out = Command::new("logseq")
        .args(["show", "--graph", graph, "--page", "Home"])
        .output()
        .unwrap();
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(
        shown.contains("[the alpha project]([[Project Alpha]])"),
        "{shown}"
    );
    // Number properties aren't printed by `show`, so query the value.
    let out = Command::new("logseq")
        .args([
            "query",
            "--graph",
            graph,
            "--output",
            "json",
            "--query",
            "[:find ?v . :where [?p :block/title \"Home\"] [?p :user.property/rating ?r] [?r :logseq.property/value ?v]]",
        ])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["result"], 5, "{json}");
    // A list property holding a single page link (applied in a property batch).
    let out = Command::new("logseq")
        .args([
            "query",
            "--graph",
            graph,
            "--output",
            "json",
            "--query",
            "[:find ?t . :where [?p :block/title \"Index (Projects)\"] [?p :user.property/related ?r] [?r :block/title ?t]]",
        ])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["data"]["result"], "Home", "{json}");
    let _ = Command::new("logseq")
        .args(["graph", "remove", "--graph", graph])
        .status();
}
