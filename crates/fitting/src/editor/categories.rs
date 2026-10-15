//! Waitlist categories and their rules (`categories.yaml`), edited as one document: the rules
//! are ordered (the first matching rule wins) and refer to the categories.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use wl_core::types::WaitlistCategory;
use wl_eve_data::EveData;

use super::{Edited, Problems, Rejection, check_name, resolve, write_yaml};
use crate::{DataError, DataFile, yaml};

const MAX_ID: usize = 64;
const MAX_NAME: usize = 100;

/// `categories.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CategoriesDocument {
    /// Categories in display order.
    pub categories: Vec<WaitlistCategory>,
    /// Rules in order: a fit takes the category of the first rule whose item is its hull or a
    /// fitted module. Fits matching no rule get `fit_check.default_category` of `rules.yaml`.
    pub rules: Vec<CategoryRuleEntry>,
}

/// One categorisation rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CategoryRuleEntry {
    /// Type name of a hull or module.
    pub item: String,
    /// Category id.
    pub category: String,
}

/// Parses `categories.yaml`.
pub fn read_categories(content: &str) -> Result<CategoriesDocument, DataError> {
    yaml::parse(DataFile::Categories, content)
}

fn check(document: &CategoriesDocument, eve: &EveData) -> Problems {
    let mut problems = Problems::default();
    let mut ids = BTreeSet::new();
    for (index, category) in document.categories.iter().enumerate() {
        let path = format!("categories[{index}]");
        if check_name(&mut problems, &format!("{path}.id"), &category.id, MAX_ID) {
            if category.id.contains(char::is_whitespace) {
                problems.error(format!("{path}.id"), "must not contain whitespace");
            } else if !ids.insert(category.id.as_str()) {
                problems.error(
                    format!("{path}.id"),
                    format!("category {:?} is listed twice", category.id),
                );
            }
        }
        check_name(
            &mut problems,
            &format!("{path}.name"),
            &category.name,
            MAX_NAME,
        );
    }
    let mut items = BTreeSet::new();
    for (index, rule) in document.rules.iter().enumerate() {
        let path = format!("rules[{index}]");
        if resolve(eve, &mut problems, &format!("{path}.item"), &rule.item).is_some()
            && !items.insert(rule.item.as_str())
        {
            problems.warn(
                format!("{path}.item"),
                format!(
                    "{:?} has an earlier rule, so this one never applies",
                    rule.item
                ),
            );
        }
        if !ids.contains(rule.category.as_str()) {
            problems.error(
                format!("{path}.category"),
                format!("unknown category {:?}", rule.category),
            );
        }
    }
    problems
}

/// Replaces `content` (`categories.yaml`) with `document`.
pub fn write_categories(content: &str, eve: &EveData, document: &CategoriesDocument) -> Edited {
    let problems = check(document, eve);
    if problems.has_errors() {
        return Err(problems.into());
    }
    let yaml = write_yaml(DataFile::Categories, content, document)
        .map_err(|error| Rejection::invalid("", error.to_string()))?;
    problems.finish(yaml)
}
