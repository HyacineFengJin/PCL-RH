use super::*;

fn row(source: CommunitySource, id: &str, url: &str) -> ProjectIdentity {
    ProjectIdentity {
        project: ProjectKey::new(source, ProjectKind::Mod, id).unwrap(),
        canonical_url: url.into(),
        aliases: Vec::new(),
    }
}

fn link(row: &mut ProjectIdentity, url: &str) {
    row.aliases.push(AliasEvidence {
        target: AliasTarget::Url(url.into()),
        evidence_url: row.canonical_url.clone(),
    });
}

fn trio() -> Vec<ProjectIdentity> {
    vec![
        row(
            CommunitySource::Modrinth,
            "Mr123abc",
            "https://modrinth.com/mod/fixture",
        ),
        row(
            CommunitySource::Curseforge,
            "123",
            "https://www.curseforge.com/minecraft/mc-mods/fixture",
        ),
        row(
            CommunitySource::Mcmod,
            "456",
            "https://www.mcmod.cn/class/456.html",
        ),
    ]
}

#[test]
fn identical_slugs_across_providers_do_not_join() {
    assert_eq!(
        merge_identities(&trio()).unwrap().groups,
        vec![vec![0], vec![1], vec![2]]
    );
}

#[test]
fn explicit_related_project_links_join_all_three_sources() {
    let mut rows = trio();
    link(
        &mut rows[2],
        "https://modrinth.com/mod/fixture?utm_source=mcmod#about",
    );
    link(
        &mut rows[2],
        "http://curseforge.com/minecraft/mc-mods/fixture/",
    );
    let merged = merge_identities(&rows).unwrap();
    assert_eq!(merged.groups, vec![vec![0, 1, 2]]);
    assert!(merged.conflicts.is_empty());
}

#[test]
fn explicit_project_ids_join_without_similar_names_or_slugs() {
    let mut rows = trio();
    rows[1].canonical_url = "https://www.curseforge.com/minecraft/mc-mods/another-name".into();
    let alias = AliasEvidence {
        target: AliasTarget::Project(rows[1].project.clone()),
        evidence_url: rows[0].canonical_url.clone(),
    };
    rows[0].aliases.push(alias);
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0, 1], vec![2]]
    );
}

#[test]
fn same_source_numeric_id_deduplicates_despite_a_renamed_url() {
    let mut rows = trio();
    let mut renamed = rows[1].clone();
    renamed.canonical_url = "https://www.curseforge.com/minecraft/mc-mods/renamed".into();
    rows.push(renamed);
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0], vec![1, 3], vec![2]]
    );
}

#[test]
fn same_source_different_ids_on_one_url_are_ambiguous() {
    let mut rows = trio();
    let mut fork = rows[1].clone();
    fork.project.id = "124".into();
    rows.push(fork);
    let merged = merge_identities(&rows).unwrap();
    assert_eq!(merged.groups, vec![vec![0], vec![1], vec![2], vec![3]]);
    assert_eq!(merged.conflicts.len(), 1);
}

#[test]
fn broad_mcmod_entry_cannot_swallow_loader_variants() {
    let mut rows = trio();
    rows.push(row(
        CommunitySource::Curseforge,
        "124",
        "https://www.curseforge.com/minecraft/mc-mods/fixture-fabric",
    ));
    link(&mut rows[2], "https://modrinth.com/mod/fixture");
    link(
        &mut rows[2],
        "https://www.curseforge.com/minecraft/mc-mods/fixture",
    );
    link(
        &mut rows[2],
        "https://www.curseforge.com/minecraft/mc-mods/fixture-fabric",
    );
    let merged = merge_identities(&rows).unwrap();
    assert_eq!(merged.groups, vec![vec![0], vec![1], vec![2], vec![3]]);
    assert_eq!(merged.conflicts[0].indices, vec![0, 1, 2, 3]);
}

#[test]
fn unseen_second_project_link_still_blocks_ambiguous_bridge() {
    let mut rows = trio();
    link(&mut rows[2], "https://modrinth.com/mod/fixture");
    link(
        &mut rows[2],
        "https://www.curseforge.com/minecraft/mc-mods/fixture",
    );
    link(
        &mut rows[2],
        "https://www.curseforge.com/minecraft/mc-mods/not-yet-fetched",
    );
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0], vec![1], vec![2]]
    );
}

#[test]
fn duplicate_rows_stay_deduplicated_in_a_conflicting_component() {
    let mut rows = trio();
    rows.push(rows[0].clone());
    link(&mut rows[2], "https://modrinth.com/mod/fixture");
    link(&mut rows[2], "https://modrinth.com/mod/some-fork");
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0, 3], vec![1], vec![2]]
    );
}

#[test]
fn transitive_same_provider_collision_rejects_entire_component() {
    let mut rows = trio();
    rows.push(row(
        CommunitySource::Modrinth,
        "Mr999abc",
        "https://modrinth.com/mod/fork",
    ));
    link(
        &mut rows[0],
        "https://www.curseforge.com/minecraft/mc-mods/fixture",
    );
    link(&mut rows[1], "https://www.mcmod.cn/class/456.html");
    link(&mut rows[2], "https://modrinth.com/mod/fork");
    let merged = merge_identities(&rows).unwrap();
    assert!(merged.groups.iter().all(|group| group.len() == 1));
    rows.reverse();
    assert!(merge_identities(&rows)
        .unwrap()
        .groups
        .iter()
        .all(|group| group.len() == 1));
}

#[test]
fn encyclopedia_mod_and_modpack_id_spaces_are_distinct() {
    let mut rows = trio();
    let mut pack = rows[2].clone();
    pack.project.kind = ProjectKind::Modpack;
    pack.canonical_url = "https://www.mcmod.cn/modpack/456.html".into();
    rows.push(pack);
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0], vec![1], vec![2], vec![3]]
    );
}

#[test]
fn different_resource_types_do_not_join_even_with_explicit_link() {
    let mut rows = trio();
    link(&mut rows[2], "https://modrinth.com/resourcepack/fixture");
    let merged = merge_identities(&rows).unwrap();
    assert_eq!(merged.groups, vec![vec![0], vec![1], vec![2]]);
    assert_eq!(merged.conflicts.len(), 1);
}

#[test]
fn evidence_must_belong_to_owning_project_page() {
    let mut rows = trio();
    link(&mut rows[2], "https://modrinth.com/mod/fixture");
    rows[2].aliases[0].evidence_url = "https://www.mcmod.cn/class/999.html".into();
    assert!(merge_identities(&rows).unwrap_err().contains("证据"));
}

#[test]
fn unsupported_hosts_routes_and_authorities_are_rejected() {
    for bad in [
        "https://modrinth.com.evil.invalid/mod/fixture",
        "https://user@modrinth.com/mod/fixture",
        "https://modrinth.com:8443/mod/fixture",
        "https://modrinth.com/mod/fixture/version/123",
        "https://modrinth.com/mod/fixture%2Ffork",
        "https://www.curseforge.com/minecraft/mc-mods/fixture/files",
        "https://www.mcmod.cn/class/version/456.html",
        "https://www.mcmod.cn/class/0.html",
        "file:///class/456.html",
    ] {
        assert!(canonical_project_url(bad).is_none(), "{bad}");
    }
}

#[test]
fn unsupported_inputs_and_budgets_fail_explicitly() {
    let mut rows = trio();
    rows[2].project.id = "0456".into();
    assert!(merge_identities(&rows).is_err());
    rows[2].project.id = "457".into();
    assert!(merge_identities(&rows).is_err());
    rows = vec![trio()[0].clone(); MAX_ROWS + 1];
    assert!(merge_identities(&rows).is_err());
}

#[test]
fn matching_is_case_sensitive_for_provider_paths_and_ids() {
    let mut rows = trio();
    link(&mut rows[2], "https://modrinth.com/mod/Fixture");
    assert_eq!(
        merge_identities(&rows).unwrap().groups,
        vec![vec![0], vec![1], vec![2]]
    );
}
