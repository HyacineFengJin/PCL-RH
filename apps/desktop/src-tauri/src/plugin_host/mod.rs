//! Native execution side of the product plugin host. Definitions are inert;
//! plugin instances own their services/resources and exist only while active.
//! UI and native parts share product IDs/configuration, not separate settings.
//!
//! Cached handles are weak and cannot keep disabled plugins in memory. A call
//! admitted before retirement owns its resources until completion; retirement
//! refuses new calls without invalidating an already admitted filesystem task.
use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, BTreeSet},
    marker::PhantomData,
    ops::Deref,
    sync::{Arc, Mutex, Weak},
};

type Resource = Arc<dyn Any + Send + Sync>;
type Key = (&'static str, u32);

pub struct Contract<T: Send + Sync + 'static> {
    pub id: &'static str,
    pub version: u32,
    marker: PhantomData<fn() -> T>,
}
impl<T: Send + Sync + 'static> Copy for Contract<T> {}
impl<T: Send + Sync + 'static> Clone for Contract<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: Send + Sync + 'static> Contract<T> {
    pub const fn new(id: &'static str, version: u32) -> Self {
        Self {
            id,
            version,
            marker: PhantomData,
        }
    }
    pub fn declaration(self) -> ServiceDeclaration {
        ServiceDeclaration {
            key: (self.id, self.version),
            implementation: TypeId::of::<T>(),
        }
    }
}
#[derive(Clone)]
pub struct ServiceDeclaration {
    key: Key,
    implementation: TypeId,
}
impl ServiceDeclaration {
    pub fn id(&self) -> &'static str {
        self.key.0
    }
    pub fn version(&self) -> u32 {
        self.key.1
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Launcher,
    Code,
    Both,
}
impl Scope {
    fn matches(self, launcher: bool, code: bool) -> bool {
        match self {
            Self::Launcher => launcher,
            Self::Code => code,
            Self::Both => launcher || code,
        }
    }
}
pub struct Definition {
    pub id: &'static str,
    pub scope: Scope,
    pub dependencies: Vec<&'static str>,
    pub services: Vec<ServiceDeclaration>,
    pub start: fn(&mut Registration<'_>) -> Result<(), String>,
}

/// Only the factory's declared, correctly typed interfaces can be published.
/// Factories put resource owners here too; dropping the activation releases
/// these owners outside the host lock, including after failed initialization.
pub struct Registration<'a> {
    declaration: &'a Definition,
    host: &'a Host,
    services: BTreeMap<Key, Resource>,
    resources: Vec<Resource>,
}
impl Registration<'_> {
    pub fn require<T: Send + Sync + 'static>(
        &self,
        provider: &str,
        contract: Contract<T>,
    ) -> Result<ServiceHandle<T>, String> {
        if !self.declaration.dependencies.contains(&provider) {
            return Err(format!(
                "Undeclared plugin dependency: {} / {provider}",
                self.declaration.id
            ));
        }
        self.host.service(provider, contract)
    }
    pub fn provide<T: Send + Sync + 'static>(
        &mut self,
        contract: Contract<T>,
        value: T,
    ) -> Result<(), String> {
        let key = (contract.id, contract.version);
        if !self
            .declaration
            .services
            .iter()
            .any(|item| item.key == key && item.implementation == TypeId::of::<T>())
        {
            return Err(format!(
                "Undeclared plugin service: {} / {}@{}",
                self.declaration.id, key.0, key.1
            ));
        }
        if self.services.contains_key(&key) {
            return Err(format!("Duplicate plugin service: {}", key.0));
        }
        self.services.insert(key, Arc::new(value));
        Ok(())
    }
    pub fn hold<T: Send + Sync + 'static>(&mut self, resource: T) {
        self.resources.push(Arc::new(resource));
    }
}

struct Activation {
    accepting: Mutex<bool>,
    services: BTreeMap<Key, Resource>,
    _resources: Vec<Resource>,
    // An admitted consumer can outlive retirement. Its providers must not drop
    // resources before that consumer finishes, regardless of catalog order.
    _dependencies: Vec<Arc<Activation>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Dormant,
    Active,
    Disabled,
    Blocked,
    Failed,
}
impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dormant => "dormant",
            Self::Active => "active",
            Self::Disabled => "disabled",
            Self::Blocked => "blocked",
            Self::Failed => "failed",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Status {
    pub id: &'static str,
    pub state: State,
    pub error: Option<String>,
}
struct Registry {
    active: BTreeMap<&'static str, Arc<Activation>>,
    statuses: BTreeMap<&'static str, Status>,
}
pub struct Host {
    definitions: Vec<Definition>,
    /// Configuration is serialized; lookups do not acquire this factory gate.
    gate: Mutex<()>,
    registry: Mutex<Registry>,
}

pub struct ServiceHandle<T: Send + Sync + 'static> {
    activation: Weak<Activation>,
    value: Weak<T>,
}
pub struct ServiceCall<T: Send + Sync + 'static> {
    // Rust drops fields in declaration order. Release the value reference
    // first, so service destructors still have the activation's resource owners.
    value: Arc<T>,
    _activation: Arc<Activation>,
}
impl<T: Send + Sync + 'static> Deref for ServiceCall<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}
impl<T: Send + Sync + 'static> ServiceHandle<T> {
    pub fn begin(&self) -> Result<ServiceCall<T>, String> {
        let activation = self.activation.upgrade().ok_or("Plugin service retired")?;
        let admission = activation
            .accepting
            .lock()
            .map_err(|_| "Plugin admission unavailable")?;
        if !*admission {
            return Err("Plugin service retired".into());
        }
        let value = self.value.upgrade().ok_or("Plugin service retired")?;
        drop(admission);
        Ok(ServiceCall {
            _activation: activation,
            value,
        })
    }
}

impl Host {
    pub fn new(definitions: Vec<Definition>) -> Result<Self, String> {
        let mut ids = BTreeSet::new();
        for definition in &definitions {
            if definition.id.is_empty() || !ids.insert(definition.id) {
                return Err("Duplicate or invalid native plugin ID".into());
            }
            let mut keys = BTreeSet::new();
            for service in &definition.services {
                if service.key.0.is_empty() || service.key.1 == 0 || !keys.insert(service.key) {
                    return Err(format!("Invalid service declarations: {}", definition.id));
                }
            }
        }
        for definition in &definitions {
            if definition.dependencies.iter().any(|id| !ids.contains(id)) {
                return Err(format!(
                    "Missing native plugin dependency: {}",
                    definition.id
                ));
            }
        }
        let statuses = definitions
            .iter()
            .map(|definition| {
                (
                    definition.id,
                    Status {
                        id: definition.id,
                        state: State::Dormant,
                        error: None,
                    },
                )
            })
            .collect();
        Ok(Self {
            definitions,
            gate: Mutex::new(()),
            registry: Mutex::new(Registry {
                active: BTreeMap::new(),
                statuses,
            }),
        })
    }
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }
    pub fn statuses(&self) -> Result<Vec<Status>, String> {
        Ok(self
            .registry
            .lock()
            .map_err(|_| "Plugin registry unavailable")?
            .statuses
            .values()
            .cloned()
            .collect())
    }
    pub fn configure(&self, disabled: &[String], launcher: bool, code: bool) -> Result<(), String> {
        let _gate = self
            .gate
            .lock()
            .map_err(|_| "Plugin factory gate unavailable")?;
        let mut desired: BTreeSet<_> = self
            .definitions
            .iter()
            .filter(|definition| {
                definition.scope.matches(launcher, code)
                    && !disabled.iter().any(|id| id == definition.id)
            })
            .map(|definition| definition.id)
            .collect();
        loop {
            let missing: Vec<_> = self
                .definitions
                .iter()
                .filter(|definition| {
                    desired.contains(definition.id)
                        && definition
                            .dependencies
                            .iter()
                            .any(|id| !desired.contains(id))
                })
                .map(|definition| definition.id)
                .collect();
            if missing.is_empty() {
                break;
            }
            for id in missing {
                desired.remove(id);
            }
        }
        let retired = {
            let mut registry = self
                .registry
                .lock()
                .map_err(|_| "Plugin registry unavailable")?;
            let mut retired = Vec::new();
            let mut retiring: BTreeSet<_> = registry
                .active
                .keys()
                .copied()
                .filter(|id| !desired.contains(id))
                .collect();
            while !retiring.is_empty() {
                let id = *retiring
                    .iter()
                    .find(|id| {
                        !self.definitions.iter().any(|definition| {
                            retiring.contains(definition.id) && definition.dependencies.contains(id)
                        })
                    })
                    .unwrap_or_else(|| retiring.first().unwrap());
                retiring.remove(id);
                if let Some(activation) = registry.active.remove(id) {
                    *activation
                        .accepting
                        .lock()
                        .map_err(|_| "Plugin admission unavailable")? = false;
                    retired.push(activation);
                }
            }
            for definition in &self.definitions {
                if desired.contains(definition.id) {
                    continue;
                }
                let state = if disabled.iter().any(|id| id == definition.id) {
                    State::Disabled
                } else if !definition.scope.matches(launcher, code) {
                    State::Dormant
                } else {
                    State::Blocked
                };
                registry.statuses.insert(
                    definition.id,
                    Status {
                        id: definition.id,
                        state,
                        error: None,
                    },
                );
            }
            retired
        };
        drop(retired);
        let mut pending: Vec<_> = self
            .definitions
            .iter()
            .filter(|definition| desired.contains(definition.id))
            .collect();
        while !pending.is_empty() {
            let mut progressed = false;
            let mut next = Vec::new();
            for definition in pending {
                {
                    let registry = self
                        .registry
                        .lock()
                        .map_err(|_| "Plugin registry unavailable")?;
                    if registry.active.contains_key(definition.id) {
                        continue;
                    }
                    if definition
                        .dependencies
                        .iter()
                        .any(|id| !registry.active.contains_key(id))
                    {
                        next.push(definition);
                        continue;
                    }
                }
                progressed = true;
                let mut registration = Registration {
                    declaration: definition,
                    host: self,
                    services: BTreeMap::new(),
                    resources: vec![],
                };
                let initialized = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    (definition.start)(&mut registration)
                }))
                .map_err(|_| "Plugin factory panicked".to_owned())
                .and_then(|result| result)
                .and_then(|()| {
                    if definition
                        .services
                        .iter()
                        .any(|service| !registration.services.contains_key(&service.key))
                    {
                        Err("Declared plugin service missing".into())
                    } else {
                        Ok(())
                    }
                });
                let mut registry = self
                    .registry
                    .lock()
                    .map_err(|_| "Plugin registry unavailable")?;
                match initialized {
                    Ok(()) => {
                        let dependencies = definition
                            .dependencies
                            .iter()
                            .map(|id| registry.active[id].clone())
                            .collect();
                        registry.active.insert(
                            definition.id,
                            Arc::new(Activation {
                                accepting: Mutex::new(true),
                                services: registration.services,
                                _resources: registration.resources,
                                _dependencies: dependencies,
                            }),
                        );
                        registry.statuses.insert(
                            definition.id,
                            Status {
                                id: definition.id,
                                state: State::Active,
                                error: None,
                            },
                        );
                    }
                    Err(error) => {
                        registry.statuses.insert(
                            definition.id,
                            Status {
                                id: definition.id,
                                state: State::Failed,
                                error: Some(error),
                            },
                        );
                    }
                }
                // A failed factory can own callbacks. Release them unlocked.
                drop(registry);
            }
            if !progressed {
                let mut registry = self
                    .registry
                    .lock()
                    .map_err(|_| "Plugin registry unavailable")?;
                for definition in next {
                    registry.statuses.insert(
                        definition.id,
                        Status {
                            id: definition.id,
                            state: State::Blocked,
                            error: Some("Native plugin dependencies unavailable".into()),
                        },
                    );
                }
                break;
            }
            pending = next;
        }
        Ok(())
    }
    pub fn service<T: Send + Sync + 'static>(
        &self,
        provider: &str,
        contract: Contract<T>,
    ) -> Result<ServiceHandle<T>, String> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| "Plugin registry unavailable")?;
        let activation = registry
            .active
            .get(provider)
            .ok_or_else(|| format!("Plugin unavailable: {provider}"))?;
        let value = activation
            .services
            .get(&(contract.id, contract.version))
            .ok_or("Plugin service protocol unavailable")?
            .clone()
            .downcast::<T>()
            .map_err(|_| "Plugin service type mismatch")?;
        Ok(ServiceHandle {
            activation: Arc::downgrade(activation),
            value: Arc::downgrade(&value),
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Ok(registry) = self.registry.get_mut() {
            for activation in registry.active.values() {
                if let Ok(mut accepting) = activation.accepting.lock() {
                    *accepting = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
