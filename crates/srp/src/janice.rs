//! Loss appraisal through Janice, adjusted by the `srp_modified` rules of `modules.yaml`.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wl_eve_data::{EveData, TypeId};
use wl_fitting::{SrpModifiedPricing, SrpModifiedRule};

use crate::Srp;
use crate::error::{SrpError, SrpResult};

/// The Janice appraisal query the legacy waitlist used (Jita, sell prices, not persisted).
const APPRAISAL_PATH: &str =
    "/api/rest/v2/appraisal?market=2&pricing=sell&persist=false&compactize=true&pricePercentage=1";

impl Srp {
    /// Appraise `destroyed_items` (lines like `Large Shield Extender II 2`) with Janice, then
    /// apply the `srp_modified` overrides (scoped to `hull`). Returns the total value in ISK and
    /// Janice's items with `effectivePrices` rewritten (and `srp_override` where a rule applied).
    pub async fn appraise(
        &self,
        destroyed_items: &[String],
        hull: Option<i64>,
        rules: &BTreeMap<TypeId, SrpModifiedRule>,
        eve: Option<&EveData>,
    ) -> SrpResult<(f64, Vec<Value>)> {
        if destroyed_items.is_empty() {
            return Ok((0.0, Vec::new()));
        }
        let settings = self.settings();
        let url = format!(
            "{}{APPRAISAL_PATH}",
            settings.janice_url.trim_end_matches('/')
        );
        let response = self
            .http()
            .post(url)
            .header("Content-Type", "text/plain")
            .header("accept", "application/json")
            .header("X-ApiKey", &settings.janice_api_key)
            .body(destroyed_items.join("\n"))
            .send()
            .await
            .map_err(|e| SrpError::BadRequest(format!("Failed to call Janice API: {e}")))?;
        let status = response.status();
        let text = response.text().await.map_err(|e| {
            SrpError::BadRequest(format!("Failed to read Janice API response: {e}"))
        })?;
        if !status.is_success() {
            return Err(SrpError::BadRequest(format!(
                "Janice API returned error: {status} - Body: {text}"
            )));
        }
        let appraisal: Value = serde_json::from_str(&text).map_err(|e| {
            SrpError::BadRequest(format!("Failed to parse Janice API response: {e}"))
        })?;
        let mut items = appraisal
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let hull = hull.and_then(|h| TypeId::try_from(h).ok());
        let total = apply_overrides(&mut items, hull, rules, eve);
        Ok((total, items))
    }
}

/// Rewrite each item's sell prices per the `srp_modified` rules and return the summed total.
pub fn apply_overrides(
    items: &mut [Value],
    hull: Option<TypeId>,
    rules: &BTreeMap<TypeId, SrpModifiedRule>,
    eve: Option<&EveData>,
) -> f64 {
    let mut total = 0.0;
    for item in items.iter_mut() {
        // Janice v2 puts the EVE type id in itemType.eid.
        let type_id = item
            .pointer("/itemType/eid")
            .or_else(|| item.pointer("/itemType/id"))
            .and_then(Value::as_i64)
            .and_then(|id| TypeId::try_from(id).ok())
            .unwrap_or(0);
        let type_name = item
            .pointer("/itemType/name")
            .and_then(Value::as_str)
            .unwrap_or("");
        let amount = item
            .get("amount")
            .and_then(Value::as_f64)
            .unwrap_or(1.0)
            .max(0.0);
        let unit = item
            .pointer("/effectivePrices/sellPrice")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let janice_total = item
            .pointer("/effectivePrices/sellPriceTotal")
            .and_then(Value::as_f64)
            .unwrap_or(unit * amount);

        let rule = rules.get(&type_id).or_else(|| {
            // Fall back to the name when the id didn't match a rule.
            (!type_name.is_empty())
                .then(|| eve?.id_of(type_name))
                .flatten()
                .and_then(|id| rules.get(&id))
        });
        let pricing = rule.filter(|r| r.applies_to_hull(hull)).map(|r| r.pricing);

        let (sell_price, sell_total, meta) = match pricing {
            Some(SrpModifiedPricing::MarketPercent(pct)) => {
                let factor = pct / 100.0;
                (
                    unit * factor,
                    janice_total * factor,
                    Some(json!({
                        "market_percent": pct,
                        "janice_sell_price": unit,
                        "janice_sell_price_total": janice_total,
                    })),
                )
            }
            Some(SrpModifiedPricing::Flat(flat)) => {
                let price = flat as f64;
                (
                    price,
                    price * amount,
                    Some(json!({
                        "flat": flat,
                        "janice_sell_price": unit,
                        "janice_sell_price_total": janice_total,
                    })),
                )
            }
            None => (unit, janice_total, None),
        };

        if let Some(obj) = item.as_object_mut() {
            match obj.get_mut("effectivePrices") {
                Some(prices) => {
                    if let Some(prices) = prices.as_object_mut() {
                        prices.insert("sellPrice".into(), json!(sell_price));
                        prices.insert("sellPriceTotal".into(), json!(sell_total));
                    }
                }
                None => {
                    obj.insert(
                        "effectivePrices".into(),
                        json!({"sellPrice": sell_price, "sellPriceTotal": sell_total}),
                    );
                }
            }
            if let Some(meta) = meta {
                obj.insert("srp_override".into(), meta);
            }
        }
        total += sell_total;
    }
    total
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn applies_flat_and_percent_rules() {
        let mut rules = BTreeMap::new();
        rules.insert(
            23416,
            SrpModifiedRule {
                pricing: SrpModifiedPricing::Flat(1_000_000_000),
                hulls: BTreeSet::from([28659]),
                exclude_hulls: BTreeSet::new(),
            },
        );
        rules.insert(
            100,
            SrpModifiedRule {
                pricing: SrpModifiedPricing::MarketPercent(50.0),
                hulls: BTreeSet::new(),
                exclude_hulls: BTreeSet::new(),
            },
        );
        let mut items = vec![
            json!({"amount": 2, "itemType": {"eid": 23416, "name": "Peace"},
                   "effectivePrices": {"sellPrice": 12345.0, "sellPriceTotal": 24690.0}}),
            json!({"amount": 1, "itemType": {"eid": 100, "name": "X"},
                   "effectivePrices": {"sellPrice": 10.0, "sellPriceTotal": 10.0}}),
            json!({"amount": 3, "itemType": {"eid": 7, "name": "Plain"},
                   "effectivePrices": {"sellPrice": 1.0}}),
        ];
        let total = apply_overrides(&mut items, Some(28659), &rules, None);
        assert_eq!(total, 2_000_000_000.0 + 5.0 + 3.0);
        assert_eq!(items[0]["effectivePrices"]["sellPrice"], 1_000_000_000.0);
        assert_eq!(items[0]["srp_override"]["flat"], 1_000_000_000);
        assert_eq!(items[0]["srp_override"]["janice_sell_price_total"], 24690.0);
        assert_eq!(items[1]["srp_override"]["market_percent"], 50.0);
        assert_eq!(items[2]["effectivePrices"]["sellPriceTotal"], 3.0);
        assert!(items[2].get("srp_override").is_none());

        // On another hull the hull-scoped rule doesn't apply.
        let mut items = vec![json!({"amount": 2, "itemType": {"eid": 23416},
                                    "effectivePrices": {"sellPrice": 5.0, "sellPriceTotal": 10.0}})];
        assert_eq!(apply_overrides(&mut items, Some(1), &rules, None), 10.0);
    }
}
