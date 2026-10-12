//! Splits Obsidian markdown into an outline of blocks.
//!
//! Obsidian notes are documents, Logseq pages are outlines. Each paragraph,
//! list item, code fence, quote, callout and math block becomes one block.
//! List nesting is preserved and, unless disabled, content is nested under
//! the closest preceding heading.

#[derive(Debug, Clone, PartialEq)]
pub enum BlockKind {
    Text,
    Heading(u8),
    Code { lang: Option<String> },
    Quote,
    Math,
    Callout { kind: String, title: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MdBlock {
    pub kind: BlockKind,
    pub text: String,
    /// Obsidian block id (`^id`) without the caret.
    pub block_id: Option<String>,
    /// Checkbox character of a task list item, e.g. ' ' or 'x'.
    pub task: Option<char>,
    pub ordered: bool,
    pub children: Vec<MdBlock>,
    /// Assigned by the converter for blocks that can be linked to.
    pub uuid: Option<uuid::Uuid>,
}

impl MdBlock {
    fn new(kind: BlockKind, text: String) -> Self {
        MdBlock {
            kind,
            text,
            block_id: None,
            task: None,
            ordered: false,
            children: Vec::new(),
            uuid: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct ParsedNote {
    pub frontmatter: Option<String>,
    pub blocks: Vec<MdBlock>,
}

#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub nest_under_headings: bool,
    pub keep_comments: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            nest_under_headings: true,
            keep_comments: false,
        }
    }
}

pub fn parse_note(source: &str, opts: ParseOptions) -> ParsedNote {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let source = source.replace("\r\n", "\n");
    let (frontmatter, body) = split_frontmatter(&source);
    let body = if opts.keep_comments {
        body.to_string()
    } else {
        strip_comments(body)
    };
    let lines: Vec<&str> = body.lines().collect();
    let mut parser = Parser::new(&lines, opts);
    parser.run();
    ParsedNote {
        frontmatter: frontmatter.map(str::to_string),
        blocks: parser.finish(),
    }
}

/// Returns (yaml, body). Frontmatter must start on the first line.
pub fn split_frontmatter(source: &str) -> (Option<&str>, &str) {
    let mut lines = source.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (None, source);
    };
    if first.trim_end() != "---" {
        return (None, source);
    }
    let start = first.len();
    let mut offset = start;
    for line in lines {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            let yaml = &source[start..offset];
            let body = &source[offset + line.len()..];
            return (Some(yaml), body);
        }
        offset += line.len();
    }
    (None, source)
}

/// Removes Obsidian `%%comments%%` (which may span lines) outside code fences.
pub fn strip_comments(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut in_comment = false;
    let mut fence: Option<(char, usize)> = None;
    for line in body.split_inclusive('\n') {
        if !in_comment {
            let (_, rest) = measure_indent(line);
            if let Some((c, n)) = fence {
                if is_fence_close(rest, c, n) {
                    fence = None;
                }
                out.push_str(line);
                continue;
            }
            if let Some((c, n, _)) = fence_open(rest) {
                fence = Some((c, n));
                out.push_str(line);
                continue;
            }
        }
        let mut kept = String::new();
        let mut i = 0;
        while i < line.len() {
            let rest = &line[i..];
            if rest.starts_with("%%") {
                in_comment = !in_comment;
                i += 2;
            } else if rest.starts_with('`') && !in_comment {
                let len = code_span_len(rest);
                kept.push_str(&rest[..len]);
                i += len;
            } else {
                let c = rest.chars().next().expect("non-empty");
                if !in_comment {
                    kept.push(c);
                }
                i += c.len_utf8();
            }
        }
        if kept.trim().is_empty() && !line.trim().is_empty() {
            // The line consisted only of comment text; drop it entirely so it
            // doesn't split surrounding paragraphs.
            continue;
        }
        if in_comment && !kept.ends_with('\n') {
            kept.push('\n');
        }
        out.push_str(&kept);
    }
    out
}

struct Node {
    block: MdBlock,
    children: Vec<usize>,
}

struct ListEntry {
    marker_indent: usize,
    content_indent: usize,
    node: usize,
}

struct Parser<'a> {
    lines: &'a [&'a str],
    pos: usize,
    opts: ParseOptions,
    nodes: Vec<Node>,
    roots: Vec<usize>,
    headings: Vec<(u8, usize)>,
    list: Vec<ListEntry>,
    /// Node receiving continuation lines of the current paragraph.
    open_text: Option<usize>,
    last_node: Option<usize>,
    prev_blank: bool,
}

impl<'a> Parser<'a> {
    fn new(lines: &'a [&'a str], opts: ParseOptions) -> Self {
        Parser {
            lines,
            pos: 0,
            opts,
            nodes: Vec::new(),
            roots: Vec::new(),
            headings: Vec::new(),
            list: Vec::new(),
            open_text: None,
            last_node: None,
            prev_blank: false,
        }
    }

    fn add(&mut self, parent: Option<usize>, block: MdBlock) -> usize {
        let id = self.nodes.len();
        self.nodes.push(Node {
            block,
            children: Vec::new(),
        });
        match parent {
            Some(p) => self.nodes[p].children.push(id),
            None => self.roots.push(id),
        }
        self.last_node = Some(id);
        id
    }

    fn heading_container(&self) -> Option<usize> {
        self.headings.last().map(|(_, n)| *n)
    }

    fn run(&mut self) {
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            if line.trim().is_empty() {
                self.prev_blank = true;
                self.open_text = None;
                self.pos += 1;
                continue;
            }
            let (indent, rest) = measure_indent(line);
            if !self.list.is_empty() && self.handle_list_context(line, indent, rest) {
                self.prev_blank = false;
                continue;
            }
            self.handle_top_level(line, indent, rest);
            self.prev_blank = false;
        }
    }

    /// Returns true if the line was consumed as part of the open list.
    fn handle_list_context(&mut self, line: &str, indent: usize, rest: &str) -> bool {
        if parse_list_marker(rest).is_some() && !is_hr(rest) {
            return false; // handled as a list item by handle_top_level
        }
        let outer = self.list[0].marker_indent;
        if indent <= outer {
            let lazy =
                !self.prev_blank && self.open_text.is_some() && !starts_block_construct(rest);
            if lazy {
                self.append_text(self.open_text.unwrap(), rest.trim_end());
                self.pos += 1;
                return true;
            }
            self.list.clear();
            self.open_text = None;
            return false;
        }
        while self.list.len() > 1 && self.list.last().unwrap().marker_indent >= indent {
            self.list.pop();
        }
        let owner = self.list.last().unwrap();
        let (owner_node, content_indent) = (owner.node, owner.content_indent);
        let content = dedent(line, content_indent.min(indent));
        let content_rest = content.trim_start();
        if let Some((c, n, info)) = fence_open(content_rest) {
            self.parse_fence(Some(owner_node), indent, c, n, info);
            return true;
        }
        if content_rest.starts_with("$$") {
            self.parse_math(Some(owner_node));
            return true;
        }
        if content_rest.starts_with('>') {
            self.parse_quote(Some(owner_node), Some(indent));
            return true;
        }
        if let Some(id) = standalone_block_id(content_rest) {
            if let Some(n) = self.last_node {
                self.nodes[n].block.block_id = Some(id);
            }
            self.pos += 1;
            return true;
        }
        let text = content_rest.trim_end();
        match self.open_text {
            Some(target) if !self.prev_blank => self.append_text(target, text),
            _ => {
                let id = self.add(
                    Some(owner_node),
                    MdBlock::new(BlockKind::Text, text.to_string()),
                );
                self.open_text = Some(id);
            }
        }
        self.pos += 1;
        true
    }

    fn handle_top_level(&mut self, line: &str, indent: usize, rest: &str) {
        let _ = line;
        if let Some((c, n, info)) = fence_open(rest) {
            self.list.clear();
            self.parse_fence(self.heading_container(), indent, c, n, info);
            return;
        }
        if rest.starts_with("$$") {
            self.list.clear();
            self.parse_math(self.heading_container());
            return;
        }
        if indent < 4
            && let Some((level, text)) = parse_heading(rest)
        {
            self.list.clear();
            self.open_text = None;
            let parent = if self.opts.nest_under_headings {
                while self.headings.last().is_some_and(|(l, _)| *l >= level) {
                    self.headings.pop();
                }
                self.heading_container()
            } else {
                None
            };
            let id = self.add(parent, MdBlock::new(BlockKind::Heading(level), text));
            if self.opts.nest_under_headings {
                self.headings.push((level, id));
            }
            self.pos += 1;
            return;
        }
        if is_hr(rest) {
            self.list.clear();
            self.open_text = None;
            self.add(
                self.heading_container(),
                MdBlock::new(BlockKind::Text, "---".into()),
            );
            self.pos += 1;
            return;
        }
        if rest.starts_with('>') {
            self.list.clear();
            self.parse_quote(self.heading_container(), None);
            return;
        }
        if let Some(marker) = parse_list_marker(rest) {
            self.start_list_item(indent, marker);
            return;
        }
        if let Some(id) = standalone_block_id(rest) {
            if let Some(n) = self.last_node {
                self.nodes[n].block.block_id = Some(id);
            }
            self.pos += 1;
            return;
        }
        match self.open_text {
            Some(target) if !self.prev_blank => self.append_text(target, rest.trim_end()),
            _ => {
                let id = self.add(
                    self.heading_container(),
                    MdBlock::new(BlockKind::Text, rest.trim_end().to_string()),
                );
                self.open_text = Some(id);
            }
        }
        self.pos += 1;
    }

    fn start_list_item(&mut self, indent: usize, marker: ListMarker) {
        while self.list.last().is_some_and(|e| e.marker_indent >= indent) {
            self.list.pop();
        }
        let parent = match self.list.last() {
            Some(e) => Some(e.node),
            None => self.heading_container(),
        };
        let mut text = marker.text.trim_end();
        let mut task = None;
        if let Some((c, after)) = parse_task_box(text) {
            task = Some(c);
            text = after;
        }
        let mut block = MdBlock::new(BlockKind::Text, text.to_string());
        block.task = task;
        block.ordered = marker.ordered;
        let id = self.add(parent, block);
        self.list.push(ListEntry {
            marker_indent: indent,
            content_indent: indent + marker.width,
            node: id,
        });
        self.open_text = Some(id);
        self.pos += 1;
    }

    fn append_text(&mut self, node: usize, text: &str) {
        let block = &mut self.nodes[node].block;
        if !block.text.is_empty() {
            block.text.push('\n');
        }
        block.text.push_str(text);
    }

    fn parse_fence(&mut self, parent: Option<usize>, indent: usize, c: char, n: usize, info: &str) {
        let lang = info
            .split_whitespace()
            .next()
            .map(|s| s.trim_matches(|c| c == '{' || c == '}').to_string())
            .filter(|s| !s.is_empty());
        self.pos += 1;
        let mut content = Vec::new();
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            let (_, rest) = measure_indent(line);
            self.pos += 1;
            if is_fence_close(rest, c, n) {
                break;
            }
            content.push(dedent(line, indent));
        }
        let mut block = MdBlock::new(BlockKind::Code { lang }, content.join("\n"));
        block.text = block.text.trim_end_matches('\n').to_string();
        self.add(parent, block);
        self.open_text = None;
    }

    fn parse_math(&mut self, parent: Option<usize>) {
        let first = self.lines[self.pos].trim();
        self.pos += 1;
        let after = &first[2..];
        if let Some(end) = after.find("$$") {
            let block = MdBlock::new(BlockKind::Math, after[..end].trim().to_string());
            self.add(parent, block);
            self.open_text = None;
            return;
        }
        let mut content = Vec::new();
        if !after.trim().is_empty() {
            content.push(after.trim().to_string());
        }
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos].trim();
            self.pos += 1;
            if let Some(stripped) = line.strip_suffix("$$") {
                if !stripped.trim().is_empty() {
                    content.push(stripped.trim().to_string());
                }
                break;
            }
            content.push(line.to_string());
        }
        self.add(parent, MdBlock::new(BlockKind::Math, content.join("\n")));
        self.open_text = None;
    }

    /// Collects consecutive `>` lines. `list_indent` is set when the quote is
    /// nested in a list item and stops at lines indented less than that.
    fn parse_quote(&mut self, parent: Option<usize>, list_indent: Option<usize>) {
        let mut content = Vec::new();
        while self.pos < self.lines.len() {
            let line = self.lines[self.pos];
            let (indent, rest) = measure_indent(line);
            if !rest.starts_with('>') || list_indent.is_some_and(|li| indent < li) {
                break;
            }
            let stripped = &rest[1..];
            let stripped = stripped
                .strip_prefix(' ')
                .or_else(|| stripped.strip_prefix('\t'))
                .unwrap_or(stripped);
            content.push(stripped.trim_end().to_string());
            self.pos += 1;
        }
        let block = match parse_callout_header(content.first().map(String::as_str).unwrap_or("")) {
            Some((kind, title)) => {
                MdBlock::new(BlockKind::Callout { kind, title }, content[1..].join("\n"))
            }
            None => MdBlock::new(BlockKind::Quote, content.join("\n")),
        };
        self.add(parent, block);
        self.open_text = None;
    }

    fn finish(mut self) -> Vec<MdBlock> {
        let roots = std::mem::take(&mut self.roots);
        roots.into_iter().map(|r| self.build(r)).collect()
    }

    fn build(&mut self, id: usize) -> MdBlock {
        let children = std::mem::take(&mut self.nodes[id].children);
        let mut block = std::mem::replace(
            &mut self.nodes[id].block,
            MdBlock::new(BlockKind::Text, String::new()),
        );
        block.children = children.into_iter().map(|c| self.build(c)).collect();
        if block.block_id.is_none() && !matches!(block.kind, BlockKind::Code { .. }) {
            let (text, id) = extract_trailing_block_id(&block.text);
            if id.is_some() {
                block.text = text;
                block.block_id = id;
            }
        }
        block
    }
}

struct ListMarker<'a> {
    ordered: bool,
    width: usize,
    text: &'a str,
}

fn parse_list_marker(rest: &str) -> Option<ListMarker<'_>> {
    let bytes = rest.as_bytes();
    let first = *bytes.first()?;
    let (ordered, marker_len) = if matches!(first, b'-' | b'*' | b'+') {
        (false, 1)
    } else if first.is_ascii_digit() {
        let digits = bytes
            .iter()
            .take(9)
            .take_while(|b| b.is_ascii_digit())
            .count();
        match bytes.get(digits) {
            Some(b'.') | Some(b')') => (true, digits + 1),
            _ => return None,
        }
    } else {
        return None;
    };
    match bytes.get(marker_len) {
        None => Some(ListMarker {
            ordered,
            width: marker_len + 1,
            text: "",
        }),
        Some(b' ') | Some(b'\t') => {
            let text = &rest[marker_len + 1..];
            let extra = text.len() - text.trim_start_matches(' ').len();
            // CommonMark: content starts after 1-4 spaces following the marker.
            let width = marker_len + 1 + if extra < 4 { extra } else { 0 };
            Some(ListMarker {
                ordered,
                width,
                text: text.trim_start(),
            })
        }
        _ => None,
    }
}

fn parse_task_box(text: &str) -> Option<(char, &str)> {
    let rest = text.strip_prefix('[')?;
    let mut chars = rest.chars();
    let c = chars.next()?;
    let after = chars.as_str().strip_prefix(']')?;
    if after.is_empty() {
        return Some((c, ""));
    }
    let after = after
        .strip_prefix(' ')
        .or_else(|| after.strip_prefix('\t'))?;
    Some((c, after))
}

fn parse_heading(rest: &str) -> Option<(u8, String)> {
    let level = rest.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with(' ') && !after.starts_with('\t') {
        return None;
    }
    let mut text = after.trim();
    // Optional closing sequence of #s.
    let trimmed = text.trim_end_matches('#');
    if trimmed.len() != text.len() && (trimmed.is_empty() || trimmed.ends_with(' ')) {
        text = trimmed.trim_end();
    }
    Some((level as u8, text.to_string()))
}

fn parse_callout_header(first: &str) -> Option<(String, String)> {
    let rest = first.trim_start().strip_prefix("[!")?;
    let end = rest.find(']')?;
    let kind = rest[..end].trim().to_lowercase();
    if kind.is_empty() {
        return None;
    }
    let after = &rest[end + 1..];
    let after = after
        .strip_prefix('+')
        .or_else(|| after.strip_prefix('-'))
        .unwrap_or(after);
    Some((kind, after.trim().to_string()))
}

fn is_hr(rest: &str) -> bool {
    let t = rest.trim_end();
    let Some(c) = t.chars().next() else {
        return false;
    };
    if !matches!(c, '-' | '*' | '_') {
        return false;
    }
    let count = t.chars().filter(|&x| x == c).count();
    count >= 3 && t.chars().all(|x| x == c || x == ' ' || x == '\t')
}

fn starts_block_construct(rest: &str) -> bool {
    fence_open(rest).is_some()
        || rest.starts_with('>')
        || rest.starts_with("$$")
        || parse_heading(rest).is_some()
        || is_hr(rest)
}

fn fence_open(rest: &str) -> Option<(char, usize, &str)> {
    let c = rest.chars().next()?;
    if c != '`' && c != '~' {
        return None;
    }
    let n = rest.chars().take_while(|&x| x == c).count();
    if n < 3 {
        return None;
    }
    let info = &rest[n..];
    if c == '`' && info.contains('`') {
        return None;
    }
    Some((c, n, info.trim()))
}

fn is_fence_close(rest: &str, c: char, n: usize) -> bool {
    let t = rest.trim_end();
    t.chars().take_while(|&x| x == c).count() >= n && t.chars().all(|x| x == c)
}

fn standalone_block_id(rest: &str) -> Option<String> {
    let id = rest.trim_end().strip_prefix('^')?;
    (is_block_id(id)).then(|| id.to_string())
}

fn is_block_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Length of the inline code span at the start of `rest` (which begins with
/// a backtick), or of the backtick run alone if it is never closed.
pub fn code_span_len(rest: &str) -> usize {
    let b = rest.as_bytes();
    let ticks = b.iter().take_while(|&&x| x == b'`').count();
    let mut j = ticks;
    while j < b.len() {
        if b[j] == b'`' {
            let run = b[j..].iter().take_while(|&&x| x == b'`').count();
            if run == ticks {
                return j + run;
            }
            j += run;
        } else {
            j += 1;
        }
    }
    ticks
}

/// Strips a trailing ` ^block-id` from the last line of `text`.
pub fn extract_trailing_block_id(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim_end();
    if let Some(pos) = trimmed.rfind('^') {
        let id = &trimmed[pos + 1..];
        let before = &trimmed[..pos];
        if is_block_id(id) && (before.is_empty() || before.ends_with([' ', '\t', '\n'])) {
            return (before.trim_end().to_string(), Some(id.to_string()));
        }
    }
    (text.to_string(), None)
}

/// Returns (indent columns with tabs as 4, rest of line after whitespace).
fn measure_indent(line: &str) -> (usize, &str) {
    let mut cols = 0;
    for (i, c) in line.char_indices() {
        match c {
            ' ' => cols += 1,
            '\t' => cols += 4 - cols % 4,
            _ => return (cols, &line[i..]),
        }
    }
    (cols, "")
}

/// Removes up to `n` columns of leading whitespace.
fn dedent(line: &str, n: usize) -> String {
    let mut cols = 0;
    for (i, c) in line.char_indices() {
        if cols >= n {
            return line[i..].to_string();
        }
        match c {
            ' ' => cols += 1,
            '\t' => cols += 4 - cols % 4,
            _ => return line[i..].to_string(),
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Vec<MdBlock> {
        parse_note(s, ParseOptions::default()).blocks
    }

    fn titles(blocks: &[MdBlock]) -> Vec<String> {
        blocks.iter().map(|b| b.text.clone()).collect()
    }

    #[test]
    fn frontmatter_is_split() {
        let note = parse_note("---\ntags: [a]\n---\nHello\n", ParseOptions::default());
        assert_eq!(note.frontmatter.as_deref(), Some("tags: [a]\n"));
        assert_eq!(titles(&note.blocks), vec!["Hello"]);
    }

    #[test]
    fn paragraphs_and_headings_nest() {
        let blocks = parse("intro\n\n# H1\npara one\nline two\n\n## H2\nchild\n# Other\nx");
        assert_eq!(titles(&blocks), vec!["intro", "H1", "Other"]);
        assert_eq!(blocks[1].kind, BlockKind::Heading(1));
        assert_eq!(
            titles(&blocks[1].children),
            vec!["para one\nline two", "H2"]
        );
        assert_eq!(titles(&blocks[1].children[1].children), vec!["child"]);
        assert_eq!(titles(&blocks[2].children), vec!["x"]);
    }

    #[test]
    fn flat_headings() {
        let note = parse_note(
            "# A\ntext\n## B\nmore",
            ParseOptions {
                nest_under_headings: false,
                keep_comments: false,
            },
        );
        assert_eq!(titles(&note.blocks), vec!["A", "text", "B", "more"]);
    }

    #[test]
    fn nested_lists_and_tasks() {
        let blocks =
            parse("- a\n\t- b\n\t\t- [ ] task\n\t- [x] done\n- c\n  continued\n1. one\n2. two");
        assert_eq!(titles(&blocks), vec!["a", "c\ncontinued", "one", "two"]);
        assert_eq!(titles(&blocks[0].children), vec!["b", "done"]);
        assert_eq!(blocks[0].children[0].children[0].task, Some(' '));
        assert_eq!(blocks[0].children[0].children[0].text, "task");
        assert_eq!(blocks[0].children[1].task, Some('x'));
        assert!(blocks[2].ordered);
    }

    #[test]
    fn code_fences_inside_lists() {
        let blocks = parse("- item\n  ```js\n  let x = 1;\n  ```\n- next");
        assert_eq!(titles(&blocks), vec!["item", "next"]);
        assert_eq!(
            blocks[0].children[0].kind,
            BlockKind::Code {
                lang: Some("js".into())
            }
        );
        assert_eq!(blocks[0].children[0].text, "let x = 1;");
    }

    #[test]
    fn code_fence_keeps_hashes() {
        let blocks = parse("```\n# not a heading\n- not a list\n```\nafter");
        assert_eq!(blocks[0].text, "# not a heading\n- not a list");
        assert_eq!(blocks[1].text, "after");
    }

    #[test]
    fn callouts_and_quotes() {
        let blocks = parse("> [!tip]- Title here\n> body line\n> more\n\n> plain quote\n> two");
        assert_eq!(
            blocks[0].kind,
            BlockKind::Callout {
                kind: "tip".into(),
                title: "Title here".into()
            }
        );
        assert_eq!(blocks[0].text, "body line\nmore");
        assert_eq!(blocks[1].kind, BlockKind::Quote);
        assert_eq!(blocks[1].text, "plain quote\ntwo");
    }

    #[test]
    fn block_ids() {
        let blocks = parse("para text ^abc-1\n\n| a | b |\n|---|---|\n\n^tbl\n\n- item ^li");
        assert_eq!(blocks[0].text, "para text");
        assert_eq!(blocks[0].block_id.as_deref(), Some("abc-1"));
        assert_eq!(blocks[1].block_id.as_deref(), Some("tbl"));
        assert_eq!(blocks[2].block_id.as_deref(), Some("li"));
        assert_eq!(blocks[2].text, "item");
    }

    #[test]
    fn comments_are_removed() {
        let blocks = parse(
            "a %%hidden%% b\n\n%%\nmulti\nline\n%%\nc\n```\n%%kept%%\n```\ncode `%% [[` stays\n\nlast",
        );
        assert_eq!(
            titles(&blocks),
            vec!["a  b", "c", "%%kept%%", "code `%% [[` stays", "last"]
        );
    }

    #[test]
    fn math_blocks() {
        let blocks = parse("$$\na^2\n$$\n$$b$$");
        assert_eq!(blocks[0].kind, BlockKind::Math);
        assert_eq!(blocks[0].text, "a^2");
        assert_eq!(blocks[1].text, "b");
    }

    #[test]
    fn list_ends_after_blank_and_dedent() {
        let blocks = parse("- a\n\nparagraph\n- b\n\n    indented para in b");
        assert_eq!(titles(&blocks), vec!["a", "paragraph", "b"]);
        assert_eq!(titles(&blocks[2].children), vec!["indented para in b"]);
    }

    #[test]
    fn hr_is_not_a_list() {
        let blocks = parse("a\n\n---\n\n* * *\nb");
        assert_eq!(titles(&blocks), vec!["a", "---", "---", "b"]);
    }
}
