use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, Instant};

use dg_lab_link_plugin_sdk::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{
    Mutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, OwnedSemaphorePermit, Semaphore,
};
use tokio_util::sync::CancellationToken;

use crate::frames::LatestFrameStore;
use crate::package::{PreparedPackage, io_error, prepare_package};
use crate::process::{BusinessHandler, HandlerSlot, Session, StateCallback};

const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INPUT_OWNERS: usize = 64;
const INPUT_SEQUENCE_RETENTION: Duration = Duration::from_secs(10);
const INPUT_OWNER_IDLE: Duration = Duration::from_secs(1);
const UI_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_UI_READERS: usize = 8;

struct InputSequence {
    sequence: u64,
    last_seen: Instant,
    binding_id: Option<String>,
}

#[derive(Default)]
struct InputSequenceCache(HashMap<String, InputSequence>);

impl InputSequenceCache {
    fn clear(&mut self) {
        self.0.clear();
    }

    /// Preserve active owners' replay protection while reclaiming old UI mounts.
    /// A pipe enqueue failure must not advance the accepted sequence number.
    fn will_accept(&mut self, key: &str, sequence: u64, now: Instant) -> Result<bool, PluginError> {
        self.0.retain(|_, entry| {
            now.saturating_duration_since(entry.last_seen) < INPUT_SEQUENCE_RETENTION
        });
        if let Some(entry) = self.0.get(key) {
            return Ok(sequence > entry.sequence);
        }
        if self.0.len() >= MAX_INPUT_OWNERS {
            let idle = self
                .0
                .iter()
                .filter(|(_, entry)| {
                    now.saturating_duration_since(entry.last_seen) >= INPUT_OWNER_IDLE
                })
                .min_by_key(|(_, entry)| entry.last_seen)
                .map(|(key, _)| key.clone());
            if let Some(idle) = idle {
                self.0.remove(&idle);
            } else {
                return Err(PluginError::new(
                    "queue_busy",
                    "同时活动的持续输入所有者数量超过容量",
                ));
            }
        }
        Ok(true)
    }

    fn record_scoped(
        &mut self,
        key: String,
        sequence: u64,
        now: Instant,
        binding_id: Option<&str>,
    ) {
        self.0.insert(
            key,
            InputSequence {
                sequence,
                last_seen: now,
                binding_id: binding_id.map(str::to_owned),
            },
        );
    }

    #[cfg(test)]
    fn record(&mut self, key: String, sequence: u64, now: Instant) {
        self.record_scoped(key, sequence, now, None);
    }

    fn invalidate_bindings(&mut self, invalidated: &HashSet<String>) {
        if invalidated.is_empty() {
            return;
        }
        self.0.retain(|_, entry| {
            entry
                .binding_id
                .as_ref()
                .is_some_and(|id| !invalidated.contains(id))
        });
    }
}

fn invalidated_bindings(previous: &[Binding], current: &[Binding]) -> HashSet<String> {
    previous
        .iter()
        .filter(|old| {
            current
                .iter()
                .find(|new| new.binding_id == old.binding_id)
                .is_none_or(|new| {
                    new.generation != old.generation
                        || new.active != old.active
                        || new.control_id != old.control_id
                        || new.channel != old.channel
                })
        })
        .map(|binding| binding.binding_id.clone())
        .collect()
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    #[default]
    Stopped,
    Starting,
    Running,
    Faulted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceState {
    #[serde(flatten)]
    pub spec: SourceSpec,
    pub revision: u64,
    pub status: SourceStatus,
    pub state: Value,
    pub last_error: Option<PluginError>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PluginRuntimeSnapshot {
    pub plugins: Vec<InstalledPlugin>,
    pub sources: Vec<SourceState>,
}

/// Immutable, low-frequency metadata. Cloning the Arc never clones configuration values.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCatalogSnapshot {
    pub revision: u64,
    pub plugins: Vec<InstalledPlugin>,
    pub sources: Vec<SourceSpec>,
    pub source_revisions: HashMap<String, u64>,
}

impl PluginCatalogSnapshot {
    fn from_registry(registry: &Registry, revision: u64) -> Self {
        let mut plugins: Vec<_> = registry.plugins.values().cloned().collect();
        plugins.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        let mut sources: Vec<_> = registry.sources.values().cloned().collect();
        sources.sort_by(|a, b| a.id.cmp(&b.id));
        Self {
            revision,
            plugins,
            sources,
            source_revisions: registry.revisions.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceRuntimeState {
    pub id: String,
    pub revision: u64,
    pub status: SourceStatus,
    pub state: Value,
    pub last_error: Option<PluginError>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    #[serde(default)]
    plugins: HashMap<String, InstalledPlugin>,
    #[serde(default)]
    sources: HashMap<String, SourceSpec>,
    #[serde(default)]
    revisions: HashMap<String, u64>,
    #[serde(default)]
    seeded: HashSet<String>,
}

struct LiveEntry {
    status: SourceStatus,
    state: Value,
    last_error: Option<PluginError>,
    session: Option<Session>,
    bindings: Vec<Binding>,
    gate: Arc<Mutex<()>>,
    ui_readers: Arc<Semaphore>,
    permit: Option<OwnedSemaphorePermit>,
    input_sequences: InputSequenceCache,
    generation: u64,
    startup_cancel: CancellationToken,
    restart_blocked: bool,
}

impl Default for LiveEntry {
    fn default() -> Self {
        Self {
            status: SourceStatus::Stopped,
            state: empty_object(),
            last_error: None,
            session: None,
            bindings: Vec::new(),
            gate: Arc::new(Mutex::new(())),
            ui_readers: Arc::new(Semaphore::new(MAX_UI_READERS)),
            permit: None,
            input_sequences: InputSequenceCache::default(),
            generation: 0,
            startup_cancel: CancellationToken::new(),
            restart_blocked: false,
        }
    }
}

struct Inner {
    root: PathBuf,
    registry: RwLock<Registry>,
    catalog: RwLock<Arc<PluginCatalogSnapshot>>,
    live: Arc<RwLock<HashMap<String, LiveEntry>>>,
    commit: Mutex<()>,
    package_transaction: Mutex<()>,
    package_gates: RwLock<HashMap<String, Weak<tokio::sync::RwLock<()>>>>,
    handler: HandlerSlot,
    operation_validator: RwLock<Option<OperationValidator>>,
    frames: LatestFrameStore,
    slots: Arc<Semaphore>,
    stopping: AtomicBool,
    shutdown_cancel: CancellationToken,
}

/// Host admission check, invoked under the instance lifecycle gate before use
/// of a native process. The host retains the original task's operation epoch.
type OperationValidator = Arc<dyn Fn(u64) -> Result<(), PluginError> + Send + Sync>;

/// Cloneable host authority. Synchronous reads and bindings updates never await a plugin.
#[derive(Clone)]
pub struct PluginManager(Arc<Inner>);

impl PluginManager {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let root = root.into();
        fs::create_dir_all(root.join("packages")).map_err(io_error)?;
        fs::create_dir_all(root.join("data")).map_err(io_error)?;
        fs::create_dir_all(root.join("staging")).map_err(io_error)?;
        let path = root.join("registry.json");
        let mut registry: Registry = if path.is_file() {
            let file = File::open(&path).map_err(io_error)?;
            if file.metadata().map_err(io_error)?.len() > MAX_REGISTRY_BYTES {
                return Err(PluginError::new("invalid_config", "插件注册表超过容量限制"));
            }
            serde_json::from_reader(file)
                .map_err(|error| PluginError::new("invalid_config", error.to_string()))?
        } else {
            Registry::default()
        };
        for plugin in registry.plugins.values_mut() {
            crate::package::validate_manifest(&plugin.manifest)?;
            if plugin.digest.len() != 64
                || !plugin.digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(PluginError::new("invalid_config", "插件摘要无效"));
            }
            plugin.directory = root
                .join("packages")
                .join(&plugin.manifest.id)
                .join(&plugin.digest);
        }
        for source in registry.sources.values() {
            validate_source(source)?;
        }
        let live = registry
            .sources
            .keys()
            .map(|id| (id.clone(), LiveEntry::default()))
            .collect();
        let catalog = Arc::new(PluginCatalogSnapshot::from_registry(&registry, 0));
        Ok(Self(Arc::new(Inner {
            root,
            registry: RwLock::new(registry),
            catalog: RwLock::new(catalog),
            live: Arc::new(RwLock::new(live)),
            commit: Mutex::new(()),
            package_transaction: Mutex::new(()),
            package_gates: RwLock::new(HashMap::new()),
            handler: Arc::new(RwLock::new(None)),
            operation_validator: RwLock::new(None),
            frames: LatestFrameStore::default(),
            slots: Arc::new(Semaphore::new(MAX_INSTANCES)),
            stopping: AtomicBool::new(false),
            shutdown_cancel: CancellationToken::new(),
        })))
    }

    pub fn set_business_handler(&self, handler: Arc<dyn BusinessHandler>) {
        *self
            .0
            .handler
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handler);
    }

    pub fn set_operation_validator(
        &self,
        validator: Arc<dyn Fn(u64) -> Result<(), PluginError> + Send + Sync>,
    ) {
        *self
            .0
            .operation_validator
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(validator);
    }

    pub fn clear_business_handler(&self) {
        *self
            .0
            .handler
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    pub fn snapshot(&self) -> PluginRuntimeSnapshot {
        let registry = self
            .0
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let live = self
            .0
            .live
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut plugins: Vec<_> = registry.plugins.values().cloned().collect();
        plugins.sort_by(|first, second| first.manifest.id.cmp(&second.manifest.id));
        let mut sources: Vec<_> = registry
            .sources
            .values()
            .map(|spec| {
                let entry = live.get(&spec.id);
                SourceState {
                    spec: spec.clone(),
                    revision: registry.revisions.get(&spec.id).copied().unwrap_or(0),
                    status: entry.map_or(SourceStatus::Stopped, |entry| entry.status),
                    state: entry.map_or_else(empty_object, |entry| entry.state.clone()),
                    last_error: entry.and_then(|entry| entry.last_error.clone()),
                }
            })
            .collect();
        sources.sort_by(|first, second| first.spec.id.cmp(&second.spec.id));
        PluginRuntimeSnapshot { plugins, sources }
    }

    pub fn cached_catalog_snapshot(&self) -> Arc<PluginCatalogSnapshot> {
        self.0
            .catalog
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn runtime_states(&self) -> Vec<SourceRuntimeState> {
        let registry = self.0.registry.read().unwrap_or_else(|p| p.into_inner());
        let live = self.0.live.read().unwrap_or_else(|p| p.into_inner());
        let mut states: Vec<_> = registry
            .sources
            .keys()
            .map(|id| {
                let entry = live.get(id);
                SourceRuntimeState {
                    id: id.clone(),
                    revision: registry.revisions.get(id).copied().unwrap_or(0),
                    status: entry.map_or(SourceStatus::Stopped, |entry| entry.status),
                    state: entry.map_or_else(empty_object, |entry| entry.state.clone()),
                    last_error: entry.and_then(|entry| entry.last_error.clone()),
                }
            })
            .collect();
        states.sort_by(|a, b| a.id.cmp(&b.id));
        states
    }

    pub fn frames(&self) -> LatestFrameStore {
        self.0.frames.clone()
    }

    pub fn try_update_bindings(
        &self,
        source_id: &str,
        bindings: &[Binding],
    ) -> Result<(), PluginError> {
        self.ensure_running()?;
        let _package = self.source_package_read(source_id)?;
        if bindings.len() > MAX_BINDINGS {
            return Err(PluginError::new("invalid_params", "插件绑定数量超过上限"));
        }
        let mut seen = HashSet::new();
        if bindings.iter().any(|binding| {
            binding.binding_id.len() > 256
                || binding.control_id.len() > 256
                || !seen.insert(&binding.binding_id)
        }) {
            return Err(PluginError::new("invalid_params", "插件绑定标识无效或重复"));
        }
        let enabled = self
            .0
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sources
            .get(source_id)
            .is_some_and(|source| source.enabled);
        let mut live = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = live.get_mut(source_id).ok_or_else(source_not_found)?;
        if entry.bindings == bindings {
            return Ok(());
        }
        let invalidated = invalidated_bindings(&entry.bindings, bindings);
        self.0.frames.replace_bindings(source_id, bindings);
        entry.bindings = bindings.to_vec();
        entry.input_sequences.invalidate_bindings(&invalidated);
        if let Some(session) = &entry.session {
            session.update_bindings(entry.bindings.clone(), &invalidated);
        }
        let start = enabled
            && !bindings.is_empty()
            && entry.status == SourceStatus::Stopped
            && !entry.restart_blocked;
        let generation = entry.generation;
        drop(live);
        if start {
            let manager = self.clone();
            let source_id = source_id.to_owned();
            tokio::spawn(async move {
                let current = manager
                    .0
                    .live
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&source_id)
                    .is_some_and(|entry| entry.generation == generation && !entry.restart_blocked);
                if current {
                    let _ = manager.start(&source_id).await;
                }
            });
        }
        Ok(())
    }

    pub async fn start(&self, source_id: &str) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.start_locked(source_id).await.map(|_| ())
    }

    async fn start_locked(&self, source_id: &str) -> Result<Session, PluginError> {
        self.ensure_running()?;
        let validator = self
            .0
            .operation_validator
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Ok(epoch) = OPERATION_EPOCH.try_with(|epoch| *epoch)
            && let Some(validator) = validator
        {
            validator(epoch)?;
        }
        let (source, plugin) = self.definition(source_id)?;
        if !source.enabled {
            return Err(PluginError::new("source_disabled", "输入源已停用"));
        }
        {
            let live = self
                .0
                .live
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(session) = live
                .get(source_id)
                .and_then(|entry| entry.session.as_ref())
                .filter(|session| session.is_alive())
            {
                return Ok(session.clone());
            }
        }
        let permit = self
            .0
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| PluginError::new("queue_busy", "最多同时运行 32 个插件实例"))?;
        let data = self.0.root.join("data").join(source_id);
        fs::create_dir_all(&data).map_err(io_error)?;
        let (generation, startup_cancel) = {
            let mut live = self
                .0
                .live
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = live.get_mut(source_id).ok_or_else(source_not_found)?;
            entry.status = SourceStatus::Starting;
            entry.last_error = None;
            entry.permit = Some(permit);
            entry.session = None;
            entry.generation = entry.generation.wrapping_add(1);
            entry.startup_cancel = self.0.shutdown_cancel.child_token();
            entry.restart_blocked = false;
            (entry.generation, entry.startup_cancel.clone())
        };
        let weak_live = Arc::downgrade(&self.0.live);
        let callback: StateCallback = Arc::new(move |source_id, error, value| {
            let Some(live) = weak_live.upgrade() else {
                return;
            };
            let mut live = live
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(entry) = live.get_mut(source_id) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            if let Some(error) = error {
                entry.status = SourceStatus::Faulted;
                entry.last_error = Some(bounded_error(error));
                entry.permit = None;
            }
            if let Some(mut value) = value {
                if let Some(log) = value.get("log").and_then(Value::as_str) {
                    if !entry.state.is_object() {
                        entry.state = empty_object();
                    }
                    let logs = entry
                        .state
                        .as_object_mut()
                        .expect("object")
                        .entry("logs")
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Some(logs) = logs.as_array_mut() {
                        logs.push(Value::String(log.chars().take(512).collect()));
                        if logs.len() > 64 {
                            logs.remove(0);
                        }
                    }
                } else if value.is_object() {
                    if let Some(logs) = entry.state.get("logs").cloned() {
                        value
                            .as_object_mut()
                            .expect("object")
                            .insert("logs".into(), logs);
                    }
                    entry.state = value;
                } else {
                    entry.state = value;
                }
                if serde_json::to_vec(&entry.state).is_ok_and(|bytes| bytes.len() > 64 * 1024) {
                    if let Some(object) = entry.state.as_object_mut() {
                        object.remove("logs");
                    }
                    if serde_json::to_vec(&entry.state).is_ok_and(|bytes| bytes.len() > 64 * 1024) {
                        entry.state = empty_object();
                    }
                }
            }
        });
        let result = Session::spawn(
            &plugin.directory.join(&plugin.manifest.executable),
            &data,
            source,
            self.0.frames.clone(),
            self.0.handler.clone(),
            callback,
            None,
            startup_cancel,
        )
        .await;
        match result {
            Ok(session) => {
                if self.0.stopping.load(Ordering::Acquire) {
                    session.shutdown().await;
                    return Err(PluginError::new("core_shutting_down", "核心正在关闭"));
                }
                let attached = {
                    let mut live = self.0.live.write().unwrap_or_else(|p| p.into_inner());
                    match live.get_mut(source_id) {
                        Some(entry)
                            if !self.0.stopping.load(Ordering::Acquire)
                                && entry.generation == generation =>
                        {
                            if !session.is_alive() {
                                entry.status = SourceStatus::Faulted;
                                entry.permit = None;
                                Err(PluginError::new(
                                    "plugin_disconnected",
                                    "插件在初始化后退出",
                                ))
                            } else {
                                entry.status = SourceStatus::Running;
                                entry.last_error = None;
                                self.0.frames.replace_bindings(source_id, &entry.bindings);
                                session.update_bindings(entry.bindings.clone(), &HashSet::new());
                                entry.session = Some(session.clone());
                                Ok(session.clone())
                            }
                        }
                        _ => Err(PluginError::new("core_shutting_down", "插件启动已取消")),
                    }
                };
                if attached.is_err() {
                    session.shutdown().await;
                }
                attached
            }
            Err(error) => {
                let mut live = self
                    .0
                    .live
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(entry) = live
                    .get_mut(source_id)
                    .filter(|entry| entry.generation == generation)
                {
                    entry.status = SourceStatus::Faulted;
                    entry.last_error = Some(bounded_error(error.clone()));
                    entry.permit = None;
                }
                Err(error)
            }
        }
    }

    pub async fn configure(
        &self,
        source_id: &str,
        config: Value,
        expected_revision: u64,
    ) -> Result<Value, PluginError> {
        let _package = self.source_package_read(source_id)?;
        if serde_json::to_vec(&config)
            .map_err(|error| PluginError::new("invalid_config", error.to_string()))?
            .len()
            > 1024 * 1024
        {
            return Err(PluginError::new("invalid_config", "插件配置超过 1 MiB"));
        }
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        let (old, _) = self.definition(source_id)?;
        let next_revision = {
            let registry = self.0.registry.read().unwrap_or_else(|p| p.into_inner());
            let current = registry.revisions.get(source_id).copied().unwrap_or(0);
            if current != expected_revision {
                return Err(PluginError::new(
                    "config_conflict",
                    format!(
                        "配置已更新（当前版本 {current}，提交版本 {expected_revision}），请重新读取"
                    ),
                ));
            }
            current
                .checked_add(1)
                .ok_or_else(|| PluginError::new("invalid_config", "配置版本已达上限"))?
        };
        let session = self.start_locked(source_id).await?;
        session
            .request(
                "configure",
                serde_json::to_value(ConfigureParams {
                    config: config.clone(),
                    validate_only: true,
                })
                .expect("configure serialize"),
            )
            .await?;
        let result = session
            .request(
                "configure",
                serde_json::to_value(ConfigureParams {
                    config: config.clone(),
                    validate_only: false,
                })
                .expect("configure serialize"),
            )
            .await;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.restore_config(source_id, &session, old.config, &error)
                    .await;
                return Err(error);
            }
        };
        let commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        let Some(source) = registry.sources.get_mut(source_id) else {
            return Err(source_not_found());
        };
        source.config = config;
        registry
            .revisions
            .insert(source_id.to_owned(), next_revision);
        let result = self.save_registry(registry);
        drop(commit);
        if let Err(error) = result {
            self.restore_config(source_id, &session, old.config, &error)
                .await;
            return Err(error);
        }
        Ok(value)
    }

    async fn restore_config(
        &self,
        source_id: &str,
        session: &Session,
        config: Value,
        original: &PluginError,
    ) {
        if session
            .request(
                "configure",
                serde_json::to_value(ConfigureParams {
                    config,
                    validate_only: false,
                })
                .expect("configure serialize"),
            )
            .await
            .is_err()
        {
            session.shutdown().await;
            self.0.frames.clear_source(source_id);
            if let Some(entry) = self
                .0
                .live
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get_mut(source_id)
            {
                entry.status = SourceStatus::Faulted;
                entry.last_error = Some(bounded_error(PluginError::new(
                    "rollback_failed",
                    format!("配置恢复失败，已停止插件：{}", original.message),
                )));
                entry.session = None;
                entry.permit = None;
            }
        }
    }

    /// Run binding configuration `[validate, apply, rollback]` actions and the
    /// external commit against one captured process. Admission is checked while
    /// holding the lifecycle gate; the same gate covers all phases. Rollback
    /// never performs another lazy start.
    pub async fn configure_binding_transaction<Admit, Commit, CommitFuture>(
        &self,
        source_id: &str,
        actions: [ActionParams; 3],
        admit: Admit,
        commit: Commit,
    ) -> Result<Value, PluginError>
    where
        Admit: FnOnce() -> Result<(), PluginError>,
        Commit: FnOnce() -> CommitFuture,
        CommitFuture: std::future::Future<Output = Result<(), PluginError>>,
    {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        // The caller checks its accepted context while owning the same gate
        // used by stop, before this transaction may initialize a process.
        admit()?;
        let session = self.start_locked(source_id).await?;
        let [validate, apply, rollback] = actions;
        session
            .request(
                "action",
                serde_json::to_value(validate).expect("action serialize"),
            )
            .await?;
        let applied = session
            .request(
                "action",
                serde_json::to_value(apply).expect("action serialize"),
            )
            .await;
        let result = match applied {
            Ok(value) => commit().await.map(|()| value),
            Err(error) => Err(error),
        };
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if let Err(rollback) = session
                    .request(
                        "action",
                        serde_json::to_value(rollback).expect("action serialize"),
                    )
                    .await
                {
                    session.shutdown().await;
                    self.0.frames.clear_source(source_id);
                    let failure = bounded_error(PluginError::new(
                        "rollback_failed",
                        format!("{error}；恢复旧配置失败，已停止插件：{rollback}"),
                    ));
                    if let Some(entry) = self
                        .0
                        .live
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .get_mut(source_id)
                    {
                        entry.status = SourceStatus::Faulted;
                        entry.last_error = Some(failure.clone());
                        entry.session = None;
                        entry.permit = None;
                        entry.input_sequences.clear();
                        entry.startup_cancel.cancel();
                        entry.generation = entry.generation.wrapping_add(1);
                        entry.restart_blocked = true;
                    }
                    return Err(failure);
                }
                Err(error)
            }
        }
    }

    pub async fn action(
        &self,
        source_id: &str,
        params: ActionParams,
    ) -> Result<Value, PluginError> {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.start_locked(source_id)
            .await?
            .request(
                "action",
                serde_json::to_value(params).expect("action serialize"),
            )
            .await
    }

    pub async fn ui(&self, source_id: &str, params: UiParams) -> Result<UiDocument, PluginError> {
        let _package = self.source_package_read(source_id)?;
        let (readers, gate, generation, status) = self
            .0
            .live
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .map(|entry| {
                (
                    entry.ui_readers.clone(),
                    entry.gate.clone(),
                    entry.generation,
                    entry.status,
                )
            })
            .ok_or_else(source_not_found)?;
        let _reader = readers
            .try_acquire_owned()
            .map_err(|_| PluginError::new("queue_busy", "插件界面读取数量超过容量"))?;
        let _guard = tokio::time::timeout(UI_WAIT_TIMEOUT, gate.clone().lock_owned())
            .await
            .map_err(|_| PluginError::new("queue_busy", "等待插件界面读取超时"))?;
        self.ensure_running()?;
        let current = self
            .0
            .live
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(source_id)
            .is_some_and(|entry| {
                let same_generation = entry.generation == generation;
                // Another queued panel may have performed the first lazy initialization.
                let initialized = status == SourceStatus::Stopped
                    && generation.checked_add(1) == Some(entry.generation)
                    && entry.status == SourceStatus::Running
                    && !entry.restart_blocked;
                Arc::ptr_eq(&entry.gate, &gate)
                    && (same_generation || initialized)
                    && (status == SourceStatus::Faulted || entry.status != SourceStatus::Faulted)
            });
        if !current {
            return Err(PluginError::new(
                "request_cancelled",
                "等待期间输入源已停止或更换，请重新读取界面",
            ));
        }
        let value = self
            .start_locked(source_id)
            .await?
            .request("ui", serde_json::to_value(params).expect("ui serialize"))
            .await?;
        let document: UiDocument = serde_json::from_value(value)
            .map_err(|error| PluginError::new("invalid_ui", error.to_string()))?;
        document.validate()?;
        Ok(document)
    }

    pub async fn input(&self, source_id: &str, params: InputParams) -> Result<Value, PluginError> {
        self.try_input(source_id, params)?;
        Ok(Value::Null)
    }

    /// Continuous input has a separate latest-wins path; no config/ordinary lock.
    pub fn try_input(&self, source_id: &str, params: InputParams) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        if params.owner.is_empty()
            || params.owner.len() > 128
            || params.action.is_empty()
            || params.action.len() > 128
        {
            return Err(PluginError::new("invalid_params", "持续输入标识无效"));
        }
        if serde_json::to_vec(&params.value)
            .map_err(|error| PluginError::new("invalid_params", error.to_string()))?
            .len()
            > 64 * 1024
        {
            return Err(PluginError::new("invalid_params", "持续输入内容超过容量"));
        }
        let mut live = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = live.get_mut(source_id).ok_or_else(source_not_found)?;
        let session = entry
            .session
            .as_ref()
            .filter(|session| session.is_alive())
            .ok_or_else(|| PluginError::new("plugin_disconnected", "请先启动插件实例"))?;
        if params.binding_id.as_ref().is_some_and(|id| {
            !entry
                .bindings
                .iter()
                .any(|binding| &binding.binding_id == id)
        }) {
            return Err(PluginError::new("invalid_binding", "输入绑定不属于该实例"));
        }
        let key = format!(
            "{}:{}:{}",
            params.binding_id.as_deref().unwrap_or(""),
            params.owner,
            params.action
        );
        let now = Instant::now();
        if !entry
            .input_sequences
            .will_accept(&key, params.sequence, now)?
        {
            return Ok(());
        }
        session.input(params.clone())?;
        entry.input_sequences.record_scoped(
            key,
            params.sequence,
            now,
            params.binding_id.as_deref(),
        );
        Ok(())
    }

    pub async fn stop(&self, source_id: &str) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.stop_locked(source_id).await;
        Ok(())
    }

    async fn stop_locked(&self, source_id: &str) {
        self.0.frames.clear_source(source_id);
        let session = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(source_id)
            .and_then(|entry| entry.session.take());
        if let Some(session) = session {
            session.shutdown().await;
        }
        if let Some(entry) = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(source_id)
        {
            entry.status = SourceStatus::Stopped;
            entry.permit = None;
            entry.input_sequences.clear();
            entry.startup_cancel.cancel();
            entry.generation = entry.generation.wrapping_add(1);
            entry.restart_blocked = true;
        }
    }

    pub async fn shutdown(&self) {
        self.0.stopping.store(true, Ordering::Release);
        self.0
            .operation_validator
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        self.0.shutdown_cancel.cancel();
        let sessions: Vec<_> = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values_mut()
            .filter_map(|entry| {
                entry.startup_cancel.cancel();
                entry.generation = entry.generation.wrapping_add(1);
                entry.session.take()
            })
            .collect();
        let mut tasks = tokio::task::JoinSet::new();
        for session in sessions {
            tasks.spawn(async move {
                session.shutdown().await;
            });
        }
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.abort_all();
        let mut live = self
            .0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (id, entry) in live.iter_mut() {
            entry.status = SourceStatus::Stopped;
            entry.permit = None;
            self.0.frames.clear_source(id);
        }
    }

    pub async fn install(
        &self,
        path: impl AsRef<Path>,
        preinstalled: bool,
    ) -> Result<InstalledPlugin, PluginError> {
        self.ensure_running()?;
        let _transaction = self
            .0
            .package_transaction
            .try_lock()
            .map_err(|_| package_busy())?;
        let prepared = self.prepare(path.as_ref().to_path_buf()).await?;
        let _package = match self.package_write(&prepared.manifest.id) {
            Ok(guard) => guard,
            Err(error) => {
                let directory = prepared.directory;
                let _ = tokio::task::spawn_blocking(move || fs::remove_dir_all(directory)).await;
                return Err(error);
            }
        };
        self.publish_package(prepared, preinstalled, false).await
    }

    pub async fn update(&self, path: impl AsRef<Path>) -> Result<InstalledPlugin, PluginError> {
        self.ensure_running()?;
        let _transaction = self
            .0
            .package_transaction
            .try_lock()
            .map_err(|_| package_busy())?;
        let prepared = self.prepare(path.as_ref().to_path_buf()).await?;
        let id = prepared.manifest.id.clone();
        let result =
            async {
                let _package = self.package_write(&id)?;
                let old = self
                    .registry_clone()
                    .plugins
                    .get(&id)
                    .cloned()
                    .ok_or_else(|| PluginError::new("plugin_not_found", "请先安装该插件"))?;
                let mut sources: Vec<_> = self
                    .registry_clone()
                    .sources
                    .values()
                    .filter(|source| source.plugin_id == id)
                    .cloned()
                    .collect();
                sources.sort_by(|first, second| first.id.cmp(&second.id));
                let mut guards = Vec::new();
                for source in &sources {
                    guards.push(self.gate(&source.id)?.try_lock_owned().map_err(|_| {
                        PluginError::new("queue_busy", "关联输入源正在处理其他操作")
                    })?);
                }
                for source in &sources {
                    self.stop_locked(&source.id).await;
                }
                for source in &mut sources {
                    let _permit = self
                        .0
                        .slots
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| PluginError::new("queue_busy", "运行插件实例已达上限"))?;
                    let data = self.0.root.join("data").join(&source.id);
                    fs::create_dir_all(&data).map_err(io_error)?;
                    let session = Session::spawn(
                        &prepared.directory.join(&prepared.manifest.executable),
                        &data,
                        source.clone(),
                        LatestFrameStore::default(),
                        self.0.handler.clone(),
                        Arc::new(|_, _, _| {}),
                        Some(old.manifest.version.clone()),
                        self.0.shutdown_cancel.child_token(),
                    )
                    .await?;
                    let migrated = session.initialized_config();
                    let validation = session
                        .request(
                            "configure",
                            serde_json::to_value(ConfigureParams {
                                config: migrated.clone(),
                                validate_only: true,
                            })
                            .expect("configure serialize"),
                        )
                        .await;
                    session.shutdown().await;
                    validation?;
                    source.config = migrated;
                    validate_source(source)?;
                }
                let _commit = self.0.commit.lock().await;
                let mut registry = self.registry_clone();
                let directory = self
                    .0
                    .root
                    .join("packages")
                    .join(&id)
                    .join(&prepared.digest);
                if directory.exists() {
                    fs::remove_dir_all(&prepared.directory).map_err(io_error)?;
                } else {
                    fs::create_dir_all(directory.parent().expect("package parent"))
                        .map_err(io_error)?;
                    fs::rename(&prepared.directory, &directory).map_err(io_error)?;
                }
                let installed = InstalledPlugin {
                    manifest: prepared.manifest.clone(),
                    digest: prepared.digest.clone(),
                    preinstalled: old.preinstalled,
                    directory,
                };
                registry.plugins.insert(id, installed.clone());
                for source in sources {
                    let revision = registry
                        .revisions
                        .get(&source.id)
                        .copied()
                        .unwrap_or(0)
                        .checked_add(1)
                        .ok_or_else(|| PluginError::new("invalid_config", "配置版本已达上限"))?;
                    registry.revisions.insert(source.id.clone(), revision);
                    registry.sources.insert(source.id.clone(), source);
                }
                self.save_registry(registry)?;
                drop(guards);
                Ok(installed)
            }
            .await;
        if result.is_err() && prepared.directory.exists() {
            let _ = fs::remove_dir_all(&prepared.directory);
        }
        result
    }

    pub async fn uninstall(&self, plugin_id: &str, clear_data: bool) -> Result<(), PluginError> {
        let _transaction = self
            .0
            .package_transaction
            .try_lock()
            .map_err(|_| package_busy())?;
        let _package = self.package_write(plugin_id)?;
        let registry = self.registry_clone();
        if !registry.plugins.contains_key(plugin_id) {
            return Err(PluginError::new("plugin_not_found", "插件未安装"));
        }
        let sources: Vec<_> = registry
            .sources
            .values()
            .filter(|source| source.plugin_id == plugin_id)
            .map(|source| source.id.clone())
            .collect();
        let mut guards = Vec::new();
        for id in &sources {
            guards.push(
                self.gate(id)?
                    .try_lock_owned()
                    .map_err(|_| package_busy())?,
            );
        }
        for id in &sources {
            self.stop_locked(id).await;
        }
        let _commit = self.0.commit.lock().await;
        self.ensure_running()?;
        let mut paths = vec![self.0.root.join("packages").join(plugin_id)];
        if clear_data {
            paths.extend(sources.iter().map(|id| self.0.root.join("data").join(id)));
        }
        let quarantined = self.quarantine(paths)?;
        let mut registry = self.registry_clone();
        registry.plugins.remove(plugin_id);
        if clear_data {
            registry
                .sources
                .retain(|_, source| source.plugin_id != plugin_id);
            for id in &sources {
                registry.revisions.remove(id);
            }
        }
        if let Err(error) = self.save_registry(registry) {
            quarantined.restore()?;
            return Err(error);
        }
        if clear_data {
            for id in sources {
                self.0
                    .live
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&id);
            }
        }
        quarantined.discard();
        Ok(())
    }

    pub async fn create_source(&self, source: SourceSpec) -> Result<SourceState, PluginError> {
        let _package = self.package_read(&source.plugin_id)?;
        validate_source(&source)?;
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        if !registry.plugins.contains_key(&source.plugin_id) {
            return Err(PluginError::new("plugin_not_found", "插件未安装"));
        }
        if registry.sources.contains_key(&source.id) {
            return Err(PluginError::new("source_exists", "输入源实例 ID 已存在"));
        }
        if registry.sources.len() >= 128 {
            return Err(PluginError::new("queue_busy", "输入源实例数量超过 128"));
        }
        registry.sources.insert(source.id.clone(), source.clone());
        registry.revisions.insert(source.id.clone(), 0);
        self.save_registry(registry)?;
        self.0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(source.id.clone(), LiveEntry::default());
        Ok(SourceState {
            spec: source,
            revision: 0,
            status: SourceStatus::Stopped,
            state: empty_object(),
            last_error: None,
        })
    }

    pub async fn set_enabled(&self, source_id: &str, enabled: bool) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        if !enabled {
            self.stop_locked(source_id).await;
        }
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        registry
            .sources
            .get_mut(source_id)
            .ok_or_else(source_not_found)?
            .enabled = enabled;
        self.save_registry(registry)
    }

    pub async fn rename_source(&self, source_id: &str, name: String) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        let _guard = self
            .gate(source_id)?
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        if name.trim().is_empty() || name.len() > 256 {
            return Err(PluginError::new("invalid_params", "输入源名称无效"));
        }
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        registry
            .sources
            .get_mut(source_id)
            .ok_or_else(source_not_found)?
            .name = name;
        self.save_registry(registry)
    }

    pub async fn delete_source(
        &self,
        source_id: &str,
        clear_data: bool,
    ) -> Result<(), PluginError> {
        let _package = self.source_package_read(source_id)?;
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.stop_locked(source_id).await;
        let _commit = self.0.commit.lock().await;
        let quarantined = if clear_data {
            Some(self.quarantine(vec![self.0.root.join("data").join(source_id)])?)
        } else {
            None
        };
        let mut registry = self.registry_clone();
        registry
            .sources
            .remove(source_id)
            .ok_or_else(source_not_found)?;
        registry.revisions.remove(source_id);
        if let Err(error) = self.save_registry(registry) {
            if let Some(quarantined) = quarantined {
                quarantined.restore()?;
            }
            return Err(error);
        }
        self.0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(source_id);
        if let Some(quarantined) = quarantined {
            quarantined.discard();
        }
        Ok(())
    }

    /// First initialization only. Tombstones survive uninstall and application restart.
    pub async fn seed_preinstalled(
        &self,
        packages: Vec<(PathBuf, SourceSpec)>,
    ) -> Result<(), PluginError> {
        for (path, source) in packages {
            if self.registry_clone().seeded.contains(&source.plugin_id) {
                continue;
            }
            if !self
                .registry_clone()
                .plugins
                .contains_key(&source.plugin_id)
            {
                self.install(path, true).await?;
            }
            if !self.registry_clone().sources.contains_key(&source.id) {
                self.create_source(source.clone()).await?;
            }
            let _commit = self.0.commit.lock().await;
            let mut registry = self.registry_clone();
            registry.seeded.insert(source.plugin_id);
            self.save_registry(registry)?;
        }
        Ok(())
    }

    fn definition(&self, source_id: &str) -> Result<(SourceSpec, InstalledPlugin), PluginError> {
        let registry = self
            .0
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let source = registry
            .sources
            .get(source_id)
            .cloned()
            .ok_or_else(source_not_found)?;
        let plugin = registry
            .plugins
            .get(&source.plugin_id)
            .cloned()
            .ok_or_else(|| PluginError::new("plugin_not_found", "输入源所属插件未安装"))?;
        Ok((source, plugin))
    }

    fn gate(&self, source_id: &str) -> Result<Arc<Mutex<()>>, PluginError> {
        self.ensure_running()?;
        self.0
            .live
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(source_id)
            .map(|entry| entry.gate.clone())
            .ok_or_else(source_not_found)
    }

    fn package_gate(&self, plugin_id: &str) -> Arc<tokio::sync::RwLock<()>> {
        let mut gates = self
            .0
            .package_gates
            .write()
            .unwrap_or_else(|p| p.into_inner());
        // Owned read/write guards retain their Arc. Keep their shared gate, but
        // reclaim IDs from completed, failed and uninstalled package operations.
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(plugin_id).and_then(Weak::upgrade) {
            return gate;
        }
        let gate = Arc::new(tokio::sync::RwLock::new(()));
        gates.insert(plugin_id.to_owned(), Arc::downgrade(&gate));
        gate
    }

    fn package_read(&self, plugin_id: &str) -> Result<OwnedRwLockReadGuard<()>, PluginError> {
        self.ensure_running()?;
        self.package_gate(plugin_id)
            .try_read_owned()
            .map_err(|_| package_busy())
    }

    fn source_package_read(
        &self,
        source_id: &str,
    ) -> Result<OwnedRwLockReadGuard<()>, PluginError> {
        let id = self
            .0
            .registry
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .sources
            .get(source_id)
            .map(|source| source.plugin_id.clone())
            .ok_or_else(source_not_found)?;
        self.package_read(&id)
    }

    fn package_write(&self, plugin_id: &str) -> Result<OwnedRwLockWriteGuard<()>, PluginError> {
        self.ensure_running()?;
        self.package_gate(plugin_id)
            .try_write_owned()
            .map_err(|_| package_busy())
    }

    fn ensure_running(&self) -> Result<(), PluginError> {
        if self.0.stopping.load(Ordering::Acquire) {
            return Err(PluginError::new("core_shutting_down", "核心正在关闭"));
        }
        Ok(())
    }

    fn registry_clone(&self) -> Registry {
        self.0
            .registry
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn save_registry(&self, registry: Registry) -> Result<(), PluginError> {
        self.ensure_running()?;
        let bytes = serde_json::to_vec_pretty(&registry)
            .map_err(|error| PluginError::new("invalid_config", error.to_string()))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(PluginError::new("invalid_config", "插件注册表超过容量限制"));
        }
        let revision = self
            .cached_catalog_snapshot()
            .revision
            .checked_add(1)
            .ok_or_else(|| PluginError::new("invalid_config", "插件目录版本已达上限"))?;
        let catalog = Arc::new(PluginCatalogSnapshot::from_registry(&registry, revision));
        let temporary = self
            .0
            .root
            .join(format!("registry-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| {
            let mut file = File::options()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(io_error)?;
            file.write_all(&bytes).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            fs::rename(&temporary, self.0.root.join("registry.json")).map_err(io_error)?;
            let mut current = self.0.registry.write().unwrap_or_else(|p| p.into_inner());
            *current = registry;
            *self.0.catalog.write().unwrap_or_else(|p| p.into_inner()) = catalog;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    async fn prepare(&self, path: PathBuf) -> Result<PreparedPackage, PluginError> {
        let staging = self.0.root.join("staging");
        tokio::task::spawn_blocking(move || prepare_package(&path, &staging))
            .await
            .map_err(|error| PluginError::new("plugin_io", error.to_string()))?
    }

    async fn publish_package(
        &self,
        prepared: PreparedPackage,
        preinstalled: bool,
        replace: bool,
    ) -> Result<InstalledPlugin, PluginError> {
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        if registry.plugins.contains_key(&prepared.manifest.id) && !replace {
            let _ = fs::remove_dir_all(&prepared.directory);
            return Err(PluginError::new(
                "plugin_exists",
                "插件已安装，请使用更新操作",
            ));
        }
        let directory = self
            .0
            .root
            .join("packages")
            .join(&prepared.manifest.id)
            .join(&prepared.digest);
        if directory.exists() {
            fs::remove_dir_all(&prepared.directory).map_err(io_error)?;
        } else {
            fs::create_dir_all(directory.parent().expect("package parent")).map_err(io_error)?;
            fs::rename(&prepared.directory, &directory).map_err(io_error)?;
        }
        let plugin = InstalledPlugin {
            manifest: prepared.manifest,
            digest: prepared.digest,
            preinstalled,
            directory,
        };
        registry
            .plugins
            .insert(plugin.manifest.id.clone(), plugin.clone());
        self.save_registry(registry)?;
        Ok(plugin)
    }

    fn quarantine(&self, paths: Vec<PathBuf>) -> Result<Quarantine, PluginError> {
        let root = self
            .0
            .root
            .join("staging")
            .join(format!("removed-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).map_err(io_error)?;
        let mut quarantine = Quarantine {
            root,
            moves: Vec::new(),
        };
        for (index, original) in paths.into_iter().enumerate() {
            if !original.exists() {
                continue;
            }
            let staged = quarantine.root.join(index.to_string());
            if let Err(error) = fs::rename(&original, &staged) {
                quarantine.restore()?;
                return Err(io_error(error));
            }
            quarantine.moves.push((original, staged));
        }
        Ok(quarantine)
    }
}

/// Move directories before committing metadata; failures restore the previous installation.
struct Quarantine {
    root: PathBuf,
    moves: Vec<(PathBuf, PathBuf)>,
}

impl Quarantine {
    fn restore(self) -> Result<(), PluginError> {
        for (original, staged) in self.moves.into_iter().rev() {
            fs::rename(staged, original).map_err(|error| {
                PluginError::new("rollback_failed", format!("插件目录恢复失败：{error}"))
            })?;
        }
        let _ = fs::remove_dir(self.root);
        Ok(())
    }

    fn discard(self) {
        // Logical deletion is committed; cleanup failure must not report a false rollback.
        tokio::task::spawn_blocking(move || {
            let _ = fs::remove_dir_all(self.root);
        });
    }
}

fn bounded_error(error: PluginError) -> PluginError {
    PluginError::new(
        error.code.chars().take(128).collect::<String>(),
        error.message.chars().take(1024).collect::<String>(),
    )
}

fn source_not_found() -> PluginError {
    PluginError::new("source_not_found", "输入源实例不存在")
}
fn package_busy() -> PluginError {
    PluginError::new(
        "plugin_busy",
        "插件正在更新、卸载或处理其他操作，请稍后重试",
    )
}
fn validate_id(id: &str) -> Result<(), PluginError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(PluginError::new("invalid_params", "输入源实例 ID 无效"));
    }
    crate::package::validate_relative_path(id).map(|_| ())
}
fn validate_source(source: &SourceSpec) -> Result<(), PluginError> {
    validate_id(&source.id)?;
    if source.name.trim().is_empty()
        || source.name.len() > 256
        || serde_json::to_vec(&source.config)
            .map_err(|error| PluginError::new("invalid_config", error.to_string()))?
            .len()
            > 1024 * 1024
    {
        return Err(PluginError::new("invalid_config", "输入源名称或配置无效"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_owned_guards_share_one_gate_and_unused_gates_are_reclaimed() {
        let temp = tempfile::tempdir().unwrap();
        let manager = PluginManager::open(temp.path()).unwrap();
        let first = manager.package_read("example.active").unwrap();
        let original = manager.0.package_gates.read().unwrap()["example.active"].clone();
        let second = manager.package_read("example.active").unwrap();
        assert!(Weak::ptr_eq(
            &original,
            &manager.0.package_gates.read().unwrap()["example.active"]
        ));
        assert_eq!(
            manager.package_write("example.active").unwrap_err().code,
            "plugin_busy"
        );
        drop(first);
        assert_eq!(
            manager.package_write("example.active").unwrap_err().code,
            "plugin_busy"
        );
        drop(second);
        assert!(original.upgrade().is_none());

        let writer = manager.package_write("example.active").unwrap();
        let replacement = manager.0.package_gates.read().unwrap()["example.active"].clone();
        assert!(!Weak::ptr_eq(&original, &replacement));
        for index in 0..128 {
            drop(
                manager
                    .package_read(&format!("example.finished-{index}"))
                    .unwrap(),
            );
            assert_eq!(
                manager.package_read("example.active").unwrap_err().code,
                "plugin_busy"
            );
            assert!(Weak::ptr_eq(
                &replacement,
                &manager.0.package_gates.read().unwrap()["example.active"]
            ));
            assert!(manager.0.package_gates.read().unwrap().len() <= 2);
        }
        drop(writer);
        assert!(replacement.upgrade().is_none());
        let _new = manager.package_read("example.new").unwrap();
        let gates = manager.0.package_gates.read().unwrap();
        assert_eq!(gates.len(), 1);
        assert!(!gates.contains_key("example.active"));
    }

    #[tokio::test]
    async fn nonexistent_package_requests_do_not_accumulate_lifecycle_locks() {
        let temp = tempfile::tempdir().unwrap();
        let manager = PluginManager::open(temp.path()).unwrap();
        let _active = manager.package_read("example.active").unwrap();
        for index in 0..1024 {
            let error = manager
                .create_source(SourceSpec {
                    id: format!("source-{index}"),
                    plugin_id: format!("example.missing-{index}"),
                    name: "Missing".into(),
                    enabled: true,
                    config: empty_object(),
                })
                .await
                .unwrap_err();
            assert_eq!(error.code, "plugin_not_found");
            let gates = manager.0.package_gates.read().unwrap();
            assert!(gates.len() <= 2);
            assert!(gates["example.active"].upgrade().is_some());
        }
        assert!(manager.cached_catalog_snapshot().sources.is_empty());
    }

    fn binding(id: &str, channel: Channel) -> Binding {
        Binding {
            binding_id: id.into(),
            control_id: "device".into(),
            channel,
            generation: 1,
            config: empty_object(),
            active: true,
        }
    }

    #[test]
    fn binding_identity_changes_invalidate_only_affected_inputs() {
        let first = binding("device/a", Channel::A);
        let second = binding("device/b", Channel::B);
        let old = vec![first.clone(), second.clone()];
        let mut config_only = first.clone();
        config_only.config = serde_json::json!({"frequency":100});
        assert!(invalidated_bindings(&old, &[config_only, second.clone()]).is_empty());
        assert_eq!(
            invalidated_bindings(&old, std::slice::from_ref(&second)),
            HashSet::from([first.binding_id.clone()])
        );
        for changed in [
            Binding {
                generation: 2,
                ..first.clone()
            },
            Binding {
                active: false,
                ..first.clone()
            },
            Binding {
                control_id: "another-device".into(),
                ..first.clone()
            },
            Binding {
                channel: Channel::B,
                ..first.clone()
            },
        ] {
            assert_eq!(
                invalidated_bindings(&old, &[changed, second.clone()]),
                HashSet::from([first.binding_id.clone()])
            );
        }
        let mut cache = InputSequenceCache::default();
        let now = Instant::now();
        cache.record_scoped("a-owner".into(), 9, now, Some("device/a"));
        cache.record_scoped("b-owner".into(), 9, now, Some("device/b"));
        cache.record_scoped("instance-owner".into(), 9, now, None);
        cache.invalidate_bindings(&HashSet::from([first.binding_id]));
        assert!(cache.will_accept("a-owner", 1, now).unwrap());
        assert!(!cache.will_accept("b-owner", 8, now).unwrap());
        assert!(cache.will_accept("instance-owner", 1, now).unwrap());
    }

    #[test]
    fn old_ui_owners_are_reclaimed_without_evicting_an_active_owner() {
        let mut cache = InputSequenceCache::default();
        let start = Instant::now();
        for owner in 0..MAX_INPUT_OWNERS {
            let key = format!("owner-{owner}");
            assert!(cache.will_accept(&key, 1, start).unwrap());
            cache.record(key, 1, start);
        }
        let active = start + Duration::from_millis(900);
        assert!(cache.will_accept("owner-0", 2, active).unwrap());
        cache.record("owner-0".into(), 2, active);
        let reopened = start + Duration::from_millis(1001);
        for owner in MAX_INPUT_OWNERS..(MAX_INPUT_OWNERS * 2 - 1) {
            let key = format!("owner-{owner}");
            assert!(cache.will_accept(&key, 1, reopened).unwrap());
            cache.record(key, 1, reopened);
            assert!(!cache.will_accept("owner-0", 1, reopened).unwrap());
            assert!(cache.0.len() <= MAX_INPUT_OWNERS);
        }
        assert!(cache.will_accept("owner-0", 3, reopened).unwrap());
        cache.record("owner-0".into(), 3, reopened);
        assert!(!cache.will_accept("owner-0", 2, reopened).unwrap());
    }

    #[test]
    fn all_active_owners_are_bounded_then_recover_after_idle_time() {
        let mut cache = InputSequenceCache::default();
        let start = Instant::now();
        for owner in 0..MAX_INPUT_OWNERS {
            cache.record(owner.to_string(), 10, start);
        }
        assert_eq!(
            cache.will_accept("new", 1, start).unwrap_err().code,
            "queue_busy"
        );
        assert!(
            cache
                .will_accept("new", 1, start + INPUT_OWNER_IDLE)
                .unwrap()
        );
        cache.record("new".into(), 1, start + INPUT_OWNER_IDLE);
        let later = start + INPUT_SEQUENCE_RETENTION + INPUT_OWNER_IDLE;
        assert!(cache.will_accept("fresh", 1, later).unwrap());
        assert!(cache.0.is_empty());
    }

    #[test]
    fn an_unqueued_input_does_not_advance_the_sequence() {
        let mut cache = InputSequenceCache::default();
        let now = Instant::now();
        assert!(cache.will_accept("owner", 5, now).unwrap());
        // Simulate a full IPC mailbox: callers do not record a rejected enqueue.
        assert!(cache.will_accept("owner", 5, now).unwrap());
        cache.record("owner".into(), 5, now);
        assert!(!cache.will_accept("owner", 5, now).unwrap());
        assert!(!cache.will_accept("owner", 4, now).unwrap());
    }

    #[tokio::test]
    async fn registry_persists_stopped_instances_and_uninstall_tombstone() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("payload");
        fs::create_dir_all(&directory).unwrap();
        let manifest = PluginManifest {
            id: "example.source".into(),
            version: "1.0.0".into(),
            protocol_version: PROTOCOL_VERSION,
            name: "示例".into(),
            publisher: "Example".into(),
            license: "MIT".into(),
            executable: "source.exe".into(),
        };
        fs::write(
            directory.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            directory.join("source.exe"),
            crate::package::executable_fixture(),
        )
        .unwrap();
        let package = temp.path().join("source.dglabplugin");
        crate::package::pack_directory(&directory, &package).unwrap();
        let root = temp.path().join("host");
        let manager = PluginManager::open(&root).unwrap();
        let source = SourceSpec {
            id: "source-example".into(),
            plugin_id: manifest.id.clone(),
            name: "输入源".into(),
            enabled: true,
            config: serde_json::json!({"foo":1}),
        };
        manager
            .seed_preinstalled(vec![(package.clone(), source.clone())])
            .await
            .unwrap();
        assert_eq!(manager.snapshot().plugins.len(), 1);
        let catalog = manager.cached_catalog_snapshot();
        assert!(Arc::ptr_eq(&catalog, &manager.cached_catalog_snapshot()));
        let states = manager.runtime_states();
        assert_eq!(states[0].revision, 0);
        assert!(
            serde_json::to_value(&states).unwrap()[0]
                .get("config")
                .is_none()
        );
        manager
            .rename_source(&source.id, "改名".into())
            .await
            .unwrap();
        let renamed = manager.cached_catalog_snapshot();
        assert!(!Arc::ptr_eq(&catalog, &renamed));
        assert!(renamed.revision > catalog.revision);
        assert_eq!(renamed.source_revisions[&source.id], 0);
        assert_eq!(catalog.sources[0].name, "输入源");
        assert_eq!(renamed.sources[0].name, "改名");
        manager.uninstall(&manifest.id, false).await.unwrap();
        let manager = PluginManager::open(&root).unwrap();
        manager
            .seed_preinstalled(vec![(package, source)])
            .await
            .unwrap();
        let snapshot = manager.snapshot();
        assert!(snapshot.plugins.is_empty());
        assert_eq!(snapshot.sources[0].spec.name, "改名");
        assert!(matches!(snapshot.sources[0].status, SourceStatus::Stopped));
    }
}
