//! Loading the example data set and the derived API data.

mod common;

use common::{eve, example_dir, example_store, id, read};
use pretty_assertions::assert_eq;
use wl_fitting::{DataError, DataFile, DataStore, SkillTier};

#[test]
fn example_data_loads_without_warnings() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    assert_eq!(data.warnings, Vec::<String>::new());
    assert_eq!(data.fits.iter().count(), 15);
    assert_eq!(
        data.categories
            .categories
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["logi", "bastion", "other"]
    );
    assert_eq!(
        data.public_tags
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["ALT", "DPS", "HQ-FC", "LOGI", "TRAINEE", "VINDI"]
    );
    assert_eq!(data.fit_notes.len(), 12);
    assert_eq!(data.implants.len(), 1);
}

#[test]
fn categorisation_uses_first_matching_rule() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let category_of = |name: &str| {
        let hull = id(eve, name);
        let fit = &data.fits.for_hull(hull)[0].fit;
        data.categorize(fit).to_string()
    };
    assert_eq!(category_of("Nestor"), "logi");
    assert_eq!(category_of("Loki"), "logi");
    // Marauder doctrine fits carry a Bastion Module.
    assert_eq!(category_of("Paladin"), "bastion");
    assert_eq!(category_of("Kronos"), "bastion");
    assert_eq!(category_of("Vindicator"), "other");

    // A hull with no rule at all falls back to the default category.
    let rifter = wl_fitting::Fitting {
        hull: id(eve, "Rifter"),
        modules: Default::default(),
        cargo: Default::default(),
    };
    assert_eq!(data.categories.categorize(&rifter), None);
    assert_eq!(data.categorize(&rifter), "other");
}

#[test]
fn skill_tiers_apply_defaults_and_merge_keys() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let paladin = &data.skills.requirements["Paladin"];

    // From the merged `_generic` block: elite given, gold defaults to 5, priority to 1.
    let hull_upgrades = paladin[&id(eve, "Hull Upgrades")];
    assert_eq!(hull_upgrades.get(SkillTier::Min), Some(4));
    assert_eq!(hull_upgrades.get(SkillTier::Elite), Some(5));
    assert_eq!(hull_upgrades.get(SkillTier::Gold), Some(5));
    assert_eq!(hull_upgrades.priority, 1);

    // `Spaceship Command` has no min: elite stays, min is null.
    let command = paladin[&id(eve, "Spaceship Command")];
    assert_eq!(command.min, None);
    assert_eq!(command.elite, Some(5));

    // Explicit priority survives the merge.
    assert_eq!(paladin[&id(eve, "Cybernetics")].priority, 5);

    // Anchor-only blocks are skill sets too, as in the legacy API output.
    assert!(data.skills.requirements.contains_key("_generic"));

    // Relevant skills include prerequisites of doctrine fit modules.
    let gunnery = id(eve, "Gunnery");
    assert!(data.skills.relevant_skills.contains(&gunnery));
    assert_eq!(data.skills.name_lookup["Gunnery"], gunnery);
    assert_eq!(data.skills.id_lookup[&gunnery], "Gunnery");
}

#[test]
fn skills_yaml_merge_matches_legacy_semantics() {
    let Some(eve) = eve() else { return };
    let dir = example_dir();
    // Explicit keys beat merged ones; earlier merged mappings beat later ones.
    let skills = "categories: {}\nrequirements:\n  _a: &a\n    Gunnery: {min: 1}\n    Navigation: {min: 1}\n  _b: &b\n    Gunnery: {min: 2}\n    Mechanics: {min: 2}\n  Megathron:\n    <<: [*a, *b]\n    Navigation: {min: 3}\n";
    std::fs::write(dir.path().join("skills.yaml"), skills).unwrap();
    std::fs::write(dir.path().join("skillplan.yaml"), "plans: []\n").unwrap();
    let store = DataStore::open(eve.clone(), dir.path()).unwrap();
    let data = store.get();
    let ship = &data.skills.requirements["Megathron"];
    assert_eq!(ship[&id(&eve, "Gunnery")].min, Some(1));
    assert_eq!(ship[&id(&eve, "Navigation")].min, Some(3));
    assert_eq!(ship[&id(&eve, "Mechanics")].min, Some(2));
}

#[test]
fn fittings_response_hides_alternatives() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let fittings = data.fittings.fittingdata.as_ref().unwrap();
    let names: Vec<&str> = fittings.iter().map(|fit| fit.name.as_str()).collect();
    assert!(
        names.iter().all(|name| !name.contains("ALTERNATIVE")),
        "{names:?}"
    );
    assert_eq!(names.len(), 9);
    // Logistics rule items, in file order.
    let eve = data.eve();
    assert_eq!(
        data.fittings.rules.as_ref().unwrap(),
        &vec![id(eve, "Nestor"), id(eve, "Loki")]
    );
    insta::assert_json_snapshot!("fittings_response", data.fittings);
}

#[test]
fn module_preload_covers_doctrine_types() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let used = data.fits.used_type_ids();
    assert_eq!(data.module_preload.len(), used.len());
    let laser = id(data.eve(), "Mega Pulse Laser II");
    assert_eq!(data.module_preload[&laser].slot, Some("high"));
}

#[test]
fn skill_plans_response_matches_snapshot() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let response = &data.skill_plans.response;
    assert_eq!(response.plans.len(), data.skill_plans.plans.len());

    // Kronos plans list the tank variant with the Kronos id; plain hulls by their own id.
    let eve = data.eve();
    let armor_kronos = response
        .plans
        .iter()
        .find(|plan| plan.source.name == "Recommended Armor Kronos")
        .unwrap();
    assert_eq!(armor_kronos.ships.len(), 1);
    assert_eq!(armor_kronos.ships[0].name, "Armor Kronos");
    assert_eq!(armor_kronos.ships[0].id, id(eve, "Kronos"));

    // Steps never repeat and prerequisites come first.
    for plan in &response.plans {
        let mut seen = std::collections::BTreeSet::new();
        for &(skill, level) in &plan.levels {
            assert!(
                seen.insert((skill, level)),
                "{} repeats a step",
                plan.source.name
            );
            if level > 1 {
                assert!(seen.contains(&(skill, level - 1)), "{}", plan.source.name);
            }
        }
    }

    insta::assert_json_snapshot!("skill_plans_response", response);
}

#[test]
fn unknown_type_names_are_all_reported() {
    let Some(eve) = eve() else { return };
    let dir = example_dir();
    let categories = read(dir.path(), "categories.yaml")
        .replace("item: Nestor", "item: Nestorr")
        .replace("item: Eos", "item: Eoss");
    std::fs::write(dir.path().join("categories.yaml"), categories).unwrap();
    let modules = read(dir.path(), "modules.yaml").replace("ECCM Script", "ECCM Scripts");
    std::fs::write(dir.path().join("modules.yaml"), modules).unwrap();

    let error = DataStore::open(eve, dir.path()).unwrap_err();
    let DataError::MissingTypes(missing) = &error else {
        panic!("unexpected error: {error}");
    };
    let names: Vec<&str> = missing.0.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["ECCM Scripts", "Eoss", "Nestorr"]);
    assert!(
        error
            .to_string()
            .contains("categories.yaml: rules[0] (logi)"),
        "{error}"
    );
}

#[test]
fn validate_does_not_write_and_replace_swaps_atomically() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    let before = store.get();
    let original = read(dir.path(), "tags.yaml");

    // Invalid content is rejected and nothing changes.
    let error = store
        .replace_file(DataFile::Tags, "public_tags: [unclosed")
        .unwrap_err();
    assert!(
        matches!(
            error,
            DataError::Yaml {
                file: "tags.yaml",
                ..
            }
        ),
        "{error}"
    );
    assert!(store.validate(DataFile::Tags, "nope: 1").is_err());
    assert_eq!(read(dir.path(), "tags.yaml"), original);

    // Validation alone writes nothing.
    store
        .validate(DataFile::Tags, "public_tags: [HQ-FC]\n")
        .unwrap();
    assert_eq!(read(dir.path(), "tags.yaml"), original);
    assert!(std::sync::Arc::ptr_eq(&before, &store.get()));

    // Replacing writes, backs up, and swaps in the new data set; old snapshots stay valid.
    store
        .replace_file(DataFile::Tags, "public_tags: [HQ-FC]\n")
        .unwrap();
    assert_eq!(read(dir.path(), "tags.yaml"), "public_tags: [HQ-FC]\n");
    assert_eq!(store.get().public_tags.len(), 1);
    assert_eq!(before.public_tags.len(), 6);
    let backups = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("tags.yaml.backup.")
        })
        .count();
    assert_eq!(backups, 1);
    assert!(!dir.path().join("tags.yaml.tmp").exists());
}

#[test]
fn reload_keeps_the_old_data_on_error() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    std::fs::write(
        dir.path().join("fits.dat"),
        "<a href=\"fitting:670:1;1::\">BAD</a>",
    )
    .unwrap();
    assert!(store.reload().is_err());
    assert_eq!(store.get().fits.iter().count(), 15);

    std::fs::write(
        dir.path().join("fits.dat"),
        "<a href=\"fitting:17740:3186;8::\">ONLY_VINDI</a>",
    )
    .unwrap();
    store.reload().unwrap();
    assert_eq!(store.get().fits.iter().count(), 1);
}

#[test]
fn rules_file_is_optional_and_overridable() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    assert_eq!(
        store.read_file(DataFile::Rules).unwrap(),
        wl_fitting::DEFAULT_RULES
    );
    assert_eq!(store.get().rules.fittings.hidden_fit_marker, "ALTERNATIVE");

    let custom = wl_fitting::DEFAULT_RULES
        .replace("hidden_fit_marker: ALTERNATIVE", "hidden_fit_marker: ARMOR");
    store.replace_file(DataFile::Rules, &custom).unwrap();
    assert!(dir.path().join("rules.yaml").exists());
    let names: Vec<String> = store
        .get()
        .fittings
        .fittingdata
        .as_ref()
        .unwrap()
        .iter()
        .map(|fit| fit.name.clone())
        .collect();
    assert!(
        names.iter().all(|name| !name.contains("ARMOR")),
        "{names:?}"
    );

    // Unknown keys in rules.yaml are typos, not silently ignored.
    let typo = wl_fitting::DEFAULT_RULES.replace("default_category:", "default_categroy:");
    assert!(store.validate(DataFile::Rules, &typo).is_err());
}

#[test]
fn fleet_motd_roles_default_to_the_legacy_roles() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let id = |name: &str| id(eve, name);
    let fleet = data.rules.fleet.clone();
    assert_eq!(fleet.role_names(), ["DDD", "LR", "PS", "MS", "MTAC"]);
    assert_eq!(fleet.motd_link_type, 1377);
    let hulls =
        |role: &str| -> Vec<i32> { fleet.role(role).unwrap().hulls.iter().copied().collect() };
    assert_eq!(hulls("ddd"), [id("Vindicator")]);
    let mut ms = vec![id("Damnation"), id("Eos"), id("Claymore")];
    ms.sort();
    assert_eq!(hulls("MS"), ms);
    assert_eq!(fleet.role("MTAC").unwrap().hulls.len(), 6);
    assert!(fleet.role("DPS").is_none());

    // A rules.yaml written before the fleet section existed still loads, with the built-in roles.
    let without_fleet = wl_fitting::DEFAULT_RULES
        .split("\nfleet:")
        .next()
        .unwrap()
        .to_string();
    store.replace_file(DataFile::Rules, &without_fleet).unwrap();
    assert_eq!(store.get().rules.fleet, fleet);

    let custom = wl_fitting::DEFAULT_RULES.replace("- name: DDD", "- name: VINDI");
    store.replace_file(DataFile::Rules, &custom).unwrap();
    assert!(store.get().rules.fleet.role("VINDI").is_some());
    let bad = wl_fitting::DEFAULT_RULES.replace("- name: DDD", "- name: \"D D\"");
    assert!(store.validate(DataFile::Rules, &bad).is_err());
}
