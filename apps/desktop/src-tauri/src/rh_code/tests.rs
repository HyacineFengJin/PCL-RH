use super::*;
struct Fixture {
    root: PathBuf,
    folder: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../work/harness-2026-10-12/native/store-fixtures")
            .join(id());
        let folder = root.join("source");
        fs::create_dir_all(&folder).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let folder = fs::canonicalize(folder).unwrap();
        let store = Store::new(&root);
        Self {
            root,
            folder,
            store,
        }
    }
    fn snapshot(&self) -> Value {
        self.store.call("bootstrap", json!({})).unwrap()
    }
    fn write(&self, op: &str, mut args: Value) -> Value {
        args["expectedRevision"] = self.snapshot()["revision"].clone();
        self.store.call(op, args).unwrap()
    }
    fn project(&self) -> String {
        self.write(
            "project_create",
            json!({"name":"Fixture","folder":self.folder}),
        )["projects"][0]["id"]
            .as_str()
            .unwrap()
            .into()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn rh_code_cas_across_stores_retains_newer_state() {
    let f = Fixture::new();
    let stale = f.snapshot()["revision"].clone();
    f.project();
    let second = Store::new(&f.root);
    assert!(second
        .call(
            "project_create",
            json!({"name":"Stale","folder":f.folder,"expectedRevision":stale})
        )
        .is_err());
    assert_eq!(f.snapshot()["projects"].as_array().unwrap().len(), 1);
}
#[test]
fn rh_code_unreadable_and_future_state_are_retained() {
    for raw in [
        b"broken".as_slice(),
        b"{\"schemaVersion\":2,\"projects\":[],\"disabledPlugins\":[]}".as_slice(),
    ] {
        let f = Fixture::new();
        fs::create_dir_all(&f.store.root).unwrap();
        let p = f.store.root.join("workspace.json");
        fs::write(&p, raw).unwrap();
        assert!(f.store.call("bootstrap", json!({})).is_err());
        assert_eq!(fs::read(&p).unwrap(), raw);
    }
}
#[cfg(unix)]
#[test]
fn rh_code_dangling_state_and_lock_links_are_not_followed() {
    use std::os::unix::fs::symlink;
    for name in ["workspace.json", "store.lock"] {
        let f = Fixture::new();
        fs::create_dir_all(&f.store.root).unwrap();
        let p = f.store.root.join(name);
        symlink(f.root.join("missing"), &p).unwrap();
        assert!(f.store.call("bootstrap", json!({})).is_err());
        assert!(fs::symlink_metadata(p).unwrap().file_type().is_symlink());
    }
}
#[test]
fn rh_code_dependency_controls_preserve_project_metadata() {
    let f = Fixture::new();
    let id = f.project();
    let rev = f.snapshot()["revision"].clone();
    assert!(f
        .store
        .call(
            "plugin_set",
            json!({"id":"rh.files","enabled":false,"expectedRevision":rev})
        )
        .is_err());
    for plugin in [
        "rh.porting",
        "rh.agent",
        "rh.graph",
        "rh.source",
        "rh.files",
    ] {
        f.write("plugin_set", json!({"id":plugin,"enabled":false}));
    }
    assert!(f.store.call("files", json!({"projectId":id})).is_err());
    assert_eq!(f.snapshot()["projects"][0]["id"], id);
    f.write("plugin_set", json!({"id":"rh.files","enabled":true}));
    assert!(f.store.call("files", json!({"projectId":id})).is_ok());
}
#[test]
fn rh_code_duplicate_area_id_and_foreign_conversation_are_rejected() {
    let f = Fixture::new();
    let project = f.project();
    let leaf = json!({"type":"area","id":"same","view":"chat"});
    let rev = f.snapshot()["revision"].clone();
    assert!(f.store.call("layout_save",json!({"projectId":project,"expectedRevision":rev,"layout":{"type":"split","id":"root","axis":"row","ratio":0.5,"children":[leaf,leaf]}})).is_err());
    assert!(f.store.call("conversation_append",json!({"projectId":project,"conversationId":"missing","expectedRevision":rev,"role":"user","content":"hi"})).is_err());
}
#[test]
fn rh_code_source_conflict_backup_and_permissions() {
    let f = Fixture::new();
    let p = f.folder.join("Main.java");
    fs::write(&p, "original").unwrap();
    let project = f.project();
    let original = hash(b"original");
    fs::write(&p, "external").unwrap();
    assert!(f.store.call("file_save",json!({"projectId":project,"path":"Main.java","sha256":original,"content":"mine","expectedRevision":f.snapshot()["revision"]})).is_err());
    assert_eq!(fs::read_to_string(&p).unwrap(), "external");
    f.write(
        "file_save",
        json!({"projectId":project,"path":"Main.java","sha256":hash(b"external"),"content":"mine"}),
    );
    assert_eq!(fs::read_to_string(&p).unwrap(), "mine");
    let backup = f.store.root.join("source-backups").join(project);
    assert_eq!(
        fs::read(
            fs::read_dir(backup)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path()
        )
        .unwrap(),
        b"external"
    );
}
#[test]
fn rh_code_completed_job_is_idempotent_and_cannot_clear_another_job() {
    let f = Fixture::new();
    let project = f.project();
    let d = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"First"}),
    );
    let conversation = d["projects"][0]["conversations"][0]["id"].clone();
    f.write("conversation_append",json!({"projectId":project,"conversationId":conversation,"role":"user","content":"request","jobId":"a","activeJob":"a","mode":"plan"}));
    assert!(f
        .store
        .call(
            "agent_bound",
            json!({"projectId":project,"conversationId":conversation,"jobId":"b"})
        )
        .is_err());
    assert!(f.store.call("conversation_complete",json!({"projectId":project,"conversationId":conversation,"jobId":"b","content":"wrong","role":"assistant","expectedRevision":f.snapshot()["revision"]})).is_err());
    for _ in 0..2 {
        f.write("conversation_complete",json!({"projectId":project,"conversationId":conversation,"jobId":"a","content":"answer","role":"assistant","activeJob":null}));
    }
    let state = f.snapshot();
    let c = &state["projects"][0]["conversations"][0];
    assert_eq!(c["messages"].as_array().unwrap().len(), 2);
    assert!(c["activeJob"].is_null());
}
#[test]
fn rh_code_native_context_is_owned_and_includes_exact_source() {
    use base64::Engine;
    let f = Fixture::new();
    fs::write(f.folder.join("Main.java"), "source").unwrap();
    fs::write(f.folder.join("binary.dat"), [0xff]).unwrap();
    fs::write(f.folder.join(".private"), "private").unwrap();
    fs::create_dir(f.folder.join("node_modules")).unwrap();
    fs::write(f.folder.join("node_modules/dependency.js"), "dependency").unwrap();
    let project = f.project();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"A"}),
    );
    let id = state["projects"][0]["conversations"][0]["id"].clone();
    let context=f.store.call("agent_context",json!({"projectId":project,"conversationId":id,"request":"plan","mode":"plan","expectedRevision":state["revision"]})).unwrap();
    assert_eq!(context["sourceFiles"].as_array().unwrap().len(), 1);
    assert_eq!(context["sourceFiles"][0]["path"], "Main.java");
    assert_eq!(context["sourceFiles"][0]["sha256"], hash(b"source"));
    assert_eq!(context["sourceFiles"][0]["size"], 6);
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(context["sourceFiles"][0]["data"].as_str().unwrap())
            .unwrap(),
        b"source"
    );
    assert_eq!(
        context["sourceManifest"],
        json!([{"path":"Main.java","sha256":hash(b"source"),"size":6}])
    );
    assert_eq!(
        context["sourceFingerprint"],
        hash(&serde_json::to_vec(&context["sourceManifest"]).unwrap())
    );
    assert_eq!(
        context["sourceOmissions"],
        json!([{"path":"binary.dat","reason":"notText"}])
    );
    assert!(context["rootIdentity"]["device"].is_u64());
    assert!(context["rootIdentity"]["inode"].is_u64());
    assert!(context.get("source").is_none());
    assert!(context["project"].get("folder").is_none());
    assert!(f.store.call("agent_context",json!({"projectId":project,"conversationId":"foreign","request":"plan","mode":"plan","expectedRevision":state["revision"]})).is_err());
}

#[test]
fn rh_code_harness_store_bridge_binds_native_source_and_gates_reviewed_application() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    let f = Fixture::new();
    fs::write(f.folder.join("Main.java"), "frozen source").unwrap();
    let project = f.project();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"Owner"}),
    );
    let conversation = state["projects"][0]["conversations"][0]["id"].clone();
    let captured = f.store.call("agent_context", json!({"projectId":project,"conversationId":conversation,"request":"Change Main.java","mode":"execute","expectedRevision":state["revision"]})).unwrap();
    let job = "harness-owned-job";
    let proposal_id = "b".repeat(32);
    let digest = "c".repeat(64);
    let approval = || json!({"projectId":project,"jobId":job,"proposalId":proposal_id,"expectedDigest":digest,"expectedRevision":f.snapshot()["revision"]});
    // This synthetic receipt is passed only through the private host bridge.
    // The renderer dispatcher below receives no authority from matching bytes.
    let export = json!({"jobId":job,"proposalId":proposal_id,"digest":digest,"projectId":project,"conversationId":conversation,
        "sourceFingerprint":captured["sourceFingerprint"],"files":[{"path":"Main.java","expectedSha256":hash(b"frozen source"),"data":STANDARD.encode(b"reviewed change"),"delete":false}]});
    let public_bind = json!({"projectId":project,"conversationId":conversation,"jobId":job,"sourceFiles":captured["sourceFiles"],"rootIdentity":captured["rootIdentity"]});
    assert!(f
        .store
        .call("harness_bind", public_bind)
        .unwrap_err()
        .contains("captured native provenance"));
    let mut foreign_context = captured.clone();
    foreign_context["conversation"]["id"] = json!("foreign-conversation");
    assert!(f
        .store
        .bind_harness(foreign_context, "foreign-job")
        .is_err());
    f.store.bind_harness(captured.clone(), job).unwrap();

    // A bound native job still needs a persisted conversation membership.
    assert!(f
        .store
        .apply_harness(approval(), export.clone())
        .unwrap_err()
        .contains("does not belong"));
    f.write("conversation_append", json!({"projectId":project,"conversationId":conversation,"role":"user","content":"Change Main.java","jobId":job,"activeJob":job,"mode":"execute"}));
    assert!(f
        .store
        .bind_harness(captured, "other-job")
        .unwrap_err()
        .contains("idle conversation"));
    assert!(f
        .store
        .apply_harness(approval(), export.clone())
        .unwrap_err()
        .contains("finish"));
    f.write("conversation_complete", json!({"projectId":project,"conversationId":conversation,"role":"assistant","content":"Review ready","jobId":job,"activeJob":null}));
    assert!(f
        .store
        .call("harness_apply", approval())
        .unwrap_err()
        .contains("verified private host export"));

    let stale = approval();
    f.write("conversation_draft", json!({"projectId":project,"conversationId":conversation,"draft":"Changes the document revision"}));
    assert!(f
        .store
        .apply_harness(stale, export.clone())
        .unwrap_err()
        .contains("refresh"));
    for key in [
        "jobId",
        "conversationId",
        "projectId",
        "sourceFingerprint",
        "digest",
    ] {
        let mut foreign = export.clone();
        foreign[key] = json!("foreign");
        assert!(f.store.apply_harness(approval(), foreign).is_err(), "{key}");
    }
    let mut foreign_approval = approval();
    foreign_approval["jobId"] = json!("unowned-job");
    assert!(f
        .store
        .apply_harness(foreign_approval, export.clone())
        .unwrap_err()
        .contains("does not belong"));

    // Even a forged history entry in another project cannot redirect the
    // original native binding or its verified host proposal.
    let other_folder = f.root.join("other-source");
    fs::create_dir(&other_folder).unwrap();
    fs::write(other_folder.join("Main.java"), "other project").unwrap();
    let other = f.write(
        "project_create",
        json!({"name":"Other","folder":other_folder}),
    )["projects"][1]["id"]
        .clone();
    let state = f.write(
        "conversation_create",
        json!({"projectId":other,"title":"Foreign"}),
    );
    let foreign_conversation = state["projects"][1]["conversations"][0]["id"].clone();
    f.write("conversation_append", json!({"projectId":other,"conversationId":foreign_conversation,"role":"user","content":"Forged membership","jobId":job}));
    let mut redirected = approval();
    redirected["projectId"] = other;
    assert!(f.store.apply_harness(redirected, export.clone()).is_err());
    assert_eq!(
        fs::read(other_folder.join("Main.java")).unwrap(),
        b"other project"
    );
    for plugin in ["rh.changes", "rh.source"] {
        f.write("plugin_set", json!({"id":plugin,"enabled":false}));
        assert!(f
            .store
            .apply_harness(approval(), export.clone())
            .unwrap_err()
            .contains("Plugin disabled"));
        f.write("plugin_set", json!({"id":plugin,"enabled":true}));
    }
    assert_eq!(
        fs::read(f.folder.join("Main.java")).unwrap(),
        b"frozen source"
    );

    // The Store derives the owner from native history, rather than trusting a
    // renderer-supplied conversation selector.
    let mut owned_approval = approval();
    owned_approval["conversationId"] = json!("renderer-selector");
    let applied = f
        .store
        .apply_harness(owned_approval, export.clone())
        .unwrap();
    assert_eq!(
        applied["projects"][0]["conversations"][0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        fs::read(f.folder.join("Main.java")).unwrap(),
        b"reviewed change"
    );
    let transaction = hash(format!("{job}:{proposal_id}").as_bytes());
    assert_eq!(
        fs::read(
            f.store
                .root
                .join("harness-transactions")
                .join(transaction)
                .join("before-0")
        )
        .unwrap(),
        b"frozen source"
    );
    let reopened = Store::new(&f.root);
    fs::write(f.folder.join("Main.java"), "handwritten after application").unwrap();
    reopened.apply_harness(approval(), export).unwrap();
    assert_eq!(
        fs::read(f.folder.join("Main.java")).unwrap(),
        b"handwritten after application"
    );
}

#[test]
fn rh_code_draft_mode_and_layout_survive_store_reopen() {
    let f = Fixture::new();
    let project = f.project();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"A"}),
    );
    let conversation = state["projects"][0]["conversations"][0]["id"].clone();
    f.write("conversation_draft",json!({"projectId":project,"conversationId":conversation,"draft":"Keep this draft","mode":"ask"}));
    f.write(
        "layout_save",
        json!({"projectId":project,"layout":{"type":"area","id":"saved-area","view":"files"}}),
    );
    let reopened = Store::new(&f.root).call("bootstrap", json!({})).unwrap();
    assert_eq!(reopened["projects"][0]["layout"]["id"], "saved-area");
    assert_eq!(
        reopened["projects"][0]["conversations"][0]["draft"],
        "Keep this draft"
    );
    assert_eq!(reopened["projects"][0]["conversations"][0]["mode"], "ask");
}
#[test]
fn rh_code_source_adoption_requires_owned_host_bytes_and_an_empty_folder() {
    use base64::Engine;
    let f = Fixture::new();
    let project = f.project();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"A"}),
    );
    let conversation = state["projects"][0]["conversations"][0]["id"].clone();
    f.write("conversation_append",json!({"projectId":project,"conversationId":conversation,"role":"assistant","content":"Source ready","jobId":"owned"}));
    let args =
        json!({"projectId":project,"jobId":"owned","expectedRevision":f.snapshot()["revision"]});
    let source = json!({"files":[{"path":"src/Main.java","sha256":hash(b"generated"),"data":base64::engine::general_purpose::STANDARD.encode(b"generated")}]});
    assert!(f.store.call("source_adopt", args.clone()).is_err());
    let mut foreign = args.clone();
    foreign["jobId"] = json!("foreign");
    assert!(f.store.adopt(foreign, source.clone()).is_err());
    f.store.adopt(args.clone(), source.clone()).unwrap();
    assert_eq!(
        fs::read(f.folder.join("src/Main.java")).unwrap(),
        b"generated"
    );
    assert!(f.store.adopt(args, source).is_err());
    assert_eq!(
        fs::read(f.folder.join("src/Main.java")).unwrap(),
        b"generated"
    );
}
#[test]
fn rh_code_unicode_drafts_and_bounded_context_keep_the_canonical_history() {
    let f = Fixture::new();
    let state = f.write(
        "project_create",
        json!({"name":"星".repeat(120),"folder":f.folder}),
    );
    let project = state["projects"][0]["id"].clone();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"讨论"}),
    );
    let conversation = state["projects"][0]["conversations"][0]["id"].clone();
    f.write(
        "conversation_draft",
        json!({"projectId":project,"conversationId":conversation,"draft":"喵".repeat(12000)}),
    );
    for _ in 0..10 {
        f.write("conversation_append", json!({"projectId":project,"conversationId":conversation,"role":"user","content":"喵".repeat(4000)}));
    }
    let state = f.snapshot();
    let ctx = f.store.call("agent_context", json!({"expectedRevision":state["revision"],"projectId":project,"conversationId":conversation,"request":"规划","mode":"plan"})).unwrap();
    assert_eq!(ctx["conversation"]["omittedMessages"], 2);
    assert_eq!(ctx["conversation"]["messages"].as_array().unwrap().len(), 8);
    assert_eq!(
        ctx["conversation"]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1800
    );
    let reopened = Store::new(&f.root).call("bootstrap", json!({})).unwrap();
    assert_eq!(
        reopened["projects"][0]["conversations"][0]["messages"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        reopened["projects"][0]["conversations"][0]["draft"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        12000
    );
}
#[test]
fn rh_code_source_adoption_retains_folder_on_invalid_snapshot() {
    use base64::Engine;
    let f = Fixture::new();
    let p = Project {
        id: id(),
        name: "A".into(),
        folder: f.folder.to_string_lossy().into_owned(),
        updated_at: 0,
        targets: vec![],
        conversations: vec![],
        layout: Value::Null,
    };
    for path in ["../outside", "src/A.java"] {
        let source = json!({"files":[{"path":path,"sha256":"invalid","data":base64::engine::general_purpose::STANDARD.encode(b"bad")}]});
        assert!(files::adopt(&p, &source).is_err());
        assert!(fs::read_dir(&f.folder).unwrap().next().is_none());
    }
}

fn area(view: &str) -> Value {
    json!({"type":"area","id":"a","view":view})
}

#[test]
fn rh_code_old_schema_defaults_presets_and_all_snapshot_responses_include_them() {
    let f = Fixture::new();
    fs::create_dir_all(&f.store.root).unwrap();
    fs::write(
        f.store.root.join("workspace.json"),
        br#"{"schemaVersion":1,"projects":[],"disabledPlugins":[]}"#,
    )
    .unwrap();
    assert_eq!(f.snapshot()["layoutPresets"], json!([]));
    let project = f.project();
    let preset = f.write(
        "layout_preset_save",
        json!({"name":"My layout","layout":area("chat")}),
    );
    let expected = preset["layoutPresets"].clone();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"A"}),
    );
    assert_eq!(state["layoutPresets"], expected);
    let conversation = state["projects"][0]["conversations"][0]["id"].clone();
    f.write("conversation_append",json!({"projectId":project,"conversationId":conversation,"role":"user","content":"request","jobId":"a","activeJob":"a"}));
    for _ in 0..2 {
        let state = f.write("conversation_complete",json!({"projectId":project,"conversationId":conversation,"jobId":"a","content":"done","role":"assistant"}));
        assert_eq!(state["layoutPresets"], expected);
    }
    assert_eq!(
        f.store
            .call("artifact_owner", json!({"projectId":project,"jobId":"a"}))
            .unwrap()["layoutPresets"],
        expected
    );
}

#[test]
fn rh_code_presets_roundtrip_update_delete_and_cas() {
    let f = Fixture::new();
    let state = f.write(
        "layout_preset_save",
        json!({"name":"  First  ","layout":area("vendor.component")}),
    );
    let key = state["layoutPresets"][0]["id"].clone();
    assert_eq!(key.as_str().unwrap().len(), 24);
    assert_eq!(state["layoutPresets"][0]["name"], "First");
    let stale = state["revision"].clone();
    let state = f.write(
        "layout_preset_save",
        json!({"id":key,"name":"Renamed","layout":area("other.missing")}),
    );
    assert_eq!(state["layoutPresets"].as_array().unwrap().len(), 1);
    assert_eq!(state["layoutPresets"][0]["id"], key);
    let reopened = Store::new(&f.root).call("bootstrap", json!({})).unwrap();
    assert_eq!(reopened["layoutPresets"], state["layoutPresets"]);
    let bytes = fs::read(f.store.root.join("workspace.json")).unwrap();
    for args in [
        json!({"id":key,"expectedRevision":stale}),
        json!({"id":"missing","expectedRevision":state["revision"]}),
    ] {
        assert!(f.store.call("layout_preset_delete", args).is_err());
        assert_eq!(
            fs::read(f.store.root.join("workspace.json")).unwrap(),
            bytes
        );
    }
    assert!(f.store.call("layout_preset_save",json!({"id":"missing","name":"No","layout":area("chat"),"expectedRevision":state["revision"]})).is_err());
    assert_eq!(
        f.write("layout_preset_delete", json!({"id":key}))["layoutPresets"],
        json!([])
    );
}

#[test]
fn rh_code_portable_presets_strip_bindings_but_project_state_survives_disabled_views() {
    let f = Fixture::new();
    let project = f.project();
    let layout = json!({"type":"split","id":"root","axis":"row","ratio":0.15,"children":[
        {"type":"area","id":"left","view":"targets","conversation":"old-conversation","file":"src/Main.java",
         "state":{"targets":{"selection":[1,2]},"vendor.missing":{"draft":"preserved"}}},
        {"type":"area","id":"right","view":"vendor.missing"}]});
    f.write("layout_save", json!({"projectId":project,"layout":layout}));
    f.write("plugin_set", json!({"id":"rh.porting","enabled":false}));
    f.write("plugin_set", json!({"id":"rh.targets","enabled":false}));
    assert_eq!(
        Store::new(&f.root).call("bootstrap", json!({})).unwrap()["projects"][0]["layout"],
        layout
    );
    assert!(f.store.call("targets_save",json!({"projectId":project,"targets":[{"minecraft":"1.21.1","loaders":["Fabric"]}],"expectedRevision":f.snapshot()["revision"]})).is_err());
    let saved = f.write(
        "layout_preset_save",
        json!({"name":"Portable","layout":layout}),
    );
    assert_eq!(
        saved["layoutPresets"][0]["layout"],
        json!({"type":"split","id":"root","axis":"row","ratio":0.15,"children":[
        {"type":"area","id":"left","view":"targets"},
        {"type":"area","id":"right","view":"vendor.missing"}]})
    );
    assert_eq!(saved["projects"][0]["layout"], layout);
}

fn balanced_layout(count: usize, next: &mut usize) -> Value {
    *next += 1;
    let key = format!("node-{next}");
    if count == 1 {
        json!({"type":"area","id":key,"view":"chat"})
    } else {
        json!({"type":"split","id":key,"axis":"column","ratio":0.85,"children":[
            balanced_layout(count / 2,next),balanced_layout(count - count / 2,next)]})
    }
}

#[test]
fn rh_code_tree_limits_types_and_binding_bounds_are_authoritative() {
    assert!(checked_layout(&Value::Null).is_ok());
    assert!(portable_layout(&Value::Null).is_err());
    assert!(checked_layout(&balanced_layout(12, &mut 0)).is_ok());
    assert!(checked_layout(&balanced_layout(13, &mut 0)).is_err());
    let mut deep = area("chat");
    for index in 0..8 {
        deep = json!({"type":"split","id":format!("split-{index}"),"axis":"row","ratio":0.5,
            "children":[deep,{"type":"area","id":format!("leaf-{index}"),"view":"chat"}]});
        assert_eq!(checked_layout(&deep).is_ok(), index < 7);
    }
    for (field, value) in [
        ("view", json!("../plugin")),
        ("view", json!("x".repeat(121))),
        ("id", json!("bad id")),
        ("conversation", json!(12)),
        ("conversation", json!("x".repeat(101))),
        ("file", json!(null)),
        ("file", json!("   ")),
        ("file", json!("../private")),
        ("file", json!(".hidden/file")),
        ("file", json!("C:/outside")),
        ("file", json!("src\\file")),
        ("file", json!("a".repeat(4097))),
        ("extra", json!("untrusted")),
        ("children", json!([])),
    ] {
        let mut tree = area("chat");
        tree[field] = value;
        assert!(checked_layout(&tree).is_err(), "accepted invalid {field}");
    }
    for ratio in [0.149, 0.851] {
        let mut tree = balanced_layout(2, &mut 0);
        tree["ratio"] = json!(ratio);
        assert!(checked_layout(&tree).is_err());
    }
    let mut tree = balanced_layout(2, &mut 0);
    tree["children"][0] = Value::Null;
    assert!(checked_layout(&tree).is_err());
}

#[test]
fn rh_code_state_validation_runs_before_portable_stripping_and_preserves_previous_bytes() {
    let f = Fixture::new();
    let project = f.project();
    let original = fs::read(f.store.root.join("workspace.json")).unwrap();
    let mut deep = json!({});
    for _ in 0..9 {
        deep = json!({"nested":deep});
    }
    for state in [
        json!([]),
        json!({"chat":[]}),
        json!({"chat":null}),
        json!({"../bad":{}}),
        json!({"chat":{"__proto__":{}}}),
        json!({"chat":{"constructor":{}}}),
        json!({"chat":{"prototype":{}}}),
        json!({"chat":{"bad key":1}}),
        json!({"chat":{"text":"x".repeat(65536)}}),
        json!({"chat":deep}),
    ] {
        let mut tree = area("chat");
        tree["state"] = state;
        for operation in ["layout_save", "layout_preset_save"] {
            assert!(f.store.call(operation,json!({"projectId":project,"name":"Unsafe","layout":tree,"expectedRevision":f.snapshot()["revision"]})).is_err());
            assert_eq!(
                fs::read(f.store.root.join("workspace.json")).unwrap(),
                original
            );
        }
    }
    assert!(f
        .store
        .call(
            "layout_save",
            json!({"projectId":project,"expectedRevision":f.snapshot()["revision"]})
        )
        .is_err());
    let mut tree = balanced_layout(12, &mut 0);
    fn add_state(node: &mut Value) {
        if node["type"] == "area" {
            node["state"] = json!({"chat":{"text":"x".repeat(24000)}});
        } else {
            for child in node["children"].as_array_mut().unwrap() {
                add_state(child);
            }
        }
    }
    add_state(&mut tree);
    assert!(checked_layout(&tree).is_err());
}

#[test]
fn rh_code_preset_count_names_and_corrupt_portable_state_are_bounded() {
    let f = Fixture::new();
    for index in 0..32 {
        f.write(
            "layout_preset_save",
            json!({"name":format!("Preset{index}"),"layout":area("chat")}),
        );
    }
    let state = f.snapshot();
    for name in [
        "More".to_owned(),
        " ".into(),
        "x".repeat(81),
        "line\nbreak".into(),
    ] {
        assert!(f
            .store
            .call(
                "layout_preset_save",
                json!({"name":name,"layout":area("chat"),"expectedRevision":state["revision"]})
            )
            .is_err());
    }
    // Updating an existing preset remains possible at the count limit.
    let key = state["layoutPresets"][0]["id"].clone();
    for name in [" ".to_owned(), "x".repeat(81), "line\nbreak".into()] {
        assert!(f.store.call("layout_preset_save",json!({"id":key,"name":name,"layout":area("chat"),"expectedRevision":state["revision"]})).is_err());
    }
    f.write(
        "layout_preset_save",
        json!({"id":key,"name":"星".repeat(80),"layout":area("chat")}),
    );
    let path = f.store.root.join("workspace.json");
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    stored["layoutPresets"][0]["layout"]["conversation"] = json!("project-private");
    let bytes = serde_json::to_vec(&stored).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(f.store.call("bootstrap", json!({})).is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn rh_code_state_exact_byte_and_depth_boundaries_accept_safe_data() {
    let mut tree = area("chat");
    let overhead = serde_json::to_vec(&json!({"chat":{"text":""}}))
        .unwrap()
        .len();
    tree["state"] = json!({"chat":{"text":"x".repeat(65536 - overhead)}});
    assert_eq!(serde_json::to_vec(&tree["state"]).unwrap().len(), 65536);
    assert!(checked_layout(&tree).is_ok());
    tree["state"]["chat"]["text"] = json!("x".repeat(65537 - overhead));
    assert!(checked_layout(&tree).is_err());
    let mut state = json!({});
    for _ in 0..7 {
        state = json!({"nested":state});
    }
    tree["state"] = json!({"chat":state});
    assert!(checked_layout(&tree).is_ok());
    tree["state"]["chat"] = json!({"nested":tree["state"]["chat"]});
    assert!(checked_layout(&tree).is_err());
}
