use super::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    thread,
    time::Instant,
};

fn project_value() -> Value {
    json!({
        "id": 123, "gameId": 432, "name": "Example Resource", "slug": "example-resource",
        "links": {"websiteUrl":"https://www.curseforge.com/minecraft/mc-mods/example-resource"},
        "summary": "Example summary", "downloadCount": 42,
        "categories":[{"slug":"technology"}], "classId":6,
        "logo":{"url":"https://media.forgecdn.net/avatars/example.png"},
        "latestFilesIndexes":[{"gameVersion":"1.21.1", "modLoader":4}],
        "dateModified":"2026-10-01T00:00:00Z", "allowModDistribution":true,"isAvailable":true,
    })
}

fn file_value(id: u32) -> Value {
    json!({
        "id": id, "gameId":432, "modId":123,"isAvailable":true,
        "displayName":format!("Example {id}"),"fileName":format!("example-{id}.jar"),
        "releaseType":1,"fileDate":"2026-10-01T00:00:00Z","fileLength":1024,
        "downloadCount":7,"downloadUrl":format!("https://edge.forgecdn.net/files/1/{id}/example.jar"),
        "hashes":[{"algo":1,"value":"0123456789abcdef0123456789abcdef01234567"}],
        "gameVersions":["1.21.1","Fabric"],
        "dependencies":[{"modId":456,"relationType":3}],
    })
}

fn project(value: Value) -> Project {
    serde_json::from_value(value).unwrap()
}
fn file(value: Value) -> File {
    serde_json::from_value(value).unwrap()
}
fn page(data: Vec<Value>, index: usize, total: usize) -> Value {
    json!({"pagination":{"index":index,"pageSize":50,"resultCount":data.len(),"totalCount":total},"data":data})
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn json(value: Value) -> Self {
        Self {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
}

/// A private fixture listener with bounded lifetime. Requests use a synthetic
/// credential and cannot contact either the real API or a download CDN.
struct Fixture {
    address: String,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Fixture {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let join = thread::spawn(move || {
            for reply in replies {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    if stopped.load(Ordering::Acquire) || Instant::now() > deadline {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2))
                        }
                        Err(_) => return,
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0; 2048];
                while request.len() < 16384 && !request.windows(4).any(|value| value == b"\r\n\r\n")
                {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => return,
                        Ok(count) => request.extend_from_slice(&chunk[..count]),
                    }
                }
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(request).unwrap());
                let mut response =
                    format!("HTTP/1.1 {} Fixture\r\nConnection: close\r\n", reply.status);
                if !reply.headers.iter().any(|(name, _)| {
                    name.eq_ignore_ascii_case("content-length")
                        || name.eq_ignore_ascii_case("transfer-encoding")
                }) {
                    response.push_str(&format!("Content-Length: {}\r\n", reply.body.len()));
                }
                for (name, value) in reply.headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(&reply.body);
            }
        });
        Self {
            address,
            requests,
            stop,
            join: Some(join),
        }
    }

    fn client(&self) -> CurseforgeClient {
        let network = Arc::new(
            pcl_network::ClientFactory::new(pcl_network::Policy {
                proxy: pcl_network::ProxyPolicy::None,
                dns: pcl_network::DnsPolicy::System,
            })
            .unwrap(),
        );
        CurseforgeClient {
            transport: Transport::loopback(network, self.address.clone()),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
    }
}

#[test]
fn ids_cannot_change_the_api_endpoint() {
    assert_eq!(numeric_id("123").unwrap(), 123);
    for id in [
        "",
        "0",
        "01",
        "+1",
        "-1",
        " 1",
        "1/2",
        "1?key=x",
        "4294967296",
    ] {
        assert!(numeric_id(id).is_err(), "{id}");
    }
}

#[test]
fn search_rejects_unsupported_filters_and_honors_loader_contract() {
    for kind in [
        ProjectKind::Mod,
        ProjectKind::Modpack,
        ProjectKind::Resourcepack,
        ProjectKind::Shader,
    ] {
        let query = search_query("", kind, None, None, None, None).unwrap();
        assert!(query.contains(&("classId", class_id(kind).unwrap().to_string())));
        assert!(query.contains(&("sortField", "6".into())));
    }
    assert!(search_query("", ProjectKind::Datapack, None, None, None, None).is_err());
    for (version, loader, sort, tag) in [
        (None, Some("fabric"), None, None),
        (Some("1.21.1"), Some("unknown"), None, None),
        (None, None, Some("follows"), None),
        (None, None, None, Some("technology")),
        (Some("1.21&evil=x"), None, None, None),
    ] {
        assert!(search_query("", ProjectKind::Mod, version, loader, sort, tag).is_err());
    }
    assert!(search_query("bad\nquery", ProjectKind::Mod, None, None, None, None).is_err());
    let query = search_query(
        "example",
        ProjectKind::Mod,
        Some("1.21.1"),
        Some("neoforge"),
        Some("updated"),
        None,
    )
    .unwrap();
    assert!(query.contains(&("modLoaderType", "6".into())));
    assert!(query.contains(&("gameVersion", "1.21.1".into())));
    assert!(query.contains(&("sortField", "3".into())));
    assert!(
        !search_query("example", ProjectKind::Mod, None, None, None, None)
            .unwrap()
            .iter()
            .any(|(name, _)| *name == "sortField")
    );
}

#[test]
fn project_normalization_preserves_ids_and_official_canonical_url() {
    let value = normalize_project(&project(project_value()), true).unwrap();
    assert_eq!(value["project_id"], "123");
    assert_eq!(value["source"], "CurseForge");
    assert_eq!(
        value["url"],
        "https://www.curseforge.com/minecraft/mc-mods/example-resource"
    );
    assert_eq!(value["versions"], json!(["1.21.1"]));
    assert_eq!(value["loaders"], json!(["fabric"]));
    assert_eq!(value["downloads"], 42);
    for (field, replacement) in [
        ("id", json!(0)),
        ("gameId", json!(1)),
        ("slug", json!("../example")),
        ("classId", json!(999)),
    ] {
        let mut input = project_value();
        input[field] = replacement;
        assert!(normalize_project(&project(input), true).is_err());
    }
    let mut input = project_value();
    input["links"]["websiteUrl"] =
        json!("https://www.curseforge.com/minecraft/mc-mods/another-project");
    assert!(normalize_project(&project(input), true).is_err());
}

#[test]
fn unsupported_icon_authorities_are_discarded() {
    for url in [
        "https://media.forgecdn.net.evil.org/a.png",
        "http://media.forgecdn.net/a.png",
        "https://user@media.forgecdn.net/a.png",
        "https://media.forgecdn.net:444/a.png",
    ] {
        let mut input = project_value();
        input["logo"]["url"] = json!(url);
        assert!(normalize_project(&project(input), false).unwrap()["icon_url"].is_null());
    }
}

#[test]
fn details_preserve_file_and_dependency_types_without_download_urls_or_hashes() {
    let value = normalize_version(&file(file_value(1))).unwrap();
    assert_eq!(value["id"], "1");
    assert_eq!(
        value["files"][0],
        json!({"filename":"example-1.jar","size":1024,"primary":true})
    );
    assert_eq!(value["game_versions"], json!(["1.21.1"]));
    assert_eq!(value["loaders"], json!(["fabric"]));
    assert_eq!(value["dependencies"][0]["dependency_type"], "required");
    let serialized = value.to_string();
    assert!(!serialized.contains("forgecdn.net"));
    assert!(!serialized.contains("0123456789abcdef"));
}

#[test]
fn download_requires_exact_project_file_and_filename() {
    let owner = project(project_value());
    let input = file_value(1);
    assert!(mapping::download_metadata(&owner, &file(input.clone()), 1, "example-1.jar").is_ok());
    assert!(mapping::download_metadata(&owner, &file(input.clone()), 2, "example-1.jar").is_err());
    assert!(mapping::download_metadata(&owner, &file(input), 1, "another.jar").is_err());
    for (field, value) in [
        ("modId", json!(999)),
        ("gameId", json!(1)),
        ("isAvailable", json!(false)),
        ("isEarlyAccessContent", json!(true)),
    ] {
        let mut input = file_value(1);
        input[field] = value;
        assert!(mapping::download_metadata(&owner, &file(input), 1, "example-1.jar").is_err());
    }
}

#[test]
fn authors_distribution_opt_out_is_enforced_and_null_still_requires_a_real_url() {
    let mut input = project_value();
    input["allowModDistribution"] = json!(false);
    assert!(
        mapping::download_metadata(&project(input), &file(file_value(1)), 1, "example-1.jar")
            .is_err()
    );
    let mut input = project_value();
    input["allowModDistribution"] = Value::Null;
    let owner = project(input);
    assert!(mapping::download_metadata(&owner, &file(file_value(1)), 1, "example-1.jar").is_ok());
    let mut input = file_value(1);
    input["downloadUrl"] = Value::Null;
    assert!(mapping::download_metadata(&owner, &file(input), 1, "example-1.jar").is_err());
}

#[test]
fn file_downloads_need_an_official_https_cdn_url_and_a_unique_sha1() {
    let owner = project(project_value());
    for url in [
        "https://edge.forgecdn.net.evil.org/files/a.jar",
        "http://edge.forgecdn.net/files/a.jar",
        "https://user@edge.forgecdn.net/files/a.jar",
        "https://127.0.0.1/files/a.jar",
        "https://edge.forgecdn.net/other/a.jar",
    ] {
        let mut input = file_value(1);
        input["downloadUrl"] = json!(url);
        assert!(mapping::download_metadata(&owner, &file(input), 1, "example-1.jar").is_err());
    }
    for hashes in [
        json!([]),
        json!([{"algo":2,"value":"0123456789abcdef0123456789abcdef"}]),
        json!([{"algo":1,"value":"bad"}]),
        json!([{"algo":1,"value":"0123456789abcdef0123456789abcdef01234567"},{"algo":1,"value":"0123456789abcdef0123456789abcdef01234567"}]),
    ] {
        let mut input = file_value(1);
        input["hashes"] = hashes;
        assert!(mapping::download_metadata(&owner, &file(input), 1, "example-1.jar").is_err());
    }
}

#[test]
fn filenames_cannot_be_paths_or_special_platform_names() {
    for name in [
        "", "../a.jar", "a/b.jar", "a\\b.jar", "C:a.jar", "x\n.jar", "CON.jar", "com1.jar",
        "name.", "..",
    ] {
        assert!(mapping::check_filename(name).is_err(), "{name}");
    }
    assert!(mapping::check_filename("example mod (1.21.1).jar").is_ok());
    assert!(mapping::check_filename(&format!("example.{}", "a".repeat(65))).is_err());
}

#[test]
fn native_download_url_and_size_guards_match_metadata_admission() {
    assert!(download_url("https://edge.forgecdn.net/files/1/1/example.jar").is_ok());
    assert!(download_url("https://edge.forgecdn.net.evil.org/files/1/1/example.jar").is_err());
    let owner = project(project_value());
    let mut input = file_value(1);
    input["fileLength"] = json!(2_u64 * 1024 * 1024 * 1024);
    assert!(mapping::download_metadata(&owner, &file(input.clone()), 1, "example-1.jar").is_ok());
    input["fileLength"] = json!(2_u64 * 1024 * 1024 * 1024 + 1);
    assert!(mapping::download_metadata(&owner, &file(input), 1, "example-1.jar").is_err());
}

#[test]
fn invalid_pagination_cannot_claim_complete_results() {
    for pagination in [
        json!({"index":1,"pageSize":50,"resultCount":0,"totalCount":0}),
        json!({"index":0,"pageSize":50,"resultCount":0,"totalCount":1}),
        json!({"index":0,"pageSize":50,"resultCount":1,"totalCount":1}),
        json!({"index":0,"pageSize":100,"resultCount":0,"totalCount":0}),
    ] {
        let response: Page<Value> =
            serde_json::from_value(json!({"data":[],"pagination":pagination})).unwrap();
        assert!(response.validate(0, 50).is_err());
    }
}

#[test]
fn exact_url_lookup_requires_canonical_official_project_pages_and_explicit_slug() {
    for (kind, route) in [
        (ProjectKind::Mod, "mc-mods"),
        (ProjectKind::Modpack, "modpacks"),
        (ProjectKind::Resourcepack, "texture-packs"),
        (ProjectKind::Shader, "shaders"),
    ] {
        let url = format!("https://www.curseforge.com/minecraft/{route}/example-resource");
        let query = project_url_query(&url, kind).unwrap();
        assert!(query.contains(&("slug", "example-resource".into())));
        assert!(query.contains(&("classId", class_id(kind).unwrap().to_string())));
        assert!(query.contains(&("pageSize", "2".into())));
        assert!(!query.iter().any(|(name, _)| *name == "searchFilter"));
    }
    for url in [
        "http://www.curseforge.com/minecraft/mc-mods/example-resource",
        "https://curseforge.com/minecraft/mc-mods/example-resource",
        "https://www.curseforge.com.evil.org/minecraft/mc-mods/example-resource",
        "https://user@www.curseforge.com/minecraft/mc-mods/example-resource",
        "https://www.curseforge.com/minecraft/mc-mods/example-resource?source=x",
        "https://www.curseforge.com/minecraft/mc-mods/example-resource#details",
        "https://www.curseforge.com/minecraft/mc-mods/example-resource/",
        "https://www.curseforge.com/minecraft/mc-mods/example-resource/files/123",
        "https://www.curseforge.com/minecraft/modpacks/example-resource",
    ] {
        assert!(project_url_query(url, ProjectKind::Mod).is_err(), "{url}");
    }
}

#[tokio::test]
async fn exact_url_lookup_resolves_one_official_id_in_one_slug_request() {
    let mut response = page(vec![project_value()], 0, 1);
    response["pagination"]["pageSize"] = json!(2);
    let fixture = Fixture::new(vec![Reply::json(response)]);
    let url = "https://www.curseforge.com/minecraft/mc-mods/example-resource";
    let hit = fixture
        .client()
        .project_by_url(url, ProjectKind::Mod)
        .await
        .unwrap();
    assert_eq!(hit["project_id"], "123");
    assert_eq!(hit["url"], url);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let request = requests[0].lines().next().unwrap();
    assert!(request.starts_with("GET /v1/mods/search?"));
    assert!(request.contains("gameId=432"));
    assert!(request.contains("classId=6"));
    assert!(request.contains("slug=example-resource"));
    assert!(!request.contains("searchFilter"));
}

#[tokio::test]
async fn exact_url_lookup_rejects_ambiguous_or_different_official_projects() {
    let mut other_project = project_value();
    other_project["slug"] = json!("another-resource");
    other_project["links"]["websiteUrl"] =
        json!("https://www.curseforge.com/minecraft/mc-mods/another-resource");
    let mut wrong_kind = project_value();
    wrong_kind["classId"] = json!(12);
    wrong_kind["links"]["websiteUrl"] =
        json!("https://www.curseforge.com/minecraft/texture-packs/example-resource");
    for (hits, total) in [
        (vec![], 0),
        (vec![project_value(), project_value()], 2),
        (vec![project_value()], 2),
        (vec![other_project], 1),
        (vec![wrong_kind], 1),
    ] {
        let mut response = page(hits, 0, total);
        response["pagination"]["pageSize"] = json!(2);
        let fixture = Fixture::new(vec![Reply::json(response)]);
        assert!(fixture
            .client()
            .project_by_url(
                "https://www.curseforge.com/minecraft/mc-mods/example-resource",
                ProjectKind::Mod
            )
            .await
            .is_err());
        assert_eq!(fixture.requests().len(), 1);
    }
}

#[tokio::test]
async fn search_sends_only_a_sensitive_header_and_explicit_official_filters() {
    let fixture = Fixture::new(vec![Reply::json(page(vec![project_value()], 0, 1))]);
    let hits = fixture
        .client()
        .search(
            "example &key=no",
            ProjectKind::Mod,
            Some("1.21.1"),
            Some("fabric"),
            Some("downloads"),
            None,
        )
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let first = requests[0].lines().next().unwrap();
    assert!(first.starts_with("GET /v1/mods/search?"));
    assert!(first.contains("gameId=432"));
    assert!(first.contains("classId=6"));
    assert!(first.contains("gameVersion=1.21.1"));
    assert!(first.contains("modLoaderType=4"));
    assert!(first.contains("sortField=6"));
    assert!(!first.contains("synthetic-test-key"));
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("x-api-key: synthetic-test-key"));
    assert!(requests[0]
        .to_ascii_lowercase()
        .contains("accept-encoding: identity"));
    assert!(!hits[0].to_string().contains("synthetic-test-key"));
}

#[tokio::test]
async fn details_page_files_without_claiming_more_than_was_returned() {
    let fixture = Fixture::new(vec![
        Reply::json(json!({"data":project_value()})),
        Reply::json(page((1..=50).map(file_value).collect(), 0, 51)),
        Reply::json(page(vec![file_value(51)], 50, 51)),
    ]);
    let details = fixture.client().details("123").await.unwrap();
    assert_eq!(details["versions_total"], 51);
    assert_eq!(details["versions_returned"], 51);
    assert_eq!(details["versions_truncated"], false);
    assert_eq!(fixture.requests().len(), 3);
}

#[tokio::test]
async fn details_have_a_finite_request_limit_and_explicit_truncation() {
    let mut replies = vec![Reply::json(json!({"data":project_value()}))];
    for index in [0, 50, 100, 150] {
        replies.push(Reply::json(page(
            ((index + 1)..=(index + 50))
                .map(|id| file_value(id as u32))
                .collect(),
            index,
            201,
        )));
    }
    let fixture = Fixture::new(replies);
    let details = fixture.client().details("123").await.unwrap();
    assert_eq!(details["versions_total"], 201);
    assert_eq!(details["versions_returned"], 200);
    assert_eq!(details["versions_limit"], 200);
    assert_eq!(details["versions_truncated"], true);
    assert_eq!(fixture.requests().len(), 5);
}

#[tokio::test]
async fn unrelated_and_duplicate_files_fail_the_whole_details_reply() {
    let mut unrelated = file_value(1);
    unrelated["modId"] = json!(999);
    for files in [vec![unrelated], vec![file_value(1), file_value(1)]] {
        let count = files.len();
        let fixture = Fixture::new(vec![
            Reply::json(json!({"data":project_value()})),
            Reply::json(page(files, 0, count)),
        ]);
        assert!(fixture.client().details("123").await.is_err());
    }
}

#[tokio::test]
async fn an_exact_project_lookup_rejects_a_response_for_another_id_before_reading_files() {
    let mut input = project_value();
    input["id"] = json!(999);
    let fixture = Fixture::new(vec![Reply::json(json!({"data": input}))]);
    assert_eq!(
        fixture.client().details("123").await.unwrap_err(),
        "CurseForge 项目 ID 或游戏归属不匹配"
    );
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn changed_pagination_is_reported_instead_of_duplicating_or_omitting_versions() {
    let fixture = Fixture::new(vec![
        Reply::json(json!({"data":project_value()})),
        Reply::json(page((1..=50).map(file_value).collect(), 0, 51)),
        Reply::json(page(vec![file_value(51)], 50, 52)),
    ]);
    assert!(fixture
        .client()
        .details("123")
        .await
        .unwrap_err()
        .contains("分页期间发生变化"));
}

#[tokio::test]
async fn download_metadata_uses_two_api_reads_and_never_fetches_the_cdn() {
    let fixture = Fixture::new(vec![
        Reply::json(json!({"data":project_value()})),
        Reply::json(json!({"data":file_value(1)})),
    ]);
    let download = fixture
        .client()
        .download("123", "1", "example-1.jar")
        .await
        .unwrap();
    assert_eq!(download.sha1, "0123456789abcdef0123456789abcdef01234567");
    assert_eq!(download.title, "Example Resource");
    assert_eq!(download.version, "Example 1");
    assert_eq!(download.name, "example-1.jar");
    assert_eq!(download.size, 1024);
    assert_eq!(
        download.url,
        "https://edge.forgecdn.net/files/1/1/example.jar"
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /v1/mods/123 "));
    assert!(requests[1].starts_with("GET /v1/mods/123/files/1 "));
}

#[tokio::test]
async fn download_stops_before_file_lookup_when_author_disallows_distribution() {
    let mut input = project_value();
    input["allowModDistribution"] = json!(false);
    let fixture = Fixture::new(vec![Reply::json(json!({"data":input}))]);
    assert!(fixture
        .client()
        .download("123", "1", "example-1.jar")
        .await
        .is_err());
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn redirect_is_rejected_without_sending_the_key_to_the_destination() {
    let fixture = Fixture::new(vec![Reply {
        status: 302,
        headers: vec![("Location".into(), "https://example.org/receive-key".into())],
        body: Vec::new(),
    }]);
    let error = fixture.client().details("123").await.unwrap_err();
    assert_eq!(error, "CurseForge 请求失败：HTTP 302");
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn provider_errors_and_invalid_json_cannot_echo_credentials() {
    for reply in [
        Reply {
            status: 403,
            headers: vec![],
            body: b"synthetic-test-key".to_vec(),
        },
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: b"synthetic-test-key".to_vec(),
        },
    ] {
        let fixture = Fixture::new(vec![reply]);
        let error = fixture.client().details("123").await.unwrap_err();
        assert!(!error.contains("synthetic-test-key"));
    }
}

#[tokio::test]
async fn oversized_and_non_json_responses_are_rejected() {
    for headers in [
        vec![
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), "2097153".into()),
        ],
        vec![("Content-Type".into(), "text/html".into())],
    ] {
        let fixture = Fixture::new(vec![Reply {
            status: 200,
            headers,
            body: Vec::new(),
        }]);
        assert!(fixture.client().details("123").await.is_err());
    }
}

#[tokio::test]
async fn exhausted_request_budget_never_performs_a_request() {
    let fixture = Fixture::new(vec![]);
    let client = fixture.client();
    let mut budget = Budget::new(0);
    let result: Result<Value, String> = client.transport.get("mods/123", &[], &mut budget).await;
    assert_eq!(result.unwrap_err(), "CurseForge 请求次数超过上限");
    assert!(fixture.requests().is_empty());
}

#[tokio::test]
async fn streaming_response_cannot_bypass_the_byte_limit_by_omitting_content_length() {
    let payload = vec![b' '; 2 * 1024 * 1024 + 1];
    let mut body = format!("{:x}\r\n", payload.len()).into_bytes();
    body.extend(payload);
    body.extend_from_slice(b"\r\n0\r\n\r\n");
    let fixture = Fixture::new(vec![Reply {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "application/json".into()),
            ("Transfer-Encoding".into(), "chunked".into()),
        ],
        body,
    }]);
    assert_eq!(
        fixture.client().details("123").await.unwrap_err(),
        "CurseForge 响应超过大小上限"
    );
}

#[tokio::test]
async fn compressed_api_responses_are_rejected_before_parsing() {
    let mut reply = Reply::json(json!({"data": project_value()}));
    reply
        .headers
        .push(("Content-Encoding".into(), "gzip".into()));
    let fixture = Fixture::new(vec![reply]);
    assert_eq!(
        fixture.client().details("123").await.unwrap_err(),
        "CurseForge 返回了不支持的内容编码"
    );
}

#[tokio::test]
async fn search_page_sends_provider_offset_and_uses_total_even_for_a_short_page() {
    let fixture = Fixture::new(vec![
        Reply::json(page(vec![project_value()], 50, 101)),
        Reply::json(page(vec![project_value()], 100, 101)),
    ]);
    let first = fixture
        .client()
        .search_page("", ProjectKind::Mod, None, None, None, None, 50)
        .await
        .unwrap();
    assert_eq!(first.hits.len(), 1);
    assert!(first.has_more);
    let last = fixture
        .client()
        .search_page("", ProjectKind::Mod, None, None, None, None, 100)
        .await
        .unwrap();
    assert!(!last.has_more);
    let requests = fixture.requests();
    assert!(requests[0].lines().next().unwrap().contains("index=50"));
    assert!(requests[1].lines().next().unwrap().contains("index=100"));
}

#[tokio::test]
async fn search_page_rejects_wrong_server_offset_and_unsupported_offsets() {
    let fixture = Fixture::new(vec![Reply::json(page(vec![project_value()], 0, 100))]);
    assert!(fixture
        .client()
        .search_page("", ProjectKind::Mod, None, None, None, None, 50)
        .await
        .is_err());
    assert_eq!(fixture.requests().len(), 1);
    for index in [1, 10_000, usize::MAX] {
        assert!(fixture
            .client()
            .search_page("", ProjectKind::Mod, None, None, None, None, index)
            .await
            .is_err());
    }
    assert_eq!(fixture.requests().len(), 1);
}

#[tokio::test]
async fn search_page_reports_the_official_ten_thousand_result_boundary() {
    let fixture = Fixture::new(vec![Reply::json(page(vec![project_value()], 9950, 20_000))]);
    let result = fixture
        .client()
        .search_page("", ProjectKind::Mod, None, None, None, None, 9950)
        .await
        .unwrap();
    assert!(result.limited);
    assert!(!result.has_more);
    assert!(fixture.requests()[0]
        .lines()
        .next()
        .unwrap()
        .contains("index=9950"));
}
