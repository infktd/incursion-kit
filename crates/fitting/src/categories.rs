//! `categories.yaml`: waitlist categories and the rules assigning fits to them.

use serde::Deserialize;
use wl_core::types::WaitlistCategory;
use wl_eve_data::{NameResolver, TypeId};

use crate::{DataError, DataFile, Fitting, yaml};

#[derive(Deserialize)]
struct CategoryFile {
    categories: Vec<WaitlistCategory>,
    rules: Vec<RuleEntry>,
}

#[derive(Deserialize)]
struct RuleEntry {
    item: String,
    category: String,
}

/// A rule: a fit whose hull is, or which fits, `type_id` goes into `category` (an id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategoryRule {
    pub type_id: TypeId,
    pub category: String,
}

/// Waitlist categories and the rules assigning fits to them.
#[derive(Debug, Clone)]
pub struct Categories {
    /// Categories in display order.
    pub categories: Vec<WaitlistCategory>,
    /// Rules in file order; the first matching rule wins.
    pub rules: Vec<CategoryRule>,
}

impl Categories {
    pub(crate) fn load(content: &str, names: &mut NameResolver<'_>) -> Result<Self, DataError> {
        let file: CategoryFile = yaml::parse(DataFile::Categories, content)?;
        let rules = file
            .rules
            .into_iter()
            .enumerate()
            .filter_map(|(index, rule)| {
                let type_id = names.id(&rule.item, || {
                    format!("categories.yaml: rules[{index}] ({})", rule.category)
                })?;
                Some(CategoryRule {
                    type_id,
                    category: rule.category,
                })
            })
            .collect();
        Ok(Self {
            categories: file.categories,
            rules,
        })
    }

    /// The category id of a fit: the first rule whose item is the hull or a fitted module.
    pub fn categorize(&self, fit: &Fitting) -> Option<&str> {
        self.rules
            .iter()
            .find(|rule| fit.hull == rule.type_id || fit.modules.contains_key(&rule.type_id))
            .map(|rule| rule.category.as_str())
    }

    /// Items of every rule assigning `category`, in file order.
    pub fn rule_items(&self, category: &str) -> Vec<TypeId> {
        self.rules
            .iter()
            .filter(|rule| rule.category == category)
            .map(|rule| rule.type_id)
            .collect()
    }

    /// The display name of a category id.
    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.categories
            .iter()
            .find(|category| category.id == id)
            .map(|category| category.name.as_str())
    }
}
