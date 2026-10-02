use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use dg_lab_link_plugin_sdk::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::frames::LatestFrameStore;
use crate::package::{PreparedPackage, io_error, prepare_package};
use crate::process::{BusinessHandler, HandlerSlot, Session, StateCallback};

const MAX_REGISTRY_BYTES: u64 = 4 * 1024 * 1024;
const MAX_INPUT_OWNERS: usize = 64;
const INPUT_SEQUENCE_RETENTION: Duration = Duration::from_secs(10);
const INPUT_OWNER_IDLE: Duration = Duration::from_secs(1);

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub digest: String,
    pub preinstalled: bool,
    #[serde(skip)]
    pub directory: PathBuf,
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

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Registry {
    #[serde(default)]
    plugins: HashMap<String, InstalledPlugin>,
    #[serde(default)]
    sources: HashMap<String, SourceSpec>,
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
    live: Arc<RwLock<HashMap<String, LiveEntry>>>,
    commit: Mutex<()>,
    package_transaction: Mutex<()>,
    handler: HandlerSlot,
    frames: LatestFrameStore,
    slots: Arc<Semaphore>,
    stopping: AtomicBool,
    shutdown_cancel: CancellationToken,
}

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
        Ok(Self(Arc::new(Inner {
            root,
            registry: RwLock::new(registry),
            live: Arc::new(RwLock::new(live)),
            commit: Mutex::new(()),
            package_transaction: Mutex::new(()),
            handler: Arc::new(RwLock::new(None)),
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
                    status: entry.map_or(SourceStatus::Stopped, |entry| entry.status),
                    state: entry.map_or_else(empty_object, |entry| entry.state.clone()),
                    last_error: entry.and_then(|entry| entry.last_error.clone()),
                }
            })
            .collect();
        sources.sort_by(|first, second| first.spec.id.cmp(&second.spec.id));
        PluginRuntimeSnapshot { plugins, sources }
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
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.start_locked(source_id).await.map(|_| ())
    }

    async fn start_locked(&self, source_id: &str) -> Result<Session, PluginError> {
        self.ensure_running()?;
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
                let mut live = self
                    .0
                    .live
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let entry = live.get_mut(source_id).ok_or_else(source_not_found)?;
                if !session.is_alive() {
                    entry.status = SourceStatus::Faulted;
                    entry.permit = None;
                    return Err(PluginError::new(
                        "plugin_disconnected",
                        "插件在初始化后退出",
                    ));
                }
                entry.status = SourceStatus::Running;
                entry.last_error = None;
                self.0.frames.replace_bindings(source_id, &entry.bindings);
                session.update_bindings(entry.bindings.clone(), &HashSet::new());
                entry.session = Some(session.clone());
                Ok(session)
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

    pub async fn configure(&self, source_id: &str, config: Value) -> Result<Value, PluginError> {
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

    pub async fn action(
        &self,
        source_id: &str,
        params: ActionParams,
    ) -> Result<Value, PluginError> {
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
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
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
        let _transaction = self.0.package_transaction.lock().await;
        let prepared = self.prepare(path.as_ref().to_path_buf()).await?;
        self.publish_package(prepared, preinstalled, false).await
    }

    pub async fn update(&self, path: impl AsRef<Path>) -> Result<InstalledPlugin, PluginError> {
        let _transaction = self.0.package_transaction.lock().await;
        let prepared = self.prepare(path.as_ref().to_path_buf()).await?;
        let id = prepared.manifest.id.clone();
        let result =
            async {
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
        let _transaction = self.0.package_transaction.lock().await;
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
        for id in &sources {
            self.stop(id).await?;
        }
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        registry.plugins.remove(plugin_id);
        if clear_data {
            registry
                .sources
                .retain(|_, source| source.plugin_id != plugin_id);
        }
        self.save_registry(registry)?;
        if clear_data {
            for id in sources {
                self.0
                    .live
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&id);
                self.remove_data(&id)?;
            }
        }
        let package_directory = self.0.root.join("packages").join(plugin_id);
        tokio::task::spawn_blocking(move || {
            if package_directory.exists() {
                fs::remove_dir_all(package_directory).map_err(io_error)?;
            }
            Ok::<(), PluginError>(())
        })
        .await
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))??;
        Ok(())
    }

    pub async fn create_source(&self, source: SourceSpec) -> Result<SourceState, PluginError> {
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
        self.save_registry(registry)?;
        self.0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(source.id.clone(), LiveEntry::default());
        Ok(SourceState {
            spec: source,
            status: SourceStatus::Stopped,
            state: empty_object(),
            last_error: None,
        })
    }

    pub async fn set_enabled(&self, source_id: &str, enabled: bool) -> Result<(), PluginError> {
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
        let gate = self.gate(source_id)?;
        let _guard = gate
            .try_lock_owned()
            .map_err(|_| PluginError::new("queue_busy", "该输入源正在处理其他操作"))?;
        self.stop_locked(source_id).await;
        let _commit = self.0.commit.lock().await;
        let mut registry = self.registry_clone();
        registry
            .sources
            .remove(source_id)
            .ok_or_else(source_not_found)?;
        self.save_registry(registry)?;
        self.0
            .live
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(source_id);
        if clear_data {
            self.remove_data(source_id)?;
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
        let bytes = serde_json::to_vec_pretty(&registry)
            .map_err(|error| PluginError::new("invalid_config", error.to_string()))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err(PluginError::new("invalid_config", "插件注册表超过容量限制"));
        }
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
            *self
                .0
                .registry
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = registry;
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

    fn remove_data(&self, source_id: &str) -> Result<(), PluginError> {
        validate_id(source_id)?;
        let path = self.0.root.join("data").join(source_id);
        if path.exists() {
            fs::remove_dir_all(path).map_err(io_error)?;
        }
        Ok(())
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
            protocol_version: 1,
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
        manager
            .rename_source(&source.id, "改名".into())
            .await
            .unwrap();
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
