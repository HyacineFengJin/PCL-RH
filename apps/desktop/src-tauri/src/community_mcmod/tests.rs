use super::*;

fn search(html: &str) -> Result<McmodSearchPage, String> {
    parse_search_html(
        html,
        "https://search.mcmod.cn/s?key=fixture",
        1,
        ProjectKind::Mod,
        false,
    )
}

fn detail(html: &str) -> Result<McmodDetail, String> {
    parse_detail_html(
        html,
        ProjectKey::new(CommunitySource::Mcmod, ProjectKind::Mod, "123").unwrap(),
        "https://www.mcmod.cn/class/123.html",
    )
}

fn wrapped(url: &str) -> String {
    format!(
        "//link.mcmod.cn/target/{}",
        base64::engine::general_purpose::STANDARD.encode(url)
    )
}

#[test]
fn search_reads_only_result_titles_and_plain_summaries() {
    let parsed = search(
        r#"
      <a href="https://www.mcmod.cn/class/999.html">Navigation</a>
      <div class="search-result"><div class="search-result-list">
      <div class="result-item"><div class="head">
        <div><a href="https://www.mcmod.cn/class/888.html">Category</a></div>
        <a href="https://www.mcmod.cn/class/123.html"><em>Fixture</em> &amp; Mod</a>
      </div><div class="body">A <b>plain</b> summary.</div>
      <div class="foot"><a href="https://www.mcmod.cn/class/123.html">Duplicate URL</a></div></div>
      </div><ul class="pagination"><li><a data-page="2">Next</a></li></ul></div>
    "#,
    )
    .unwrap();
    assert_eq!(parsed.hits.len(), 1);
    assert_eq!(parsed.hits[0].project.id, "123");
    assert_eq!(parsed.hits[0].title, "Fixture & Mod");
    assert_eq!(parsed.hits[0].summary, "A plain summary.");
    assert!(parsed.hits[0].identity().aliases.is_empty());
    assert!(parsed.has_more);
}

#[test]
fn browse_keeps_modpack_namespace_and_safe_cover_url() {
    let html = r#"<div class="modlist-list-frame"><div class="modlist-block">
      <div class="title"><p class="name"><a href="/modpack/123.html">Fixture Pack</a></p></div>
      <div class="intro-content">A short introduction.</div>
      <div class="cover"><img src="//i.mcmod.cn/modpack/cover/fixture.png"></div>
      <a href="/class/999.html">Unrelated item</a></div></div>"#;
    let parsed = parse_search_html(
        html,
        "https://www.mcmod.cn/modpack.html",
        1,
        ProjectKind::Modpack,
        true,
    )
    .unwrap();
    assert_eq!(parsed.hits[0].project.kind, ProjectKind::Modpack);
    assert_eq!(
        parsed.hits[0].icon_url.as_deref(),
        Some("https://i.mcmod.cn/modpack/cover/fixture.png")
    );
    assert!(!parsed.has_more);
}

#[test]
fn valid_empty_results_are_distinct_from_unrecognized_or_challenge_pages() {
    assert!(search(
        r#"<div class="search-result"><p class="warning">没有找到与查询有关的内容。</p></div>"#
    )
    .unwrap()
    .hits
    .is_empty());
    for html in [
        "<title>Access denied</title>",
        "<div class='search-result'>Challenge</div>",
        "<html>参数错误</html>",
    ] {
        assert!(search(html).is_err());
    }
}

#[test]
fn invalid_or_multiple_project_titles_refuse_the_page() {
    for anchors in [
        "<a href='https://www.mcmod.cn.evil.invalid/class/123.html'>Fake</a>",
        "<a href='/item/123.html'>Item</a>",
        "<a href='/class/123.html'>First</a><a href='/class/124.html'>Second</a>",
    ] {
        let html = format!("<div class='search-result'><div class='search-result-list'><div class='result-item'><div class='head'>{anchors}</div></div></div></div>");
        assert!(search(&html).is_err());
    }
}

#[test]
fn duplicate_result_ids_collapse_without_counting_footer_links() {
    let item = "<div class='result-item'><div class='head'><a href='/class/123.html'>Fixture</a></div></div>";
    let html = format!(
        "<div class='search-result'><div class='search-result-list'>{item}{item}</div></div>"
    );
    assert_eq!(search(&html).unwrap().hits.len(), 1);
}

#[test]
fn detail_uses_related_links_only_and_decodes_without_a_redirect_request() {
    let cf = wrapped("https://www.curseforge.com/minecraft/mc-mods/fixture");
    let mr = wrapped("https://modrinth.com/mod/fixture");
    let html = format!(
        r#"
      <meta name="description" content="Fixture description">
      <div class="class-title"><h3>中文 &amp; 名称</h3><h4>Fixture Mod</h4></div>
      <div class="class-info"><div class="common-link-frame"><a href="{cf}">CF</a><a href="{mr}">MR</a></div></div>
      <div class="description"><a href="https://modrinth.com/mod/unrelated">Recommended</a></div>
    "#
    );
    let parsed = detail(&html).unwrap();
    assert_eq!(parsed.title, "中文 & 名称");
    assert_eq!(parsed.secondary_title.as_deref(), Some("Fixture Mod"));
    assert_eq!(parsed.aliases.len(), 2);
    assert_eq!(parsed.summary, "Fixture description");
    assert_eq!(parsed.aliases[0].evidence_url, parsed.url);
    let identity = parsed.identity();
    assert_eq!(
        crate::community_identity::merge_identities(&[identity])
            .unwrap()
            .groups,
        vec![vec![0]]
    );
}

#[test]
fn related_link_target_validation_rejects_other_sites_and_file_routes() {
    for raw in [
        wrapped("https://modrinth.com.evil.invalid/mod/fixture"),
        wrapped("https://www.curseforge.com/minecraft/mc-mods/fixture/files/123"),
        wrapped("http://127.0.0.1/mod/fixture"),
        wrapped("javascript:alert(1)"),
        wrapped("https://user@modrinth.com/mod/fixture"),
        "//link.mcmod.cn/target/named-alias".into(),
        "//link.mcmod.cn.evil.invalid/target/aHR0cHM6Ly9tb2RyaW50aC5jb20vbW9kL2ZpeHR1cmU=".into(),
        "https://www.mcmod.cn/class/999.html".into(),
    ] {
        assert!(related_project_url(&raw).is_none(), "{raw}");
    }
    assert_eq!(
        related_project_url("https://modrinth.com/mod/fixture#versions").as_deref(),
        Some("https://modrinth.com/mod/fixture")
    );
}

#[test]
fn broad_entry_preserves_all_distinct_related_projects_for_conflict_detection() {
    let first = wrapped("https://www.curseforge.com/minecraft/mc-mods/fixture-forge");
    let second = wrapped("https://www.curseforge.com/minecraft/mc-mods/fixture-fabric");
    let html = format!("<div class='class-title'><h3>Fixture</h3></div><div class='class-info'><div class='common-link-frame'><a href='{first}'>Forge</a><a href='{second}'>Fabric</a></div></div>");
    let parsed = detail(&html).unwrap();
    assert_eq!(parsed.aliases.len(), 2);
    let merged = crate::community_identity::merge_identities(&[parsed.identity()]).unwrap();
    assert_eq!(merged.conflicts.len(), 1);
}

#[test]
fn search_urls_encode_query_as_data_and_validate_kind_and_bounds() {
    let url = search_url("a&filter=7", 2, ProjectKind::Modpack).unwrap();
    let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
    assert_eq!(pairs.get("key").unwrap(), "a&filter=7");
    assert_eq!(pairs.get("filter").unwrap(), "2");
    assert_eq!(pairs.get("page").unwrap(), "2");
    assert_eq!(
        search_url(" ", 1, ProjectKind::Mod).unwrap().path(),
        "/modlist.html"
    );
    assert!(search_url("fixture", 0, ProjectKind::Mod).is_err());
    assert!(search_url("fixture", 101, ProjectKind::Mod).is_err());
    assert!(search_url("fixture", 1, ProjectKind::Shader).is_err());
    assert!(search_url(&"界".repeat(256), 1, ProjectKind::Mod).is_err());
    assert!(search_url("a\n", 1, ProjectKind::Mod).is_err());
}

#[test]
fn response_and_result_count_limits_are_explicit_errors() {
    assert!(search(&"x".repeat(MAX_RESPONSE_BYTES + 1)).is_err());
    let item = "<div class='result-item'><div class='head'><a href='/class/123.html'>Fixture</a></div></div>";
    assert!(search(&format!(
        "<div class='search-result'><div class='search-result-list'>{}</div></div>",
        item.repeat(MAX_HITS + 1)
    ))
    .is_err());
    assert!(detail("<div class='class-title'><h3>A</h3><h3>B</h3></div>").is_err());
}

#[test]
fn image_urls_are_restricted_to_public_mcmod_image_hosts() {
    for bad in [
        "http://127.0.0.1/icon.png",
        "https://user@i.mcmod.cn/a.png",
        "https://i.mcmod.cn.evil.invalid/a.png",
        "data:image/png;base64,AAAA",
    ] {
        assert!(image_url(bad).is_none());
    }
}

/// Synthetic HTTP only. The listener and accepted socket both have deadlines,
/// and each worker is joined so failures do not leak a fixture server.
fn http_fixture(response: Vec<u8>) -> (Url, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(socket) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "fixture was never requested"
                    );
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
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
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0 && request.len() + count <= 8192);
            request.extend_from_slice(&buffer[..count]);
        }
        // A client may reject a large declared body before consuming it.
        let _ = stream.write_all(&response);
    });
    (
        Url::parse(&format!("http://{address}/fixture")).unwrap(),
        worker,
    )
}

fn fixture_client() -> McmodClient {
    McmodClient {
        client: Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn server_errors_redirects_and_non_html_responses_remain_explicit() {
    for (response, expected) in [
        (b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), "HTTP 403"),
        (b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/unrequested\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), "HTTP 302"),
        (b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(), "不是 HTML"),
        (b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff".to_vec(), "UTF-8"),
    ] {
        let (url, worker) = http_fixture(response);
        let result = fixture_client().read_html(url).await;
        worker.join().unwrap();
        assert!(result.unwrap_err().contains(expected), "{expected}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn declared_and_streaming_response_sizes_are_bounded() {
    let declared = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_RESPONSE_BYTES + 1).into_bytes();
    let mut streamed =
        b"HTTP/1.0 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n".to_vec();
    streamed.resize(streamed.len() + MAX_RESPONSE_BYTES + 1, b'x');
    for response in [declared, streamed] {
        let (url, worker) = http_fixture(response);
        let result = fixture_client().read_html(url).await;
        worker.join().unwrap();
        assert!(result.unwrap_err().contains("大小上限"));
    }
}
