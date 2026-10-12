# obsidian-to-logseq

Converts an Obsidian vault into a **Logseq DB graph** (the SQLite-based Logseq
"DB version", not the markdown file graph).

The vault is converted into Logseq's EDN graph import format
(`sqlite.build` / `:graph-human`). The tool can then either

* create the DB graph directly through the `logseq` CLI (`--graph`), or
* write an `.edn` file plus an assets folder for a manual import.

## Usage

```sh
cargo build --release

# Create a new Logseq DB graph "my-vault" from the vault (needs the `logseq` CLI)
./target/release/obsidian-to-logseq ~/Obsidian/MyVault --graph my-vault

# Recreate it after changing options
./target/release/obsidian-to-logseq ~/Obsidian/MyVault --graph my-vault --replace

# Only write MyVault.edn and MyVault-assets/
./target/release/obsidian-to-logseq ~/Obsidian/MyVault -o MyVault.edn
```

For a manual import, use `logseq graph import --graph <name> --type edn --input
MyVault.edn` or the app's *Import → EDN to DB graph*, then copy the files from
`MyVault-assets/` into `~/logseq/graphs/<name>/assets/`. Import into a new,
empty graph: Logseq merges imported pages into existing pages with the same
title. For large vaults, `--graph` is much faster (see *How importing works*).

Options:

| Option | Effect |
| --- | --- |
| `--graph NAME` | Create the DB graph `NAME` and import into it |
| `--replace` | Delete and recreate the graph if it exists |
| `-o FILE` | Write the EDN file (in `--graph` mode: also keep a copy) |
| `--daily-format FMT` / `--daily-folder DIR` | Daily note naming (moment.js syntax), defaults from `.obsidian/daily-notes.json` |
| `--no-journals` | Keep daily notes as regular pages |
| `--flat` | Don't nest content under headings |
| `--tags-as-pages` | Turn `#tags` into page links instead of Logseq tags |
| `--keep-comments` | Keep `%%comments%%` |
| `--exclude PATH` | Skip a vault-relative file or folder (repeatable) |
| `--chunk-blocks N` | Blocks per import request with `--graph` (default 15000) |
| `--logseq-bin`, `--root-dir`, `--timeout-ms` | Logseq CLI settings |

## What gets converted

| Obsidian | Logseq DB |
| --- | --- |
| Note | Page (title = file name). Duplicate names get the folder appended: `Index (Projects)` |
| Daily note | Journal page |
| Paragraph, list item | Block; lists keep their nesting, content is nested under its heading |
| Heading | Block with the heading property |
| `- [ ]`, `- [x]`, `- [/]`, `- [-]` | `#Task` with status Todo / Done / Doing / Canceled |
| Tasks plugin `📅` `⏳` `⏫🔼🔽🔺` | Deadline, scheduled, priority |
| Numbered list | Blocks with the numbered list type |
| `[[Note]]`, `[[Note\|text]]`, `[text](Note.md)` | Page reference, with label when given |
| `[[Note#Heading]]`, `[[Note#^id]]`, `^id` | Block reference to the heading or block |
| `![[Note]]`, `![[Note#Heading]]` on its own line | Embedded page / block |
| `![[image.png]]`, `![](image.png)`, `[[file.pdf]]` | `#Asset` stored in the graph's `assets/` folder |
| `#tag`, `#nested/tag`, frontmatter `tags` | Logseq tag (class). A note with the same name becomes the tag's page |
| Frontmatter properties | Properties; type inferred per key (text, number, checkbox, url, date, node) |
| Frontmatter `aliases` | Page aliases |
| Callouts `> [!tip] Title` | `#+BEGIN_TIP` admonition (note, tip, important, warning, caution, quote) |
| Block quote | `#Quote` block |
| Code fence | `#Code` block with language |
| `$$math$$` | `#Math` block |
| `==highlight==` | `^^highlight^^` |
| `%%comment%%` | Removed (unless `--keep-comments`) |
| Links to missing notes | Empty page, like Obsidian creates on click |

Note and page timestamps come from the files' creation and modification times.

Notes whose names collide with Logseq built-in pages (e.g. `Task`, `Tags`,
`Contents`) are renamed to `Task (Obsidian)`, since importing them under the
same name would merge them into the built-in page. Attachments that no note
links to are skipped.

## How importing works

`--graph` runs these steps with the `logseq` CLI:

1. `logseq graph create` creates the graph. The new graph's page titles are
   then read so that colliding notes can be renamed.
2. The converted data is sent straight to the graph's db-worker, the same
   thing `logseq graph import` does internally. This bypasses the CLI's EDN
   reader, which takes quadratic time on large files. It also surfaces import
   validation errors, which `logseq graph import` doesn't report.
3. The import is split into several requests:
   * **pages**: all pages, journals, tags, property definitions and assets;
   * **blocks**: the blocks, in chunks of whole pages (`--chunk-blocks`,
     default 15,000), because the db-worker drops requests that run longer
     than about 10 minutes;
   * **links**: blocks that link to a block in a later chunk get their text
     in a final fix-up, since a reference to a not-yet-imported block fails
     validation;
   * **properties**: properties stored as value entities (text, number and
     URL properties, and numbered list markers) are applied in small batches
     to the existing pages and blocks. In a single transaction Logseq's import
     takes time proportional to property values × blocks. For the Obsidian Help
     vault (179 notes, 8k blocks) the split brings the import down from
     4.5 minutes to about 25 s.

   After the first request, each one only validates the entities it touches
   (`:batch-import-edn` with `:validate-scope :tx`, as Logseq's own API
   does). Re-validating the whole graph each time takes about a minute on a
   graph with 280k blocks.
4. Assets are copied into the graph's `assets/` folder.

## Development

```sh
cargo test                                      # unit + fixture tests
O2L_E2E=1 cargo test -- --ignored               # end-to-end import via the logseq CLI
```

`tests/fixtures/sample-vault` exercises most of the conversions above.
