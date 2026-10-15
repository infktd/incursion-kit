//! Page bodies: Markdown with directive-style components.
//!
//! The server stores Markdown and never renders it; [`analyze`] validates the components and
//! extracts what the API serves next to the body (headings, links, plain text for search, the
//! read time). The syntax is the block subset of the generic-directives proposal that
//! `remark-directive` implements, so a frontend can render it with that plugin:
//!
//! - a **leaf** directive is a line `::name{attrs}`;
//! - a **container** opens with `:::name{attrs}` (three or more colons) and closes with a line of
//!   at least as many colons. A container inside another must use **fewer** colons than its
//!   parent (`::::steps` around `:::step`), so every fence closes exactly one directive;
//! - attributes: `key="value"`, `key='value'`, `key=value` (no spaces or quotes) or a bare `key`.
//!   Quoted values may use `&quot;`, `&amp;`, `&#39;`, `&lt;` and `&gt;`.
//!
//! Lines inside fenced code blocks (```` ``` ```` or `~~~`) are never directives. Inline
//! (`:name[...]`) directives are not used and not checked. The components are listed in
//! [`DIRECTIVES`]; `docs/wiki.md` documents them for frontend authors.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::Serialize;

/// What a directive is allowed to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    Container,
    Both,
}

/// The known components: name, form, allowed attributes.
pub const DIRECTIVES: &[(&str, &[&str])] = &[
    ("callout", &["kind", "title"]),
    ("steps", &[]),
    ("step", &["title"]),
    ("details", &["summary"]),
    ("fit", &["name", "caption", "role"]),
];

/// Kinds of `:::callout`.
pub const CALLOUT_KINDS: &[&str] = &["note", "tip", "warning", "danger"];

fn form_of(name: &str) -> Option<Form> {
    Some(match name {
        "callout" | "steps" | "step" | "details" => Form::Container,
        "fit" => Form::Both,
        _ => return None,
    })
}

/// One problem with a page body. `line` is 1-based.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = WikiProblem)]
pub struct Problem {
    /// 1-based line of the body.
    pub line: usize,
    /// Machine-readable kind: `unknown_directive`, `wrong_form`, `unknown_attribute`,
    /// `missing_attribute`, `invalid_attribute`, `invalid_directive`, `misplaced_directive`,
    /// `nesting`, `unclosed_directive`, `unexpected_close`, `unknown_fit`, `invalid_eft`.
    pub code: String,
    pub message: String,
}

impl Problem {
    fn new(line: usize, code: &str, message: impl Into<String>) -> Self {
        Self {
            line,
            code: code.to_string(),
            message: message.into(),
        }
    }
}

/// A heading for the "on this page" list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[schema(as = WikiHeading)]
pub struct Heading {
    /// 1 to 6 (`#` to `######`).
    pub depth: u8,
    /// The heading text without Markdown markup.
    pub text: String,
    /// Anchor id, as GitHub (and `rehype-slug`) generate it: lowercase, punctuation removed,
    /// spaces to hyphens, `-1`, `-2`... for repeats.
    pub id: String,
}

/// Everything [`analyze`] finds in a body.
#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub problems: Vec<Problem>,
    pub headings: Vec<Heading>,
    /// Wiki pages the body links to (`/wiki/<slug>` targets), sorted, without duplicates.
    pub links: Vec<String>,
    /// Doctrine fits referenced by `::fit{name=…}`.
    pub fit_names: Vec<String>,
    /// The text without markup, for search and the read time.
    pub plain_text: String,
    pub words: usize,
}

impl Analysis {
    /// Reading time in minutes at 200 words per minute, at least 1.
    pub fn read_minutes(&self) -> u32 {
        read_minutes(self.words)
    }
}

pub fn read_minutes(words: usize) -> u32 {
    ((words as f64 / 200.0).round() as u32).max(1)
}

/// A parsed directive line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectiveLine {
    pub colons: usize,
    pub name: String,
    pub label: Option<String>,
    pub attrs: BTreeMap<String, String>,
}

#[derive(Debug)]
enum LineKind {
    Open(DirectiveLine),
    Leaf(DirectiveLine),
    Close(usize),
    /// A malformed directive line; `colons` ≥ 3 opens a container all the same, so its
    /// closing fence still matches.
    Invalid(usize, String),
    Text,
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn classify(line: &str) -> LineKind {
    if leading_spaces(line) > 3 {
        return LineKind::Text;
    }
    let s = line.trim();
    let colons = s.len() - s.trim_start_matches(':').len();
    if colons >= 3 {
        let rest = &s[colons..];
        if rest.trim().is_empty() {
            return LineKind::Close(colons);
        }
        return match parse_directive(rest) {
            Ok((name, label, attrs)) => LineKind::Open(DirectiveLine {
                colons,
                name,
                label,
                attrs,
            }),
            Err(e) => LineKind::Invalid(colons, e),
        };
    }
    if colons == 2 && s[2..].starts_with(|c: char| c.is_ascii_alphabetic()) {
        return match parse_directive(&s[2..]) {
            Ok((name, label, attrs)) => LineKind::Leaf(DirectiveLine {
                colons,
                name,
                label,
                attrs,
            }),
            Err(e) => LineKind::Invalid(colons, e),
        };
    }
    LineKind::Text
}

type Parsed = (String, Option<String>, BTreeMap<String, String>);

/// `name[label]{attrs}` (after the colons).
fn parse_directive(s: &str) -> Result<Parsed, String> {
    let name_len = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let name = &s[..name_len];
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err("a directive name must start with a letter".into());
    }
    let mut rest = &s[name_len..];
    let mut label = None;
    if let Some(after) = rest.strip_prefix('[') {
        let end = after
            .find(']')
            .ok_or_else(|| "unclosed `[` after the directive name".to_string())?;
        label = Some(after[..end].to_string());
        rest = &after[end + 1..];
    }
    let mut attrs = BTreeMap::new();
    if let Some(after) = rest.strip_prefix('{') {
        let end = attribute_block_end(after)
            .ok_or_else(|| "unclosed `{` in the directive attributes".to_string())?;
        attrs = parse_attributes(&after[..end])?;
        rest = &after[end + 1..];
    }
    if !rest.trim().is_empty() {
        return Err(format!(
            "unexpected text after the directive: `{}` (attributes go in `{{…}}`)",
            rest.trim()
        ));
    }
    Ok((name.to_ascii_lowercase(), label, attrs))
}

/// Index of the `}` that ends an attribute block, skipping quoted values.
fn attribute_block_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '}' => return Some(i),
            None => {}
        }
    }
    None
}

fn parse_attributes(s: &str) -> Result<BTreeMap<String, String>, String> {
    let mut attrs = BTreeMap::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() && chars[i] != '=' {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        let valid_key = key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '#' || c == '.')
            && key[1..]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid_key {
            return Err(format!("invalid attribute name `{key}`"));
        }
        let value = if i < chars.len() && chars[i] == '=' {
            i += 1;
            match chars.get(i) {
                Some(&q) if q == '"' || q == '\'' => {
                    let begin = i + 1;
                    let end = (begin..chars.len())
                        .find(|&j| chars[j] == q)
                        .ok_or_else(|| format!("unclosed quote in attribute `{key}`"))?;
                    i = end + 1;
                    decode_entities(&chars[begin..end].iter().collect::<String>())
                }
                Some(_) => {
                    let begin = i;
                    while i < chars.len() && !chars[i].is_whitespace() {
                        i += 1;
                    }
                    let raw: String = chars[begin..i].iter().collect();
                    if raw.contains(['"', '\'', '=', '<', '>', '`']) {
                        return Err(format!(
                            "unquoted value of `{key}` contains quotes or `=`: quote it"
                        ));
                    }
                    raw
                }
                None => return Err(format!("missing value after `{key}=`")),
            }
        } else {
            String::new()
        };
        let key = if let Some(id) = key.strip_prefix('#') {
            attrs.insert("id".to_string(), id.to_string());
            continue;
        } else if let Some(class) = key.strip_prefix('.') {
            attrs.insert("class".to_string(), class.to_string());
            continue;
        } else {
            key
        };
        if attrs.insert(key.clone(), value).is_some() {
            return Err(format!("attribute `{key}` is given twice"));
        }
    }
    Ok(attrs)
}

pub(crate) fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Quote an attribute value for a directive (the inverse of the attribute parser).
pub fn encode_attribute(value: &str) -> String {
    let escaped = value.replace('&', "&amp;").replace('"', "&quot;");
    format!("\"{escaped}\"")
}

/// An open container while scanning.
struct Open {
    directive: DirectiveLine,
    line: usize,
    /// Non-blank text directly inside (not in a child container).
    text_lines: Vec<(usize, String)>,
    /// Child directive names (containers and leaves).
    children: Vec<String>,
}

#[derive(Default)]
struct Fence {
    marker: char,
    len: usize,
}

fn code_fence(line: &str) -> Option<(char, usize)> {
    if leading_spaces(line) > 3 {
        return None;
    }
    let s = line.trim_start();
    let marker = s.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = s.chars().take_while(|c| *c == marker).count();
    (len >= 3).then_some((marker, len))
}

/// Validate and analyze a page body. `fit_names`: the doctrine fit names `::fit{name=…}` may use,
/// or `None` to accept any name (no fit data loaded).
pub fn analyze(body: &str, fit_names: Option<&HashSet<String>>) -> Analysis {
    let mut out = Analysis::default();
    let mut stack: Vec<Open> = Vec::new();
    let mut fence: Option<Fence> = None;
    let mut text = String::new();
    let mut slugger = Slugger::default();
    let mut links = BTreeSet::new();

    for (index, line) in body.lines().enumerate() {
        let number = index + 1;
        if let Some(open) = &fence {
            if let Some((marker, len)) = code_fence(line)
                && marker == open.marker
                && len >= open.len
                && line
                    .trim_start()
                    .trim_start_matches(marker)
                    .trim()
                    .is_empty()
            {
                fence = None;
            } else {
                push_text(&mut text, line);
                if let Some(top) = stack.last_mut() {
                    top.text_lines.push((number, line.to_string()));
                }
            }
            continue;
        }
        if let Some((marker, len)) = code_fence(line) {
            fence = Some(Fence { marker, len });
            if let Some(top) = stack.last_mut() {
                top.text_lines.push((number, line.to_string()));
            }
            continue;
        }

        match classify(line) {
            LineKind::Text => {
                if let Some((depth, heading)) = atx_heading(line) {
                    let plain = inline_plain(&heading);
                    out.headings.push(Heading {
                        depth,
                        id: slugger.slug(&plain),
                        text: plain,
                    });
                }
                collect_links(line, &mut links);
                push_text(&mut text, &line_plain(line));
                if !line.trim().is_empty()
                    && let Some(top) = stack.last_mut()
                {
                    top.text_lines.push((number, line.to_string()));
                }
            }
            LineKind::Invalid(colons, message) => {
                out.problems
                    .push(Problem::new(number, "invalid_directive", message));
                if colons >= 3 {
                    stack.push(Open {
                        directive: DirectiveLine {
                            colons,
                            name: String::new(),
                            label: None,
                            attrs: BTreeMap::new(),
                        },
                        line: number,
                        text_lines: Vec::new(),
                        children: Vec::new(),
                    });
                }
            }
            LineKind::Leaf(directive) => {
                for key in ["title", "caption", "summary"] {
                    if let Some(v) = directive.attrs.get(key) {
                        push_text(&mut text, v);
                    }
                }
                check_directive(&directive, number, false, stack.last(), fit_names, &mut out);
                if let Some(top) = stack.last_mut() {
                    top.children.push(directive.name.clone());
                }
            }
            LineKind::Open(directive) => {
                for key in ["title", "summary", "caption"] {
                    if let Some(v) = directive.attrs.get(key) {
                        push_text(&mut text, v);
                    }
                }
                if let Some(parent) = stack.last()
                    && directive.colons >= parent.directive.colons
                {
                    out.problems.push(Problem::new(
                        number,
                        "nesting",
                        format!(
                            "`{}` is inside `{}`, which opens with {} colons: give the outer \
                             directive more colons than the inner one (e.g. `{}{}`)",
                            directive.name,
                            parent.directive.name,
                            parent.directive.colons,
                            ":".repeat(directive.colons + 1),
                            parent.directive.name
                        ),
                    ));
                }
                check_directive(&directive, number, true, stack.last(), fit_names, &mut out);
                if let Some(top) = stack.last_mut() {
                    top.children.push(directive.name.clone());
                }
                stack.push(Open {
                    directive,
                    line: number,
                    text_lines: Vec::new(),
                    children: Vec::new(),
                });
            }
            LineKind::Close(colons) => match stack.last() {
                None => out.problems.push(Problem::new(
                    number,
                    "unexpected_close",
                    "closing fence without an open directive",
                )),
                Some(top) if colons < top.directive.colons => out.problems.push(Problem::new(
                    number,
                    "unexpected_close",
                    format!(
                        "`{}` (line {}) opened with {} colons; close it with as many",
                        top.directive.name, top.line, top.directive.colons
                    ),
                )),
                Some(_) => {
                    if let Some(open) = stack.pop() {
                        finish_container(&open, fit_names, &mut out);
                    }
                }
            },
        }
    }
    for open in stack.iter().rev() {
        out.problems.push(Problem::new(
            open.line,
            "unclosed_directive",
            format!(
                "`{}` is never closed (add a line `{}`)",
                open.directive.name,
                ":".repeat(open.directive.colons)
            ),
        ));
    }
    out.problems.sort_by_key(|p| p.line);
    out.words = text.split_whitespace().count();
    out.plain_text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    out.links = links.into_iter().collect();
    out
}

fn push_text(text: &mut String, line: &str) {
    if !line.trim().is_empty() {
        text.push_str(line.trim());
        text.push('\n');
    }
}

fn check_directive(
    d: &DirectiveLine,
    line: usize,
    container: bool,
    parent: Option<&Open>,
    fit_names: Option<&HashSet<String>>,
    out: &mut Analysis,
) {
    let mut problems = Vec::new();
    let mut problem =
        |code: &str, message: String| problems.push(Problem::new(line, code, message));
    let Some(form) = form_of(&d.name) else {
        let known: Vec<&str> = DIRECTIVES.iter().map(|(n, _)| *n).collect();
        problem(
            "unknown_directive",
            format!(
                "unknown directive `{}` (known: {})",
                d.name,
                known.join(", ")
            ),
        );
        out.problems.extend(problems);
        return;
    };
    if form == Form::Container && !container {
        problem(
            "wrong_form",
            format!(
                "`{}` is a container: open it with `:::{}` and close it with `:::`",
                d.name, d.name
            ),
        );
    }
    if d.label.is_some() {
        problem(
            "invalid_attribute",
            format!("`{}` takes no `[label]`: use attributes in `{{…}}`", d.name),
        );
    }
    let allowed = DIRECTIVES
        .iter()
        .find(|(n, _)| *n == d.name)
        .map(|(_, a)| *a)
        .unwrap_or_default();
    for key in d.attrs.keys() {
        if !allowed.contains(&key.as_str()) {
            problem(
                "unknown_attribute",
                format!(
                    "`{}` has no attribute `{key}`{}",
                    d.name,
                    if allowed.is_empty() {
                        String::new()
                    } else {
                        format!(" (allowed: {})", allowed.join(", "))
                    }
                ),
            );
        }
    }
    let attr = |key: &str| d.attrs.get(key).map(|v| v.trim()).filter(|v| !v.is_empty());
    let parent_name = parent.map(|p| p.directive.name.as_str());
    if parent_name == Some("steps") && d.name != "step" {
        problem(
            "misplaced_directive",
            format!(
                "`steps` may only contain `step` directives, not `{}`",
                d.name
            ),
        );
    }
    match d.name.as_str() {
        "callout" => {
            if let Some(kind) = d.attrs.get("kind")
                && !CALLOUT_KINDS.contains(&kind.as_str())
            {
                problem(
                    "invalid_attribute",
                    format!(
                        "callout kind `{kind}` is not one of {}",
                        CALLOUT_KINDS.join(", ")
                    ),
                );
            }
        }
        "step" => {
            if parent_name != Some("steps") {
                problem(
                    "misplaced_directive",
                    "`step` must be inside `steps`".to_string(),
                );
            }
            if attr("title").is_none() {
                problem("missing_attribute", "`step` needs a `title`".to_string());
            }
        }
        "details" => {
            if attr("summary").is_none() {
                problem(
                    "missing_attribute",
                    "`details` needs a `summary`".to_string(),
                );
            }
        }
        "fit" if !container => match attr("name") {
            None => problem(
                "missing_attribute",
                "`::fit` needs the `name` of a doctrine fit (or paste an EFT fit in a \
                 `:::fit` container)"
                    .to_string(),
            ),
            Some(name) => {
                if d.attrs.contains_key("role") {
                    problem(
                        "invalid_attribute",
                        "`role` is only for pasted EFT fits (`:::fit`)".to_string(),
                    );
                }
                if fit_names.is_some_and(|names| !names.contains(name)) {
                    problem("unknown_fit", format!("no doctrine fit is called `{name}`"));
                }
                out.fit_names.push(name.to_string());
            }
        },
        "fit" if d.attrs.contains_key("name") => {
            problem(
                "invalid_attribute",
                "a pasted EFT fit (`:::fit`) has no `name`: use `::fit{name=…}` for a \
                 doctrine fit"
                    .to_string(),
            );
        }
        _ => {}
    }
    out.problems.extend(problems);
}

fn finish_container(open: &Open, _fit_names: Option<&HashSet<String>>, out: &mut Analysis) {
    let d = &open.directive;
    match d.name.as_str() {
        "steps" => {
            if let Some((line, _)) = open.text_lines.first() {
                out.problems.push(Problem::new(
                    *line,
                    "misplaced_directive",
                    "text inside `steps` must be inside a `step`",
                ));
            }
            if !open.children.iter().any(|c| c == "step") {
                out.problems.push(Problem::new(
                    open.line,
                    "missing_attribute",
                    "`steps` needs at least one `step`",
                ));
            }
        }
        "fit" => {
            let first = open.text_lines.iter().find(|(_, l)| !l.trim().is_empty());
            let valid = first.is_some_and(|(_, l)| {
                let l = l.trim();
                l.starts_with('[')
                    && l.ends_with(']')
                    && l[1..l.len() - 1]
                        .split_once(',')
                        .is_some_and(|(hull, name)| {
                            !hull.trim().is_empty() && !name.trim().is_empty()
                        })
            });
            if !valid {
                out.problems.push(Problem::new(
                    first.map(|(l, _)| *l).unwrap_or(open.line),
                    "invalid_eft",
                    "a pasted fit must be in EFT format: its first line is `[Ship, Fit name]`",
                ));
            }
        }
        _ => {}
    }
}

/// `## Text ##` → (2, "Text").
fn atx_heading(line: &str) -> Option<(u8, String)> {
    if leading_spaces(line) > 3 {
        return None;
    }
    let s = line.trim();
    let depth = s.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&depth) {
        return None;
    }
    let rest = &s[depth..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let mut text = rest.trim();
    // Optional closing sequence: ` ##`.
    let without = text.trim_end_matches('#');
    if without.len() != text.len() && (without.is_empty() || without.ends_with([' ', '\t'])) {
        text = without.trim_end();
    }
    Some((depth as u8, text.to_string()))
}

/// GitHub-style anchor ids.
#[derive(Default)]
struct Slugger {
    seen: BTreeMap<String, usize>,
}

impl Slugger {
    fn slug(&mut self, text: &str) -> String {
        let base: String = text
            .to_lowercase()
            .chars()
            .filter_map(|c| {
                if c == ' ' {
                    Some('-')
                } else if c.is_alphanumeric() || c == '-' || c == '_' {
                    Some(c)
                } else {
                    None
                }
            })
            .collect();
        let mut slug = base.clone();
        while let Some(count) = self.seen.get(&slug).copied() {
            let next = count + 1;
            self.seen.insert(slug.clone(), next);
            slug = format!("{base}-{next}");
        }
        self.seen.insert(slug.clone(), 0);
        slug
    }
}

/// Inline Markdown to plain text: links and images keep their text, emphasis, code and HTML
/// markers go, escapes are resolved.
pub(crate) fn inline_plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' if i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() => {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            '!' if chars.get(i + 1) == Some(&'[') => {
                i += 1;
                continue;
            }
            '[' => {
                // A link or image `[text](destination)`: keep the text only.
                let close = (i + 1..chars.len()).find(|&j| chars[j] == ']' && chars[j - 1] != '\\');
                if let Some(close) = close
                    && chars.get(close + 1) == Some(&'(')
                    && let Some(end) = (close + 2..chars.len()).find(|&j| chars[j] == ')')
                {
                    let inner: String = chars[i + 1..close].iter().collect();
                    out.push_str(&inline_plain(&inner));
                    i = end + 1;
                    continue;
                }
            }
            '<' => {
                if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '>')
                    && chars
                        .get(i + 1)
                        .is_some_and(|n| n.is_ascii_alphabetic() || *n == '/')
                {
                    out.push(' ');
                    i = end + 1;
                    continue;
                }
            }
            '*' | '_' | '`' | '|' => {
                // `_` inside words (snake_case) is text.
                let inner_word = c == '_'
                    && i > 0
                    && chars[i - 1].is_alphanumeric()
                    && chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                if !inner_word {
                    if c == '|' {
                        out.push(' ');
                    }
                    i += 1;
                    continue;
                }
            }
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    out.trim().to_string()
}

/// One body line to plain text (block markers removed).
fn line_plain(line: &str) -> String {
    let mut s = line.trim();
    s = s.trim_start_matches('#').trim_start();
    s = s.trim_start_matches('>').trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            s = rest;
        }
    }
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && s[digits..].starts_with(". ") {
        s = &s[digits + 2..];
    }
    if s.chars()
        .all(|c| matches!(c, '-' | '|' | ':' | ' ' | '=' | '*' | '_'))
    {
        return String::new();
    }
    inline_plain(s)
}

/// Prefix of links to wiki pages: a page links to another as `/wiki/<slug>`.
pub const WIKI_LINK_PREFIX: &str = "/wiki/";

/// Wiki link targets in a line: `](/wiki/slug)`, `](/wiki/slug#anchor)`, `]: /wiki/slug`.
fn collect_links(line: &str, links: &mut BTreeSet<String>) {
    for pattern in ["](", "]: "] {
        let mut rest = line;
        while let Some(pos) = rest.find(pattern) {
            let after = rest[pos + pattern.len()..].trim_start_matches('<');
            if let Some(target) = after.strip_prefix(WIKI_LINK_PREFIX) {
                let end = target
                    .find(|c: char| c.is_whitespace() || matches!(c, ')' | '#' | '?' | '"' | '>'))
                    .unwrap_or(target.len());
                let slug = target[..end].trim_end_matches('/');
                if crate::slug::validate_slug(slug).is_ok() {
                    links.insert(slug.to_string());
                }
            }
            rest = &rest[pos + pattern.len()..];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn codes(body: &str) -> Vec<(usize, String)> {
        analyze(body, None)
            .problems
            .into_iter()
            .map(|p| (p.line, p.code))
            .collect()
    }

    #[test]
    fn valid_components() {
        let body = r#"# Title

:::callout{kind=warning title="Read &quot;this&quot;"}
Text with a [link](/wiki/guides/newbro#top).
:::

::::steps
:::step{title="One"}
Do it.
:::

:::step{title='Two'}
::fit{name="HQ Nestor" caption="Fly this"}
:::
::::

:::details{summary="More"}
Hidden.
:::

:::fit{role=DPS caption="Pasted"}
[Vindicator, Test]
Module
:::

```
:::not-a-directive
```
"#;
        let a = analyze(body, None);
        assert_eq!(a.problems, vec![]);
        assert_eq!(a.links, vec!["guides/newbro"]);
        assert_eq!(a.fit_names, vec!["HQ Nestor"]);
        assert_eq!(a.headings.len(), 1);
        assert!(a.plain_text.contains("Read \"this\""));
        assert!(a.plain_text.contains("One"));
    }

    #[test]
    fn problems_are_reported_with_lines() {
        assert_eq!(
            codes(":::nope\nx\n:::"),
            vec![(1, "unknown_directive".into())]
        );
        assert_eq!(codes("::callout"), vec![(1, "wrong_form".into())]);
        assert_eq!(
            codes(":::callout{kind=loud}\n:::"),
            vec![(1, "invalid_attribute".into())]
        );
        assert_eq!(
            codes(":::callout{size=1}\n:::"),
            vec![(1, "unknown_attribute".into())]
        );
        assert_eq!(
            codes(":::callout\ntext"),
            vec![(1, "unclosed_directive".into())]
        );
        assert_eq!(codes("text\n:::"), vec![(2, "unexpected_close".into())]);
        assert_eq!(
            codes(":::steps\n:::step{title=a}\nx\n:::\n:::"),
            vec![(2, "nesting".into())]
        );
        assert_eq!(
            codes("::::steps\nloose text\n:::step{title=a}\nx\n:::\n::::"),
            vec![(2, "misplaced_directive".into())]
        );
        assert_eq!(
            codes(":::step{title=a}\n:::"),
            vec![(1, "misplaced_directive".into())]
        );
        assert_eq!(
            codes("::::steps\n:::step\nx\n:::\n::::"),
            vec![(2, "missing_attribute".into())]
        );
        assert_eq!(codes("::fit"), vec![(1, "missing_attribute".into())]);
        assert_eq!(
            codes(":::fit\nnot eft\n:::"),
            vec![(2, "invalid_eft".into())]
        );
        assert_eq!(
            codes(":::callout{title=\"x}\n:::"),
            vec![(1, "invalid_directive".into())]
        );
        assert_eq!(
            codes(":::callout{a=1} junk\n:::"),
            vec![(1, "invalid_directive".into())]
        );
    }

    #[test]
    fn fit_names_are_checked_when_known() {
        let names: HashSet<String> = ["HQ Nestor".to_string()].into();
        let ok = analyze("::fit{name=\"HQ Nestor\"}", Some(&names));
        assert!(ok.problems.is_empty());
        let bad = analyze("::fit{name=\"HQ Nestr\"}", Some(&names));
        assert_eq!(bad.problems[0].code, "unknown_fit");
    }

    #[test]
    fn headings_get_github_ids() {
        let a = analyze(
            "## Your first fleet in four steps\n## FAQ & more ##\n## FAQ & more\n####### no\n#no",
            None,
        );
        let ids: Vec<_> = a.headings.iter().map(|h| h.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["your-first-fleet-in-four-steps", "faq--more", "faq--more-1"]
        );
        assert_eq!(a.headings[1].text, "FAQ & more");
    }

    #[test]
    fn plain_text_and_read_time() {
        let a = analyze("**Bold** [link](/x) and `code` \\[x]", None);
        assert_eq!(a.plain_text, "Bold link and code [x]");
        assert_eq!(a.read_minutes(), 1);
        let long = "word ".repeat(1000);
        assert_eq!(analyze(&long, None).read_minutes(), 5);
    }

    #[test]
    fn attribute_round_trip() {
        let value = "a \"quoted\" & <b>";
        let line = format!(":::callout{{title={}}}\n:::", encode_attribute(value));
        let LineKind::Open(d) = classify(line.lines().next().unwrap()) else {
            panic!()
        };
        assert_eq!(d.attrs["title"], value);
    }
}
