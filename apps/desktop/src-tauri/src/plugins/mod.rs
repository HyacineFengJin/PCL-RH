//! Bundled product plugins. This is the native entry table, matching the same
//! IDs/catalog used by the existing orange Plugin manager and WebUI executor.
pub(crate) mod diagnostics;
pub(crate) mod home;
pub(crate) mod instances;
use crate::plugin_host::{Definition, Host, Scope};

pub fn host() -> Host {
    let host = Host::new(vec![
        Definition {
            id: instances::ID,
            scope: Scope::Launcher,
            dependencies: vec![],
            services: vec![crate::product_plugins::contracts::copy::INSTANCE_COPY.declaration()],
            start: instances::start,
        },
        Definition {
            id: home::ID,
            scope: Scope::Launcher,
            dependencies: vec![],
            services: vec![home::ANNOUNCEMENTS.declaration()],
            start: home::start,
        },
        Definition {
            id: diagnostics::ID,
            scope: Scope::Launcher,
            dependencies: vec![],
            services: vec![diagnostics::ANALYSIS.declaration()],
            start: diagnostics::start,
        },
    ])
    .expect("valid bundled native plugin definitions");
    validate_catalog(&host).expect("native interfaces match the product catalog");
    host
}

fn validate_catalog(host: &Host) -> Result<(), String> {
    let catalog: serde_json::Value =
        serde_json::from_str(include_str!("../../../../../code/plugins.json"))
            .map_err(|_| "Invalid product plugin catalog")?;
    let catalog = catalog.as_array().ok_or("Invalid product plugin catalog")?;
    if catalog.iter().any(|manifest| {
        manifest.get("nativeServices").is_some()
            && !host
                .definitions()
                .iter()
                .any(|definition| manifest["id"] == definition.id)
    }) {
        return Err("Native plugin declaration has no compiled entry".into());
    }
    for definition in host.definitions() {
        let manifest = catalog
            .iter()
            .find(|item| item["id"] == definition.id)
            .ok_or("Native plugin missing from product catalog")?;
        let declared = manifest["nativeServices"]
            .as_array()
            .ok_or("Missing native service declarations")?;
        if declared.len() != definition.services.len()
            || definition.services.iter().any(|service| {
                !declared.iter().any(|item| {
                    item["id"] == service.id()
                        && item["version"].as_u64() == Some(service.version() as u64)
                })
            })
        {
            return Err(format!("Native plugin interface drift: {}", definition.id));
        }
        let scope = match definition.scope {
            Scope::Launcher => "launcher",
            Scope::Code => "rh-code",
            Scope::Both => "both",
        };
        if manifest["scope"] != scope
            || manifest["dependencies"]
                .as_array()
                .is_none_or(|dependencies| {
                    dependencies.len() != definition.dependencies.len()
                        || definition
                            .dependencies
                            .iter()
                            .any(|id| !dependencies.iter().any(|item| item == id))
                })
        {
            return Err(format!("Native plugin lifecycle drift: {}", definition.id));
        }
    }
    Ok(())
}
