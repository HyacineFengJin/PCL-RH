use super::*;
use crate::plugin_host::{Definition, Registration, Scope};
use contracts::{ANNOUNCEMENTS, FAILURE_ANALYSIS};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
struct Fixture {
    dir: PathBuf,
    runtime: Arc<Runtime>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
impl Fixture {
    fn new() -> Self {
        let base =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../work/tests/product-plugins");
        fs::create_dir_all(&base).unwrap();
        let dir = base.join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        let runtime = Arc::new(Runtime::new(&dir));
        Self { dir, runtime }
    }
    fn configuration(&self) -> Value {
        self.runtime
            .call("plugin_configuration", json!({}))
            .unwrap()
    }
    fn set(&self, id: &str, enabled: bool) -> Result<Value, String> {
        self.runtime.call(
            "plugin_control",
            json!({"id":id,"enabled":enabled,"expectedRevision":self.configuration()["revision"]}),
        )
    }
    fn write(&self, value: Value) -> Vec<u8> {
        fs::create_dir_all(&self.runtime.root).unwrap();
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(self.runtime.root.join("workspace.json"), &bytes).unwrap();
        bytes
    }
}
#[test]
fn launcher_services_and_configuration_are_cold_and_read_only() {
    let f = Fixture::new();
    let c = f.configuration();
    assert_eq!(c["revision"], "new");
    assert!(c.get("projects").is_none());
    let _call = f.runtime.service(FAILURE_ANALYSIS).unwrap();
    assert!(!f.runtime.root.exists());
    let fixture = crate::integration_tests::Fixture::new();
    let shared = fixture.shared();
    assert!(shared.code.get().is_none());
    shared.product_plugins.service(FAILURE_ANALYSIS).unwrap();
    assert!(shared.code.get().is_none());
    let c = shared
        .product_plugins
        .call("plugin_configuration", json!({}))
        .unwrap();
    shared
        .product_plugins
        .call(
            "plugin_control",
            json!({"id":crate::plugins::home::ID,"enabled":false,"expectedRevision":c["revision"]}),
        )
        .unwrap();
    assert!(shared.code.get().is_none());
}
#[test]
fn real_service_disable_reenable_and_reopen_share_one_control_plane() {
    let f = Fixture::new();
    let id = crate::plugins::diagnostics::ID;
    let admitted = f.runtime.service(FAILURE_ANALYSIS).unwrap();
    f.set(id, false).unwrap();
    assert!(f.runtime.service(FAILURE_ANALYSIS).is_err());
    assert!(Runtime::new(&f.dir).service(FAILURE_ANALYSIS).is_err());
    assert!(!admitted
        .0
        .analyze("java.lang.OutOfMemoryError", None)
        .is_empty());
    assert!(f.runtime.call("native_plugin", json!({"id":id})).is_err());
    f.set(id, true).unwrap();
    assert!(Runtime::new(&f.dir).service(FAILURE_ANALYSIS).is_ok());
}
#[test]
fn corrupt_or_future_controls_are_preserved_and_retire_saved_admission() {
    for raw in [
        json!(null),
        json!({"schemaVersion":2,"disabledPlugins":[]}),
        json!({"schemaVersion":1}),
        json!({"schemaVersion":1,"disabledPlugins":["unknown.plugin"]}),
        json!({"schemaVersion":1,"disabledPlugins":["launcher.home.announcements","launcher.home.announcements"]}),
    ] {
        let f = Fixture::new();
        f.runtime.service(FAILURE_ANALYSIS).unwrap();
        let saved = f
            .runtime
            .native_plugins
            .resolve(FAILURE_ANALYSIS)
            .unwrap()
            .unwrap();
        let bytes = f.write(raw);
        assert!(f.runtime.service(ANNOUNCEMENTS).is_err());
        assert!(saved.begin().is_err());
        assert_eq!(
            fs::read(f.runtime.root.join("workspace.json")).unwrap(),
            bytes
        );
    }
}
#[test]
fn controls_preserve_unreadable_project_data_without_bootstrapping_it() {
    let f = Fixture::new();
    let raw = json!({"schemaVersion":1,"disabledPlugins":[],"projects":"retained invalid metadata","layoutPresets":[{"unknown":"retained"}]});
    f.write(raw.clone());
    assert!(f.runtime.service(ANNOUNCEMENTS).is_ok());
    f.set(crate::plugins::home::ID, false).unwrap();
    let after: Value =
        serde_json::from_slice(&fs::read(f.runtime.root.join("workspace.json")).unwrap()).unwrap();
    assert_eq!(after["projects"], raw["projects"]);
    assert_eq!(after["layoutPresets"], raw["layoutPresets"]);
    assert!(f.runtime.optional_service(ANNOUNCEMENTS).unwrap().is_none());
    assert!(crate::rh_code::Store::with_runtime(f.runtime.clone())
        .call("bootstrap", json!({}))
        .is_err());
}
#[test]
fn legacy_code_writers_and_product_writers_obey_the_same_disk_cas() {
    let f = Fixture::new();
    let code = crate::rh_code::Store::with_runtime(f.runtime.clone());
    let old = code.call("bootstrap", json!({})).unwrap();
    f.set(crate::plugins::home::ID, false).unwrap();
    assert!(code
        .call(
            "plugin_set",
            json!({"expectedRevision":old["revision"],"id":crate::plugins::home::ID,"enabled":true})
        )
        .is_err());
    let current = code.call("bootstrap", json!({})).unwrap();
    code.call("plugin_set",json!({"expectedRevision":current["revision"],"id":crate::plugins::home::ID,"enabled":true})).unwrap();
    assert!(f.runtime.service(ANNOUNCEMENTS).is_ok());
    let stale = f.configuration();
    let second = Runtime::new(&f.dir);
    second.call("plugin_control",json!({"expectedRevision":stale["revision"],"id":crate::plugins::home::ID,"enabled":false})).unwrap();
    assert!(f.runtime.call("plugin_control",json!({"expectedRevision":stale["revision"],"id":crate::plugins::diagnostics::ID,"enabled":false})).is_err());
    let lease = document_io::workspace_lease(f.runtime.root()).unwrap();
    assert!(f.set(crate::plugins::home::ID, true).is_err());
    drop(lease);
}
#[test]
fn active_agent_and_dependency_guards_apply_to_both_control_adapters() {
    let f = Fixture::new();
    let bytes=f.write(json!({"schemaVersion":1,"disabledPlugins":[],"projects":[{"conversations":[{"activeJob":"fixture-job"}]}]}));
    assert!(f.set("rh.agent", false).is_err());
    assert_eq!(
        fs::read(f.runtime.root.join("workspace.json")).unwrap(),
        bytes
    );
    assert!(f.set("rh.project", false).is_err());
    assert!(f.set("rh.files", false).is_err());
}
struct ScopedService(Arc<AtomicBool>);
struct Resource(Arc<AtomicBool>);
impl Drop for Resource {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
const SCOPED: Contract<ScopedService> = Contract::new("fixture.code", 1);
fn scoped(reg: &mut Registration<'_>) -> Result<(), String> {
    let flag = Arc::new(AtomicBool::new(true));
    reg.hold(Resource(flag.clone()));
    reg.provide(SCOPED, ScopedService(flag))
}
fn scoped_runtime(f: &mut Fixture) {
    Arc::get_mut(&mut f.runtime).unwrap().native_plugins = Host::new(vec![Definition {
        id: "rh.project",
        scope: Scope::Code,
        dependencies: vec![],
        services: vec![SCOPED.declaration()],
        start: scoped,
    }])
    .unwrap();
}
#[test]
fn independent_scope_owners_retire_only_their_own_code_services() {
    let mut f = Fixture::new();
    scoped_runtime(&mut f);
    assert!(f.runtime.optional_service(SCOPED).unwrap().is_none());
    let a = f
        .runtime
        .call("plugin_runtime", json!({"action":"open"}))
        .unwrap();
    let b = f
        .runtime
        .call("plugin_runtime", json!({"action":"open"}))
        .unwrap();
    let call = f.runtime.service(SCOPED).unwrap();
    let flag = call.0.clone();
    f.runtime
        .call(
            "plugin_runtime",
            json!({"action":"close","lease":a["lease"]}),
        )
        .unwrap();
    f.runtime.service(SCOPED).unwrap();
    // Late duplicate cleanup cannot retire the other live renderer owner.
    f.runtime
        .call(
            "plugin_runtime",
            json!({"action":"close","lease":a["lease"]}),
        )
        .unwrap();
    f.runtime.service(SCOPED).unwrap();
    f.runtime
        .call(
            "plugin_runtime",
            json!({"action":"close","lease":b["lease"]}),
        )
        .unwrap();
    assert!(f.runtime.service(SCOPED).is_err());
    assert!(flag.load(Ordering::SeqCst));
    drop(call);
    assert!(!flag.load(Ordering::SeqCst));
    assert!(!f.runtime.root.exists());
}
#[test]
fn corrupt_configuration_cannot_prevent_scope_cleanup() {
    let mut f = Fixture::new();
    scoped_runtime(&mut f);
    let lease = f
        .runtime
        .call("plugin_runtime", json!({"action":"open"}))
        .unwrap();
    let call = f.runtime.service(SCOPED).unwrap();
    let flag = call.0.clone();
    drop(call);
    f.write(json!({"schemaVersion":2,"disabledPlugins":[]}));
    assert!(f
        .runtime
        .call(
            "plugin_runtime",
            json!({"action":"close","lease":lease["lease"]})
        )
        .is_err());
    assert!(!flag.load(Ordering::SeqCst));
    assert!(f.runtime.scopes.lock().unwrap().code_leases.is_empty());
}
struct Alternate;
impl contracts::FailureAnalyzer for Alternate {
    fn analyze(&self, _: &str, _: Option<&str>) -> Vec<contracts::FailureFinding> {
        vec![contracts::FailureFinding {
            code: "java_memory".into(),
            evidence: vec!["replacement implementation".into()],
        }]
    }
}
fn alternate(reg: &mut Registration<'_>) -> Result<(), String> {
    reg.provide(
        FAILURE_ANALYSIS,
        contracts::AnalysisService(Arc::new(Alternate)),
    )
}
#[test]
fn actual_diagnostic_consumer_resolves_another_plugin_id_by_contract() {
    let fixture = crate::integration_tests::Fixture::new();
    let mut shared = fixture.shared();
    Arc::get_mut(&mut shared.product_plugins)
        .unwrap()
        .native_plugins = Host::new(vec![Definition {
        id: "fixture.alternate-provider",
        scope: Scope::Launcher,
        dependencies: vec![],
        services: vec![FAILURE_ANALYSIS.declaration()],
        start: alternate,
    }])
    .unwrap();
    let root = shared.config.resolve(None).unwrap();
    let run = crate::RunStatus {
        stage: "error".into(),
        message: "synthetic failure".into(),
        version: Some("fixture".into()),
        exit_code: Some(1),
        root_id: Some(root.id.clone()),
        root_path: Some(root.path.clone()),
        ..Default::default()
    };
    *shared.status.lock().unwrap() = run.clone();
    crate::launcher_monitor_runtime::set_failure_log_start(&shared, Some(0));
    let report = crate::launcher_failure::report(&shared, &root.id, &run).unwrap();
    assert_eq!(report.findings[0].evidence, ["replacement implementation"]);
    assert!(shared.code.get().is_none());
}
#[test]
fn disabling_analysis_keeps_log_report_and_export_receipt_available() {
    let fixture = crate::integration_tests::Fixture::new();
    let shared = fixture.shared();
    let root = shared.config.resolve(None).unwrap();
    let run = crate::RunStatus {
        stage: "error".into(),
        message: "java.lang.OutOfMemoryError".into(),
        version: Some("fixture".into()),
        exit_code: Some(1),
        root_id: Some(root.id.clone()),
        root_path: Some(root.path.clone()),
        ..Default::default()
    };
    *shared.status.lock().unwrap() = run.clone();
    crate::launcher_monitor_runtime::set_failure_log_start(&shared, Some(0));
    let runtime = &shared.product_plugins;
    let c = runtime.call("plugin_configuration", json!({})).unwrap();
    runtime.call("plugin_control",json!({"id":crate::plugins::diagnostics::ID,"enabled":false,"expectedRevision":c["revision"]})).unwrap();
    let report = crate::launcher_failure::report(&shared, &root.id, &run).unwrap();
    assert!(report.findings.is_empty());
    let prepared =
        crate::launcher_failure::prepare(&shared, &root.id, &run, &report.content_revision)
            .unwrap();
    assert!(prepared.text().contains("java.lang.OutOfMemoryError"));
    let c = runtime.call("plugin_configuration", json!({})).unwrap();
    runtime.call("plugin_control",json!({"id":crate::plugins::diagnostics::ID,"enabled":true,"expectedRevision":c["revision"]})).unwrap();
    assert!(
        crate::launcher_failure::prepare(&shared, &root.id, &run, &report.content_revision)
            .is_err()
    );
    assert!(shared.code.get().is_none());
}

#[test]
fn project_mutations_and_legacy_snapshots_remain_compatible_with_product_controls() {
    let f = Fixture::new();
    let code = crate::rh_code::Store::with_runtime(f.runtime.clone());
    let stale = f.configuration();
    let folder = f.dir.join("source");
    fs::create_dir(&folder).unwrap();
    let created = code
        .call(
            "project_create",
            json!({"name":"Fixture","folder":folder,"expectedRevision":stale["revision"]}),
        )
        .unwrap();
    assert!(f.runtime.call("plugin_control", json!({"id":crate::plugins::home::ID,"enabled":false,"expectedRevision":stale["revision"]})).is_err());
    code.call("conversation_create", json!({"projectId":created["projects"][0]["id"],"title":"Fixture conversation","expectedRevision":created["revision"]})).unwrap();
    let mut old: Value =
        serde_json::from_slice(&fs::read(f.runtime.root.join("workspace.json")).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("layoutPresets");
    let p = old["projects"][0].as_object_mut().unwrap();
    p.remove("instructions");
    p.remove("buildPolicy");
    let c = p["conversations"][0].as_object_mut().unwrap();
    c.remove("draft");
    c.remove("contextNote");
    c.remove("contextPaths");
    f.write(old);
    let current = f.configuration();
    let result=code.call("plugin_set",json!({"id":crate::plugins::home::ID,"enabled":false,"expectedRevision":current["revision"]})).unwrap();
    assert_eq!(result["projects"][0]["id"], created["projects"][0]["id"]);
    assert_eq!(result["projects"][0]["instructions"], "");
    assert_eq!(result["projects"][0]["conversations"][0]["draft"], "");
    assert_eq!(result["layoutPresets"], json!([]));
    let before = fs::read(f.runtime.root.join("workspace.json")).unwrap();
    assert!(code
        .call(
            "project_create",
            json!({"name":"Stale","folder":folder,"expectedRevision":current["revision"]})
        )
        .is_err());
    assert_eq!(
        fs::read(f.runtime.root.join("workspace.json")).unwrap(),
        before
    );
}
