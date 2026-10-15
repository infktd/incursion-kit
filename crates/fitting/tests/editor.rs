//! Structured editing of the data files: round trips through the files and the loader, EFT,
//! validation problems and the change listener.

mod common;

use std::sync::{Arc, Mutex};

use common::{example_store, id, read};
use pretty_assertions::assert_eq;
use wl_fitting::editor::{
    self, CategoriesDocument, CategoryRuleEntry, DoctrineFitInput, FitSelector, Rejection,
    TagsDocument, TypeKind,
};
use wl_fitting::{
    DEFAULT_RULES, DataFile, DataSet, DataStore, EditError, Fitting, FittingNote, ImplantEntry,
    ImplantSet, ImplantSlot, SkillPlan, SkillPlanLevel,
};

/// Applies an edit through the store (validated, written, reloaded).
fn apply(
    store: &DataStore,
    file: DataFile,
    edit: impl FnOnce(&str, &DataSet) -> editor::Edited,
) -> Result<Vec<String>, EditError<Rejection>> {
    store
        .edit(file, false, edit)
        .map(|(_, warnings): (_, Vec<String>)| warnings)
}

fn errors(result: Result<Vec<String>, EditError<Rejection>>) -> Vec<(String, String)> {
    match result {
        Err(EditError::Edit(Rejection::Invalid(problems))) => problems
            .errors
            .into_iter()
            .map(|e| (e.path, e.message))
            .collect(),
        other => panic!("expected validation errors, got {other:?}"),
    }
}

/// The written files load with a fresh store (the loader of the server, independent of the
/// in-memory state).
fn reopen(store: &DataStore) -> DataStore {
    DataStore::open(store.eve().clone(), store.dir()).expect("written files load")
}

#[test]
fn every_example_fit_survives_eft_both_ways() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    for fit in data.fits.iter() {
        let eft = editor::render_eft(&fit.fit, &fit.name, eve);
        let legacy = Fitting::from_eft(&eft, eve).expect("legacy parser reads rendered EFT");
        assert_eq!(legacy, vec![fit.fit.clone()], "{}:\n{eft}", fit.name);
        let parsed = editor::parse_eft(&eft, eve, "eft");
        assert_eq!(parsed.problems.errors, vec![], "{}", fit.name);
        assert_eq!(parsed.fit.as_ref(), Some(&fit.fit), "{}", fit.name);
        assert_eq!(parsed.name.as_deref(), Some(fit.name.as_str()));
    }
}

#[test]
fn eft_problems_carry_lines() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let text = "\n[Vindicator, Test]\nNo Such Module\nMega Pulse Laser II, Conflagration L\n\nHobgoblin II x5 /OFFLINE\n";
    let parsed = editor::parse_eft(text, eve, "eft");
    let lines: Vec<(Option<usize>, &str)> = parsed
        .problems
        .errors
        .iter()
        .map(|e| (e.line, e.message.as_str()))
        .collect();
    assert_eq!(lines, [(Some(3), "unknown type \"No Such Module\"")]);
    assert!(parsed.fit.is_none());
    assert!(
        parsed.problems.warnings[0].contains("loaded charge"),
        "{:?}",
        parsed.problems.warnings
    );

    let parsed = editor::parse_eft("[Rifter, x]\n[Rifter, y]\n", eve, "eft");
    assert_eq!(parsed.problems.errors[0].line, Some(2));
    let parsed = editor::parse_eft("[Hobgoblin II, x]\n", eve, "eft");
    assert_eq!(
        parsed.problems.errors[0].message,
        "\"Hobgoblin II\" is not a ship"
    );
    let parsed = editor::parse_eft("", eve, "eft");
    assert_eq!(parsed.problems.errors[0].line, None);

    // Loaded charges are ignored, stacked drones are fitted.
    let parsed = editor::parse_eft(
        "[Vindicator, T]\nNeutron Blaster Cannon II, Void L\n\n\n\n\nHobgoblin II x5\n",
        eve,
        "eft",
    );
    let fit = parsed.fit.expect("fit");
    assert_eq!(fit.modules[&id(eve, "Hobgoblin II")], 5);
    assert_eq!(fit.modules[&id(eve, "Neutron Blaster Cannon II")], 1);
    assert!(fit.cargo.is_empty());
}

#[test]
fn doctrine_fits_round_trip_through_fits_dat() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    let before = read(dir.path(), "fits.dat");
    let data = store.get();
    let list = editor::doctrine_fits(&before, &data);
    assert_eq!(list.len(), 15);
    let vindi = list.iter().find(|f| f.name == "ARMOR_VINDI").unwrap();
    assert_eq!(vindi.section.as_deref(), Some("OTHER"));
    assert_eq!(vindi.category, "other");
    assert!(!vindi.hidden);
    assert!(
        list.iter()
            .find(|f| f.name == "ALTERNATIVE_VINDI")
            .unwrap()
            .hidden
    );
    assert_eq!(list[0].section.as_deref(), Some("LOGI"));

    // Create from EFT in an existing section.
    let input = DoctrineFitInput {
        name: "TEST_VINDI".into(),
        eft: Some(vindi.eft.replace("ARMOR_VINDI", "ignored")),
        dna: None,
        section: Some("LOGI".into()),
    };
    apply(&store, DataFile::Fits, |content, data| {
        editor::create_fit(content, data, &input)
    })
    .expect("created");
    let after = read(dir.path(), "fits.dat");
    // Everything else is untouched.
    assert!(after.starts_with(&before[..before.find("ARMOR_LOKI_MWD</a>").unwrap()]));
    let reopened = reopen(&store);
    let created = reopened
        .get()
        .fits
        .find(vindi.hull.id, "TEST_VINDI")
        .cloned()
        .expect("loads");
    assert_eq!(
        created.fit,
        data.fits.find(vindi.hull.id, "ARMOR_VINDI").unwrap().fit
    );
    let listed = editor::doctrine_fits(&after, &reopened.get());
    let new = listed.iter().find(|f| f.name == "TEST_VINDI").unwrap();
    assert_eq!(new.section.as_deref(), Some("LOGI"));
    assert_eq!(new.dna, vindi.dna);

    // Duplicate names are refused with a path.
    let duplicate = errors(apply(&store, DataFile::Fits, |content, data| {
        editor::create_fit(content, data, &input)
    }));
    assert_eq!(duplicate[0].0, "name");

    // Update by DNA, rename and move to a new section.
    let paladin = list.iter().find(|f| f.name == "ARMOR_PALADIN").unwrap();
    let update = DoctrineFitInput {
        name: "TEST_PALADIN".into(),
        eft: None,
        dna: Some(paladin.dna.clone()),
        section: Some("NEW SECTION".into()),
    };
    let selector = FitSelector {
        name: "TEST_VINDI".into(),
        hull: None,
    };
    apply(&store, DataFile::Fits, |content, data| {
        editor::update_fit(content, data, &selector, &update)
    })
    .expect("updated");
    let listed = editor::doctrine_fits(&read(dir.path(), "fits.dat"), &store.get());
    let moved = listed.last().unwrap();
    assert_eq!(
        (
            moved.name.as_str(),
            moved.section.as_deref(),
            moved.dna.as_str()
        ),
        ("TEST_PALADIN", Some("NEW SECTION"), paladin.dna.as_str())
    );
    assert!(!listed.iter().any(|f| f.name == "TEST_VINDI"));

    // Delete restores the original fits.
    let selector = FitSelector {
        name: "TEST_PALADIN".into(),
        hull: None,
    };
    apply(&store, DataFile::Fits, |content, _| {
        editor::delete_fit(content, &selector)
    })
    .expect("deleted");
    let names = |store: &DataStore| {
        let mut names: Vec<String> = store.get().fits.iter().map(|f| f.name.clone()).collect();
        names.sort();
        names
    };
    assert_eq!(names(&reopen(&store)), {
        let mut names: Vec<String> = data.fits.iter().map(|f| f.name.clone()).collect();
        names.sort();
        names
    });
    assert!(matches!(
        apply(&store, DataFile::Fits, |content, _| editor::delete_fit(
            content, &selector
        )),
        Err(EditError::Edit(Rejection::NotFound(_)))
    ));
}

#[test]
fn fit_input_errors_have_paths() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let check = |input: DoctrineFitInput| {
        errors(apply(&store, DataFile::Fits, |content, data| {
            editor::create_fit(content, data, &input)
        }))
    };
    let no_fit = check(DoctrineFitInput {
        name: "<b>".into(),
        ..Default::default()
    });
    assert_eq!(
        no_fit,
        [
            (
                "name".to_string(),
                "must not contain `<` or `>`".to_string()
            ),
            (
                "eft".to_string(),
                "give the fit as EFT text or as DNA".to_string()
            ),
        ]
    );
    let bad_dna = check(DoctrineFitInput {
        name: "X".into(),
        dna: Some("abc".into()),
        ..Default::default()
    });
    assert_eq!(bad_dna[0].0, "dna");
    let mismatch = check(DoctrineFitInput {
        name: "X".into(),
        eft: Some("[Vindicator, X]\n".into()),
        dna: Some("17740:2048;1::".into()),
        ..Default::default()
    });
    assert_eq!(
        mismatch,
        [(
            "dna".to_string(),
            "does not describe the same fit as `eft`".to_string()
        )]
    );
    let unknown = check(DoctrineFitInput {
        name: "X".into(),
        eft: Some("[Vindicator, X]\nNo Such Thing\n".into()),
        ..Default::default()
    });
    assert_eq!(unknown[0].0, "eft");
}

#[test]
fn dry_runs_write_nothing() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    let before = read(dir.path(), "tags.yaml");
    let document = TagsDocument {
        public_tags: vec!["A".into()],
    };
    let (_, warnings) = store
        .edit(DataFile::Tags, true, |content, _| {
            editor::write_tags(content, &document)
        })
        .expect("valid");
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(read(dir.path(), "tags.yaml"), before);
    assert_eq!(store.get().public_tags.len(), 6);
}

#[test]
fn notes_implants_tags_and_categories_round_trip() {
    let Some((dir, store)) = example_store() else {
        return;
    };

    // Notes: create, update (rename), delete; the leading comment block is kept.
    let original_notes = store.get().fit_notes.clone();
    let note = FittingNote {
        name: "TEST_NOTE".into(),
        description: "Line one with **bold**\nline two: with a colon\n".into(),
    };
    apply(&store, DataFile::FitNotes, |content, _| {
        editor::create_note(content, &note)
    })
    .expect("note created");
    let written = read(dir.path(), "fitnotes.yaml");
    assert!(
        written.starts_with("# Notes shown next to doctrine fits"),
        "{written}"
    );
    let reopened = reopen(&store);
    assert_eq!(reopened.get().fit_notes.last(), Some(&note));
    assert_eq!(
        reopened.get().fit_notes[..12],
        original_notes[..],
        "existing notes are unchanged"
    );
    let views = editor::fit_notes(&store.get());
    assert_eq!(
        views
            .iter()
            .find(|n| n.name == "ARMOR_VINDI")
            .unwrap()
            .target,
        editor::NoteTarget::Fit
    );
    assert_eq!(views[0].target, editor::NoteTarget::Tier);
    let renamed = FittingNote {
        name: "TEST_NOTE_2".into(),
        ..note.clone()
    };
    apply(&store, DataFile::FitNotes, |content, _| {
        editor::update_note(content, "TEST_NOTE", &renamed)
    })
    .expect("note updated");
    assert_eq!(store.get().fit_notes.last(), Some(&renamed));
    let taken = errors(apply(&store, DataFile::FitNotes, |content, _| {
        editor::update_note(
            content,
            "TEST_NOTE_2",
            &FittingNote {
                name: "Shield".into(),
                description: String::new(),
            },
        )
    }));
    assert_eq!(taken[0].0, "name");
    apply(&store, DataFile::FitNotes, |content, _| {
        editor::delete_note(content, "TEST_NOTE_2")
    })
    .expect("note deleted");
    assert_eq!(reopen(&store).get().fit_notes, original_notes);

    // Implant sets.
    let set = ImplantSet {
        name: "Budget".into(),
        slots: vec![ImplantSlot {
            slot: 1,
            default: vec![ImplantEntry {
                name: "High-grade Ascendancy Alpha".into(),
                note: Some("+speed".into()),
                group: Some("All".into()),
            }],
            alternative: vec![],
        }],
    };
    apply(&store, DataFile::Implants, |content, data| {
        editor::create_implant_set(content, data.eve(), &set)
    })
    .expect("set created");
    let reopened = reopen(&store);
    assert_eq!(reopened.get().implants.last(), Some(&set));
    assert_eq!(reopened.get().implants[0], store.get().implants[0]);
    let mut bad = set.clone();
    bad.name = "Other".into();
    bad.slots[0].slot = 11;
    bad.slots[0].default[0].name = String::new();
    bad.slots.push(bad.slots[0].clone());
    bad.slots[1].slot = 2;
    bad.slots[1].default[0].name = "Hobgoblin II".into();
    let mut problems = match store.edit(DataFile::Implants, true, |content, data| {
        editor::create_implant_set(content, data.eve(), &bad)
    }) {
        Err(EditError::Edit(Rejection::Invalid(problems))) => problems,
        other => panic!("{other:?}"),
    };
    let paths: Vec<String> = problems.errors.drain(..).map(|e| e.path).collect();
    assert_eq!(paths, ["slots[0].slot", "slots[0].default[0].name"]);
    assert!(problems.warnings[0].starts_with("slots[1].default[0].name"));
    apply(&store, DataFile::Implants, |content, _| {
        editor::delete_implant_set(content, "Budget")
    })
    .expect("set deleted");

    // Tags keep their order in the file.
    let tags = TagsDocument {
        public_tags: vec!["ZED".into(), "HQ-FC".into(), "LOGI".into()],
    };
    apply(&store, DataFile::Tags, |content, _| {
        editor::write_tags(content, &tags)
    })
    .expect("tags");
    assert_eq!(
        editor::read_tags(&read(dir.path(), "tags.yaml")).unwrap(),
        tags
    );
    assert!(read(dir.path(), "tags.yaml").starts_with("# Tags shown"));
    assert_eq!(reopen(&store).get().public_tags.len(), 3);
    let bad_tags = TagsDocument {
        public_tags: vec!["A,B".into(), "X".into(), "X".into(), " ".into()],
    };
    let paths: Vec<String> = errors(apply(&store, DataFile::Tags, |content, _| {
        editor::write_tags(content, &bad_tags)
    }))
    .into_iter()
    .map(|(path, _)| path)
    .collect();
    assert_eq!(
        paths,
        ["public_tags[0]", "public_tags[2]", "public_tags[3]"]
    );

    // Categories: the example file re-serialized loads to the same rules.
    let original = editor::read_categories(&read(dir.path(), "categories.yaml")).unwrap();
    let before = store.get().categories.rules.clone();
    apply(&store, DataFile::Categories, |content, data| {
        editor::write_categories(content, data.eve(), &original)
    })
    .expect("categories");
    assert_eq!(reopen(&store).get().categories.rules, before);
    assert_eq!(
        editor::read_categories(&read(dir.path(), "categories.yaml")).unwrap(),
        original
    );
    let mut bad = original.clone();
    bad.rules.push(CategoryRuleEntry {
        item: "Nope".into(),
        category: "missing".into(),
    });
    bad.categories.push(bad.categories[0].clone());
    let paths: Vec<String> = errors(apply(&store, DataFile::Categories, |content, data| {
        editor::write_categories(content, data.eve(), &bad)
    }))
    .into_iter()
    .map(|(path, _)| path)
    .collect();
    let last = original.rules.len();
    assert_eq!(
        paths,
        [
            "categories[3].id".to_string(),
            format!("rules[{last}].item"),
            format!("rules[{last}].category"),
        ]
    );
}

#[test]
fn rules_round_trip_from_the_built_in_default() {
    let Some((dir, store)) = example_store() else {
        return;
    };
    let default = editor::read_rules(DEFAULT_RULES).unwrap();
    let rules_before = store.get().rules.clone();
    apply(&store, DataFile::Rules, |content, data| {
        editor::write_rules(content, data.eve(), &default)
    })
    .expect("rules written");
    let written = read(dir.path(), "rules.yaml");
    assert!(written.starts_with("# Community rules"), "{written}");
    assert_eq!(editor::read_rules(&written).unwrap(), default);
    assert_eq!(reopen(&store).get().rules, rules_before);

    let mut bad = default.clone();
    bad.fit_check.badge_auto_approve[0].hull = "Nope".into();
    bad.fit_check.implant_module_rules[0].implant_set = "MISSING".into();
    bad.fittings.hidden_fit_marker = String::new();
    bad.skill_plans.default_tank_level = "nope".into();
    bad.fleet.as_mut().unwrap().motd_roles[1].name = "ddd".into();
    let paths: Vec<String> = errors(apply(&store, DataFile::Rules, |content, data| {
        editor::write_rules(content, data.eve(), &bad)
    }))
    .into_iter()
    .map(|(path, _)| path)
    .collect();
    assert_eq!(
        paths,
        [
            "fit_check.badge_auto_approve[0].hull",
            "fit_check.implant_module_rules[0].implant_set",
            "fittings.hidden_fit_marker",
            "skill_plans.default_tank_level",
            "fleet.motd_roles[1].name",
        ]
    );
}

#[test]
fn data_set_errors_surface_without_a_path() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    // A category referenced by rules.yaml disappears: a warning, not an error.
    let document = CategoriesDocument {
        categories: vec![wl_core::types::WaitlistCategory {
            id: "only".into(),
            name: "Only".into(),
        }],
        rules: vec![],
    };
    let (validation, warnings) = store
        .edit(DataFile::Categories, true, |content, data| {
            editor::write_categories(content, data.eve(), &document)
        })
        .expect("loads with warnings");
    assert!(warnings.is_empty());
    assert!(
        validation.warnings[0].contains("categories not defined"),
        "{:?}",
        validation.warnings
    );

    let error = store
        .edit(DataFile::Tags, true, |_, _| {
            Ok::<_, Rejection>(("public_tags: 3\n".to_string(), ()))
        })
        .unwrap_err();
    let EditError::Data(error) = error else {
        panic!("{error:?}")
    };
    let fields = editor::data_error_fields(&error);
    assert_eq!(fields[0].path, "");
    assert!(fields[0].message.starts_with("tags.yaml: invalid YAML"));
}

#[test]
fn skill_plan_problems_have_paths_and_legacy_messages() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let plan = SkillPlan {
        name: " ".into(),
        description: String::new(),
        plan: vec![
            SkillPlanLevel::Fit {
                hull: "Vindicator".into(),
                fit: "NOPE".into(),
            },
            SkillPlanLevel::Skills {
                from: "Vindicator".into(),
                tier: "max".into(),
            },
            SkillPlanLevel::Skill {
                from: "Gunnery".into(),
                level: 6,
            },
        ],
    };
    let problems: Vec<(String, String)> = data
        .plan_problems(&plan)
        .into_iter()
        .map(|p| (p.path(), p.legacy_message()))
        .collect();
    assert_eq!(
        problems,
        [
            ("name".into(), "Plan name cannot be empty".into()),
            (
                "plan[0].fit".into(),
                "Plan step 1: Fit 'NOPE' not found for hull 'Vindicator'".into()
            ),
            (
                "plan[1].tier".into(),
                "Plan step 2: Invalid tier 'max'. Must be 'min', 'elite', or 'gold'".into()
            ),
            (
                "plan[2].level".into(),
                "Plan step 3: Skill level must be between 1 and 5, got 6".into()
            ),
        ]
    );
    assert_eq!(
        data.validate_plan(&plan).unwrap_err().to_string(),
        "validation error: Plan name cannot be empty"
    );
}

#[test]
fn type_search_ranks_and_filters() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let data = store.get();
    let eve = data.eve();
    let ships = editor::search_types(eve, "vindicator", &[TypeKind::Ship], 5);
    assert_eq!(ships[0].name, "Vindicator");
    assert!(ships.iter().all(|hit| hit.kind == TypeKind::Ship));
    let skills = editor::search_types(eve, "gunnery", &[TypeKind::Skill], 3);
    assert_eq!(skills[0].name, "Gunnery");
    let modules = editor::search_types(eve, "blaster cannon ii", &[TypeKind::Module], 50);
    assert!(
        modules
            .iter()
            .any(|hit| hit.name == "Neutron Blaster Cannon II")
    );
    assert!(modules.iter().all(|hit| hit.slot == Some("high")));
    assert_eq!(editor::search_types(eve, "a", &[], 7).len(), 7);
    let implants = editor::search_types(eve, "ascendancy alpha", &[TypeKind::Implant], 10);
    assert!(
        implants
            .iter()
            .any(|hit| hit.name == "High-grade Ascendancy Alpha")
    );
}

#[test]
fn the_listener_hears_every_change() {
    let Some((_dir, store)) = example_store() else {
        return;
    };
    let heard = Arc::new(Mutex::new(Vec::new()));
    let sink = heard.clone();
    store.on_change(move |file| sink.lock().unwrap().push(file));
    let tags = TagsDocument {
        public_tags: vec!["A".into()],
    };
    store
        .edit(DataFile::Tags, true, |content, _| {
            editor::write_tags(content, &tags)
        })
        .unwrap();
    store
        .edit(DataFile::Tags, false, |content, _| {
            editor::write_tags(content, &tags)
        })
        .unwrap();
    store
        .replace_file(DataFile::Tags, "public_tags: [B]\n")
        .unwrap();
    store.replace_file(DataFile::Tags, "nope: 1\n").unwrap_err();
    store.reload().unwrap();
    assert_eq!(
        *heard.lock().unwrap(),
        [Some(DataFile::Tags), Some(DataFile::Tags), None]
    );
}
