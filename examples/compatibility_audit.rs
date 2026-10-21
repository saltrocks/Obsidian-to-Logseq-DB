//! Documentation-driven format probes. A nonzero exit means compatibility gaps,
//! not a test-runner failure. See docs/compatibility.md for scope and sources.
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use obsidian_to_logseq::convert::{self, Conversion, ConvertOptions};
use obsidian_to_logseq::edn::Edn;
use obsidian_to_logseq::vault::Vault;
use serde_json::{Value, json};

fn convert_files(files: &[(&str, &str)]) -> Result<Conversion> {
    let root = tempfile::tempdir()?;
    for (name, content) in files {
        let path = root.path().join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(path, content)?;
    }
    let mut vault = Vault::scan(root.path(), &[])?;
    vault.load_daily_config(None, None)?;
    convert::convert(
        &vault,
        &ConvertOptions {
            nest_under_headings: true,
            keep_comments: false,
            tags_as_pages: false,
            journals: true,
            reserved_titles: convert::bundled_reserved_titles()
                .into_iter()
                .map(|t| t.to_lowercase())
                .collect(),
        },
    )
}

fn get<'a>(value: &'a Edn, key: &str) -> Option<&'a Edn> {
    match value {
        Edn::Map(entries) => entries
            .iter()
            .find(|(k, _)| *k == Edn::kw(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn items(value: Option<&Edn>) -> &[Edn] {
    match value {
        Some(Edn::Vector(items) | Edn::Set(items)) => items,
        _ => &[],
    }
}

fn page<'a>(conv: &'a Conversion, title: &str) -> &'a Edn {
    items(get(&conv.edn, "pages-and-blocks"))
        .iter()
        .find(|p| get(p, "page").and_then(|p| get(p, "block/title")) == Some(&Edn::str(title)))
        .unwrap_or_else(|| panic!("missing page {title}"))
}

fn blocks<'a>(conv: &'a Conversion, title: &str) -> &'a [Edn] {
    items(get(page(conv, title), "blocks"))
}

fn title(value: &Edn) -> &str {
    match get(value, "block/title") {
        Some(Edn::Str(s)) => s,
        _ => "",
    }
}

fn uuid(value: &Edn) -> String {
    match get(value, "block/uuid") {
        Some(Edn::Uuid(u)) => u.to_string(),
        _ => String::new(),
    }
}

fn has_title(value: &Edn, expected: &str) -> bool {
    title(value) == expected
        || match value {
            Edn::Map(entries) => entries.iter().any(|(_, v)| has_title(v, expected)),
            Edn::Vector(values) | Edn::Set(values) => values.iter().any(|v| has_title(v, expected)),
            _ => false,
        }
}

fn probe(
    rows: &mut Vec<Value>,
    id: &str,
    source: &str,
    expected: &str,
    pass: bool,
    observed: Value,
) {
    rows.push(json!({
        "id": id, "source": source, "expected": expected,
        "status": if pass { "pass" } else { "gap" }, "observed": observed,
    }));
}

fn collect_probes() -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let links = "obsidian-help/en/Linking notes and files/Internal links.md";
    let embeds = "obsidian-help/en/Linking notes and files/Embed files.md";
    let syntax = "obsidian-help/en/Editing and formatting/Basic formatting syntax.md";
    let props = "obsidian-help/en/Editing and formatting/Properties.md";
    let tags = "obsidian-help/en/Editing and formatting/Tags.md";

    let c = convert_files(&[
        (
            "A.md",
            "[[Target Note|label]]\n\n[encoded](Target%20Note.md)\n\n[[Target Note#Heading]]\n\n[[Target Note#^para]]\n\n![[Target Note#Heading]]",
        ),
        ("Target Note.md", "## Heading\n\nParagraph ^para"),
    ])?;
    let b = blocks(&c, "A");
    let target = uuid(get(page(&c, "Target Note"), "page").unwrap());
    let heading = &blocks(&c, "Target Note")[0];
    let h = uuid(heading);
    let paragraph = uuid(&items(get(heading, "build/children"))[0]);
    for (id, actual, expected) in [
        (
            "wikilink_label",
            title(&b[0]),
            format!("[label]([[{target}]])"),
        ),
        (
            "markdown_percent_encoded",
            title(&b[1]),
            format!("[encoded]([[{target}]])"),
        ),
        ("heading_reference", title(&b[2]), format!("[[{h}]]")),
        (
            "paragraph_reference",
            title(&b[3]),
            format!("[[{paragraph}]]"),
        ),
    ] {
        probe(
            &mut rows,
            id,
            links,
            "Reference the exact target UUID",
            actual == expected,
            json!(actual),
        );
    }
    probe(
        &mut rows,
        "standalone_wiki_embed",
        embeds,
        "Embed the heading using block/link",
        get(&b[4], "block/link") == Some(&Edn::uuid_ref(h.parse()?)),
        json!(b[4].to_edn_string()),
    );

    let c = convert_files(&[("A.md", "## Heading\n\n[local](#Heading)")])?;
    let h = &blocks(&c, "A")[0];
    let t = title(&items(get(h, "build/children"))[0]);
    probe(
        &mut rows,
        "markdown_local_anchor",
        links,
        "Resolve a same-note Markdown heading link",
        t == format!("[local]([[{}]])", uuid(h)),
        json!(t),
    );

    let c = convert_files(&[("A.md", "[missing](Missing.md)")])?;
    probe(
        &mut rows,
        "markdown_missing_note",
        links,
        "Create a placeholder like [[Missing]]",
        c.stats.placeholder_pages == 1,
        json!({"placeholders": c.stats.placeholder_pages, "text": title(&blocks(&c, "A")[0])}),
    );

    let c = convert_files(&[("A.md", "![](B.md)"), ("B.md", "Body")])?;
    probe(
        &mut rows,
        "markdown_note_embed",
        embeds,
        "Preserve a standalone Markdown note embed as block/link",
        get(&blocks(&c, "A")[0], "block/link").is_some(),
        json!(blocks(&c, "A")[0].to_edn_string()),
    );

    let c = convert_files(&[
        ("A.md", "[[B#Second#Details]]"),
        (
            "B.md",
            "# First\n## Details\nOne\n# Second\n## Details\nTwo",
        ),
    ])?;
    let second = &items(get(&blocks(&c, "B")[1], "build/children"))[0];
    probe(
        &mut rows,
        "qualified_duplicate_heading",
        links,
        "Use the full heading hierarchy to disambiguate Details",
        title(&blocks(&c, "A")[0]) == format!("[[{}]]", uuid(second)),
        json!(title(&blocks(&c, "A")[0])),
    );

    let c = convert_files(&[
        ("Folder/A.md", "[[Projects/Target]]"),
        ("Projects/Target.md", "Root target"),
        ("Folder/Projects/Target.md", "Shadow target"),
    ])?;
    let expected = uuid(get(page(&c, "Target"), "page").unwrap());
    probe(
        &mut rows,
        "vault_root_path",
        links,
        "A folder-qualified link starts at the vault root",
        title(&blocks(&c, "A")[0]) == format!("[[{expected}]]"),
        json!(title(&blocks(&c, "A")[0])),
    );

    let c = convert_files(&[("A.md", "- One\n- Two\n\n^whole-list\n\n![[#^whole-list]]")])?;
    let b = blocks(&c, "A");
    let whole_list = b.iter().any(|b| {
        get(b, "block/uuid").is_some()
            && items(get(b, "build/children"))
                .iter()
                .any(|c| title(c) == "One")
            && items(get(b, "build/children"))
                .iter()
                .any(|c| title(c) == "Two")
    });
    probe(
        &mut rows,
        "whole_list_reference",
        embeds,
        "A list block identifier includes every list item",
        whole_list,
        json!(b.iter().map(Edn::to_edn_string).collect::<Vec<_>>()),
    );

    let c = convert_files(&[("A.md", "#Tag #tag #parent/child #1984")])?;
    probe(
        &mut rows,
        "tag_case_hierarchy_numeric",
        tags,
        "Three classes; numeric-only #1984 stays text",
        c.stats.classes == 3 && title(&blocks(&c, "A")[0]).contains("#1984"),
        json!({"classes": c.stats.classes}),
    );
    let c = convert_files(&[("A.md", "#🚀 #café")])?;
    probe(
        &mut rows,
        "emoji_tags",
        tags,
        "Recognize both emoji and accented tags",
        c.stats.classes == 2,
        json!({"classes": c.stats.classes, "text": title(&blocks(&c, "A")[0])}),
    );

    let code = "#literal [[Ghost]] ==raw== %%raw%%";
    let c = convert_files(&[("A.md", &format!("```text\n{code}\n```\n\n`{code}`"))])?;
    probe(
        &mut rows,
        "code_literals",
        syntax,
        "Keep fenced and inline code literal",
        title(&blocks(&c, "A")[0]) == code
            && title(&blocks(&c, "A")[1]) == format!("`{code}`")
            && c.stats.classes == 0
            && c.stats.placeholder_pages == 0,
        json!({"tags": c.stats.classes, "placeholders": c.stats.placeholder_pages}),
    );
    let formula = "$\\text{[[Ghost]] ==x==}$";
    let c = convert_files(&[("A.md", formula)])?;
    probe(
        &mut rows,
        "inline_math_literal",
        "obsidian-help/en/Editing and formatting/Advanced formatting syntax.md",
        "Keep math contents literal without creating tags or links",
        title(&blocks(&c, "A")[0]) == formula,
        json!({"text": title(&blocks(&c, "A")[0]), "tags": c.stats.classes, "placeholders": c.stats.placeholder_pages}),
    );

    let c = convert_files(&[("A.md", "Visible %%hidden%% ==bright==\n\n$$x^2$$")])?;
    probe(
        &mut rows,
        "comments_highlights_display_math",
        syntax,
        "Remove comments, translate highlights, preserve display math",
        title(&blocks(&c, "A")[0]) == "Visible  ^^bright^^" && title(&blocks(&c, "A")[1]) == "x^2",
        json!(blocks(&c, "A").iter().map(title).collect::<Vec<_>>()),
    );

    let c = convert_files(&[("A.md", "- [?] Completed in core Obsidian")])?;
    let status =
        get(&blocks(&c, "A")[0], "build/properties").and_then(|p| get(p, "logseq.property/status"));
    probe(
        &mut rows,
        "core_nonstandard_checkbox",
        syntax,
        "Core Obsidian treats any non-space checkbox character as complete",
        status == Some(&Edn::kw("logseq.property/status.done")),
        json!(status.map(Edn::to_edn_string)),
    );

    let c = convert_files(&[(
        "A.md",
        "---\n{\"tags\":[\"journal\"],\"publish\":false,\"rating\":2.5}\n---\nBody",
    )])?;
    probe(
        &mut rows,
        "json_frontmatter",
        props,
        "Read JSON frontmatter with a checkbox, number, and tag",
        c.stats.properties == 2 && c.stats.classes == 1 && c.warnings.is_empty(),
        json!({"properties": c.stats.properties, "tags": c.stats.classes}),
    );
    let c = convert_files(&[
        ("A.md", "---\nvalue: 12\n---\nA"),
        ("B.md", "---\nvalue: text\n---\nB"),
    ])?;
    let schema = get(&c.edn, "properties")
        .and_then(|p| get(p, "user.property/value"))
        .unwrap();
    probe(
        &mut rows,
        "mixed_property_types",
        props,
        "Preserve incompatible scalar types as text",
        get(schema, "logseq.property/type") == Some(&Edn::kw("default")),
        json!(schema.to_edn_string()),
    );
    let c = convert_files(&[
        (".obsidian/types.json", r#"{"types":{"code":"text"}}"#),
        ("A.md", "---\ncode: '2024-01-02'\n---\nA"),
    ])?;
    let schema = get(&c.edn, "properties")
        .and_then(|p| get(p, "user.property/code"))
        .unwrap();
    probe(
        &mut rows,
        "configured_property_type",
        props,
        "Respect explicitly configured text instead of inferring a date",
        get(schema, "logseq.property/type") == Some(&Edn::kw("default")),
        json!(schema.to_edn_string()),
    );

    let c = convert_files(&[
        ("A.md", "![[paper.pdf#page=3&height=400]]"),
        ("paper.pdf", "%PDF-1.4"),
    ])?;
    probe(
        &mut rows,
        "pdf_page_selector",
        embeds,
        "Preserve a PDF page selector and viewer height",
        c.edn.to_edn_string().contains("page=3"),
        json!({"assets": c.stats.assets, "text": title(&blocks(&c, "A")[0])}),
    );
    let c = convert_files(&[("A.md", "![[image.png|320x200]]"), ("image.png", "image")])?;
    let edn = c.edn.to_edn_string();
    probe(
        &mut rows,
        "image_width",
        embeds,
        "Carry an image's width into asset resize metadata",
        edn.contains(":width 320"),
        json!({"width_320": edn.contains(":width 320")}),
    );
    probe(
        &mut rows,
        "image_height",
        embeds,
        "Carry both dimensions of a width x height embed",
        edn.contains(":height 200"),
        json!({"height_200": edn.contains(":height 200")}),
    );

    let c = convert_files(&[
        (
            "A.md",
            "| Link | Value |\n| --- | --- |\n| [[B\\|label]] | **bold** |",
        ),
        ("B.md", "Body"),
    ])?;
    let b = uuid(get(page(&c, "B"), "page").unwrap());
    probe(
        &mut rows,
        "table_escaped_wikilink",
        "obsidian-help/en/Editing and formatting/Advanced formatting syntax.md",
        "Preserve table markup and rewrite an escaped-pipe wikilink",
        title(&blocks(&c, "A")[0]).contains(&format!("[label]([[{b}]])")),
        json!(title(&blocks(&c, "A")[0])),
    );
    let c = convert_files(&[("A.md", "A claim[^source].\n\n[^source]: Supporting source.")])?;
    probe(
        &mut rows,
        "footnotes",
        syntax,
        "Keep the footnote with its referencing block",
        title(&blocks(&c, "A")[0]).contains("[^source]: Supporting source."),
        json!(title(&blocks(&c, "A")[0])),
    );
    let c = convert_files(&[("A.md", "> [!tip]- Title\n> Body")])?;
    let b = &blocks(&c, "A")[0];
    probe(
        &mut rows,
        "folded_callout",
        "obsidian-help/en/Editing and formatting/Callouts.md",
        "Preserve a collapsed callout's title and body",
        get(b, "block/collapsed?") == Some(&Edn::Bool(true)) && has_title(b, "Body"),
        json!(b.to_edn_string()),
    );
    let c = convert_files(&[("A.md", "> [!note] Outer\n> > [!tip] Inner\n> > Body")])?;
    probe(
        &mut rows,
        "nested_callout",
        "obsidian-help/en/Editing and formatting/Callouts.md",
        "Translate the nested callout instead of leaving Obsidian syntax",
        !c.edn.to_edn_string().contains("[!tip]"),
        json!(title(&blocks(&c, "A")[0])),
    );

    let c = convert_files(&[
        (
            ".obsidian/daily-notes.json",
            r#"{"folder":"Daily","format":"YYYY/MM/DD"}"#,
        ),
        ("Daily/2024/01/02.md", "Journal"),
    ])?;
    probe(
        &mut rows,
        "daily_path_format",
        "obsidian-help/en/Plugins/Daily notes.md",
        "Recognize a date format with nested folders",
        c.stats.journals == 1,
        json!({"journals": c.stats.journals}),
    );
    let c = convert_files(&[
        (".obsidian/templates.json", r#"{"folder":"Templates"}"#),
        ("Templates/A.md", "{{title}} {{date}} {{time}}"),
    ])?;
    probe(
        &mut rows,
        "template_variables",
        "obsidian-help/en/Plugins/Templates.md",
        "Translate standard template variables inside a Template block",
        c.stats.templates == 1 && has_title(&c.edn, "<% current page %> <% today %> <% time %>"),
        json!({"templates": c.stats.templates}),
    );
    let c = convert_files(&[
        (".obsidian/templates.json", r#"{"folder":"Templates"}"#),
        ("Templates/A.md", "{{date:YYYY}} {{time:HH}}"),
    ])?;
    probe(
        &mut rows,
        "template_custom_format",
        "obsidian-help/en/Plugins/Templates.md",
        "Retain explicit date/time format tokens",
        !has_title(&c.edn, "<% today %> <% time %>"),
        json!(c.edn.to_edn_string()),
    );

    // Broader feature inventory: ordinary document formatting and persisted
    // plugin content, in addition to the original link/property edge cases.
    let text = "**bold** _italic_ ~~strike~~ **bold _nested_**";
    let c = convert_files(&[("A.md", text)])?;
    probe(
        &mut rows,
        "basic_emphasis",
        syntax,
        "Preserve emphasis and strikethrough markup",
        title(&blocks(&c, "A")[0]) == text,
        json!(title(&blocks(&c, "A")[0])),
    );
    let c = convert_files(&[("A.md", "First line\ncontinued\n\nSecond paragraph")])?;
    let b = blocks(&c, "A");
    probe(
        &mut rows,
        "paragraph_structure",
        syntax,
        "Keep paragraph boundaries and continuation text",
        b.len() == 2
            && title(&b[0]) == "First line\ncontinued"
            && title(&b[1]) == "Second paragraph",
        json!(b.iter().map(title).collect::<Vec<_>>()),
    );
    let c = convert_files(&[("A.md", "First line  \nSecond line")])?;
    let t = title(&blocks(&c, "A")[0]);
    probe(
        &mut rows,
        "markdown_hard_break",
        syntax,
        "Preserve an explicit two-space hard break, or translate it to an explicit break",
        t.contains("  \n") || t.contains("\\\n") || t.contains("<br"),
        json!(t),
    );
    let c = convert_files(&[(
        "A.md",
        "# One\n## Two\n### Three\n#### Four\n##### Five\n###### Six",
    )])?;
    let edn = c.edn.to_edn_string();
    probe(
        &mut rows,
        "six_heading_levels",
        syntax,
        "Convert all six ATX heading levels",
        (1..=6).all(|n| edn.contains(&format!(":logseq.property/heading {n}"))),
        json!(c.stats.blocks),
    );
    let c = convert_files(&[("A.md", "    [[Ghost]] #literal\n    indented code")])?;
    let b = &blocks(&c, "A")[0];
    probe(
        &mut rows,
        "indented_code",
        syntax,
        "Treat four-space indented code as literal code",
        items(get(b, "build/tags")).contains(&Edn::kw("logseq.class/Code-block"))
            && title(b) == "[[Ghost]] #literal\nindented code"
            && c.stats.placeholder_pages == 0,
        json!({"block": b.to_edn_string(), "placeholders": c.stats.placeholder_pages}),
    );
    let c = convert_files(&[("A.md", "[label][ref]\n\n[ref]: B.md"), ("B.md", "Target")])?;
    let target = uuid(get(page(&c, "B"), "page").unwrap());
    probe(
        &mut rows,
        "reference_style_link",
        "obsidian-help/en/Editing and formatting/Obsidian Flavored Markdown.md",
        "Resolve a CommonMark reference-style internal link to the converted page",
        title(&blocks(&c, "A")[0]) == format!("[label]([[{target}]])"),
        json!(title(&blocks(&c, "A")[0])),
    );
    let c = convert_files(&[
        ("A.md", "![[clip.mp3]]\n\n![[clip.mp4]]"),
        ("clip.mp3", "audio bytes"),
        ("clip.mp4", "video bytes"),
    ])?;
    let edn = c.edn.to_edn_string();
    probe(
        &mut rows,
        "audio_video_asset_metadata",
        "obsidian-help/en/Files and folders/Accepted file formats.md",
        "Register linked MP3/MP4 files as assets with their extensions (not a playback test)",
        c.assets.len() == 2
            && edn.contains(":logseq.property.asset/type \"mp3\"")
            && edn.contains(":logseq.property.asset/type \"mp4\""),
        json!({"assets": c.assets.iter().map(|a| &a.file_name).collect::<Vec<_>>()}),
    );
    let c = convert_files(&[("A.md", "```mermaid\ngraph TD\nA-->B\n```")])?;
    let b = &blocks(&c, "A")[0];
    probe(
        &mut rows,
        "mermaid_source",
        "obsidian-help/en/Editing and formatting/Advanced formatting syntax.md",
        "Preserve Mermaid source and language (not a diagram-rendering test)",
        title(b) == "graph TD\nA-->B"
            && get(b, "build/properties").and_then(|p| get(p, "logseq.property.code/lang"))
                == Some(&Edn::str("mermaid")),
        json!(b.to_edn_string()),
    );
    let c = convert_files(&[("A.md", "```query\ntag:#project\n```")])?;
    let b = &blocks(&c, "A")[0];
    probe(
        &mut rows,
        "embedded_search",
        "obsidian-help/en/Plugins/Search.md",
        "Translate an Obsidian search embed into a functioning Logseq query",
        items(get(b, "build/tags")).contains(&Edn::kw("logseq.class/Query")),
        json!(b.to_edn_string()),
    );
    let html = "<div>[[Ghost]] ==literal==</div>";
    let c = convert_files(&[("A.md", html)])?;
    probe(
        &mut rows,
        "html_literal_context",
        "obsidian-help/en/Editing and formatting/HTML content.md",
        "Do not interpret Markdown inside an HTML element",
        title(&blocks(&c, "A")[0]) == html && c.stats.placeholder_pages == 0,
        json!({"text": title(&blocks(&c, "A")[0]), "placeholders": c.stats.placeholder_pages}),
    );
    let c = convert_files(&[
        (".obsidian/templates.json", r#"{"folder":"Templates"}"#),
        ("Templates/A.md", "---\nproject: Apollo\n---\nTemplate body"),
    ])?;
    let template = &blocks(&c, "A")[0];
    probe(
        &mut rows,
        "template_frontmatter",
        "obsidian-help/en/Plugins/Templates.md",
        "Keep template properties in the material inserted into the destination note",
        template.to_edn_string().contains(":user.property/project"),
        json!({"template": template.to_edn_string(), "source_page": get(page(&c, "A"), "page").map(Edn::to_edn_string)}),
    );

    Ok(rows)
}

fn main() -> Result<()> {
    let rows = collect_probes()?;
    let pass = rows.iter().filter(|r| r["status"] == "pass").count();
    let report = json!({"scope": "Generated EDN semantics; not UI rendering or exhaustive Obsidian support", "total": rows.len(), "passed": pass, "gaps": rows.len() - pass, "probes": rows});
    let output = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "target/compatibility/features.json".into());
    let path = Path::new(&output);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(&report)?)
        .context("write compatibility report")?;
    for row in report["probes"].as_array().unwrap() {
        println!(
            "{:4} {}",
            row["status"].as_str().unwrap(),
            row["id"].as_str().unwrap()
        );
    }
    println!(
        "{pass}/{} probes pass; report: {}",
        report["total"],
        path.display()
    );
    if report["gaps"].as_u64().unwrap() != 0 {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documented_supported_formats() -> Result<()> {
        let probes = collect_probes()?;
        // Gaps are reported by the executable; these supported behaviors are
        // regression checks in the ordinary test suite.
        for id in [
            "wikilink_label",
            "markdown_percent_encoded",
            "heading_reference",
            "paragraph_reference",
            "standalone_wiki_embed",
            "tag_case_hierarchy_numeric",
            "code_literals",
            "comments_highlights_display_math",
            "json_frontmatter",
            "mixed_property_types",
            "image_width",
            "table_escaped_wikilink",
            "footnotes",
            "folded_callout",
            "daily_path_format",
            "template_variables",
            "basic_emphasis",
            "paragraph_structure",
            "six_heading_levels",
            "audio_video_asset_metadata",
            "mermaid_source",
        ] {
            let probe = probes.iter().find(|p| p["id"] == id).unwrap();
            assert_eq!(probe["status"], "pass", "{probe:#}");
        }
        Ok(())
    }
}
