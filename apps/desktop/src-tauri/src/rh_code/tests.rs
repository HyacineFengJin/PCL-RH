use super::*;
struct Fixture {
    root: PathBuf,
    folder: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pcl-rh-code-{}", id()));
        let folder = root.join("source");
        fs::create_dir_all(&folder).unwrap();
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
    let f = Fixture::new();
    fs::write(f.folder.join("Main.java"), "source").unwrap();
    let project = f.project();
    let state = f.write(
        "conversation_create",
        json!({"projectId":project,"title":"A"}),
    );
    let id = state["projects"][0]["conversations"][0]["id"].clone();
    let context=f.store.call("agent_context",json!({"projectId":project,"conversationId":id,"request":"plan","mode":"plan","expectedRevision":state["revision"]})).unwrap();
    assert_eq!(context["source"]["Main.java"], "source");
    assert_eq!(context["sourceFiles"][0]["sha256"], hash(b"source"));
    assert!(f.store.call("agent_context",json!({"projectId":project,"conversationId":"foreign","request":"plan","mode":"plan","expectedRevision":state["revision"]})).is_err());
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
