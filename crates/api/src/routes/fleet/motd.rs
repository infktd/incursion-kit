//! Role assignments in the fleet MOTD.
//!
//! FCs assign fleet roles in the MOTD with lines like `DDD: <a href="showinfo:1377//123">Pilot
//! One</a>, <a href="showinfo:1377//456">Pilot Two</a>`. The role names come from `rules.yaml`
//! (`fleet.motd_roles`).
//!
//! - [`parse_roles`] reads the assignments: pilot links after each `ROLE:` label, or, when the
//!   MOTD has no such links at all, plain-text names.
//! - [`add_to_role`] puts a pilot under a role, editing the MOTD in place (keeping its markup).
//! - [`remove_absent`] drops the links of pilots who are not in the fleet from every role.
//!
//! A role's *section* runs from its label to the next label of another role (or the end of the
//! MOTD). Labels are matched case-insensitively and only at the start of a word (so `PS:` does
//! not match inside `DPS:`).

use std::collections::{BTreeMap, HashSet};

/// Where a role's section is in the MOTD (byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Section {
    /// Just after the `ROLE:` label.
    start: usize,
    /// The next other role's label, or the end of the MOTD.
    end: usize,
}

/// A `<a ...>...</a>` link inside a section (byte offsets into the MOTD).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Anchor {
    /// The `<` of `<a`.
    start: usize,
    /// Just after the opening tag's `>`.
    open_end: usize,
    /// The `<` of `</a>`, when the link is closed within the section.
    close: Option<usize>,
    /// The link text (outside nested tags), trimmed.
    name: String,
}

impl Anchor {
    /// Just after `</a>`.
    fn end(&self) -> Option<usize> {
        self.close.map(|close| close + "</a>".len())
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The first `role:` label at or after `from` that starts a word: `(label start, after colon)`.
fn find_label(motd: &str, role: &str, from: usize) -> Option<(usize, usize)> {
    let lower = motd.to_ascii_lowercase(); // same byte offsets as `motd`
    let pattern = format!("{}:", role.to_ascii_lowercase());
    lower
        .get(from..)?
        .match_indices(&pattern)
        .map(|(pos, _)| from + pos)
        .find(|&pos| pos == 0 || !is_word_byte(lower.as_bytes()[pos - 1]))
        .map(|pos| (pos, pos + pattern.len()))
}

/// `role`'s section: from its first label to the next label of another role.
fn section(motd: &str, role: &str, roles: &[&str]) -> Option<Section> {
    let (_, start) = find_label(motd, role, 0)?;
    let end = roles
        .iter()
        .filter(|other| !other.eq_ignore_ascii_case(role))
        .filter_map(|other| find_label(motd, other, start).map(|(pos, _)| pos))
        .min()
        .unwrap_or(motd.len());
    Some(Section { start, end })
}

/// The links in `motd[start..end]`. Text inside tags nested in a link is not part of its name.
fn anchors(motd: &str, start: usize, end: usize) -> Vec<Anchor> {
    let section = &motd[start..end];
    let mut found = Vec::new();
    let mut current: Option<Anchor> = None;
    let mut name = String::new();
    let mut depth = 0usize; // 1 = directly inside the link

    let mut chars = section.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch != '<' {
            if current.is_some() && depth == 1 {
                name.push(ch);
            }
            continue;
        }
        let closing = chars.next_if(|&(_, c)| c == '/').is_some();
        let mut tag = String::new();
        while let Some((_, c)) = chars.next_if(|&(_, c)| c != ' ' && c != '>') {
            tag.push(c);
        }
        let mut tag_end = section.len();
        for (j, c) in chars.by_ref() {
            if c == '>' {
                tag_end = j + 1;
                break;
            }
        }

        if tag.eq_ignore_ascii_case("a") {
            if closing {
                if let Some(mut anchor) = current.take() {
                    anchor.close = Some(start + i);
                    anchor.name = name.trim().to_string();
                    found.push(anchor);
                }
                name.clear();
                depth = 0;
            } else {
                // An unclosed link is replaced by the new one.
                name.clear();
                depth = 1;
                current = Some(Anchor {
                    start: start + i,
                    open_end: start + tag_end,
                    close: None,
                    name: String::new(),
                });
            }
        } else if current.is_some() {
            if closing {
                depth = depth.saturating_sub(1).max(1);
            } else {
                depth += 1;
            }
        }
    }
    if let Some(mut anchor) = current {
        anchor.name = name.trim().to_string();
        found.push(anchor);
    }
    found
}

/// Remove every tag (and stray `>`).
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Names in plain text: split on the first delimiter present (`,` `;` `|` `/` `\`), else on
/// whitespace.
fn parse_names(text: &str) -> Vec<String> {
    let cleaned = strip_tags(text);
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return Vec::new();
    }
    let split = |parts: &mut dyn Iterator<Item = &str>| -> Vec<String> {
        parts
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    };
    if let Some(delimiter) = [',', ';', '|', '/', '\\']
        .into_iter()
        .find(|&d| cleaned.contains(d))
    {
        return split(&mut cleaned.split(delimiter));
    }
    split(&mut cleaned.split_whitespace())
}

fn starts_with_label(line: &str, role: &str) -> bool {
    line.len() > role.len()
        && line.as_bytes()[role.len()] == b':'
        && line
            .get(..role.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(role))
}

/// The pilots assigned to each role, keyed by the role's configured name. Roles without anyone
/// are left out.
pub fn parse_roles(motd: &str, roles: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut assignments = BTreeMap::new();
    for role in roles {
        let Some(section) = section(motd, role, roles) else {
            continue;
        };
        let names: Vec<String> = anchors(motd, section.start, section.end)
            .into_iter()
            .map(|anchor| anchor.name)
            .filter(|name| !name.is_empty())
            .collect();
        if !names.is_empty() {
            assignments.insert(role.to_string(), names);
        }
    }
    if assignments.is_empty() {
        assignments = parse_plain_text_roles(motd, roles);
    }
    assignments
}

/// `text` up to the first line break or role label.
fn until_label<'a>(text: &'a str, roles: &[&str]) -> &'a str {
    let end = text
        .find(['\n', '\r'])
        .into_iter()
        .chain(
            roles
                .iter()
                .filter_map(|role| find_label(text, role, 0).map(|(pos, _)| pos)),
        )
        .min()
        .unwrap_or(text.len());
    &text[..end]
}

/// The fallback for MOTDs without pilot links: names after `ROLE:` up to the end of the line (or
/// the next label), plus up to four following lines when they don't start with a label.
fn parse_plain_text_roles(motd: &str, roles: &[&str]) -> BTreeMap<String, Vec<String>> {
    let clean = strip_tags(motd);
    let lines: Vec<&str> = clean.split('\n').collect();
    let mut assignments = BTreeMap::new();
    for role in roles {
        let mut found: Vec<String> = Vec::new();
        let mut add = |names: Vec<String>| {
            for name in names {
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        };

        if let Some((_, start)) = find_label(&clean, role, 0) {
            add(parse_names(until_label(&clean[start..], roles)));
        }

        for (i, line) in lines.iter().enumerate() {
            let line = line.trim();
            if !starts_with_label(line, role) {
                continue;
            }
            add(parse_names(until_label(&line[role.len() + 1..], roles)));
            for next in lines.iter().take(i + 5).skip(i + 1) {
                let next = next.trim();
                if next.is_empty() {
                    continue;
                }
                if roles.iter().any(|other| starts_with_label(next, other)) {
                    break;
                }
                add(parse_names(next));
            }
        }

        if !found.is_empty() {
            assignments.insert(role.to_string(), found);
        }
    }
    assignments
}

/// The `showinfo:<type>//` type of the first link in the section, or `default`.
fn link_type(motd: &str, anchors: &[Anchor], default: i64) -> String {
    anchors
        .first()
        .and_then(|anchor| href(motd, anchor))
        .and_then(|(start, end)| {
            let href = &motd[start..end];
            let rest = &href[href.find("showinfo:")? + "showinfo:".len()..];
            Some(rest[..rest.find("//")?].to_string())
        })
        .unwrap_or_else(|| default.to_string())
}

/// The byte range of the link's `href="..."` value.
fn href(motd: &str, anchor: &Anchor) -> Option<(usize, usize)> {
    let tag = &motd[anchor.start..anchor.open_end];
    let start = anchor.start + tag.find("href=\"")? + "href=\"".len();
    let end = start + motd[start..anchor.open_end].find('"')?;
    Some((start, end))
}

/// The byte range of the ID after `//` in the link's href.
fn href_id(motd: &str, anchor: &Anchor) -> Option<(usize, usize)> {
    let (start, end) = href(motd, anchor)?;
    let id_start = start + motd[start..end].find("//")? + 2;
    Some((id_start, end))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn link(link_type: &str, character_id: i64, name: &str) -> String {
    format!(
        "<a href=\"showinfo:{link_type}//{character_id}\">{}</a>",
        escape(name)
    )
}

fn splice(motd: &str, edits: &mut [(usize, usize, String)]) -> String {
    edits.sort_by_key(|(start, _, _)| *start);
    let mut out = String::with_capacity(motd.len() + 64);
    let mut at = 0;
    for (start, end, text) in edits.iter() {
        out.push_str(&motd[at..*start]);
        out.push_str(text);
        at = *end;
    }
    out.push_str(&motd[at..]);
    out
}

/// Point an existing link at `character_id` and rename it to `name`: the ID after `//` in the
/// href is replaced, and so is either the whole link text (`replace_text`) or the old name
/// inside it.
fn retarget(
    motd: &str,
    anchor: &Anchor,
    character_id: i64,
    name: &str,
    replace_text: bool,
) -> Option<String> {
    let (id_start, id_end) = href_id(motd, anchor)?;
    let close = anchor.close?;
    let mut edits = vec![(id_start, id_end, character_id.to_string())];
    if replace_text {
        edits.push((anchor.open_end, close, escape(name)));
    } else if let Some(pos) = motd[anchor.open_end..close]
        .to_ascii_lowercase()
        .find(&anchor.name.to_ascii_lowercase())
    {
        let start = anchor.open_end + pos;
        edits.push((start, start + anchor.name.len(), escape(name)));
    }
    Some(splice(motd, &mut edits))
}

/// Put `name` (character `character_id`) under `role`:
///
/// - already listed: the link is updated (ID and spelling);
/// - otherwise, with pilots of the role in fleet: the link is added after the last of them;
/// - no links in the section: the rest of the label's line is replaced by the link;
/// - a single link to someone not in fleet: that link is replaced;
/// - otherwise the link is added after the section's last link;
/// - no such role in the MOTD: a `ROLE: <link>` line is appended.
///
/// New links copy the `showinfo:<type>//` type of the section's first link, or use
/// `default_link_type`. `in_fleet` holds the lowercased names of the fleet's members.
pub fn add_to_role(
    motd: &str,
    role: &str,
    name: &str,
    character_id: i64,
    roles: &[&str],
    in_fleet: &HashSet<String>,
    default_link_type: i64,
) -> String {
    let Some(section) = section(motd, role, roles) else {
        return append_role(motd, role, name, character_id, default_link_type);
    };
    let anchors = anchors(motd, section.start, section.end);
    let kind = link_type(motd, &anchors, default_link_type);
    let new_link = link(&kind, character_id, name);
    let name_lower = name.to_lowercase();
    let insert_at = |pos: usize, delimiter: &str| {
        splice(motd, &mut [(pos, pos, format!("{delimiter}{new_link}"))])
    };

    if anchors.is_empty() {
        // Replace whatever follows the label on its line.
        let content = &motd[section.start..section.end];
        let lower = content.to_ascii_lowercase();
        let line_end = [lower.find("<br"), lower.find('\n')]
            .into_iter()
            .flatten()
            .min();
        let (end, suffix) = match line_end {
            Some(pos) => (section.start + pos, ""),
            None => (section.end, "<br>"),
        };
        return splice(
            motd,
            &mut [(section.start, end, format!(" {new_link}{suffix}"))],
        );
    }

    if let Some(existing) = anchors.iter().find(|a| a.name.to_lowercase() == name_lower)
        && let Some(updated) = retarget(motd, existing, character_id, name, false)
    {
        return updated;
    }
    let listed = anchors.iter().any(|a| a.name.to_lowercase() == name_lower);

    if !listed
        && let Some(last_in_fleet) = anchors
            .iter()
            .filter(|a| in_fleet.contains(&a.name.to_lowercase()))
            .filter_map(Anchor::end)
            .max()
    {
        let after = motd[last_in_fleet..section.end].trim_start();
        let delimiter = if after.starts_with(',') { ", " } else { " " };
        return insert_at(last_in_fleet, delimiter);
    }

    if !listed
        && let [only] = anchors.as_slice()
        && let Some(updated) = retarget(motd, only, character_id, name, true)
    {
        return updated;
    }

    match anchors.iter().filter_map(Anchor::end).max() {
        Some(last) => {
            let after = motd[last..section.end].trim_start();
            let delimiter = if after.is_empty() || after.starts_with('<') {
                ""
            } else {
                ", "
            };
            insert_at(last, delimiter)
        }
        None => append_role(motd, role, name, character_id, default_link_type),
    }
}

fn append_role(
    motd: &str,
    role: &str,
    name: &str,
    character_id: i64,
    default_link_type: i64,
) -> String {
    let mut out = motd.to_string();
    if !out.trim().is_empty() {
        out.push('\n');
    }
    out.push_str(&format!(
        "{}: {}\n",
        role.to_uppercase(),
        link(&default_link_type.to_string(), character_id, name)
    ));
    out
}

/// Remove, from every role's section, the links of pilots not in `in_fleet` (lowercased names),
/// with their delimiter (see [`removal_range`]).
pub fn remove_absent(motd: &str, roles: &[&str], in_fleet: &HashSet<String>) -> String {
    let mut motd = motd.to_string();
    for role in roles.iter().rev() {
        while let Some(section) = section(&motd, role, roles) {
            let absent = anchors(&motd, section.start, section.end)
                .into_iter()
                .find(|a| {
                    a.close.is_some()
                        && !a.name.is_empty()
                        && !in_fleet.contains(&a.name.to_lowercase())
                });
            let Some(anchor) = absent else {
                break;
            };
            let end = anchor.end().unwrap_or(anchor.open_end);
            motd.replace_range(removal_range(&motd, section.start, &anchor, end), "");
        }
    }
    motd
}

/// What to cut to remove a link: the link with the whitespace and comma after it; or, when no
/// comma follows but a previous link precedes it, the delimiter before it instead (so removing
/// the last of `A, B` leaves `A`, not `A, `).
fn removal_range(
    motd: &str,
    section_start: usize,
    anchor: &Anchor,
    end: usize,
) -> std::ops::Range<usize> {
    let after = &motd[end..];
    let trimmed = after.trim_start();
    if let Some(rest) = trimmed.strip_prefix(',') {
        let space = usize::from(rest.starts_with(' '));
        return anchor.start..end + (after.len() - trimmed.len()) + 1 + space;
    }
    let before = motd[section_start..anchor.start].trim_end();
    let before = before.strip_suffix(',').unwrap_or(before).trim_end();
    if before.ends_with("</a>") {
        return section_start + before.len()..end;
    }
    anchor.start..end + (after.len() - trimmed.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const ROLES: [&str; 5] = ["DDD", "LR", "PS", "MS", "MTAC"];

    fn a(id: i64, name: &str) -> String {
        format!("<a href=\"showinfo:1377//{id}\">{name}</a>")
    }

    fn fleet(names: &[&str]) -> HashSet<String> {
        names.iter().map(|n| n.to_lowercase()).collect()
    }

    fn roles(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
        pairs
            .iter()
            .map(|(role, names)| {
                (
                    role.to_string(),
                    names.iter().map(|n| n.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn parses_links_after_labels() {
        let motd = format!(
            "<font size=\"12\">Welcome</font><br>DDD: {}, {}<br>LR: {}<br>MTAC: none yet<br>\
             DPS: {}",
            a(1, "Pilot One"),
            a(2, "Pilot Two"),
            a(3, "Kronos Guy"),
            a(4, "Dps Guy"),
        );
        assert_eq!(
            parse_roles(&motd, &ROLES),
            roles(&[
                ("DDD", &["Pilot One", "Pilot Two"]),
                ("LR", &["Kronos Guy"]),
                // Sections run up to the next role label; "DPS:" is not one.
                ("MTAC", &["Dps Guy"]),
            ])
        );
    }

    #[test]
    fn labels_match_case_insensitively_but_only_at_word_starts() {
        let motd = format!("dps: {}<br>ps: {}", a(1, "Not Ps"), a(2, "Real Ps"));
        assert_eq!(parse_roles(&motd, &ROLES), roles(&[("PS", &["Real Ps"])]));
    }

    #[test]
    fn link_text_in_nested_tags_is_ignored() {
        let motd = "DDD: <a href=\"showinfo:1377//1\"><b>Bold</b></a> <a href=\"x\">Plain</a>";
        assert_eq!(parse_roles(motd, &ROLES), roles(&[("DDD", &["Plain"])]));
    }

    #[test]
    fn falls_back_to_plain_text() {
        let motd = "Fleet rules<br>\nDDD: Alice, Bob\nLR:\nCarol\n\nMS: Dave / Eve\nMTAC: Frank";
        assert_eq!(
            parse_roles(motd, &ROLES),
            roles(&[
                ("DDD", &["Alice", "Bob"]),
                ("LR", &["Carol"]),
                ("MS", &["Dave", "Eve"]),
                ("MTAC", &["Frank"]),
            ])
        );
        assert_eq!(
            parse_roles("PS: One Two", &ROLES),
            roles(&[("PS", &["One", "Two"])])
        );
        assert!(parse_roles("nothing here", &ROLES).is_empty());
        assert!(parse_roles("", &ROLES).is_empty());
    }

    #[test]
    fn plain_text_takes_names_on_the_same_line_up_to_the_next_label() {
        let motd = "DDD: Alice LR: Bob";
        assert_eq!(
            parse_roles(motd, &ROLES),
            roles(&[("DDD", &["Alice"]), ("LR", &["Bob"])])
        );
    }

    #[test]
    fn adding_after_pilots_in_fleet() {
        let motd = format!(
            "DDD: {}, {}<br>LR: {}",
            a(1, "One"),
            a(2, "Two"),
            a(3, "Three")
        );
        let out = add_to_role(
            &motd,
            "DDD",
            "New Guy",
            9,
            &ROLES,
            &fleet(&["One", "Two", "Three", "New Guy"]),
            1377,
        );
        assert_eq!(
            out,
            format!(
                "DDD: {}, {} {}<br>LR: {}",
                a(1, "One"),
                a(2, "Two"),
                a(9, "New Guy"),
                a(3, "Three")
            )
        );
    }

    #[test]
    fn adding_after_space_separated_pilots_uses_a_space() {
        let motd = format!("DDD: {} {}<br>", a(1, "One"), a(2, "Two"));
        let out = add_to_role(&motd, "ddd", "New", 9, &ROLES, &fleet(&["One"]), 1377);
        assert_eq!(
            out,
            format!("DDD: {} {} {}<br>", a(1, "One"), a(9, "New"), a(2, "Two"))
        );
    }

    #[test]
    fn existing_pilot_is_updated_in_place() {
        let motd = format!("DDD: {}<br>", a(1, "pilot one"));
        let out = add_to_role(
            &motd,
            "DDD",
            "Pilot One",
            42,
            &ROLES,
            &fleet(&["Pilot One"]),
            1377,
        );
        assert_eq!(out, format!("DDD: {}<br>", a(42, "Pilot One")));
    }

    #[test]
    fn single_absent_pilot_is_replaced() {
        let motd = "LR: <a href=\"showinfo:1375//1\">Gone</a><br>bye";
        let out = add_to_role(motd, "LR", "Here", 7, &ROLES, &fleet(&["Here"]), 1377);
        assert_eq!(out, "LR: <a href=\"showinfo:1375//7\">Here</a><br>bye");
    }

    #[test]
    fn several_absent_pilots_get_the_new_one_appended() {
        let motd = format!("MS: {}, {}<br>", a(1, "Gone"), a(2, "Also Gone"));
        let out = add_to_role(&motd, "MS", "New", 9, &ROLES, &fleet(&["New"]), 1377);
        assert_eq!(
            out,
            format!(
                "MS: {}, {}{}<br>",
                a(1, "Gone"),
                a(2, "Also Gone"),
                a(9, "New")
            )
        );
        assert_eq!(
            remove_absent(&out, &ROLES, &fleet(&["New"])),
            format!("MS: {}<br>", a(9, "New"))
        );
    }

    #[test]
    fn label_without_links_gets_its_line_replaced() {
        let out = add_to_role(
            "PS: TBD<br>Welcome!",
            "PS",
            "New",
            9,
            &ROLES,
            &fleet(&[]),
            1377,
        );
        assert_eq!(out, format!("PS: {}<br>Welcome!", a(9, "New")));
        let out = add_to_role("PS:", "PS", "New", 9, &ROLES, &fleet(&[]), 1377);
        assert_eq!(out, format!("PS: {}<br>", a(9, "New")));
        let out = add_to_role("PS: x LR: y", "PS", "New", 9, &ROLES, &fleet(&[]), 1377);
        assert_eq!(out, format!("PS: {}<br>LR: y", a(9, "New")));
    }

    #[test]
    fn missing_role_is_appended() {
        let out = add_to_role("Hello", "mtac", "New", 9, &ROLES, &fleet(&[]), 1377);
        assert_eq!(out, format!("Hello\nMTAC: {}\n", a(9, "New")));
        let out = add_to_role("", "LR", "New", 9, &ROLES, &fleet(&[]), 1377);
        assert_eq!(out, format!("LR: {}\n", a(9, "New")));
    }

    #[test]
    fn names_are_escaped() {
        let out = add_to_role("", "LR", "<b>x</b>", 9, &ROLES, &fleet(&[]), 1377);
        assert_eq!(
            out,
            "LR: <a href=\"showinfo:1377//9\">&lt;b&gt;x&lt;/b&gt;</a>\n"
        );
    }

    #[test]
    fn removing_absent_pilots_keeps_the_rest() {
        let motd = format!(
            "Intro {}<br>DDD: {}, {} {}<br>LR: {}<br>",
            a(5, "Not A Role"),
            a(1, "One"),
            a(2, "Two"),
            a(3, "Three"),
            a(4, "Four")
        );
        let out = remove_absent(&motd, &ROLES, &fleet(&["Two"]));
        assert_eq!(
            out,
            format!(
                "Intro {}<br>DDD: {}<br>LR: <br>",
                a(5, "Not A Role"),
                a(2, "Two")
            )
        );
    }

    #[test]
    fn non_ascii_text_is_handled() {
        let motd = format!("Ünïcödé — DDD: {}, {}<br>", a(1, "Åsa"), a(2, "Bø"));
        assert_eq!(
            parse_roles(&motd, &ROLES),
            roles(&[("DDD", &["Åsa", "Bø"])])
        );
        let out = remove_absent(&motd, &ROLES, &fleet(&["åsa"]));
        assert_eq!(out, format!("Ünïcödé — DDD: {}<br>", a(1, "Åsa")));
        let out = add_to_role(&motd, "DDD", "Çé", 3, &ROLES, &fleet(&["Åsa"]), 1377);
        assert_eq!(
            out,
            format!(
                "Ünïcödé — DDD: {}, {}, {}<br>",
                a(1, "Åsa"),
                a(3, "Çé"),
                a(2, "Bø")
            )
        );
    }
}
