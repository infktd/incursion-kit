//! Checks against the real static data file (ported from the legacy waitlist's type tests).
//!
//! These need `deploy/sde/sde.sqlite` (or `WL_SDE_PATH`); build it with
//! `cargo run -p wl-eve-data --example build_sde`. Without it the tests are skipped.

use std::path::PathBuf;

use wl_eve_data::{AttributeId, Category, EffectId, EveData, TypeId};

async fn eve() -> Option<EveData> {
    let path = std::env::var_os("WL_SDE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/sde/sde.sqlite")
        });
    if !path.exists() {
        eprintln!(
            "SKIPPED: static data file {} not found; build it with \
             `cargo run -p wl-eve-data --example build_sde`",
            path.display()
        );
        return None;
    }
    Some(EveData::load(&path).await.expect("static data file loads"))
}

fn id_of(eve: &EveData, name: &str) -> TypeId {
    eve.id_of(name)
        .unwrap_or_else(|| panic!("{name} not in SDE"))
}

#[tokio::test]
async fn capsule() {
    let Some(eve) = eve().await else { return };
    let item = eve.require(670).unwrap();
    assert_eq!(item.name, "Capsule");
    assert_eq!(item.category, Category::Ship);
    assert_eq!(item.category.category_name(), "ship");
    assert_eq!(eve.name_of(670), Some("Capsule"));
    assert_eq!(eve.id_of("Capsule"), Some(670));
    assert!(eve.info().is_some());
}

#[tokio::test]
async fn microwarpdrive() {
    let Some(eve) = eve().await else { return };
    let name = "Core X-Type 500MN Microwarpdrive";
    let item = eve.require(id_of(&eve, name)).unwrap();
    assert_eq!(item.category, Category::Module);
    assert_eq!(item.name, name);
    assert_eq!(item.attribute(AttributeId::META_LEVEL), Some(16.0));
    assert!(!item.effects.contains(&EffectId::LOW_POWER));
    assert!(item.effects.contains(&EffectId::MED_POWER));
    assert!(!item.effects.contains(&EffectId::HIGH_POWER));
    assert!(!item.effects.contains(&EffectId::RIG_SLOT));
}

#[tokio::test]
async fn ids_of() {
    let Some(eve) = eve().await else { return };
    for name in [
        "Core X-Type 500MN Microwarpdrive",
        "Gist X-Type 500MN Microwarpdrive",
        "The Friend Ship",
    ] {
        assert!(eve.id_of(name).is_some_and(|id| id > 0), "{name}");
    }
    for name in ["Imaginary Titan", "Core Y-Type 500MN Microwarpdrive"] {
        assert_eq!(eve.id_of(name), None, "{name}");
    }
}

#[tokio::test]
async fn requirements() {
    let Some(eve) = eve().await else { return };
    let monitor = eve.require(id_of(&eve, "Monitor")).unwrap();
    for skill in [
        "Amarr Cruiser",
        "Gallente Cruiser",
        "Minmatar Cruiser",
        "Caldari Cruiser",
        "Flag Cruisers",
    ] {
        assert_eq!(
            monitor.skill_requirements.get(&id_of(&eve, skill)),
            Some(&1),
            "{skill}"
        );
    }
    assert_eq!(monitor.skill_requirements.len(), 5);
}

#[tokio::test]
async fn variations() {
    let Some(eve) = eve().await else { return };
    let variations = eve.type_variations(id_of(&eve, "Core X-Type 500MN Microwarpdrive"));
    assert_eq!(
        variations.get(&id_of(&eve, "500MN Microwarpdrive I")),
        Some(&0)
    );
    assert_eq!(
        variations.get(&id_of(&eve, "500MN Microwarpdrive II")),
        Some(&5)
    );
    assert_eq!(
        variations.get(&id_of(&eve, "Core X-Type 500MN Microwarpdrive")),
        Some(&16)
    );
}

#[tokio::test]
async fn module_info_for_fitting_types() {
    let Some(eve) = eve().await else { return };
    let laser = id_of(&eve, "Mega Pulse Laser II");
    let info = eve.module_info([laser, 670, 12816]);
    assert_eq!(info[&laser].slot, Some("high"));
    assert_eq!(info[&670].category, "ship");
    assert_eq!(info[&12816].category, "charge");
}
