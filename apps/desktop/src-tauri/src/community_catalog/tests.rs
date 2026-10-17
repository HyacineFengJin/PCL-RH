use super::*;
use crate::community_identity::AliasEvidence;

fn request(query: &str) -> SearchRequest {
    SearchRequest::parse(query.into(), None, None, None, None, None, None).unwrap()
}

#[test]
fn page_bounds_preserve_legacy_first_page_without_a_shared_provider_page_cap() {
    assert_eq!(checked_page(None).unwrap(), 1);
    assert_eq!(checked_page(Some(100)).unwrap(), 100);
    assert_eq!(checked_page(Some(101)).unwrap(), 101);
    assert_eq!(checked_page(Some(u32::MAX)).unwrap(), u32::MAX);
    assert!(checked_page(Some(0)).is_err());
    assert_eq!(
        provider_offset(u32::MAX, 30).unwrap(),
        (u64::from(u32::MAX) - 1) * 30
    );
    assert!(provider_offset(0, 30).is_err());
}

#[test]
fn modrinth_pagination_uses_validated_provider_count_before_grouping() {
    let hit = provider_hit(CommunitySource::Modrinth, "Abcd1234", "example");
    for (total, more) in [(31, false), (100, true)] {
        let result = modrinth_page(
            json!({"hits": [hit.clone()], "offset":30, "limit":30, "total_hits":total}),
            ProjectKind::Mod,
            2,
        )
        .unwrap();
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.has_more, more);
    }
    let result = modrinth_page(
        json!({"hits":[], "offset":60, "limit":30, "total_hits":31}),
        ProjectKind::Mod,
        3,
    )
    .unwrap();
    assert!(!result.has_more);
}

#[test]
fn modrinth_malformed_pagination_is_a_source_failure_not_a_guessed_last_page() {
    let valid = json!({"hits": [provider_hit(CommunitySource::Modrinth, "Abcd1234", "example")], "offset":30, "limit":30, "total_hits":100});
    for (field, value) in [
        ("offset", json!(0)),
        ("limit", json!(50)),
        ("total_hits", json!(30)),
        ("total_hits", Value::Null),
        ("hits", json!([])),
    ] {
        let mut response = valid.clone();
        response[field] = value;
        assert!(
            modrinth_page(response, ProjectKind::Mod, 2).is_err(),
            "{field}"
        );
    }
}

#[test]
fn merged_pages_keep_provider_progress_and_partial_success_independent_from_card_count() {
    for more in [false, true] {
        let result = finish_page(
            vec![
                success(provider(CommunitySource::Modrinth, "Abcd1234", "example")),
                success(encyclopedia("123", &["https://modrinth.com/mod/example"])),
                failure(CommunitySource::Curseforge),
            ],
            Vec::new(),
            RequestedSource::All,
            2,
            more,
        )
        .unwrap();
        assert_eq!(hits(&result).len(), 1);
        assert_eq!(hits(&result)[0]["sources"].as_array().unwrap().len(), 2);
        assert_eq!(result["pagination"], json!({"page":2, "has_next":more}));
        assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn provider_limits_are_independent_and_modrinth_can_continue_beyond_page_one_hundred() {
    let mut value = request("example");
    value.page = 101;
    let mut warnings = Vec::new();
    assert!(source_page_allowed(&value, CommunitySource::Modrinth, &mut warnings).unwrap());
    assert!(source_page_allowed(&value, CommunitySource::Curseforge, &mut warnings).unwrap());
    assert!(!source_page_allowed(&value, CommunitySource::Mcmod, &mut warnings).unwrap());
    value.page = 201;
    assert!(!source_page_allowed(&value, CommunitySource::Curseforge, &mut warnings).unwrap());
    assert_eq!(warnings.len(), 2);
    value.source = RequestedSource::One(CommunitySource::Curseforge);
    assert!(source_page_allowed(&value, CommunitySource::Curseforge, &mut warnings).is_err());
    let result = finish_page(
        vec![success(provider(
            CommunitySource::Modrinth,
            "Abcd1234",
            "example",
        ))],
        Vec::new(),
        RequestedSource::All,
        100,
        true,
    )
    .unwrap();
    assert_eq!(result["pagination"]["has_next"], true);
    assert!(result["warnings"].as_array().unwrap().is_empty());
}

fn provider_hit(source: CommunitySource, id: &str, slug: &str) -> Value {
    json!({
        "project_id": id, "slug": slug, "title": "Fixture Mod", "description": "Description",
        "url": format!("{}{}/{}", source_prefix(source), if source == CommunitySource::Curseforge { "minecraft/mc-mods" } else { "mod" }, slug),
        "icon_url": "https://cdn.modrinth.com/icon.png", "categories": ["fabric"],
        "display_categories": ["utility"], "versions": ["1.21.1"], "downloads": 100,
        "date_modified": "2026-10-01T00:00:00Z",
    })
}

fn provider(source: CommunitySource, id: &str, slug: &str) -> CatalogRow {
    provider_rows(
        source,
        ProjectKind::Mod,
        vec![provider_hit(source, id, slug)],
    )
    .unwrap()
    .remove(0)
}

fn encyclopedia(id: &str, aliases: &[&str]) -> CatalogRow {
    let project = ProjectKey::new(CommunitySource::Mcmod, ProjectKind::Mod, id).unwrap();
    let url = format!("https://www.mcmod.cn/class/{id}.html");
    let aliases = aliases
        .iter()
        .map(|target| AliasEvidence {
            target: AliasTarget::Url((*target).into()),
            evidence_url: url.clone(),
        })
        .collect();
    CatalogRow {
        source: CommunitySource::Mcmod,
        hit: json!({ "project_id": id, "title": "合成模组", "description": "摘要", "url": url,
            "source": "MC百科", "categories": [], "display_categories": [], "versions": [],
            "downloads": 0, "date_modified": "", "icon_url": null, "attribution": "MC百科" }),
        identity: ProjectIdentity {
            project,
            canonical_url: url,
            aliases,
        },
    }
}

fn success(row: CatalogRow) -> SourceBatch {
    SourceBatch {
        evidence_only: false,
        source: row.source,
        result: Ok(vec![row]),
    }
}
fn failure(source: CommunitySource) -> SourceBatch {
    SourceBatch {
        evidence_only: false,
        source,
        result: Err("synthetic unavailable".into()),
    }
}
fn hits(result: &Value) -> &Vec<Value> {
    result["hits"].as_array().unwrap()
}

#[test]
fn each_available_source_survives_other_source_failures() {
    for surviving in [
        CommunitySource::Modrinth,
        CommunitySource::Curseforge,
        CommunitySource::Mcmod,
    ] {
        let mut batches = Vec::new();
        for source in [
            CommunitySource::Modrinth,
            CommunitySource::Curseforge,
            CommunitySource::Mcmod,
        ] {
            batches.push(if source == surviving {
                success(match source {
                    CommunitySource::Modrinth => provider(source, "Mr123abc", "fixture"),
                    CommunitySource::Curseforge => provider(source, "123", "fixture"),
                    CommunitySource::Mcmod => encyclopedia("456", &[]),
                })
            } else {
                failure(source)
            });
        }
        let result = assemble_results(batches, Vec::new(), RequestedSource::All).unwrap();
        assert_eq!(hits(&result).len(), 1);
        assert_eq!(hits(&result)[0]["source"], source_name(surviving));
        assert_eq!(result["warnings"].as_array().unwrap().len(), 2);
    }
}

#[test]
fn all_failures_error_but_a_successful_empty_search_is_valid() {
    assert!(assemble_results(
        vec![
            failure(CommunitySource::Modrinth),
            failure(CommunitySource::Curseforge)
        ],
        Vec::new(),
        RequestedSource::All
    )
    .is_err());
    let result = assemble_results(
        vec![
            SourceBatch {
                evidence_only: false,
                source: CommunitySource::Modrinth,
                result: Ok(Vec::new()),
            },
            failure(CommunitySource::Curseforge),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert!(hits(&result).is_empty());
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
}

#[test]
fn explicit_source_failure_is_not_reported_as_empty_success() {
    let result = assemble_results(
        vec![failure(CommunitySource::Mcmod)],
        Vec::new(),
        RequestedSource::One(CommunitySource::Mcmod),
    );
    assert!(result.unwrap_err().contains("MC百科"));
}

#[test]
fn matching_names_and_slugs_do_not_merge_across_sources() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            success(provider(CommunitySource::Curseforge, "123", "fixture")),
            success(encyclopedia("456", &[])),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 3);
}

#[test]
fn explicit_three_source_match_uses_download_provider_id_and_chinese_title() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            success(provider(
                CommunitySource::Curseforge,
                "123",
                "different-slug",
            )),
            success(encyclopedia(
                "456",
                &[
                    "https://modrinth.com/mod/fixture",
                    "https://www.curseforge.com/minecraft/mc-mods/different-slug",
                ],
            )),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 1);
    let hit = &hits(&result)[0];
    assert_eq!(hit["source"], "Modrinth");
    assert_eq!(hit["project_id"], "Mr123abc");
    assert_eq!(hit["identity_key"], "modrinth:Mr123abc");
    assert_eq!(hit["title"], "合成模组");
    assert_eq!(hit["original_title"], "Fixture Mod");
    assert_eq!(hit["sources"].as_array().unwrap().len(), 3);
    assert_eq!(hit["sources"][1]["project_id"], "123");
    assert_eq!(hit["sources"][2]["project_id"], "456");
}

#[test]
fn known_link_url_never_turns_a_slug_into_a_provider_id() {
    let result = assemble_results(
        vec![success(encyclopedia(
            "456",
            &[
                "https://modrinth.com/mod/eightChr",
                "https://www.curseforge.com/minecraft/mc-mods/fixture",
            ],
        ))],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    let links = hits(&result)[0]["sources"].as_array().unwrap();
    assert!(links[0]["project_id"].is_null());
    assert!(links[1]["project_id"].is_null());
    assert_eq!(links[2]["project_id"], "456");
}

#[test]
fn curseforge_is_primary_when_modrinth_is_unavailable() {
    let result = assemble_results(
        vec![
            failure(CommunitySource::Modrinth),
            success(provider(CommunitySource::Curseforge, "123", "fixture")),
            success(encyclopedia(
                "456",
                &["https://www.curseforge.com/minecraft/mc-mods/fixture"],
            )),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 1);
    assert_eq!(hits(&result)[0]["identity_key"], "curseforge:123");
    assert_eq!(hits(&result)[0]["source"], "CurseForge");
}

#[test]
fn broad_entry_keeps_same_provider_forks_independent() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            SourceBatch {
                evidence_only: false,
                source: CommunitySource::Curseforge,
                result: Ok(vec![
                    provider(CommunitySource::Curseforge, "123", "fixture-forge"),
                    provider(CommunitySource::Curseforge, "124", "fixture-fabric"),
                ]),
            },
            success(encyclopedia(
                "456",
                &[
                    "https://modrinth.com/mod/fixture",
                    "https://www.curseforge.com/minecraft/mc-mods/fixture-forge",
                    "https://www.curseforge.com/minecraft/mc-mods/fixture-fabric",
                ],
            )),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 4);
    assert!(hits(&result)
        .iter()
        .all(|hit| hit["sources"].as_array().unwrap().len() == 1));
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
}

#[test]
fn duplicate_provider_ids_keep_one_card_and_one_source_link() {
    let row = provider(CommunitySource::Modrinth, "Mr123abc", "fixture");
    let result = assemble_results(
        vec![SourceBatch {
            evidence_only: false,
            source: row.source,
            result: Ok(vec![row.clone(), row]),
        }],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 1);
    assert_eq!(hits(&result)[0]["sources"].as_array().unwrap().len(), 1);
}

#[test]
fn eight_character_slug_is_not_promoted_to_actual_project_id() {
    let row = provider(CommunitySource::Modrinth, "RealId12", "eightChr");
    assert_eq!(row.identity.project.id, "RealId12");
    assert_eq!(
        row.identity.canonical_url,
        "https://modrinth.com/mod/eightChr"
    );
    assert!(row.identity.aliases.is_empty());
}

#[test]
fn invalid_provider_payload_rejects_that_batch() {
    let mut hit = provider_hit(CommunitySource::Curseforge, "123", "fixture");
    hit["url"] = json!("https://modrinth.com/mod/fixture");
    assert!(provider_rows(CommunitySource::Curseforge, ProjectKind::Mod, vec![hit]).is_err());
    assert!(provider_rows(
        CommunitySource::Modrinth,
        ProjectKind::Mod,
        vec![provider_hit(CommunitySource::Modrinth, "a-slug", "fixture")]
    )
    .is_err());
    assert!(provider_rows(
        CommunitySource::Modrinth,
        ProjectKind::Mod,
        vec![provider_hit(CommunitySource::Modrinth, "Mr123abc", "fixture"); MAX_PROVIDER_HITS + 1]
    )
    .is_err());
}

#[test]
fn provider_resource_kind_mismatch_is_rejected() {
    let mut hit = provider_hit(CommunitySource::Curseforge, "123", "fixture");
    hit["url"] = json!("https://www.curseforge.com/minecraft/modpacks/fixture");
    assert!(provider_rows(CommunitySource::Curseforge, ProjectKind::Mod, vec![hit]).is_err());
}

#[test]
fn active_unsupported_mcmod_filters_have_explicit_skip_reasons() {
    let mut value = request("fixture");
    assert!(value.mcmod_skip_reason().is_none());
    value.version = Some("1.21.1".into());
    assert!(value.mcmod_skip_reason().unwrap().contains("筛选"));
    value.version = None;
    value.loader = Some("fabric".into());
    assert!(value.mcmod_skip_reason().is_some());
    value.loader = None;
    value.tag = Some("utility".into());
    assert!(value.mcmod_skip_reason().is_some());
    value.tag = None;
    value.sort = "downloads".into();
    assert!(value.mcmod_skip_reason().unwrap().contains("排序"));
    value.sort = "relevance".into();
    value.kind = ProjectKind::Shader;
    assert!(value.mcmod_skip_reason().unwrap().contains("仅支持"));
}

#[test]
fn all_default_browse_skips_mcmod_without_detail_requests() {
    let mut value = request("");
    assert!(!value.wants_mcmod());
    value.source = RequestedSource::One(CommunitySource::Mcmod);
    assert!(value.wants_mcmod());
    assert!(value.mcmod_skip_reason().is_none());
    value.source = RequestedSource::All;
    value.query = "fixture".into();
    assert!(value.wants_mcmod());
}

#[test]
fn empty_filter_sentinels_are_inactive_and_loader_is_normalized() {
    let value = SearchRequest::parse(
        "fixture".into(),
        None,
        None,
        Some("任意".into()),
        Some("NeoForge".into()),
        Some("默认".into()),
        Some("全部".into()),
    )
    .unwrap();
    assert_eq!(value.version, None);
    assert_eq!(value.tag, None);
    assert_eq!(value.loader.as_deref(), Some("neoforge"));
    assert_eq!(value.sort, "relevance");
}

#[test]
fn facets_encode_filter_values_as_json_data() {
    let mut value = request("fixture");
    value.version = Some("1.21.1".into());
    value.loader = Some("fabric".into());
    value.tag = Some("utility\"], [\"injected".into());
    let facets: Value = serde_json::from_str(&modrinth_facets(&value)).unwrap();
    assert_eq!(facets.as_array().unwrap().len(), 4);
    assert_eq!(facets[1][0], "versions:1.21.1");
    assert_eq!(facets[3][0], "categories:utility\"], [\"injected");
    value.kind = ProjectKind::Datapack;
    let facets: Value = serde_json::from_str(&modrinth_facets(&value)).unwrap();
    assert_eq!(facets[0][0], "all_project_types:datapack");
}

#[test]
fn unknown_input_values_and_budgets_are_rejected_before_network() {
    assert!(SearchRequest::parse("x".repeat(1025), None, None, None, None, None, None).is_err());
    assert!(SearchRequest::parse(
        "fixture".into(),
        Some("world".into()),
        None,
        None,
        None,
        None,
        None
    )
    .is_err());
    assert!(SearchRequest::parse(
        "fixture".into(),
        None,
        Some("mirror".into()),
        None,
        None,
        None,
        None
    )
    .is_err());
    assert!(SearchRequest::parse(
        "fixture".into(),
        None,
        None,
        None,
        None,
        Some("random".into()),
        None
    )
    .is_err());
    assert!(active_filter(Some("a\n".into())).is_err());
}

#[test]
fn warnings_are_bounded_and_deduplicated() {
    let mut warnings = vec!["duplicate".into(), "duplicate".into()];
    warnings.extend((0..30).map(|index| format!("{index}{}", "界".repeat(1024))));
    let result = assemble_results(
        vec![success(encyclopedia("456", &[]))],
        warnings,
        RequestedSource::All,
    )
    .unwrap();
    let warnings = result["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 16);
    assert!(warnings
        .iter()
        .all(|value| value.as_str().unwrap().chars().count() <= 512));
    assert_eq!(warnings[0], "duplicate");
}

#[test]
fn encyclopedia_identity_key_preserves_numeric_namespaces() {
    let mut key = ProjectKey::new(CommunitySource::Mcmod, ProjectKind::Mod, "123").unwrap();
    assert_eq!(identity_key(&key), "mcmod:class/123");
    key.kind = ProjectKind::Modpack;
    assert_eq!(identity_key(&key), "mcmod:modpack/123");
}

#[test]
fn icons_reject_untrusted_origins_and_keep_each_provider_cdn() {
    assert!(safe_icon(
        "https://cdn.modrinth.com.evil.invalid/icon.png",
        CommunitySource::Modrinth
    )
    .is_none());
    assert!(safe_icon(
        "https://user@cdn.modrinth.com/icon.png",
        CommunitySource::Modrinth
    )
    .is_none());
    assert!(safe_icon(
        "https://mediafilez.forgecdn.net/avatars/icon.png",
        CommunitySource::Curseforge
    )
    .is_some());
    assert!(safe_icon(
        "https://cdn.modrinth.com/icon.png",
        CommunitySource::Curseforge
    )
    .is_none());
}

#[test]
fn curseforge_loader_and_author_distribution_metadata_are_preserved() {
    let mut hit = provider_hit(CommunitySource::Curseforge, "123", "fixture");
    hit["loaders"] = json!(["forge", "neoforge"]);
    hit["download_allowed"] = json!(false);
    hit["is_available"] = json!(true);
    let row = provider_rows(CommunitySource::Curseforge, ProjectKind::Mod, vec![hit])
        .unwrap()
        .remove(0);
    assert!(row.hit["categories"]
        .as_array()
        .unwrap()
        .contains(&json!("neoforge")));
    assert_eq!(row.hit["download_allowed"], false);
    assert_eq!(row.hit["is_available"], true);
}

fn evidence(rows: Vec<CatalogRow>) -> SourceBatch {
    SourceBatch {
        source: CommunitySource::Mcmod,
        evidence_only: true,
        result: Ok(rows),
    }
}

#[test]
fn all_filtered_queries_read_encyclopedia_as_evidence_only() {
    let mut value = request("fixture");
    value.version = Some("1.21.1".into());
    assert_eq!(value.encyclopedia_use(), Ok(EncyclopediaUse::EvidenceOnly));
    value.version = None;
    value.loader = Some("fabric".into());
    assert_eq!(value.encyclopedia_use(), Ok(EncyclopediaUse::EvidenceOnly));
    value.loader = None;
    value.tag = Some("utility".into());
    assert_eq!(value.encyclopedia_use(), Ok(EncyclopediaUse::EvidenceOnly));
    value.tag = None;
    value.sort = "downloads".into();
    assert_eq!(value.encyclopedia_use(), Ok(EncyclopediaUse::EvidenceOnly));
    value.source = RequestedSource::One(CommunitySource::Mcmod);
    assert!(value.encyclopedia_use().is_err());
    value.source = RequestedSource::All;
    value.query.clear();
    assert_eq!(value.encyclopedia_use(), Ok(EncyclopediaUse::Skip));
}

#[test]
fn evidence_only_cannot_bypass_unsupported_kind_or_query_bound() {
    let mut value = request("fixture");
    value.version = Some("1.21.1".into());
    value.kind = ProjectKind::Shader;
    assert!(value.encyclopedia_use().is_err());
    value.kind = ProjectKind::Mod;
    value.query = "a".repeat(256);
    assert!(value.encyclopedia_use().is_err());
    value.query = "fixture".into();
    value.source = RequestedSource::One(CommunitySource::Mcmod);
    assert!(value.encyclopedia_use().is_err());
}

#[test]
fn filtered_provider_rows_merge_using_evidence_without_extra_unfiltered_cards() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            success(provider(CommunitySource::Curseforge, "123", "fixture")),
            evidence(vec![
                encyclopedia(
                    "456",
                    &[
                        "https://modrinth.com/mod/fixture",
                        "https://www.curseforge.com/minecraft/mc-mods/fixture",
                    ],
                ),
                encyclopedia(
                    "457",
                    &[
                        "https://modrinth.com/mod/unfiltered",
                        "https://www.curseforge.com/minecraft/mc-mods/unfiltered",
                    ],
                ),
            ]),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 1);
    let hit = &hits(&result)[0];
    assert_eq!(hit["source"], "Modrinth");
    assert_eq!(hit["project_id"], "Mr123abc");
    assert_eq!(hit["title"], "合成模组");
    assert_eq!(hit["versions"], json!(["1.21.1"]));
    assert_eq!(hit["sources"].as_array().unwrap().len(), 3);
}

#[test]
fn evidence_success_cannot_mask_both_filtered_provider_failures() {
    let result = assemble_results(
        vec![
            failure(CommunitySource::Modrinth),
            failure(CommunitySource::Curseforge),
            evidence(vec![encyclopedia(
                "456",
                &["https://modrinth.com/mod/fixture"],
            )]),
        ],
        Vec::new(),
        RequestedSource::All,
    );
    let error = result.unwrap_err();
    assert!(error.contains("Modrinth"));
    assert!(error.contains("CurseForge"));
}

#[test]
fn successful_empty_filtered_provider_discards_encyclopedia_only_groups() {
    let result = assemble_results(
        vec![
            SourceBatch {
                source: CommunitySource::Modrinth,
                evidence_only: false,
                result: Ok(Vec::new()),
            },
            failure(CommunitySource::Curseforge),
            evidence(vec![encyclopedia(
                "456",
                &["https://modrinth.com/mod/not-compatible"],
            )]),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert!(hits(&result).is_empty());
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
}

#[test]
fn failed_evidence_keeps_filtered_providers_independent_with_warning() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            success(provider(CommunitySource::Curseforge, "123", "fixture")),
            SourceBatch {
                source: CommunitySource::Mcmod,
                evidence_only: true,
                result: Err("synthetic HTTP 403".into()),
            },
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 2);
    assert!(result["warnings"][0].as_str().unwrap().contains("MC百科"));
}

#[test]
fn conflicting_evidence_never_hides_or_combines_filtered_forks() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            SourceBatch {
                source: CommunitySource::Curseforge,
                evidence_only: false,
                result: Ok(vec![
                    provider(CommunitySource::Curseforge, "123", "fixture-forge"),
                    provider(CommunitySource::Curseforge, "124", "fixture-fabric"),
                ]),
            },
            evidence(vec![encyclopedia(
                "456",
                &[
                    "https://modrinth.com/mod/fixture",
                    "https://www.curseforge.com/minecraft/mc-mods/fixture-forge",
                    "https://www.curseforge.com/minecraft/mc-mods/fixture-fabric",
                ],
            )]),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 3);
    assert!(hits(&result).iter().all(|hit| hit["source"] != "MC百科"));
    assert!(hits(&result)
        .iter()
        .all(|hit| hit["sources"].as_array().unwrap().len() == 1));
}

#[test]
fn one_filtered_provider_can_borrow_evidence_without_creating_failed_provider_card() {
    let result = assemble_results(
        vec![
            success(provider(CommunitySource::Modrinth, "Mr123abc", "fixture")),
            failure(CommunitySource::Curseforge),
            evidence(vec![encyclopedia(
                "456",
                &[
                    "https://modrinth.com/mod/fixture",
                    "https://www.curseforge.com/minecraft/mc-mods/fixture",
                ],
            )]),
        ],
        Vec::new(),
        RequestedSource::All,
    )
    .unwrap();
    assert_eq!(hits(&result).len(), 1);
    assert_eq!(hits(&result)[0]["source"], "Modrinth");
    let cf = hits(&result)[0]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|link| link["source"] == "CurseForge")
        .unwrap();
    assert!(cf["project_id"].is_null());
    assert_eq!(result["warnings"].as_array().unwrap().len(), 1);
}
