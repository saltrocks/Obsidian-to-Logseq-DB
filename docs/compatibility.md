# Compatibility audit — 2026-10-21

For the feature-by-feature migration assessment, see the
[Obsidian feature coverage matrix](obsidian-feature-coverage.md). This document
also records runtime-import and engineering checks from the initial audit.

The converter successfully imports the fixture and both official Obsidian
documentation vaults into the installed Logseq DB build. Logseq reports no
invalid entities. This establishes working import compatibility for those
inputs; it does **not** establish lossless Obsidian conversion. Thirty targeted
format probes initially identified **16 supported cases and 14 fidelity gaps**.
These are selected examples, not a percentage of all Obsidian features.
The feature-focused follow-up expanded the probes to **41 checks: 21 passes
and 20 gaps**, including six further gaps detailed in the feature matrix.

## Sources and runtime

Official repositories were shallow-cloned into the ignored `references/`
directory. The Help vault supplements the developer documentation because it
documents user-facing Markdown and file semantics.

| Local checkout | Upstream | Audited commit |
| --- | --- | --- |
| `references/logseq` | [logseq/logseq](https://github.com/logseq/logseq) | `18686f93e1e38d4bfeb8268e2566a61a87c2e0ed` |
| `references/logseq-docs` | [logseq/docs](https://github.com/logseq/docs) | `27ccef5400b75678bc18e46f1d40bfa74c8d1f45` |
| `references/obsidian-developer-docs` | [obsidianmd/obsidian-developer-docs](https://github.com/obsidianmd/obsidian-developer-docs) | `1c26772ac13d6e9870c865936d389fdfe694e742` |
| `references/obsidian-help` | [obsidianmd/obsidian-help](https://github.com/obsidianmd/obsidian-help) | `cd6c9f3c7d5492bbbec94e8f403572b84eeb325b` |

Runtime tests used the installed `logseq` CLI: revision **`3caf728-dirty`**,
build time **2026-10-16T01:14:33.471Z**. Rust was **1.99.1** and LLVM tools were
**23.1.3**, matching rustc. The cloned Logseq HEAD was inspected, not compiled
or executed. Thus the runtime results apply to the installed build, not every
Logseq release or the newer cloned revision.

The source review checked the [Build EDN schemas][build],
[export/import implementation][export], [CLI contract][cli], built-in classes
and properties, and the `/v1/invoke` transport. All **39** built-in class/property
keyword literals used by `convert.rs` and `phases.rs` occur in the cloned DB
sources. This name check complements, rather than replaces, the real imports.

## Executed checks

| Check | Result |
| --- | --- |
| Original tests before changes | 48 passed; 1 end-to-end test ignored |
| Final regular suite | 50 passed; 2 end-to-end tests ignored by default |
| Explicit end-to-end suite | 2 passed: phased Transit import and serialized EDN import |
| Documentation format probes | Initially 30 (16 pass, 14 gaps); expanded to 41 (21 pass, 20 gaps) in the feature assessment |
| `cargo fmt --check` | Passed |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| Production line coverage | **3,287 / 3,598 = 91.36%** |

End-to-end tests now use a fresh temporary root, stop their workers even on
assertion failure, and never replace a graph in the user's normal Logseq root.
Explicitly running an ignored test actually runs it; the former `O2L_E2E`
environment gate could silently return success without testing an import.
Use `O2L_LOGSEQ_BIN` to select another CLI executable.

The phased fixture test uses `--chunk-blocks 1`, forcing separate whole-page
chunks and a link repair phase. It validates the final graph, checks rendered
link targets, queries scalar and node properties, and compares copied PNG/PDF
bytes. The serialized EDN test goes through `logseq graph import`, validates
the resulting graph, and checks imported note UUIDs and a numeric property.
Neither test treats a successful import exit code alone as sufficient proof.

## Real documentation vaults

Both English vaults were converted to EDN and also imported through `--graph`
with a chunk target of 1,000 blocks. Every graph passed `logseq graph validate`
with `errors: null` and `invalid-entity-ids: []`.

| Input | Source notes | Converted content blocks | Copied assets | Final DB pages | Final DB entities |
| --- | ---: | ---: | ---: | ---: | ---: |
| Obsidian Help `en/` | 179 | 8,086 | 120 | 508 | 10,261 |
| Obsidian Developer Docs `en/` | 1,319 | 12,199 | 19 | 2,411 | 14,856 |
| Total | **1,498** | **20,285** | **139** | **2,919** | **25,117** |

DB counts include built-in pages, alias pages, assets and property-value
entities. They are not expected to equal source counts. The Help conversion
created 176 aliases and one missing-note placeholder; the developer conversion
created 945 aliases and two placeholders. There were four and five alias
collision warnings, respectively. Unreferenced attachments were skipped: 25
and 9, respectively.

All 139 copied assets matched a source attachment's SHA-256, and the number of
copied files matched each converter asset count. This verifies bytes and counts;
it does not verify PDF navigation or image rendering.

The generated EDN, asset folders, graphs and probe output are under
`target/compatibility/`. The two corpus graph names are `help-docs` and
`developer-docs`, rooted at `target/compatibility/logseq-root`. Their workers
were stopped after validation. Generated files are ignored by Git.

## Supported feature coverage

| Feature family | Evidence and practical scope |
| --- | --- |
| Pages and identity | Fixture covers duplicate filenames, reserved-title renaming, aliases and missing wikilinks; actual imports verify note UUIDs. |
| Links | Probes verify ordinary wikilinks, labels, percent-encoded Markdown paths, heading UUIDs and paragraph block IDs. |
| Embeds and assets | Standalone wiki page/block embeds, local PNG/PDF assets, image width and asset copying are covered. |
| Outlines | Unit/fixture coverage includes heading nesting, flat mode, nested lists and numbered-list properties. |
| Tags | Case-insensitive tags, numeric non-tags, nested tag inheritance, note/tag merging and tags-as-pages are covered. |
| Properties | Inferred text/number/checkbox/URL/date/datetime/node types, JSON frontmatter, mixed types, duplicate YAML keys and invalid/non-finite values are covered. |
| Tasks | Fixture covers Todo/Doing/Done/Canceled mappings, priority, deadlines, scheduling, supported recurrence and extra Tasks plugin dates. These are plugin-oriented mappings. |
| Formatting | Fenced/inline code literals, display math, comments, highlights, escaped-pipe table links, footnotes, quotes and folded callouts are covered. Table/rendering appearance is not UI-tested. |
| Daily notes | Standard date patterns, folder scoping, nested date paths and journal UUIDs are covered. |
| Templates/favorites | Standard template variables, Template blocks, bookmarks and skipped unsupported bookmark types are covered. |
| DB format and transport | Both serialized EDN and direct Transit imports, chunk fix-ups, deferred properties and graph validation are exercised. |

## Initial 14 fidelity gaps

These remain unresolved. The feature matrix additionally records hard-break
markers, indented code, reference-style links, embedded Search queries, HTML
literal context and template frontmatter.

Each ID below is executable in
[`examples/compatibility_audit.rs`](../examples/compatibility_audit.rs).
The report includes expected semantics and observed generated EDN. Examples
here are minimal synthetic reproducers derived from the linked official docs.

| Priority | Probe ID / input | Observed behavior | Contract / implementation |
| --- | --- | --- | --- |
| High | `vault_root_path`: from `Folder/A.md`, `[[Projects/Target]]`, with both root and nested targets | Chooses `Folder/Projects/Target.md` before the root-qualified path. The docs specify that folder paths start at the vault root. | [Internal links][links]; `vault.rs::resolve` |
| High | `qualified_duplicate_heading`: `[[B#Second#Details]]` with two `Details` headings | Resolves the first `Details` heading, ignoring its parent hierarchy. | [Internal links][links]; `convert.rs::resolve_link` |
| High | `whole_list_reference`: two list items followed by a standalone `^whole-list` | Assigns the ID to the final item; an embed loses the rest of the list. | [List embeds][embeds]; `markdown.rs::handle_top_level` |
| High | `inline_math_literal`: `$\text{[[Ghost]] ==x==}$` | Rewrites the formula's brackets as a page reference and its equals signs as a highlight; creates an unwanted page. | [Inline math][advanced]; `convert.rs::render_inline` |
| Medium | `markdown_local_anchor`: `[local](#Heading)` | Leaves the local Markdown anchor unchanged instead of referencing the converted heading block. | [Internal links][links]; `convert.rs::render_md_link` |
| Medium | `markdown_missing_note`: `[missing](Missing.md)` | Leaves a file link and creates no placeholder, unlike the equivalent wikilink. | [Internal links][links]; `convert.rs::render_md_link` |
| Medium | `markdown_note_embed`: `![](B.md)` alone | Becomes an ordinary page reference rather than `block/link`. | [Embeds][embeds]; `convert.rs::standalone_embed` |
| Medium | `emoji_tags`: `#🚀 #café` | Accented tag is converted; emoji tag is left as text without tag semantics. | [Tag characters][tags]; `convert.rs::tag_len` |
| Medium | `core_nonstandard_checkbox`: `- [?] completed` | Becomes Todo. Core Obsidian considers any non-space checkbox character complete; the converter favors Tasks-style states for some markers. | [Task lists][basic]; `convert.rs::task_status` |
| Medium | `configured_property_type`: `.obsidian/types.json` sets `code` to text, YAML value is `'2024-01-02'` | Infers a date and creates a journal reference, ignoring the explicit property type. | [Property types][properties]; `convert.rs::build_schemas` |
| Medium | `pdf_page_selector`: `![[paper.pdf#page=3&height=400]]` | Copies the PDF but drops the page and height selectors. | [PDF embeds][embeds]; `convert.rs::resolve_link` |
| Lower | `image_height`: `![[image.png\|320x200]]` | Preserves width 320 but discards height 200. | [Image embeds][embeds]; `convert.rs::embed_width` |
| Lower | `nested_callout`: a tip callout inside a note callout | Translates the outer callout but leaves `[!tip]` syntax in its body. | [Nested callouts][callouts]; `markdown.rs::parse_quote` |
| Lower | `template_custom_format`: `{{date:YYYY}} {{time:HH}}` | Becomes `<% today %> <% time %>`; explicit format tokens are discarded. | [Template variables][templates]; `convert.rs::template_variables` |

The existing documented exclusions also remain: Canvas/Bases semantics,
Dataview/plugin execution, folder structure, numbered-list starting values and
inline wiki embeds. CSS classes are dropped; image width is global to an asset
and its first size wins. A linked Canvas/Base file may be copied as an opaque
asset; this is not conversion of its functionality.

Further coverage limits: the converter assumes `.obsidian/` even though
[`Vault.configDir` can differ][configdir]; property types are inferred; date/time
and template formats do not cover the entire Moment.js/Obsidian configuration
space. Browser/desktop rendering, visual equivalence, plugin behavior, sync,
failure recovery, and extremely large single pages were not certified here.

## Defect fixed during the audit

Serializing `Edn::Int(i64::MIN)` called `i64::abs()`, which overflows and panics
in a debug build. The new integer-boundary regression test was first run against
the original implementation and failed with `attempt to negate with overflow`.
The serializer now checks the safe JSON integer range without negating the
value. Both signed extremes use Transit integer strings; safe bounds and zero
remain JSON numbers. The test passes after the change. This verifies the wire
encoding, not Logseq's exact arithmetic for numbers outside JavaScript's safe
integer range.

## Initial code coverage measurement

This engineering metric predates the additional feature probes and is not a
measure of Obsidian feature support.

[`scripts/coverage.py`](../scripts/coverage.py) uses rustc instrumentation and
matching LLVM tools, with fresh profile counters per invocation. This run
includes all regular tests, both end-to-end tests, and a CLI file-export smoke
check. Counts exclude dependencies, standalone tests/examples and the trailing
`#[cfg(test)]` modules in `src/`. They measure executed lines, not branches or
format fidelity. Even a probe exposing a gap executes and covers code.

| Production file | Covered / executable lines | Line coverage |
| --- | ---: | ---: |
| `convert.rs` | 1,409 / 1,502 | 93.81% |
| `datefmt.rs` | 124 / 146 | 84.93% |
| `edn.rs` | 81 / 85 | 95.29% |
| `logseq.rs` | 161 / 210 | 76.67% |
| `main.rs` | 131 / 185 | 70.81% |
| `markdown.rs` | 524 / 569 | 92.09% |
| `phases.rs` | 305 / 314 | 97.13% |
| `properties.rs` | 199 / 221 | 90.05% |
| `transit.rs` | 51 / 52 | 98.08% |
| `vault.rs` | 302 / 314 | 96.18% |
| **Total** | **3,287 / 3,598** | **91.36%** |

The weakest areas are CLI orchestration and worker error/recovery paths. See
`target/compatibility/coverage/summary.json`, `coverage.lcov`, `tests.log`, and
`html/index.html`. The raw LLVM summary/HTML includes embedded test functions;
`summary.json` filters them out for the production totals above.

## Repeat the checks

```sh
cargo test --locked --all-targets
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings

# Requires the Logseq CLI; each test owns a temporary graph root.
cargo test --locked --test convert_fixture -- --ignored --test-threads=1

# Exit 1 is expected while any format gap remains. JSON contains every result.
cargo run --locked --example compatibility_audit

# Requires llvm-cov and llvm-profdata matching `rustc -vV`.
python3 scripts/coverage.py --e2e
```

The format probes and ordinary tests do not require the documentation clones or
a running Logseq instance. The coverage command without `--e2e` skips Logseq
imports and still exercises CLI file export.

To repeat the corpus imports without replacing any existing graphs:

```sh
cargo build --locked
audit_root="$(mktemp -d)"
for corpus in obsidian-help obsidian-developer-docs; do
  target/debug/obsidian-to-logseq "references/$corpus/en" \
    --graph "$corpus" --root-dir "$audit_root" --chunk-blocks 1000
  logseq graph validate --graph "$corpus" --root-dir "$audit_root" --output json
  logseq server stop --graph "$corpus" --root-dir "$audit_root"
done
```

[build]: https://github.com/logseq/logseq/blob/18686f93e1e38d4bfeb8268e2566a61a87c2e0ed/deps/db/src/logseq/db/sqlite/build.cljs
[export]: https://github.com/logseq/logseq/blob/18686f93e1e38d4bfeb8268e2566a61a87c2e0ed/deps/db/src/logseq/db/sqlite/export.cljs
[cli]: https://github.com/logseq/logseq/blob/18686f93e1e38d4bfeb8268e2566a61a87c2e0ed/docs/cli/logseq-cli.md
[links]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Linking%20notes%20and%20files/Internal%20links.md
[embeds]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Linking%20notes%20and%20files/Embed%20files.md
[tags]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Tags.md
[basic]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Basic%20formatting%20syntax.md
[advanced]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Advanced%20formatting%20syntax.md
[properties]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Properties.md
[callouts]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Callouts.md
[templates]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Templates.md
[configdir]: https://github.com/obsidianmd/obsidian-developer-docs/blob/1c26772ac13d6e9870c865936d389fdfe694e742/en/Reference/TypeScript%20API/Vault/configDir.md
