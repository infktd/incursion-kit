//! Doctrine fits in `fits.dat`: listing, EFT parsing and rendering, and in-place edits of the
//! fitting links.
//!
//! `fits.dat` is an in-game fitting link dump: `<a href="fitting:DNA">NAME</a>` links with
//! headers and formatting between them. Only the links matter to the loader. The text between
//! links (tags stripped) names the section the following fits are in (`LOGI`, `MARAUDERS`, …).
//! Edits replace, insert or remove single links and leave everything else untouched.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::{Deserialize, Serialize};
use wl_eve_data::{Category, EveData, Slot, TypeId};

use super::{Edited, Problems, Rejection, TypeRef, check_name};
use crate::doctrine::FIT_LINK;
use crate::{DataSet, FitError, Fitting};

/// EFT sections before the cargo hold: everything from the eighth section on is cargo (see
/// [`Fitting::from_eft`]).
const EFT_CARGO_SECTION: usize = 7;
/// SDE category of strategic cruiser subsystems.
const SUBSYSTEM_CATEGORY: i32 = 32;
const MAX_NAME: usize = 100;

/// A fitting link in `fits.dat`.
#[derive(Debug, Clone)]
struct Link {
    /// Byte range of `<a …>…</a>`.
    range: Range<usize>,
    name: String,
    dna: String,
    section: Option<String>,
}

impl Link {
    fn hull(&self) -> Option<TypeId> {
        self.dna.split(':').next()?.parse().ok()
    }
}

fn links(content: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let mut section: Option<String> = None;
    let mut previous_end = 0;
    for captures in FIT_LINK.captures_iter(content) {
        let whole = captures.get(0).expect("match");
        let between = section_text(&content[previous_end..whole.start()]);
        if !between.is_empty() {
            section = Some(between);
        }
        links.push(Link {
            range: whole.range(),
            dna: captures[1].to_string(),
            name: captures[2].to_string(),
            section: section.clone(),
        });
        previous_end = whole.end();
    }
    links
}

/// The visible text of a piece of `fits.dat` markup: tags removed, whitespace collapsed.
fn section_text(markup: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in markup.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn link_html(name: &str, dna: &str) -> String {
    format!(r#"<a href="fitting:{dna}">{name}</a>"#)
}

/// Which doctrine fit a request addresses: by name, and by hull when several fits share it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct FitSelector {
    pub name: String,
    pub hull: Option<TypeId>,
}

fn find(links: &[Link], selector: &FitSelector) -> Result<usize, Rejection> {
    let matches: Vec<usize> = links
        .iter()
        .enumerate()
        .filter(|(_, link)| {
            link.name == selector.name && selector.hull.is_none_or(|hull| link.hull() == Some(hull))
        })
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [] => Err(Rejection::NotFound("Fit not found".to_string())),
        [index] => Ok(*index),
        _ => Err(Rejection::invalid(
            "hull",
            format!(
                "several fits are named {:?}: pass the hull type id to choose one",
                selector.name
            ),
        )),
    }
}

/// One fitted item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FitItem {
    #[schema(value_type = i32)]
    pub id: TypeId,
    pub name: String,
    pub count: i64,
}

/// A fit's items by slot, for display and editing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct FitLayout {
    pub high: Vec<FitItem>,
    pub med: Vec<FitItem>,
    pub low: Vec<FitItem>,
    pub rig: Vec<FitItem>,
    pub subsystem: Vec<FitItem>,
    /// Fitted items that go in no slot.
    pub other: Vec<FitItem>,
    pub drones: Vec<FitItem>,
    pub cargo: Vec<FitItem>,
}

impl FitLayout {
    pub fn of(fit: &Fitting, eve: &EveData) -> Self {
        let mut layout = Self::default();
        let item = |id: TypeId, count: i64| FitItem {
            id,
            name: eve.name_of(id).unwrap_or_default().to_string(),
            count,
        };
        for (&id, &count) in &fit.modules {
            let ty = eve.get(id);
            let list = match ty.and_then(|ty| ty.slot()) {
                Some(Slot::High) => &mut layout.high,
                Some(Slot::Med) => &mut layout.med,
                Some(Slot::Low) => &mut layout.low,
                Some(Slot::Rig) => &mut layout.rig,
                Some(Slot::Drone) => &mut layout.drones,
                None if ty.is_some_and(|ty| ty.category == Category::Other(SUBSYSTEM_CATEGORY)) => {
                    &mut layout.subsystem
                }
                None => &mut layout.other,
            };
            list.push(item(id, count));
        }
        for (&id, &count) in &fit.cargo {
            layout.cargo.push(item(id, count));
        }
        for list in [
            &mut layout.high,
            &mut layout.med,
            &mut layout.low,
            &mut layout.rig,
            &mut layout.subsystem,
            &mut layout.other,
            &mut layout.drones,
            &mut layout.cargo,
        ] {
            list.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        }
        layout
    }
}

/// Renders a fit as EFT text that [`Fitting::from_eft`] (and [`parse_eft`]) read back as the
/// same fit: low, med, high, rig and subsystem sections with one line per fitted module, drones
/// as `Name xN`, then the cargo after an empty section.
pub fn render_eft(fit: &Fitting, name: &str, eve: &EveData) -> String {
    let layout = FitLayout::of(fit, eve);
    let hull = eve.name_of(fit.hull).unwrap_or_default();
    let mut out = format!("[{hull}, {name}]\n");
    let fitted = |items: &[FitItem]| -> String {
        items
            .iter()
            .flat_map(|item| std::iter::repeat_n(format!("{}\n", item.name), item.count as usize))
            .collect()
    };
    let stacked = |items: &[FitItem]| -> String {
        items
            .iter()
            .map(|item| format!("{} x{}\n", item.name, item.count))
            .collect()
    };
    let mut subsystems = layout.subsystem.clone();
    subsystems.extend(layout.other.iter().cloned());
    let sections = [
        fitted(&layout.low),
        fitted(&layout.med),
        fitted(&layout.high),
        fitted(&layout.rig),
        fitted(&subsystems),
        stacked(&layout.drones),
        String::new(),
        stacked(&layout.cargo),
    ];
    let last = sections
        .iter()
        .rposition(|section| !section.is_empty())
        .unwrap_or(0);
    for (index, section) in sections.iter().enumerate().take(last + 1) {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(section);
    }
    out
}

/// The outcome of [`parse_eft`].
#[derive(Debug, Clone, Default)]
pub struct EftParse {
    /// The fit name of the header line.
    pub name: Option<String>,
    pub hull: Option<TypeId>,
    /// The fit, when there are no errors.
    pub fit: Option<Fitting>,
    pub problems: Problems,
}

/// Parses one fit in EFT format for the doctrine editor, reporting every problem with its line.
///
/// Reads what [`Fitting::from_eft`] reads, the same way (sections, stacked items, cargo), and
/// also accepts what in-game exports contain: blank lines before the header, `/OFFLINE`
/// markers and loaded charges (`Module, Charge`; the charge is ignored with a warning: doctrine
/// fits carry charges in the cargo). Errors are reported with `path` (the request field).
pub fn parse_eft(text: &str, eve: &EveData, path: &str) -> EftParse {
    let mut parse = EftParse::default();
    let problems = &mut parse.problems;
    let mut header_seen = false;
    let mut section = 0;
    let mut modules: BTreeMap<TypeId, i64> = BTreeMap::new();
    let mut cargo: BTreeMap<TypeId, i64> = BTreeMap::new();

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') && !line.starts_with("[Empty ") {
            if header_seen {
                problems.line_error(path, number, "only one fit can be given at a time");
                break;
            }
            header_seen = true;
            let inner = &line[1..line.len() - 1];
            let Some((hull_name, fit_name)) = inner.split_once(',') else {
                problems.line_error(path, number, "expected a `[Hull, Fit name]` header");
                continue;
            };
            let (hull_name, fit_name) = (hull_name.trim(), fit_name.trim());
            if !fit_name.is_empty() {
                parse.name = Some(fit_name.to_string());
            }
            match eve.by_name(hull_name) {
                None => problems.line_error(path, number, format!("unknown hull {hull_name:?}")),
                Some(ty) if ty.category != Category::Ship => {
                    problems.line_error(path, number, format!("{hull_name:?} is not a ship"));
                }
                Some(ty) => parse.hull = Some(ty.id),
            }
            continue;
        }
        if !header_seen {
            if !line.is_empty() {
                problems.line_error(
                    path,
                    number,
                    "expected the `[Hull, Fit name]` header line first",
                );
            }
            continue;
        }
        if line.starts_with("[Empty ") {
            continue;
        }
        if line.is_empty() {
            section += 1;
            continue;
        }

        let line = line.strip_suffix("/OFFLINE").map_or(line, str::trim_end);
        let (item_text, count, stacked) = match line.rsplit_once(" x") {
            Some((name, count)) if count.trim().parse::<i64>().is_ok() => {
                (name.trim(), count.trim().parse::<i64>().unwrap_or(1), true)
            }
            _ => (line, 1, false),
        };
        let (item_name, charge) = match item_text.split_once(", ") {
            Some((module, charge)) if eve.id_of(item_text).is_none() => {
                (module.trim(), Some(charge.trim()))
            }
            _ => (item_text, None),
        };
        let Some(ty) = eve.by_name(item_name) else {
            problems.line_error(path, number, format!("unknown type {item_name:?}"));
            continue;
        };
        if count <= 0 {
            problems.line_error(path, number, "the count must be positive");
            continue;
        }
        if let Some(charge) = charge {
            if eve.id_of(charge).is_none() {
                problems.line_error(path, number, format!("unknown charge {charge:?}"));
                continue;
            }
            problems.warn(format!(
                "line {number}: the loaded charge {charge:?} is ignored; put charges in the cargo"
            ));
        }
        if !ty.published {
            problems.warn(format!(
                "line {number}: {item_name:?} is not published in game"
            ));
        }
        let is_cargo = section >= EFT_CARGO_SECTION
            || ty.is_always_cargo()
            || (stacked && ty.category != Category::Drone);
        if !is_cargo && ty.slot().is_none() && ty.category != Category::Other(SUBSYSTEM_CATEGORY) {
            problems.warn(format!(
                "line {number}: {item_name:?} does not go in a slot but is listed as fitted"
            ));
        }
        let target = if is_cargo { &mut cargo } else { &mut modules };
        let entry = target.entry(ty.id).or_insert(0);
        match entry.checked_add(count) {
            Some(total) => *entry = total,
            None => problems.line_error(path, number, "the count is too large"),
        }
    }

    if !header_seen {
        problems.error(path, "no fit found: start with a `[Hull, Fit name]` line");
    }
    if let (Some(hull), false) = (parse.hull, problems.has_errors()) {
        let fit = Fitting {
            hull,
            modules,
            cargo,
        };
        match check_fit(&fit, eve) {
            Ok(()) => parse.fit = Some(fit),
            Err(message) => problems.error(path, message),
        }
    }
    parse
}

/// What the `fits.dat` loader checks.
fn check_fit(fit: &Fitting, eve: &EveData) -> Result<(), String> {
    let describe = |error: FitError| match error {
        FitError::InvalidHull => "the hull is not a ship".to_string(),
        FitError::InvalidModule => {
            "the fit has an unknown item, or a charge or implant fitted to a slot".to_string()
        }
        FitError::InvalidCount => "every count must be positive".to_string(),
        FitError::Parse => "the fit does not parse".to_string(),
    };
    fit.validate(eve).map_err(describe)?;
    fit.to_dna(eve).map(|_| ()).map_err(describe)
}

/// A doctrine fit as the editor shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DoctrineFitView {
    /// Fit name, the link text in `fits.dat`.
    pub name: String,
    pub hull: TypeRef,
    /// Ship DNA, normalised.
    pub dna: String,
    /// The fit as EFT text (`[Hull, Name]`, low/med/high/rig/subsystem sections, drones,
    /// cargo).
    pub eft: String,
    pub layout: FitLayout,
    /// The `fits.dat` header the fit is listed under (`LOGI`, …), if any.
    pub section: Option<String>,
    /// Whether the public fittings list leaves the fit out (its name contains the hidden-fit
    /// marker of `rules.yaml`, `ALTERNATIVE` by default).
    pub hidden: bool,
    /// The waitlist category id the fit is put in.
    pub category: String,
    /// Index of the fit in `fits.dat` (file order).
    pub position: usize,
}

/// Every doctrine fit of `content` (`fits.dat`), in file order.
pub fn doctrine_fits(content: &str, data: &DataSet) -> Vec<DoctrineFitView> {
    let eve = data.eve();
    links(content)
        .into_iter()
        .enumerate()
        .filter_map(|(position, link)| {
            let fit = Fitting::from_dna(&link.dna, eve).ok()?;
            Some(DoctrineFitView {
                hull: TypeRef::of(eve, fit.hull),
                dna: fit.to_dna(eve).ok()?,
                eft: render_eft(&fit, &link.name, eve),
                layout: FitLayout::of(&fit, eve),
                section: link.section,
                hidden: link.name.contains(&data.rules.fittings.hidden_fit_marker),
                category: data.categorize(&fit).to_string(),
                position,
                name: link.name,
            })
        })
        .collect()
}

/// A doctrine fit to create, or the new state of one to update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, utoipa::ToSchema)]
pub struct DoctrineFitInput {
    /// Fit name (unique; skill plans and fit notes refer to it). At most 100 characters, no `<`.
    pub name: String,
    /// The fit as EFT text. The header's fit name is ignored (`name` applies).
    #[serde(default)]
    pub eft: Option<String>,
    /// The fit as ship DNA. When both are given they must describe the same fit.
    #[serde(default)]
    pub dna: Option<String>,
    /// The `fits.dat` header to list the fit under: a new fit goes after the last fit of that
    /// section (a new section is appended at the end of the file); on update a different
    /// section moves the fit. Without it, a new fit goes after the last fit and an updated one
    /// stays where it is.
    #[serde(default)]
    pub section: Option<String>,
}

/// Checks the request and resolves the fit.
fn resolve_input(
    input: &DoctrineFitInput,
    eve: &EveData,
    problems: &mut Problems,
) -> Option<Fitting> {
    if check_name(problems, "name", &input.name, MAX_NAME) && input.name.contains(['<', '>']) {
        problems.error("name", "must not contain `<` or `>`");
    }
    if let Some(section) = input.section.as_deref().filter(|s| !s.trim().is_empty())
        && check_name(problems, "section", section, MAX_NAME)
        && section.contains(['<', '>'])
    {
        problems.error("section", "must not contain `<` or `>`");
    }

    let eft = input.eft.as_deref().filter(|text| !text.trim().is_empty());
    let dna = input
        .dna
        .as_deref()
        .map(str::trim)
        .filter(|dna| !dna.is_empty());
    let from_eft = eft.and_then(|text| {
        let parse = parse_eft(text, eve, "eft");
        problems.errors.extend(parse.problems.errors);
        problems.warnings.extend(parse.problems.warnings);
        parse.fit
    });
    let from_dna = dna.and_then(|dna| {
        let valid = dna
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | ';' | '_'));
        let fit = valid.then(|| Fitting::from_dna(dna, eve).ok()).flatten();
        match fit {
            None => {
                problems.error("dna", "not valid ship DNA (`hull:type;count:…::`)");
                None
            }
            Some(fit) => match check_fit(&fit, eve) {
                Ok(()) => Some(fit),
                Err(message) => {
                    problems.error("dna", message);
                    None
                }
            },
        }
    });
    match (eft, dna, from_eft, from_dna) {
        (None, None, _, _) => {
            problems.error("eft", "give the fit as EFT text or as DNA");
            None
        }
        (Some(_), Some(_), Some(a), Some(b)) if a != b => {
            problems.error("dna", "does not describe the same fit as `eft`");
            None
        }
        (_, _, Some(fit), _) | (_, _, None, Some(fit)) => Some(fit),
        _ => None,
    }
}

fn section_of(input: &DoctrineFitInput) -> Option<&str> {
    input
        .section
        .as_deref()
        .map(str::trim)
        .filter(|section| !section.is_empty())
}

fn check_unique(links: &[Link], name: &str, except: Option<usize>, problems: &mut Problems) {
    if links
        .iter()
        .enumerate()
        .any(|(index, link)| Some(index) != except && link.name == name)
    {
        problems.error("name", format!("a fit named {name:?} already exists"));
    }
}

/// Inserts a link: after the last fit of `section` (or of the file), or in a new section at the
/// end.
fn insert(content: &str, section: Option<&str>, html: &str) -> String {
    let links = links(content);
    let after = match section {
        Some(section) => links
            .iter()
            .rev()
            .find(|link| link.section.as_deref() == Some(section)),
        None => links.last(),
    };
    match (after, section) {
        (Some(link), _) => format!(
            "{}<br>{html}{}",
            &content[..link.range.end],
            &content[link.range.end..]
        ),
        (None, Some(section)) => {
            let body = content.trim_end();
            let gap = if body.is_empty() { "" } else { "<br><br>" };
            format!("{body}{gap}<b>{section}</b><br>{html}<br>\n")
        }
        (None, None) => {
            let body = content.trim_end();
            format!("{body}{html}<br>\n")
        }
    }
}

/// Removes a link and the line break after it (or, for the last link before other markup, the
/// line break before it), so that removing an inserted link restores the file.
fn remove(content: &str, link: &Link) -> String {
    let before = &content[..link.range.start];
    let rest = &content[link.range.end..];
    match rest.strip_prefix("<br>") {
        Some(rest) => format!("{before}{rest}"),
        None => format!("{}{rest}", before.strip_suffix("<br>").unwrap_or(before)),
    }
}

/// Adds a doctrine fit to `content` (`fits.dat`).
pub fn create_fit(content: &str, data: &DataSet, input: &DoctrineFitInput) -> Edited {
    let mut problems = Problems::default();
    let fit = resolve_input(input, data.eve(), &mut problems);
    check_unique(&links(content), &input.name, None, &mut problems);
    let Some(fit) = fit.filter(|_| !problems.has_errors()) else {
        return Err(problems.into());
    };
    let dna = fit.to_dna(data.eve()).unwrap_or_default();
    let content = insert(content, section_of(input), &link_html(&input.name, &dna));
    problems.finish(content)
}

/// Replaces (and possibly renames or moves) the doctrine fit `selector` in `content`.
pub fn update_fit(
    content: &str,
    data: &DataSet,
    selector: &FitSelector,
    input: &DoctrineFitInput,
) -> Edited {
    let all = links(content);
    let index = find(&all, selector)?;
    let mut problems = Problems::default();
    let fit = resolve_input(input, data.eve(), &mut problems);
    check_unique(&all, &input.name, Some(index), &mut problems);
    let Some(fit) = fit.filter(|_| !problems.has_errors()) else {
        return Err(problems.into());
    };
    let dna = fit.to_dna(data.eve()).unwrap_or_default();
    let html = link_html(&input.name, &dna);
    let link = &all[index];
    let content = match section_of(input) {
        Some(section) if link.section.as_deref() != Some(section) => {
            insert(&remove(content, link), Some(section), &html)
        }
        _ => format!(
            "{}{html}{}",
            &content[..link.range.start],
            &content[link.range.end..]
        ),
    };
    problems.finish(content)
}

/// Removes the doctrine fit `selector` from `content`.
pub fn delete_fit(content: &str, selector: &FitSelector) -> Edited {
    let all = links(content);
    let index = find(&all, selector)?;
    Problems::default().finish(remove(content, &all[index]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<font><b>LOGI<br><br></font><font><a href="fitting:1:2;1::">A</a><br><a href="fitting:1:3;1::">B</a><br><br></font><font>DPS<br></font><font><a href="fitting:4:5;2::">C</a></font>"#;

    #[test]
    fn finds_links_and_sections() {
        let found = links(SAMPLE);
        let summary: Vec<(&str, &str, Option<&str>)> = found
            .iter()
            .map(|l| (l.name.as_str(), l.dna.as_str(), l.section.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("A", "1:2;1::", Some("LOGI")),
                ("B", "1:3;1::", Some("LOGI")),
                ("C", "4:5;2::", Some("DPS")),
            ]
        );
        assert_eq!(found[2].hull(), Some(4));
    }

    #[test]
    fn inserts_into_sections_and_removes() {
        let html = link_html("N", "9:1;1::");
        let into_logi = insert(SAMPLE, Some("LOGI"), &html);
        let names: Vec<_> = links(&into_logi)
            .into_iter()
            .map(|l| (l.name, l.section))
            .collect();
        assert_eq!(names[2], ("N".to_string(), Some("LOGI".to_string())));
        assert_eq!(names[3].0, "C");

        let new_section = insert(SAMPLE, Some("SUPPORT"), &html);
        let last = links(&new_section).pop().unwrap();
        assert_eq!(
            (last.name.as_str(), last.section.as_deref()),
            ("N", Some("SUPPORT"))
        );

        let at_end = insert("", None, &html);
        assert_eq!(links(&at_end).len(), 1);

        let first = &links(SAMPLE)[0];
        let removed = remove(SAMPLE, first);
        assert!(removed.contains(r#"<font><a href="fitting:1:3;1::">B</a>"#));
        assert_eq!(links(&removed).len(), 2);
    }

    #[test]
    fn selects_by_name_and_hull() {
        let doubled = format!("{SAMPLE}{}", link_html("A", "4:5;1::"));
        let all = links(&doubled);
        let select = |hull| FitSelector {
            name: "A".into(),
            hull,
        };
        assert!(matches!(
            find(&all, &select(None)),
            Err(Rejection::Invalid(_))
        ));
        assert_eq!(find(&all, &select(Some(4))), Ok(3));
        assert_eq!(find(&all, &select(Some(1))), Ok(0));
        assert!(matches!(
            find(
                &all,
                &FitSelector {
                    name: "Z".into(),
                    hull: None
                }
            ),
            Err(Rejection::NotFound(_))
        ));
    }
}
