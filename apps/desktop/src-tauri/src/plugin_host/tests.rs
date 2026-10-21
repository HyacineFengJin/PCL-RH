use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
const NUMBER: Contract<usize> = Contract::new("sample.number", 1);
const NUMBER_V2: Contract<usize> = Contract::new("sample.number", 2);
fn value(registration: &mut Registration<'_>) -> Result<(), String> {
    registration.provide(NUMBER, 42)
}
fn definition(id: &'static str) -> Definition {
    Definition {
        id,
        scope: Scope::Launcher,
        dependencies: vec![],
        services: vec![NUMBER.declaration()],
        start: value,
    }
}

#[test]
fn definitions_are_inert_and_interfaces_are_versioned_and_typed() {
    let host = Host::new(vec![definition("one")]).unwrap();
    assert_eq!(host.statuses().unwrap()[0].state, State::Dormant);
    assert!(host.service("one", NUMBER).is_err());
    host.configure(&[], true, false).unwrap();
    assert_eq!(*host.service("one", NUMBER).unwrap().begin().unwrap(), 42);
    assert!(host.service("one", NUMBER_V2).is_err());
    assert!(host
        .service("one", Contract::<String>::new("sample.number", 1))
        .is_err());
}

#[test]
fn retirement_revokes_saved_handles_but_admitted_calls_complete() {
    let host = Host::new(vec![definition("one")]).unwrap();
    host.configure(&[], true, false).unwrap();
    let old = host.service("one", NUMBER).unwrap();
    let admitted = old.begin().unwrap();
    host.configure(&["one".into()], true, false).unwrap();
    assert!(old.begin().is_err());
    assert_eq!(*admitted, 42);
    host.configure(&[], true, false).unwrap();
    assert!(old.begin().is_err());
    assert_eq!(*host.service("one", NUMBER).unwrap().begin().unwrap(), 42);
}

#[test]
fn cached_handles_do_not_retain_plugin_resources() {
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct ResourceOwner;
    impl Drop for ResourceOwner {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn start(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.hold(ResourceOwner);
        registration.provide(NUMBER, 9)
    }
    let mut definition = definition("resources");
    definition.start = start;
    let host = Host::new(vec![definition]).unwrap();
    host.configure(&[], true, false).unwrap();
    let handle = host.service("resources", NUMBER).unwrap();
    host.configure(&["resources".into()], true, false).unwrap();
    assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    assert!(handle.begin().is_err());
}

#[test]
fn scopes_and_transitive_dependencies_control_actual_service_availability() {
    let first = definition("first");
    let mut second = definition("second");
    second.dependencies.push("first");
    second.scope = Scope::Both;
    let mut code = definition("code");
    code.scope = Scope::Code;
    let host = Host::new(vec![second, code, first]).unwrap();
    host.configure(&[], true, false).unwrap();
    assert!(host.service("second", NUMBER).is_ok());
    assert!(host.service("code", NUMBER).is_err());
    host.configure(&["first".into()], true, false).unwrap();
    assert!(host.service("second", NUMBER).is_err());
    assert_eq!(
        host.statuses()
            .unwrap()
            .iter()
            .find(|status| status.id == "second")
            .unwrap()
            .state,
        State::Blocked
    );
    host.configure(&[], true, true).unwrap();
    assert!(host.service("code", NUMBER).is_ok());
    host.configure(&[], true, false).unwrap();
    assert!(host.service("code", NUMBER).is_err());
}

#[test]
fn partial_failed_registration_is_not_published_and_other_plugins_remain_usable() {
    fn fail(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.provide(NUMBER, 1)?;
        Err("synthetic factory failure".into())
    }
    let mut failed = definition("failed");
    failed.start = fail;
    let mut dependent = definition("dependent");
    dependent.dependencies.push("failed");
    let host = Host::new(vec![failed, dependent, definition("unrelated")]).unwrap();
    host.configure(&[], true, false).unwrap();
    assert!(host.service("failed", NUMBER).is_err());
    assert!(host.service("dependent", NUMBER).is_err());
    assert!(host.service("unrelated", NUMBER).is_ok());
}

#[test]
fn undeclared_duplicate_and_wrong_type_services_cannot_publish() {
    fn undeclared(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.provide(NUMBER_V2, 2)
    }
    fn duplicate(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.provide(NUMBER, 1)?;
        registration.provide(NUMBER, 2)
    }
    fn wrong(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.provide(Contract::<String>::new("sample.number", 1), "wrong".into())
    }
    for start in [
        undeclared as fn(&mut Registration<'_>) -> Result<(), String>,
        duplicate,
        wrong,
    ] {
        let mut d = definition("invalid");
        d.start = start;
        let host = Host::new(vec![d]).unwrap();
        host.configure(&[], true, false).unwrap();
        assert!(host.service("invalid", NUMBER).is_err());
        assert_eq!(host.statuses().unwrap()[0].state, State::Failed);
    }
}

#[test]
fn required_service_omission_and_dependency_cycles_are_visible() {
    fn empty(_: &mut Registration<'_>) -> Result<(), String> {
        Ok(())
    }
    let mut missing = definition("missing");
    missing.start = empty;
    let mut a = definition("a");
    a.dependencies.push("b");
    let mut b = definition("b");
    b.dependencies.push("a");
    let host = Host::new(vec![missing, a, b]).unwrap();
    host.configure(&[], true, false).unwrap();
    assert_eq!(
        host.statuses()
            .unwrap()
            .iter()
            .find(|status| status.id == "missing")
            .unwrap()
            .state,
        State::Failed
    );
    assert_eq!(
        host.statuses()
            .unwrap()
            .iter()
            .find(|status| status.id == "a")
            .unwrap()
            .state,
        State::Blocked
    );
}

#[test]
fn distinct_providers_can_implement_the_same_contract_without_replacing_each_other() {
    fn alternate(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.provide(NUMBER, 99)
    }
    let mut other = definition("alternate");
    other.start = alternate;
    let host = Host::new(vec![definition("original"), other]).unwrap();
    host.configure(&[], true, false).unwrap();
    assert_eq!(
        *host.service("original", NUMBER).unwrap().begin().unwrap(),
        42
    );
    assert_eq!(
        *host.service("alternate", NUMBER).unwrap().begin().unwrap(),
        99
    );
    host.configure(&["original".into()], true, false).unwrap();
    assert_eq!(
        *host.service("alternate", NUMBER).unwrap().begin().unwrap(),
        99
    );
}

#[test]
fn dropping_the_host_retires_admission_without_destroying_admitted_calls() {
    let host = Host::new(vec![definition("one")]).unwrap();
    host.configure(&[], true, false).unwrap();
    let handle = host.service("one", NUMBER).unwrap();
    let call = handle.begin().unwrap();
    drop(host);
    assert!(handle.begin().is_err());
    assert_eq!(*call, 42);
}

#[test]
fn factories_consume_declared_dependencies_and_admitted_consumers_pin_providers() {
    static PROVIDER_DROPS: AtomicUsize = AtomicUsize::new(0);
    struct ProviderResource;
    impl Drop for ProviderResource {
        fn drop(&mut self) {
            PROVIDER_DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn provider(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.hold(ProviderResource);
        registration.provide(NUMBER, 41)
    }
    fn consumer(registration: &mut Registration<'_>) -> Result<(), String> {
        assert!(registration.require("undeclared", NUMBER).is_err());
        let dependency = registration.require("provider", NUMBER)?.begin()?;
        registration.provide(NUMBER, *dependency + 1)
    }
    let mut base = definition("provider");
    base.start = provider;
    let mut dependent = definition("consumer");
    dependent.start = consumer;
    dependent.dependencies.push("provider");
    // Deliberately opposite dependency order.
    let host = Host::new(vec![dependent, base]).unwrap();
    host.configure(&[], true, false).unwrap();
    let call = host.service("consumer", NUMBER).unwrap().begin().unwrap();
    assert_eq!(*call, 42);
    host.configure(&["provider".into()], true, false).unwrap();
    assert!(host.service("consumer", NUMBER).is_err());
    assert_eq!(PROVIDER_DROPS.load(Ordering::SeqCst), 0);
    drop(call);
    assert_eq!(PROVIDER_DROPS.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_factories_release_resources_and_can_retry_without_replacing_other_instances() {
    static STARTS: AtomicUsize = AtomicUsize::new(0);
    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct PartialResource;
    impl Drop for PartialResource {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn retry(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.hold(PartialResource);
        if STARTS.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("synthetic factory panic");
        }
        registration.provide(NUMBER, 7)
    }
    let mut unstable = definition("unstable");
    unstable.start = retry;
    let host = Host::new(vec![unstable, definition("stable")]).unwrap();
    host.configure(&[], true, false).unwrap();
    assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    let stable = host.service("stable", NUMBER).unwrap();
    host.configure(&[], true, false).unwrap();
    assert_eq!(*stable.begin().unwrap(), 42);
    assert_eq!(
        *host.service("unstable", NUMBER).unwrap().begin().unwrap(),
        7
    );
}

#[test]
fn last_admitted_call_drops_the_service_before_its_resource_owners() {
    static ORDER: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    struct Service;
    struct Owner;
    impl Drop for Service {
        fn drop(&mut self) {
            ORDER.lock().unwrap().push("service");
        }
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            ORDER.lock().unwrap().push("resource");
        }
    }
    const SERVICE: Contract<Service> = Contract::new("sample.owned", 1);
    fn start(registration: &mut Registration<'_>) -> Result<(), String> {
        registration.hold(Owner);
        registration.provide(SERVICE, Service)
    }
    let host = Host::new(vec![Definition {
        id: "owned",
        scope: Scope::Launcher,
        dependencies: vec![],
        services: vec![SERVICE.declaration()],
        start,
    }])
    .unwrap();
    host.configure(&[], true, false).unwrap();
    let call = host.service("owned", SERVICE).unwrap().begin().unwrap();
    host.configure(&["owned".into()], true, false).unwrap();
    assert!(ORDER.lock().unwrap().is_empty());
    drop(call);
    assert_eq!(*ORDER.lock().unwrap(), ["service", "resource"]);
}
