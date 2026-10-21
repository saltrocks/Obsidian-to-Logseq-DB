# Obsidian → Logseq feature coverage

**The converter covers common Markdown notes, but migration of an entire
Obsidian vault is partial.** The largest gaps affect link resolution, some
Markdown constructs, embeds, property configuration, templates, bookmarks,
Canvas, Bases, and executable queries/plugin content.

This inventory is organized by Obsidian features, using the official Help and
developer documentation pinned in the [compatibility audit](compatibility.md#sources-and-runtime).
The evidence is the converter implementation, existing fixture/unit/import
tests, and 41 focused probes in
[`compatibility_audit.rs`](../examples/compatibility_audit.rs). It is not a
source-code coverage measurement. The probes are selected examples, not a
complete or equally weighted denominator for Obsidian's feature set.

Statuses describe migration of the feature listed in each row:

- **Supported:** the stated conversion works for the tested cases.
- **Partial:** some content or functionality converts, with a known limitation.
- **Source only:** text or attachment bytes survive; working behavior or rendering
  has not been established.
- **Missing:** the converter does not migrate the feature's semantics.

“Supported” does not certify every CommonMark edge case or visual equivalence
between the two applications. UI rendering has not been systematically tested.

## Notes and organization

Source: [Obsidian storage][storage], [file formats][formats], [aliases][aliases].

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| Markdown notes, including empty notes | Supported | A page per note; note contents become blocks. | `converts_sample_vault`, actual DB imports |
| Duplicate filenames in different folders | Supported | Distinct page names with folder suffixes where needed. | Fixture: two `Index.md` notes |
| Names colliding with Logseq built-ins | Supported | Renamed to avoid merging with built-in pages. | Fixture: `Task.md`; imported graph checks |
| Creation/modification timestamps | Partial | Uses filesystem times. Sync/copy operations may already have changed those times; no Obsidian history is recovered. | `vault.rs::file_times`, fixture output |
| Folder hierarchy / file explorer organization | Missing | Folders help resolve names and links but are not rebuilt as an equivalent hierarchy in Logseq. | `create_note_entities`, `Vault::scan` |
| Aliases | Partial | Creates alias pages and resolves alias references; aliases colliding with notes/built-ins are skipped with warnings. | Fixture; corpus collision warnings |
| Unreferenced attachments | Missing | Only referenced attachments are selected for copying. | `finish`; corpus counts |
| Symbolic links | Partial | Follows readable links and warns on unreadable entries; no symlink relationship is retained in the destination. | `dangling_symlink_is_skipped`, `Vault::scan` |

## Markdown and formatting

Sources: [basic formatting][basic], [advanced formatting][advanced],
[Obsidian Flavored Markdown][markdown], [HTML content][html].

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| Paragraphs and continuation lines | Supported | Paragraph boundaries become blocks; continuation text stays together. | `paragraph_structure` |
| Explicit Markdown hard breaks | Partial | Two trailing spaces are stripped, losing the explicit hard-break marker. Destination soft-break rendering may mask the difference. | `markdown_hard_break` **gap** |
| ATX headings `#` through `######` | Supported | Six heading levels become heading properties; content can nest under them. | `six_heading_levels`; heading-nesting/flat unit tests |
| Bold, italic, nested emphasis, strikethrough | Supported | Retains the Markdown markup. | `basic_emphasis` |
| Basic `==highlight==` | Supported | Translates to Logseq `^^highlight^^`. | Fixture; `comments_highlights_display_math` |
| Colored highlights | Partial | No color-specific conversion; color emoji is retained as text inside the highlight. | `render_inline`; [color syntax][basic] |
| Horizontal rules | Supported | Preserves a rule as `---`; original rule spelling is normalized. | `hr_is_not_a_list` |
| Unordered lists and nesting | Supported | Converts items into nested blocks. | `nested_lists_and_tasks`, list-context tests |
| Ordered lists | Partial | Preserves ordering style and nesting; the starting number is discarded. | Fixture/deferred property tests; `parse_list_marker` |
| Inline code and fenced code | Supported | Preserves literal text; fenced blocks carry the language. Backtick/tilde fences are recognized. | `code_literals`, code fence unit tests |
| Indented code | Missing | Four-space indented code becomes ordinary text; literal links/tags inside it can be interpreted. | `indented_code` **gap** |
| Markdown tables | Partial | Retains table text and rewrites links; layout/rendering and complex multiline cells are not certified. | `table_escaped_wikilink`; corpus import |
| Block quotes | Supported | Converts ordinary quotes to Logseq Quote blocks. | Fixture; quote parser tests |
| Display math `$$…$$` | Supported | Preserves formula text in a Math block. | Display-math unit/fixture tests |
| Inline math `$…$` | Partial | Simple expressions survive; Markdown-like content inside formulas can be rewritten. | `inline_math_literal` **gap** |
| Numbered/named and inline footnotes | Supported | Keeps definitions with blocks that reference them. | `footnotes`; `footnotes_join_their_references` |
| `%%comments%%` | Supported | Removes comments by default; `--keep-comments` retains them; code literals are protected. | Comment parser tests; `code_literals` |
| HTML blocks and inline HTML | Partial | Retains HTML text but may incorrectly interpret Markdown inside HTML. No general HTML-to-Logseq transformation. | `html_literal_context` **gap** |
| Mermaid diagrams | Source only | Preserves source as a Code block with language `mermaid`; successful diagram rendering is unverified. | `mermaid_source` |

## Links and block references

Source: [internal links][links].

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| Ordinary `[[Note]]` links and custom labels | Supported | Resolves notes to UUID references and retains labels. | `wikilink_label`; fixture/import checks |
| Inline Markdown links to existing notes | Supported | Resolves `[label](Note.md)`, including percent-encoded paths. | `markdown_percent_encoded` |
| Vault-root folder-qualified links | Partial | A matching relative shadow path can incorrectly take priority over the root path. | `vault_root_path` **gap** |
| Explicit `./` and `../` relative links | Supported | Normalizes relative paths within the vault. | `resolves_like_obsidian` |
| Links to headings | Partial | Simple headings work; duplicate leaf headings are not disambiguated using the full parent path. | `heading_reference`; `qualified_duplicate_heading` **gap** |
| Same-note Markdown anchors | Missing | `[label](#Heading)` stays unchanged rather than targeting the converted heading block. | `markdown_local_anchor` **gap** |
| Paragraph block IDs and references | Supported | `^id` and `[[Note#^id]]` become UUID references. | `paragraph_reference`; block-ID tests |
| IDs for an entire list | Partial | A standalone ID after a list targets only the last item. | `whole_list_reference` **gap** |
| Links to missing notes | Partial | Missing wikilinks create placeholders; missing Markdown links remain file paths. | Fixture; `markdown_missing_note` **gap** |
| Reference-style Markdown links | Missing | `[label][ref]` plus `[ref]: Note.md` is not resolved to the converted page. | `reference_style_link` **gap** |
| External web links | Supported | Retains ordinary external Markdown URLs. | `render_md_link`; fixture |
| `obsidian://` links | Source only | Keeps the original URI; it still points into Obsidian. | `has_scheme`, `render_md_link` |
| Backlinks / outgoing-link relationships | Partial | Logseq derives references from successfully converted links. Incorrect or unconverted links also affect derived relationships. | Actual reference/import checks; link gaps above |

## Embeds, attachments and web content

Sources: [embeds][embeds], [accepted file formats][formats], [web embeds][web].

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| Standalone wiki page/heading/block embeds | Supported | Emits `block/link` for supported targets. Target-resolution limitations above still apply. | `standalone_wiki_embed`; fixture |
| Wiki embeds inside a sentence | Partial | Converts them to references instead of transcluding content. | `render_wikilink`; documented limitation |
| Markdown note embeds | Missing | A standalone `![](Note.md)` becomes a page reference. | `markdown_note_embed` **gap** |
| Referenced local images | Supported | Registers assets and copies file bytes. PNG byte copying and image asset properties are tested; all image decoders are not tested. | Fixture and corpus asset verification |
| Image sizing | Partial | Preserves width; drops height; first width wins for all uses of the same asset. | `image_width`; `image_height` **gap** |
| External images | Source only | Retains external image URLs; width/height directives are not normalized into asset metadata. | `render_md_link`; fixture |
| Audio and video attachments | Source only | Registers linked files as assets with their extension. Playback and codec support are not validated. | `audio_video_asset_metadata` |
| PDFs | Partial | Copies PDFs, but page/height selectors are lost; annotation semantics are not imported. | Fixture byte checks; `pdf_page_selector` **gap** |
| YouTube and tweet embeds | Partial | Converts recognized image-embed URLs into Logseq macros. Live external rendering is not tested. | `web_embeds`, fixture |
| Generic iframe/web-page embeds | Source only | No dedicated iframe conversion; HTML may remain as text. | `render_inline`, `render_md_link` |
| Canvas embeds | Missing | A linked `.canvas` may be an opaque asset; nodes, edges, layout and embedded content are not migrated. | `Vault::scan`, `asset_for`; Canvas limitation |

## Properties, tags and tasks

Sources: [properties][properties], [tags][tags], [task lists][basic].
Tasks-plugin mappings are an additional supported convention, not the core
Obsidian checkbox specification.

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| YAML and JSON frontmatter | Supported | Reads both formats; malformed YAML is warned about and skipped. | `json_frontmatter`; fixture; duplicate-key tests |
| Text, number, checkbox and URL values | Supported | Infers matching property types from compatible values. | `infers_types`; fixture; actual property queries |
| Date and date/time values | Partial | Recognizes supported ISO shapes; interprets times using local time rules. Explicit timezone/locale parity is not certified. | Property/date tests; `parse_iso_datetime`, `local_ms` |
| List properties and note-valued properties | Partial | Retains compatible lists and node refs; mixed types are coerced to text and checkbox/datetime lists cannot retain those scalar types. | `mixed_property_types`; fixture; `SchemaBuilder::build` |
| Explicit property-type assignments | Missing | Ignores `.obsidian/types.json`; text looking like a date is retyped as a date. | `configured_property_type` **gap** |
| `cssclasses` / appearance metadata | Missing | Drops CSS classes. | `DROPPED_KEYS`; fixture |
| Ordinary tags, case folding, nested tags | Supported | Maps tags to classes and nested tags to inheritance; numeric-only tags stay text. | `tag_case_hierarchy_numeric`; fixture |
| Full Unicode tag syntax | Partial | Accented letters work; emoji tags do not. | `emoji_tags` **gap** |
| Core unchecked/checked tasks `[ ]`, `[x]` | Supported | Maps to Todo/Done. | Fixture |
| Other core checkbox characters | Partial | Core Obsidian considers non-space characters complete; the converter maps `/` and `-` to plugin-style states and unknown markers to Todo. | `core_nonstandard_checkbox` **gap** |
| Tasks-plugin scheduling, priority, dates | Supported | Maps the documented converter-supported date and priority markers to properties. | `task_meta`; fixture |
| Tasks-plugin recurrence and custom states | Partial | Simple date-backed intervals convert; weekday/date-specific recurrence and arbitrary custom workflows do not. | `task_meta`, `parse_recurrence`; README limitations |

## Callouts, journals, templates and bookmarks

Sources: [callouts][callouts], [daily notes][daily], [templates][templates],
[bookmarks][bookmarks], [configuration folders][config].

| Obsidian feature | Status | What the converter preserves / loses | Evidence |
| --- | --- | --- | --- |
| Callout text and titles | Partial | Maps to a smaller set of Logseq admonitions; unsupported types become a title. Original icons/colors are not preserved. | `callout_admonition`; fixture |
| Folded callouts | Supported | Preserves the folded title/body structure as collapsed blocks. | `folded_callout`; fixture |
| Nested callouts | Missing | Inner callout syntax remains inside the outer body. | `nested_callout` **gap** |
| Daily notes / journals | Partial | Reads daily-folder/date settings and supported date tokens, including nested paths; does not cover every Moment.js format or locale. | `daily_path_format`; daily/date tests |
| Template folder and standard variables | Supported | Creates Template blocks and converts `title`, `date`, `time`. | `template_variables`; fixture |
| Template date/time formatting | Missing | Explicit format tokens are discarded. | `template_custom_format` **gap** |
| Template properties applied on insertion | Missing | Frontmatter belongs to the source template page; it is absent from the inserted Template block content. | `template_frontmatter` **gap** |
| File bookmarks | Supported | Converts note bookmarks to favorites. | Fixture |
| Heading/block bookmarks | Partial | Favorites the containing note instead of the exact target. | Fixture; `collect_bookmarks` |
| Bookmark groups, titles and non-file bookmarks | Missing | Group hierarchy/custom labels do not survive; folder, search, URL and graph bookmarks are skipped. | `collect_bookmarks`, `favorites`; fixture warning |
| Alternate configuration folder | Missing | Reads `.obsidian/` only; no general `Vault.configDir` support. | `load_obsidian_config`; [developer contract][configdir] |

## Other persisted Obsidian features

Sources: [core plugin inventory][core], [Canvas][canvas], [Bases][bases],
[search embeds][search], [community plugins][plugins].

| Obsidian feature | Status | Migration outcome |
| --- | --- | --- |
| Canvas graphs | Missing | No conversion of cards, groups, edges or positions. Unlinked `.canvas` files are skipped. |
| Bases | Missing | No conversion of `.base` filters, formulas, views or layouts; linked files may be opaque assets. |
| Embedded core Search queries | Missing | `query` fences become Code blocks, not functioning Logseq queries. Confirmed by `embedded_search`. |
| Dataview / DataviewJS | Source only | Code-fence text can survive; query execution and computed results are not migrated. |
| Templater and other plugin commands | Source only | No plugin runtime or command translation; ordinary note text may survive. |
| General community-plugin data | Missing | Plugin configuration/databases in the configuration directory are not imported. Tasks syntax is a specific exception, not general plugin support. |
| Saved workspace and graph layout | Missing | No migration of pane state, tabs, graph filters or layout. |
| Fold state of ordinary notes/headings/lists | Missing | No loading of Obsidian's saved editor fold state; folded callout syntax is handled separately. |
| File Recovery / Sync history | Missing | Only current vault files are read; snapshots and version history are not migrated. |
| Publish configuration | Missing | Ordinary YAML values may survive as properties; publishing settings, routing behavior, access control and site deployment do not migrate. |

Editor commands and services such as Quick Switcher, Command Palette, hotkeys,
themes, Sync accounts and Publish hosting are application behavior. They are
not counted as successfully converted simply because Logseq provides a similar
UI. Likewise, preserving a Mermaid/Dataview program as a code block does not
establish feature parity.

## Findings and next implementation priorities

The expanded probes produced **21 passes and 20 gaps across 41 checks**. Some
passing checks deliberately cover source preservation only, as labeled above.
This is evidence for individual rows, not “51% Obsidian support.”

The first audit found 14 gaps. Broadening the feature inventory confirmed six
more: hard-break markers, indented code, reference-style internal links, embedded
Search queries, Markdown interpretation inside HTML, and template frontmatter.

Prioritize data fidelity before optional application features:

1. Correct root-relative/heading/list targets and protect inline math, indented
   code and HTML from accidental link/tag rewriting.
2. Complete Markdown links and embeds, explicit property types, and Unicode tags.
3. Preserve image/PDF options, nested callouts, template properties/formats and
   bookmark targets where Logseq can represent them.
4. Design separate migrations for Canvas, Bases and executable queries. Their
   absence is not evidence that Logseq cannot represent any useful counterpart.

Run `cargo run --locked --example compatibility_audit` to reproduce the focused
checks. It writes `target/compatibility/features.json` and exits 1 while gaps
remain. Ordinary regression tests assert the supported cases. This follow-up
expands and corrects the feature assessment; it does not implement those missing
conversions.

[storage]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Files%20and%20folders/How%20Obsidian%20stores%20data.md
[formats]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Files%20and%20folders/Accepted%20file%20formats.md
[aliases]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Linking%20notes%20and%20files/Aliases.md
[basic]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Basic%20formatting%20syntax.md
[advanced]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Advanced%20formatting%20syntax.md
[markdown]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Obsidian%20Flavored%20Markdown.md
[html]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/HTML%20content.md
[links]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Linking%20notes%20and%20files/Internal%20links.md
[embeds]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Linking%20notes%20and%20files/Embed%20files.md
[web]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Embed%20web%20pages.md
[properties]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Properties.md
[tags]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Tags.md
[callouts]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Editing%20and%20formatting/Callouts.md
[daily]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Daily%20notes.md
[templates]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Templates.md
[bookmarks]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Bookmarks.md
[config]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Files%20and%20folders/Configuration%20folder.md
[configdir]: https://github.com/obsidianmd/obsidian-developer-docs/blob/1c26772ac13d6e9870c865936d389fdfe694e742/en/Reference/TypeScript%20API/Vault/configDir.md
[core]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Core%20plugins.md
[canvas]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Canvas.md
[bases]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Bases/Bases%20syntax.md
[search]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Plugins/Search.md
[plugins]: https://github.com/obsidianmd/obsidian-help/blob/cd6c9f3c7d5492bbbec94e8f403572b84eeb325b/en/Extending%20Obsidian/Community%20plugins.md
