//! One native product runtime, shared by launcher consumers and Code adapters.
//! Services and scope leases live here; project schemas remain in rh_code.
//! Configuration keeps its historical path/format and cooperative writer lock.
pub(crate) mod catalog;
pub(crate) mod contracts;
use crate::{
    document_io,
    plugin_host::{Contract, Host, ServiceCall},
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Mutex, MutexGuard},
};
pub(crate) const LIMIT: usize = 8 * 1024 * 1024;
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Control {
    schema_version: u32,
    disabled_plugins: Vec<String>,
}
struct Configuration {
    disabled: Vec<String>,
    revision: String,
    bytes: Option<Vec<u8>>,
}
#[derive(Default)]
struct Scopes {
    code_leases: HashSet<String>,
}
pub struct Runtime {
    root: PathBuf,
    gate: Mutex<()>,
    scopes: Mutex<Scopes>,
    native_plugins: Host,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
impl Runtime {
    pub fn new(project: &Path) -> Self {
        Self {
            root: project.join(".pcl-rust/rh-code"),
            gate: Mutex::new(()),
            scopes: Mutex::new(Scopes::default()),
            native_plugins: crate::plugins::host(),
        }
    }
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, ()>, String> {
        self.gate
            .lock()
            .map_err(|_| "Product plugin configuration unavailable".into())
    }
    fn read_configuration(&self) -> Result<Configuration, String> {
        let path = self.root.join("workspace.json");
        let bytes = match document_io::read(&path, LIMIT) {
            Ok(bytes) => Some(bytes),
            Err(_)
                if fs::symlink_metadata(&path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                None
            }
            Err(error) => return Err(error),
        };
        let (disabled, revision) = match &bytes {
            Some(bytes) => {
                let c: Control = serde_json::from_slice(bytes)
                    .map_err(|_| "Plugin configuration unreadable; retained for recovery")?;
                if c.schema_version != 1 {
                    return Err("Unsupported plugin configuration; retained for recovery".into());
                }
                (c.disabled_plugins, hash(bytes))
            }
            None => (vec![], "new".into()),
        };
        catalog::validate_disabled(&disabled)?;
        Ok(Configuration {
            disabled,
            revision,
            bytes,
        })
    }
    /// Caller holds the shared control gate through document publication and
    /// reconciliation. A launcher lookup never resets another owner's Code lease.
    pub(crate) fn reconcile(&self, disabled: &[String]) -> Result<(), String> {
        let code = !self
            .scopes
            .lock()
            .map_err(|_| "Plugin scopes unavailable")?
            .code_leases
            .is_empty();
        self.native_plugins.configure(disabled, true, code)
    }
    fn checked_configuration(&self) -> Result<Configuration, String> {
        match self.read_configuration() {
            Ok(c) => {
                self.reconcile(&c.disabled)?;
                Ok(c)
            }
            Err(error) => {
                // Do not leave cached weak handles admitting new calls after an
                // invalid control file has been observed. Accepted calls survive.
                let all = self
                    .native_plugins
                    .definitions()
                    .iter()
                    .map(|d| d.id.to_owned())
                    .collect::<Vec<_>>();
                self.native_plugins.configure(&all, false, false)?;
                Err(error)
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn service<T: Send + Sync + 'static>(
        &self,
        contract: Contract<T>,
    ) -> Result<ServiceCall<T>, String> {
        self.optional_service(contract)?
            .ok_or_else(|| format!("Plugin service disabled or dormant: {}", contract.id))
    }
    pub(crate) fn optional_service<T: Send + Sync + 'static>(
        &self,
        contract: Contract<T>,
    ) -> Result<Option<ServiceCall<T>>, String> {
        let _gate = self.lock()?;
        self.checked_configuration()?;
        self.native_plugins
            .resolve(contract)?
            .map(|h| h.begin())
            .transpose()
    }
    pub(crate) fn handles(operation: &str) -> bool {
        matches!(
            operation,
            "plugin_configuration" | "native_plugin" | "plugin_runtime" | "plugin_control"
        )
    }
    pub(crate) fn call(&self, operation: &str, args: Value) -> Result<Value, String> {
        let _gate = self.lock()?;
        if !args.is_object() {
            return Err("Plugin arguments must be an object".into());
        }
        if operation == "plugin_runtime" && args["action"] == "close" {
            let lease = args["lease"].as_str().ok_or("Missing plugin scope lease")?;
            self.scopes
                .lock()
                .map_err(|_| "Plugin scopes unavailable")?
                .code_leases
                .remove(lease);
            self.checked_configuration()?;
            return Ok(json!({"released":true}));
        }
        let c = self.checked_configuration()?;
        match operation {
            "plugin_configuration" => Ok(
                json!({"revision":c.revision,"disabledPlugins":c.disabled,"plugins":catalog::manifests()}),
            ),
            "native_plugin" => {
                let id = args["id"].as_str().ok_or("Missing plugin ID")?;
                let status = self
                    .native_plugins
                    .statuses()?
                    .into_iter()
                    .find(|s| s.id == id)
                    .ok_or("Unknown native plugin")?;
                if status.state != crate::plugin_host::State::Active {
                    return Err(status
                        .error
                        .unwrap_or_else(|| format!("Plugin unavailable: {id}")));
                }
                Ok(json!({"id":status.id,"state":status.state.as_str()}))
            }
            "plugin_runtime" if args["action"] == "open" => {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                let lease = format!(
                    "{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                );
                {
                    let mut scopes = self
                        .scopes
                        .lock()
                        .map_err(|_| "Plugin scopes unavailable")?;
                    if scopes.code_leases.len() >= 16 {
                        return Err("Too many live plugin scope owners".into());
                    }
                    scopes.code_leases.insert(lease.clone());
                }
                if let Err(error) = self.reconcile(&c.disabled) {
                    self.scopes
                        .lock()
                        .map_err(|_| "Plugin scopes unavailable")?
                        .code_leases
                        .remove(&lease);
                    return Err(error);
                }
                Ok(json!({"lease":lease}))
            }
            "plugin_control" => self.update_control(args),
            _ => Err("Unsupported product plugin operation".into()),
        }
    }
    fn update_control(&self, args: Value) -> Result<Value, String> {
        let _lease = document_io::workspace_lease(&self.root)?;
        // Re-read under the same disk lease used by legacy Code writers.
        let c = self.read_configuration()?;
        if args["expectedRevision"].as_str() != Some(c.revision.as_str()) {
            return Err("Plugin configuration changed; refresh before saving".into());
        }
        let mut document: Value = match c.bytes {
            Some(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| "Unreadable plugin document")?
            }
            None => {
                json!({"schemaVersion":1,"projects":[],"disabledPlugins":[],"layoutPresets":[]})
            }
        };
        let mut disabled = c.disabled;
        let active = if args["id"] == "rh.agent" && args["enabled"] == false {
            active_agent(&document)?
        } else {
            false
        };
        catalog::change(&mut disabled, &args, active)?;
        document["disabledPlugins"] = json!(disabled);
        let bytes = serde_json::to_vec(&document).map_err(|e| e.to_string())?;
        if bytes.len() > LIMIT {
            return Err("RH Code state exceeds 8 MiB; retained previous state".into());
        }
        document_io::atomic(&self.root.join("workspace.json"), &bytes)?;
        self.reconcile(&disabled)?;
        Ok(
            json!({"revision":hash(&bytes),"disabledPlugins":disabled,"plugins":catalog::manifests()}),
        )
    }
}
fn active_agent(document: &Value) -> Result<bool, String> {
    let projects = document["projects"]
        .as_array()
        .ok_or("Cannot verify active Agent tasks in retained Code state")?;
    for project in projects {
        let conversations = project["conversations"]
            .as_array()
            .ok_or("Cannot verify active Agent tasks in retained Code state")?;
        if conversations.iter().any(|c| !c["activeJob"].is_null()) {
            return Ok(true);
        }
    }
    Ok(false)
}
#[cfg(test)]
mod tests;
