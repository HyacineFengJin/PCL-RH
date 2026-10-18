use super::*;

#[test]
fn exact_four_domains_and_strict_uri_syntax_share_preview_authority() {
    for host in [
        "cdn.modrinth.com",
        "github.com",
        "raw.githubusercontent.com",
        "gitlab.com",
    ] {
        assert!(
            declaration(&format!("https://{host}:443/file%20name?key=value"))
                .unwrap()
                .is_some()
        );
    }
    for valid_unsupported in [
        "https://github.com.evil.test/file",
        "https://github.com./file",
        "https://objects.githubusercontent.com/file",
        "https://cdn.modrinth.com:444/file",
        "https://127.0.0.1/file",
        "https://localhost/file",
    ] {
        assert!(declaration(valid_unsupported).unwrap().is_none());
    }
    for invalid in [
        "http://cdn.modrinth.com/file",
        "https:cdn.modrinth.com/file",
        "https://@github.com/file",
        "https://user:pass@github.com/file",
        "https://github.com/file#part",
        "https://github.com/file%",
        "https://github.com/file%ZZ",
        "https://github.com/file name",
        "https://github.com/中文",
        "https://github.com/file\\name",
    ] {
        assert!(declaration(invalid).is_err(), "{invalid}");
    }
    assert!(declaration(&format!("https://github.com/{}", "x".repeat(4096))).is_err());
}

#[test]
fn redirect_chain_resolves_relative_urls_and_checks_raw_authorities() {
    let start = declaration("https://cdn.modrinth.com/root/start?key=secret")
        .unwrap()
        .unwrap();
    let mut chain = Chain::new(start.clone());
    chain.follow("../artifact?key=other").unwrap();
    assert_eq!(
        chain.current().as_str(),
        "https://cdn.modrinth.com/artifact?key=other"
    );
    chain
        .follow("https://raw.githubusercontent.com/repo/artifact")
        .unwrap();
    assert_eq!(
        chain.current().host_str(),
        Some("raw.githubusercontent.com")
    );
    for bad in [
        "http://github.com/file?secret=keep",
        "//@github.com/file?secret=keep",
        "https://@github.com/file?secret=keep",
        "https://user@github.com/file?secret=keep",
        "https://github.com:444/file?secret=keep",
        "//evil.test/file?secret=keep",
        "#secret",
        "https:github.com/file",
        "/file%ZZ?secret=keep",
    ] {
        let error = Chain::new(start.clone()).follow(bad).unwrap_err();
        assert!(!error.contains("secret"));
        assert!(!error.contains("keep"));
    }
}

#[test]
fn redirect_cycles_and_sixth_hop_are_bounded() {
    let start = declaration("https://github.com/start").unwrap().unwrap();
    let mut chain = Chain::new(start.clone());
    chain.follow("/second").unwrap();
    assert!(chain.follow("/start").unwrap_err().contains("循环"));
    let mut chain = Chain::new(start);
    for i in 1..=5 {
        chain.follow(&format!("/hop{i}")).unwrap();
    }
    assert!(chain.follow("/hop6").unwrap_err().contains("5 次"));
}

#[test]
fn local_manifests_allow_only_exact_official_forge_file_hosts_without_changing_standard_inputs() {
    for host in ["mediafilez.forgecdn.net", "edge.forgecdn.net"] {
        let url = format!("https://{host}/files/1/2/example.jar");
        assert!(declaration(&url).unwrap().is_none());
        assert!(local_declaration(&url).unwrap().is_some());
    }
    for url in [
        "https://edge.forgecdn.net.evil.test/files/1/2/a.jar",
        "https://media.forgecdn.net/files/1/2/a.jar",
        "https://edge.forgecdn.net/avatars/a.jar",
        "https://edge.forgecdn.net:444/files/1/2/a.jar",
        "https://edge.forgecdn.net/files/../avatars/a.jar",
    ] {
        assert!(local_declaration(url).unwrap().is_none(), "{url}");
    }
    for url in [
        "http://edge.forgecdn.net/files/a.jar",
        "https://user@edge.forgecdn.net/files/a.jar",
        "https://@edge.forgecdn.net/files/a.jar",
        "https://edge.forgecdn.net/files/a.jar#fragment",
    ] {
        assert!(local_declaration(url).is_err(), "{url}");
    }
    let start = local_declaration("https://edge.forgecdn.net/files/1/2/a.jar")
        .unwrap()
        .unwrap();
    let mut local = Chain::local(start.clone());
    local
        .follow("https://mediafilez.forgecdn.net/files/1/2/b.jar")
        .unwrap();
    assert!(local
        .follow("https://mediafilez.forgecdn.net.evil.test/files/c.jar")
        .is_err());
    assert!(Chain::new(start)
        .follow("https://edge.forgecdn.net/files/1/2/b.jar")
        .is_err());
}
