//! MDX pages (from a static-site wiki) to Markdown with directives.
//!
//! Converts the components such wikis use into the directive syntax of [`crate::markdown`]:
//!
//! | MDX | Markdown |
//! |---|---|
//! | `<Callout title="T" kind="tip">…</Callout>` | `:::callout{title="T" kind="tip"}` … `:::` |
//! | `<Steps>` `<Step title="T">…</Step>` `</Steps>` | `::::steps` `:::step{title="T"}` … `:::` `::::` |
//! | `<Details summary="S">…</Details>` | `:::details{summary="S"}` … `:::` |
//! | `<Fit name="N" caption="C" />` | `::fit{name="N" caption="C"}` |
//! | `<Fit eft="[Ship, Name]…" role="R" caption="C" />` | `:::fit{role="R" caption="C"}` + the EFT lines + `:::` |
//!
//! Tags must stand on their own lines (a tag may span several lines, as a long `eft` attribute
//! does). Content inside components is de-indented, so indented MDX children don't turn into
//! Markdown code blocks. `{/* comments */}` lines are dropped. Anything else that only MDX
//! understands (other components, `import` / `export`, inline JSX) is an error: nothing is
//! guessed.

use crate::markdown::encode_attribute;

/// A conversion failure at a 1-based line of the MDX body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertError {
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone)]
enum Attr {
    Str(String),
    Bool,
}

#[derive(Debug)]
struct Component {
    name: String,
    attrs: Vec<(String, Attr)>,
    line: usize,
    children: Vec<Node>,
}

#[derive(Debug)]
enum Node {
    Text(String),
    Component(Component),
}

enum TagScan {
    Complete {
        name: String,
        attrs: Vec<(String, Attr)>,
        self_closing: bool,
        rest: String,
    },
    Incomplete,
}

fn is_component_start(s: &str) -> bool {
    s.strip_prefix('<')
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_uppercase()))
}

fn is_component_end(s: &str) -> bool {
    s.strip_prefix("</")
        .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_uppercase()))
}

/// Scan an opening tag at the start of `s` (`<Name attr="v" …>` or `… />`).
fn scan_tag(s: &str) -> Result<TagScan, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 1;
    let start = i;
    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
        i += 1;
    }
    let name: String = chars[start..i].iter().collect();
    let mut attrs = Vec::new();
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            return Ok(TagScan::Incomplete);
        }
        match chars[i] {
            '>' => {
                return Ok(TagScan::Complete {
                    name,
                    attrs,
                    self_closing: false,
                    rest: chars[i + 1..].iter().collect(),
                });
            }
            '/' => {
                if chars.get(i + 1) == Some(&'>') {
                    return Ok(TagScan::Complete {
                        name,
                        attrs,
                        self_closing: true,
                        rest: chars[i + 2..].iter().collect(),
                    });
                }
                if i + 1 >= chars.len() {
                    return Ok(TagScan::Incomplete);
                }
                return Err(format!("unexpected `/` in <{name}>"));
            }
            c if c.is_ascii_alphabetic() => {
                let key_start = i;
                while i < chars.len()
                    && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_')
                {
                    i += 1;
                }
                let key: String = chars[key_start..i].iter().collect();
                if chars.get(i) != Some(&'=') {
                    attrs.push((key, Attr::Bool));
                    continue;
                }
                i += 1;
                match chars.get(i) {
                    None => return Ok(TagScan::Incomplete),
                    Some(&q) if q == '"' || q == '\'' => {
                        let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == q) else {
                            return Ok(TagScan::Incomplete);
                        };
                        let value: String = chars[i + 1..end].iter().collect();
                        attrs.push((key, Attr::Str(crate::markdown::decode_entities(&value))));
                        i = end + 1;
                    }
                    Some('{') => {
                        let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '}') else {
                            return Ok(TagScan::Incomplete);
                        };
                        let expr: String = chars[i + 1..end].iter().collect();
                        let expr = expr.trim();
                        let value = literal(expr).ok_or_else(|| {
                            format!(
                                "attribute `{key}` of <{name}> is a JavaScript expression \
                                 (`{{{expr}}}`): only string, number and boolean literals \
                                 can be converted"
                            )
                        })?;
                        attrs.push((key, value));
                        i = end + 1;
                    }
                    Some(_) => return Err(format!("attribute `{key}` of <{name}> has no value")),
                }
            }
            c => return Err(format!("unexpected `{c}` in <{name}>")),
        }
    }
}

/// A JSX expression that is a plain literal.
fn literal(expr: &str) -> Option<Attr> {
    for q in ['"', '\'', '`'] {
        if expr.len() >= 2 && expr.starts_with(q) && expr.ends_with(q) {
            let inner = &expr[1..expr.len() - 1];
            if q == '`' && inner.contains("${") {
                return None;
            }
            return Some(Attr::Str(
                inner
                    .replace("\\n", "\n")
                    .replace(&format!("\\{q}"), &q.to_string()),
            ));
        }
    }
    match expr {
        "true" => Some(Attr::Bool),
        _ if expr.parse::<f64>().is_ok() => Some(Attr::Str(expr.to_string())),
        _ => None,
    }
}

fn code_fence(line: &str) -> Option<(char, usize)> {
    let s = line.trim_start();
    let marker = s.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = s.chars().take_while(|c| *c == marker).count();
    (len >= 3).then_some((marker, len))
}

/// Parse an MDX body into text and components.
fn parse(body: &str) -> Result<Vec<Node>, ConvertError> {
    let lines: Vec<&str> = body.lines().collect();
    let mut stack: Vec<Component> = Vec::new();
    let mut root: Vec<Node> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut i = 0;

    fn push(stack: &mut [Component], root: &mut Vec<Node>, node: Node) {
        match stack.last_mut() {
            Some(top) => top.children.push(node),
            None => root.push(node),
        }
    }

    while i < lines.len() {
        let line = lines[i];
        let number = i + 1;
        let err = |message: String| ConvertError {
            line: number,
            message,
        };
        let trimmed = line.trim();
        if let Some((marker, len)) = fence {
            if code_fence(line).is_some_and(|(m, l)| m == marker && l >= len)
                && trimmed.trim_start_matches(marker).is_empty()
            {
                fence = None;
            }
            push(&mut stack, &mut root, Node::Text(line.to_string()));
            i += 1;
            continue;
        }
        if let Some(f) = code_fence(line) {
            fence = Some(f);
            push(&mut stack, &mut root, Node::Text(line.to_string()));
            i += 1;
            continue;
        }
        if trimmed.starts_with("{/*") && trimmed.ends_with("*/}") {
            i += 1;
            continue;
        }
        if stack.is_empty() && (trimmed.starts_with("import ") || trimmed.starts_with("export ")) {
            return Err(err(format!(
                "MDX `{}` statements can't be converted",
                trimmed.split_whitespace().next().unwrap_or_default()
            )));
        }
        if is_component_end(trimmed) {
            let end = trimmed
                .find('>')
                .ok_or_else(|| err("unterminated closing tag".into()))?;
            let name = trimmed[2..end].trim();
            if !trimmed[end + 1..].trim().is_empty() {
                return Err(err(format!(
                    "put </{name}> on its own line (text after it: `{}`)",
                    trimmed[end + 1..].trim()
                )));
            }
            let Some(component) = stack.pop() else {
                return Err(err(format!("</{name}> without an opening tag")));
            };
            if component.name != name {
                return Err(err(format!(
                    "</{name}> closes <{}> (line {})",
                    component.name, component.line
                )));
            }
            push(&mut stack, &mut root, Node::Component(component));
            i += 1;
            continue;
        }
        if is_component_start(trimmed) {
            let mut text = trimmed.to_string();
            let mut last = i;
            let scan = loop {
                match scan_tag(&text).map_err(err)? {
                    TagScan::Complete {
                        name,
                        attrs,
                        self_closing,
                        rest,
                    } => break (name, attrs, self_closing, rest),
                    TagScan::Incomplete => {
                        last += 1;
                        let Some(next) = lines.get(last) else {
                            return Err(err("unterminated component tag".into()));
                        };
                        text.push('\n');
                        text.push_str(next);
                    }
                }
            };
            let (name, attrs, self_closing, rest) = scan;
            if !rest.trim().is_empty() {
                return Err(err(format!(
                    "put <{name}> on its own line (text after it: `{}`)",
                    rest.trim()
                )));
            }
            let component = Component {
                name,
                attrs,
                line: number,
                children: Vec::new(),
            };
            if self_closing {
                push(&mut stack, &mut root, Node::Component(component));
            } else {
                stack.push(component);
            }
            i = last + 1;
            continue;
        }
        if trimmed
            .split('<')
            .skip(1)
            .any(|f| is_component_start_fragment(f.strip_prefix('/').unwrap_or(f)))
        {
            return Err(err(
                "inline components can't be converted: put component tags on their own lines"
                    .into(),
            ));
        }
        push(&mut stack, &mut root, Node::Text(line.to_string()));
        i += 1;
    }
    if let Some(open) = stack.last() {
        return Err(ConvertError {
            line: open.line,
            message: format!("<{}> is never closed", open.name),
        });
    }
    Ok(root)
}

/// `Foo …` after a `<` inside a text line: a JSX component, not HTML.
fn is_component_start_fragment(after_lt: &str) -> bool {
    after_lt.starts_with(|c: char| c.is_ascii_uppercase())
}

/// What a component becomes.
enum Target {
    Container(&'static str),
    Leaf(&'static str),
}

struct Mapped {
    target: Target,
    attrs: Vec<(&'static str, String)>,
    /// Extra body lines (a pasted EFT fit).
    body: Option<String>,
}

fn take(
    c: &Component,
    allowed: &[(&str, &'static str)],
) -> Result<Vec<(&'static str, String)>, ConvertError> {
    let mut out = Vec::new();
    for (key, value) in &c.attrs {
        let Some((_, target)) = allowed.iter().find(|(k, _)| k == key) else {
            return Err(ConvertError {
                line: c.line,
                message: format!(
                    "<{}> has no attribute `{key}` that can be converted",
                    c.name
                ),
            });
        };
        let value = match value {
            Attr::Str(s) => s.trim().to_string(),
            Attr::Bool => String::new(),
        };
        // Editors write empty optional fields as "": leave them out.
        if !value.is_empty() {
            out.push((*target, value));
        }
    }
    Ok(out)
}

fn map(c: &Component) -> Result<Mapped, ConvertError> {
    let err = |message: String| ConvertError {
        line: c.line,
        message,
    };
    let mapped = match c.name.as_str() {
        "Callout" => Mapped {
            target: Target::Container("callout"),
            attrs: take(c, &[("title", "title"), ("kind", "kind"), ("type", "kind")])?,
            body: None,
        },
        "Steps" => Mapped {
            target: Target::Container("steps"),
            attrs: take(c, &[])?,
            body: None,
        },
        "Step" => Mapped {
            target: Target::Container("step"),
            attrs: take(c, &[("title", "title")])?,
            body: None,
        },
        "Details" => Mapped {
            target: Target::Container("details"),
            attrs: take(c, &[("summary", "summary"), ("title", "summary")])?,
            body: None,
        },
        "Fit" => {
            if !c.children.is_empty() {
                return Err(err("<Fit> takes no children: use <Fit … />".into()));
            }
            let mut attrs = take(
                c,
                &[
                    ("name", "name"),
                    ("eft", "eft"),
                    ("role", "role"),
                    ("caption", "caption"),
                ],
            )?;
            let name = attrs.iter().any(|(k, _)| *k == "name");
            let eft = attrs
                .iter()
                .position(|(k, _)| *k == "eft")
                .map(|i| attrs.remove(i).1);
            if name {
                // A doctrine fit wins over pasted EFT (as the static wiki rendered it).
                attrs.retain(|(k, _)| *k == "name" || *k == "caption");
                Mapped {
                    target: Target::Leaf("fit"),
                    attrs,
                    body: None,
                }
            } else if let Some(eft) = eft {
                Mapped {
                    target: Target::Container("fit"),
                    attrs,
                    body: Some(eft),
                }
            } else {
                return Err(err("<Fit> needs a `name` or an `eft` fit".into()));
            }
        }
        other => return Err(err(format!("unknown component <{other}>"))),
    };
    Ok(mapped)
}

/// Nesting depth of containers in `c` (1 for a container without container children).
fn container_depth(c: &Component) -> Result<usize, ConvertError> {
    let mapped = map(c)?;
    if matches!(mapped.target, Target::Leaf(_)) {
        return Ok(0);
    }
    let mut deepest = 0;
    for child in &c.children {
        if let Node::Component(child) = child {
            deepest = deepest.max(container_depth(child)?);
        }
    }
    Ok(deepest + 1)
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn render(nodes: &[Node], dedent: bool, out: &mut Vec<String>) -> Result<(), ConvertError> {
    let common = if dedent {
        nodes
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) if !t.trim().is_empty() => Some(indent(t)),
                _ => None,
            })
            .min()
            .unwrap_or(0)
    } else {
        0
    };
    // Inside a component, leading and trailing blank lines are noise.
    let first = if dedent {
        nodes
            .iter()
            .position(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()))
            .unwrap_or(nodes.len())
    } else {
        0
    };
    let last = if dedent {
        nodes
            .iter()
            .rposition(|n| !matches!(n, Node::Text(t) if t.trim().is_empty()))
            .map_or(0, |p| p + 1)
    } else {
        nodes.len()
    };
    for node in nodes.get(first..last.max(first)).unwrap_or_default() {
        match node {
            Node::Text(t) => {
                let t = if t.trim().is_empty() {
                    ""
                } else {
                    t.get(common..).unwrap_or(t.trim_start())
                };
                out.push(t.trim_end().to_string());
            }
            Node::Component(c) => render_component(c, out)?,
        }
    }
    Ok(())
}

fn attribute_block(attrs: &[(&'static str, String)]) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = attrs
        .iter()
        .map(|(k, v)| {
            let v = v.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("{k}={}", encode_attribute(&v))
        })
        .collect();
    format!("{{{}}}", parts.join(" "))
}

fn render_component(c: &Component, out: &mut Vec<String>) -> Result<(), ConvertError> {
    let mapped = map(c)?;
    match mapped.target {
        Target::Leaf(name) => out.push(format!("::{name}{}", attribute_block(&mapped.attrs))),
        Target::Container(name) => {
            let fence = ":".repeat(2 + container_depth(c)?);
            out.push(format!("{fence}{name}{}", attribute_block(&mapped.attrs)));
            if let Some(body) = &mapped.body {
                out.extend(body.trim().lines().map(|l| l.trim_end().to_string()));
            }
            render(&c.children, true, out)?;
            out.push(fence);
        }
    }
    Ok(())
}

/// Convert an MDX body to Markdown with directives.
pub fn convert(body: &str) -> Result<String, ConvertError> {
    let nodes = parse(body)?;
    let mut out = Vec::new();
    render(&nodes, false, &mut out)?;
    let mut text = out.join("\n");
    while text.ends_with("\n\n") {
        text.pop();
    }
    if !text.ends_with('\n') && !text.is_empty() {
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn converts_components() {
        let mdx = r#"<Callout>
  Anything in \[square brackets] is a placeholder.
</Callout>

## Steps

<Steps>
  <Step title="Join comms">
    Join the channel.

    Then talk.
  </Step>

  <Step title="Fit">
    <Fit name="HQ Nestor" caption="A &quot;fit&quot;" />
  </Step>
</Steps>

<Fit
  role="[Role]"
  caption="[What]"
  eft="[Ship name, Fit name]
Low slot module

Drone x5"
/>
"#;
        let md = convert(mdx).unwrap();
        assert_eq!(
            md,
            r#":::callout
Anything in \[square brackets] is a placeholder.
:::

## Steps

::::steps
:::step{title="Join comms"}
Join the channel.

Then talk.
:::

:::step{title="Fit"}
::fit{name="HQ Nestor" caption="A &quot;fit&quot;"}
:::
::::

:::fit{role="[Role]" caption="[What]"}
[Ship name, Fit name]
Low slot module

Drone x5
:::
"#
        );
        let analysis = crate::markdown::analyze(&md, None);
        assert_eq!(analysis.problems, vec![]);
    }

    #[test]
    fn titled_callout_and_errors() {
        assert_eq!(
            convert("<Callout title=\"Template\">\nx\n</Callout>").unwrap(),
            ":::callout{title=\"Template\"}\nx\n:::\n"
        );
        let e = |s: &str| convert(s).unwrap_err();
        assert!(e("<Tabs>\n</Tabs>").message.contains("unknown component"));
        assert!(e("import X from 'y'").message.contains("import"));
        assert_eq!(e("a\n<Callout>\nx").line, 2);
        assert!(e("<Callout>\n</Steps>").message.contains("closes"));
        assert!(e("text <Fit name=\"x\" /> more").message.contains("inline"));
        assert!(e("<Fit name={fits[0]} />").message.contains("expression"));
        // HTML and code are left alone.
        assert_eq!(
            convert("<br>\n```\n<Nope>\n```").unwrap(),
            "<br>\n```\n<Nope>\n```\n"
        );
    }
}
