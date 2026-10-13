//! Resolving type names from data files, reporting every unknown name at once.

use std::{collections::BTreeMap, fmt};

use crate::{EveData, Type, TypeId};

/// Resolves type names (as written in data files) to type ids.
///
/// Unknown names do not stop resolution: they are collected together with where they were used,
/// so that loading a data set can report every problem in one go and fail fast at boot. Call
/// [`NameResolver::finish`] once everything has been resolved.
pub struct NameResolver<'a> {
    eve: &'a EveData,
    missing: BTreeMap<String, Vec<String>>,
}

impl<'a> NameResolver<'a> {
    pub(crate) fn new(eve: &'a EveData) -> Self {
        Self {
            eve,
            missing: BTreeMap::new(),
        }
    }

    /// The static data this resolver looks names up in.
    pub fn eve(&self) -> &'a EveData {
        self.eve
    }

    /// Resolves an exact type name. `context` says where the name was used (for example
    /// `"modules.yaml: cargo_ignore"`) and is only evaluated when the name is unknown.
    pub fn id<C: fmt::Display>(
        &mut self,
        name: &str,
        context: impl FnOnce() -> C,
    ) -> Option<TypeId> {
        self.get(name, context).map(|ty| ty.id)
    }

    /// Like [`NameResolver::id`], returning the whole type.
    pub fn get<C: fmt::Display>(
        &mut self,
        name: &str,
        context: impl FnOnce() -> C,
    ) -> Option<&'a Type> {
        let found = self.eve.by_name(name);
        if found.is_none() {
            self.report(name, context().to_string());
        }
        found
    }

    /// Records a name as missing without looking it up (for names derived from other names).
    pub fn report(&mut self, name: &str, context: String) {
        let contexts = self.missing.entry(name.to_string()).or_default();
        if !contexts.contains(&context) {
            contexts.push(context);
        }
    }

    /// Whether any name failed to resolve so far.
    pub fn has_missing(&self) -> bool {
        !self.missing.is_empty()
    }

    /// Fails with every unknown name seen, if any.
    pub fn finish(self) -> Result<(), MissingTypes> {
        if self.missing.is_empty() {
            Ok(())
        } else {
            Err(MissingTypes(
                self.missing
                    .into_iter()
                    .map(|(name, used_in)| MissingType { name, used_in })
                    .collect(),
            ))
        }
    }
}

/// A type name that is not in the static data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingType {
    pub name: String,
    /// Where the name was used.
    pub used_in: Vec<String>,
}

/// Type names that are not in the static data, sorted by name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct MissingTypes(pub Vec<MissingType>);

impl fmt::Display for MissingTypes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} unknown type name(s):", self.0.len())?;
        for missing in &self.0 {
            write!(
                f,
                "\n  - {:?} (used in {})",
                missing.name,
                missing.used_in.join("; ")
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{EveData, Tables, TypeRow};

    #[test]
    fn collects_every_missing_name() {
        let eve = EveData::from_tables(
            Tables {
                types: vec![TypeRow {
                    id: 1,
                    name: "Known".into(),
                    group_id: 1,
                    category_id: Some(7),
                    published: true,
                }],
                ..Tables::default()
            },
            None,
        );
        let mut resolver = eve.resolver();
        assert_eq!(resolver.id("Known", || "a"), Some(1));
        assert_eq!(resolver.id("known", || "b"), None);
        assert_eq!(resolver.id("Gone", || "c"), None);
        assert_eq!(resolver.id("Gone", || "d"), None);
        let error = resolver.finish().unwrap_err();
        assert_eq!(error.0.len(), 2);
        assert_eq!(error.0[0].name, "Gone");
        assert_eq!(error.0[0].used_in, vec!["c", "d"]);
        assert_eq!(
            error.to_string(),
            "2 unknown type name(s):\n  - \"Gone\" (used in c; d)\n  - \"known\" (used in b)"
        );
    }
}
