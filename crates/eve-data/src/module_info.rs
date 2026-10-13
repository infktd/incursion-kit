//! Module descriptions served by `/api/module/info` and `/api/module/preload`.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::{Category, EveData, TypeId};

/// Name, category and slot of a type, as sent to the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ModuleInfo {
    pub name: String,
    /// `ship`, `module`, `charge`, `drone`, `implant` or `_other`.
    #[schema(value_type = String)]
    pub category: &'static str,
    /// `high`, `med`, `low`, `rig`, `drone`, `other` (exactly when the category is `_other`) or
    /// `null`.
    #[schema(value_type = Option<String>)]
    pub slot: Option<&'static str>,
}

impl EveData {
    /// Module info for the given ids; unknown ids are left out.
    pub fn module_info(
        &self,
        ids: impl IntoIterator<Item = TypeId>,
    ) -> BTreeMap<TypeId, ModuleInfo> {
        ids.into_iter()
            .filter_map(|id| {
                let ty = self.get(id)?;
                let slot = match ty.category {
                    Category::Other(_) => Some("other"),
                    _ => ty.slot().map(|slot| slot.as_str()),
                };
                Some((
                    id,
                    ModuleInfo {
                        name: ty.name.clone(),
                        category: ty.category.category_name(),
                        slot,
                    },
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::{EffectId, EffectRow, EveData, Tables, TypeRow};

    #[test]
    fn serializes_like_the_legacy_endpoint() {
        let row = |id, name: &str, category_id| TypeRow {
            id,
            name: name.into(),
            group_id: 1,
            category_id: Some(category_id),
            published: true,
        };
        let eve = EveData::from_tables(
            Tables {
                types: vec![row(1, "Gun", 7), row(2, "Ammo", 8), row(3, "Thing", 25)],
                effects: vec![EffectRow {
                    type_id: 1,
                    effect: EffectId::HIGH_POWER,
                }],
                ..Tables::default()
            },
            None,
        );
        let info = eve.module_info([1, 2, 3, 4]);
        assert_eq!(
            serde_json::to_string(&info).unwrap(),
            r#"{"1":{"name":"Gun","category":"module","slot":"high"},"2":{"name":"Ammo","category":"charge","slot":null},"3":{"name":"Thing","category":"_other","slot":"other"}}"#
        );
    }
}
