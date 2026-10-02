use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use dg_lab_link_plugin_sdk::protocol::{Message, read_message, write_message};
use dg_lab_link_plugin_sdk::*;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::LatestFrameStore;

pub type BusinessFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, PluginError>> + Send + 'a>>;
pub trait BusinessHandler: Send + Sync + 'static {
    fn call<'a>(
        &'a self,
        source_id: &'a str,
        command: Value,
        operation_epoch: Option<u64>,
    ) -> BusinessFuture<'a>;
    fn begin_operation<'a>(&'a self, _source_id: &'a str) -> BusinessFuture<'a> {
        Box::pin(async {
            Err(PluginError::new(
                "context_unavailable",
                "核心不支持主动操作上下文",
            ))
        })
    }
}

pub(crate) type HandlerSlot = Arc<RwLock<Option<Arc<dyn BusinessHandler>>>>;
pub(crate) type StateCallback = Arc<dyn Fn(&str, Option<PluginError>, Option<Value>) + Send + Sync>;
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, PluginError>>>>>;
type InputMailbox = Arc<Mutex<HashMap<String, (InputParams, Option<u64>)>>>;

#[derive(Clone)]
pub(crate) struct Session {
    sender: mpsc::Sender<Message>,
    pending: Pending,
    request_id: Arc<AtomicU64>,
    cancellation: CancellationToken,
    bindings: Arc<Mutex<Option<Vec<Binding>>>>,
    bindings_ready: Arc<Notify>,
    inputs: InputMailbox,
    input_ready: Arc<Notify>,
    finished: Arc<Notify>,
    exited: Arc<std::sync::atomic::AtomicBool>,
    initialized_config: Value,
}

impl Session {
    // A private constructor keeps per-process transport, authority and lifetime
    // explicit instead of introducing shared global runtime state.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn(
        executable: &Path,
        data_directory: &Path,
        mut source: SourceSpec,
        frames: LatestFrameStore,
        handler: HandlerSlot,
        state: StateCallback,
        migration_from: Option<String>,
        startup_cancel: CancellationToken,
    ) -> Result<Self, PluginError> {
        if startup_cancel.is_cancelled() {
            return Err(PluginError::new("core_shutting_down", "插件启动已取消"));
        }
        let mut command = tokio::process::Command::new(executable);
        command
            .current_dir(executable.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = command
            .spawn()
            .map_err(|error| PluginError::new("plugin_start_failed", error.to_string()))?;
        #[cfg(target_os = "windows")]
        let job = WindowsJob::attach(
            child
                .id()
                .ok_or_else(|| PluginError::new("plugin_start_failed", "插件进程没有 PID"))?,
        )?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| PluginError::new("plugin_start_failed", "缺少插件标准输出"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| PluginError::new("plugin_start_failed", "缺少插件标准输入"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| PluginError::new("plugin_start_failed", "缺少插件标准错误"))?;
        let (sender, mut queue) = mpsc::channel(64);
        let mut session = Self {
            sender: sender.clone(),
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_id: Arc::new(AtomicU64::new(0)),
            cancellation: CancellationToken::new(),
            bindings: Arc::new(Mutex::new(None)),
            bindings_ready: Arc::new(Notify::new()),
            inputs: Arc::new(Mutex::new(HashMap::new())),
            input_ready: Arc::new(Notify::new()),
            finished: Arc::new(Notify::new()),
            exited: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            initialized_config: source.config.clone(),
        };
        let lease = uuid::Uuid::new_v4().to_string();
        frames.begin_source(&source.id, &lease);
        let writer_session = session.clone();
        let writer_state = state.clone();
        let writer_source = source.id.clone();
        let writer = tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    biased;
                    _ = writer_session.cancellation.cancelled() => break,
                    Some(message) = queue.recv() => Some(message),
                    _ = writer_session.bindings_ready.notified() => {
                        writer_session.bindings.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take()
                            .map(|bindings| Message::Notification { method: "bindings".into(), params: serde_json::to_value(bindings).expect("bindings serialize"), operation_epoch: None })
                    }
                    _ = writer_session.input_ready.notified() => {
                        let input = {
                            let mut inputs = writer_session.inputs.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                            let key = inputs.keys().next().cloned();
                            key.and_then(|key| inputs.remove(&key))
                        };
                        if !writer_session.inputs.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).is_empty() { writer_session.input_ready.notify_one(); }
                        input.map(|(params, operation_epoch)| Message::Notification { method: "input".into(), params: serde_json::to_value(params).expect("input serialize"), operation_epoch })
                    }
                };
                let Some(message) = message else {
                    continue;
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(1),
                    write_message(&mut stdin, &message),
                )
                .await;
                let error = match result {
                    Ok(Ok(())) => continue,
                    Ok(Err(error)) => error,
                    Err(_) => PluginError::new("plugin_io", "插件输入管道超时"),
                };
                writer_state(&writer_source, Some(error), None);
                writer_session.cancellation.cancel();
                break;
            }
        });
        let reader_session = session.clone();
        let reader_source = source.id.clone();
        let reader_state = state.clone();
        let reader_frames = frames.clone();
        let reader_lease = lease.clone();
        let reader = tokio::spawn(async move {
            let calls = Arc::new(Semaphore::new(16));
            loop {
                let message = tokio::select! {
                    _ = reader_session.cancellation.cancelled() => break,
                    message = read_message(&mut stdout) => message,
                };
                match message {
                    Ok(Some(Message::Response { id, result, error })) => {
                        if let Some(sender) = reader_session
                            .pending
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .remove(&id)
                        {
                            let _ = sender.send(error.map_or(Ok(result), Err));
                        }
                    }
                    Ok(Some(Message::Notification { method, params, .. })) => match method.as_str()
                    {
                        "frame" => {
                            if let Ok(notification) = serde_json::from_value(params) {
                                reader_frames.push_leased(
                                    &reader_source,
                                    &reader_lease,
                                    notification,
                                );
                            }
                        }
                        "status"
                            if serde_json::to_vec(&params)
                                .is_ok_and(|bytes| bytes.len() <= 64 * 1024) =>
                        {
                            reader_state(&reader_source, None, Some(params))
                        }
                        _ => {}
                    },
                    Ok(Some(Message::Request {
                        id,
                        method,
                        params,
                        operation_epoch,
                    })) => {
                        let handler = handler
                            .read()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .clone();
                        let permit = calls.clone().try_acquire_owned();
                        let callback_sender = reader_session.sender.clone();
                        let callback_source = reader_source.clone();
                        let callback_cancel = reader_session.cancellation.clone();
                        tokio::spawn(async move {
                            let response = match (method.as_str(), handler, permit) {
                                (
                                    "core.call" | "core.begin_operation",
                                    Some(handler),
                                    Ok(_permit),
                                ) => {
                                    let request = if method == "core.begin_operation" {
                                        handler.begin_operation(&callback_source)
                                    } else {
                                        handler.call(&callback_source, params, operation_epoch)
                                    };
                                    tokio::select! {
                                        _ = callback_cancel.cancelled() => return,
                                        result = tokio::time::timeout(Duration::from_secs(15), request) => {
                                            result.unwrap_or_else(|_| Err(PluginError::new("request_timeout", "核心业务调用超时")))
                                        }
                                    }
                                }
                                ("core.call" | "core.begin_operation", None, _) => Err(
                                    PluginError::new("core_unavailable", "核心业务接口尚未就绪"),
                                ),
                                (_, _, Err(_)) => {
                                    Err(PluginError::new("queue_busy", "插件业务请求超过容量"))
                                }
                                _ => Err(PluginError::new(
                                    "method_not_found",
                                    "未知插件反向请求方法",
                                )),
                            };
                            let (result, error) = response.map_or_else(
                                |error| (Value::Null, Some(error)),
                                |value| (value, None),
                            );
                            if callback_sender
                                .try_send(Message::Response { id, result, error })
                                .is_err()
                            {
                                callback_cancel.cancel();
                            }
                        });
                    }
                    Ok(None) => {
                        if !reader_session.cancellation.is_cancelled() {
                            reader_state(
                                &reader_source,
                                Some(PluginError::new(
                                    "plugin_disconnected",
                                    "插件进程关闭了输出管道",
                                )),
                                None,
                            );
                        }
                        reader_session.cancellation.cancel();
                        break;
                    }
                    Err(error) => {
                        reader_state(&reader_source, Some(error), None);
                        reader_session.cancellation.cancel();
                        break;
                    }
                }
            }
            reader_session
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
        });
        let stderr_session = session.clone();
        let stderr_source = source.id.clone();
        let stderr_state = state.clone();
        let stderr_reader = tokio::spawn(async move {
            // read_until is limited with fill_buf: an unbroken line cannot allocate unbounded memory.
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                let chunk = tokio::select! {
                    _ = stderr_session.cancellation.cancelled() => break,
                    chunk = reader.fill_buf() => match chunk { Ok(chunk) => chunk, Err(_) => break },
                };
                if chunk.is_empty() {
                    break;
                }
                let length = chunk
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(chunk.len(), |index| index + 1);
                let remaining = 4096_usize.saturating_sub(line.len());
                line.extend_from_slice(&chunk[..length.min(remaining)]);
                let newline = chunk[length - 1] == b'\n';
                reader.consume(length);
                if newline {
                    stderr_state(
                        &stderr_source,
                        None,
                        Some(serde_json::json!({"log":String::from_utf8_lossy(&line).trim_end()})),
                    );
                    line.clear();
                }
            }
        });
        let monitor_session = session.clone();
        let monitor_source = source.id.clone();
        tokio::spawn(async move {
            #[cfg(target_os = "windows")]
            let _job = job;
            let result = tokio::select! {
                result = child.wait() => result,
                _ = monitor_session.cancellation.cancelled() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            if !monitor_session.cancellation.is_cancelled() {
                let message = result.map_or_else(
                    |error| error.to_string(),
                    |status| format!("插件进程退出：{status}"),
                );
                state(
                    &monitor_source,
                    Some(PluginError::new("plugin_exited", message)),
                    None,
                );
            }
            monitor_session.cancellation.cancel();
            monitor_session
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clear();
            frames.clear_leased(&monitor_source, &lease);
            writer.abort();
            reader.abort();
            stderr_reader.abort();
            monitor_session.exited.store(true, Ordering::Release);
            monitor_session.finished.notify_waiters();
        });
        if let Some(from_version) = migration_from {
            let request = session.request(
                "migrate",
                serde_json::to_value(MigrateParams {
                    from_version,
                    config: source.config,
                })
                .expect("migration serialize"),
            );
            let result = tokio::select! {
                _ = startup_cancel.cancelled() => Err(PluginError::new("core_shutting_down", "插件启动已取消")),
                result = request => result,
            };
            match result {
                Ok(config) => {
                    source.config = config.clone();
                    session.initialized_config = config;
                }
                Err(error) => {
                    session.cancellation.cancel();
                    return Err(error);
                }
            }
        }
        let params = InitializeParams {
            protocol_version: PROTOCOL_VERSION,
            source,
            data_directory: data_directory.to_string_lossy().into_owned(),
        };
        let request = session.request(
            "initialize",
            serde_json::to_value(params).expect("initialize serialize"),
        );
        let result = tokio::select! {
            _ = startup_cancel.cancelled() => Err(PluginError::new("core_shutting_down", "插件启动已取消")),
            result = request => result,
        };
        if let Err(error) = result {
            session.cancellation.cancel();
            return Err(error);
        }
        Ok(session)
    }

    pub fn initialized_config(&self) -> Value {
        self.initialized_config.clone()
    }

    pub fn is_alive(&self) -> bool {
        !self.cancellation.is_cancelled()
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, PluginError> {
        if !self.is_alive() {
            return Err(PluginError::new("plugin_disconnected", "插件进程未运行"));
        }
        let id = self.request_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if pending.len() >= 32 {
                return Err(PluginError::new("queue_busy", "插件请求数量超过容量"));
            }
            pending.insert(id, sender);
        }
        if self
            .sender
            .try_send(Message::Request {
                id,
                method: method.into(),
                params,
                operation_epoch: OPERATION_EPOCH.try_with(|epoch| *epoch).ok(),
            })
            .is_err()
        {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            return Err(PluginError::new("queue_busy", "插件请求队列已满"));
        }
        let result = tokio::time::timeout(Duration::from_secs(5), receiver).await;
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PluginError::new("plugin_disconnected", "插件连接已关闭")),
            Err(_) => Err(PluginError::new(
                "request_timeout",
                "插件请求超时，未自动重试",
            )),
        }
    }

    pub fn update_bindings(&self, bindings: Vec<Binding>, invalidated: &HashSet<String>) {
        if !invalidated.is_empty() {
            self.inputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .retain(|_, (input, _)| {
                    input
                        .binding_id
                        .as_ref()
                        .is_some_and(|id| !invalidated.contains(id))
                });
        }
        // Clear stale pending input before publishing the replacement bindings.
        // The manager's live lock prevents a new input from interleaving here.
        *self
            .bindings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(bindings);
        self.bindings_ready.notify_one();
    }

    pub fn input(&self, params: InputParams) -> Result<(), PluginError> {
        let key = format!(
            "{}:{}:{}",
            params.binding_id.as_deref().unwrap_or(""),
            params.owner,
            params.action
        );
        let mut inputs = self
            .inputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !inputs.contains_key(&key) && inputs.len() >= 64 {
            return Err(PluginError::new("queue_busy", "插件持续输入数量超过容量"));
        }
        if inputs
            .get(&key)
            .is_some_and(|(old, _)| old.sequence >= params.sequence)
        {
            return Ok(());
        }
        inputs.insert(key, (params, OPERATION_EPOCH.try_with(|epoch| *epoch).ok()));
        drop(inputs);
        self.input_ready.notify_one();
        Ok(())
    }

    pub async fn shutdown(&self) {
        if self.is_alive() {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                self.request("shutdown", Value::Null),
            )
            .await;
        }
        self.cancellation.cancel();
        if !self.exited.load(Ordering::Acquire) {
            let notification = self.finished.notified();
            if !self.exited.load(Ordering::Acquire) {
                let _ = tokio::time::timeout(Duration::from_secs(2), notification).await;
            }
        }
    }
}

#[cfg(target_os = "windows")]
struct WindowsJob(usize);

#[cfg(target_os = "windows")]
impl WindowsJob {
    fn attach(pid: u32) -> Result<Self, PluginError> {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::System::JobObjects::*;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };
        // Handles remain owned here; closing the job kills all contained children.
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(PluginError::new(
                    "plugin_start_failed",
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            let job = Self(handle as usize);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                return Err(PluginError::new(
                    "plugin_start_failed",
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            let process: HANDLE = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(PluginError::new(
                    "plugin_start_failed",
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            let assigned = AssignProcessToJobObject(handle, process);
            CloseHandle(process);
            if assigned == 0 {
                return Err(PluginError::new(
                    "plugin_start_failed",
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            Ok(job)
        }
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(
                self.0 as windows_sys::Win32::Foundation::HANDLE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let (sender, _) = mpsc::channel(64);
        Session {
            sender,
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_id: Arc::new(AtomicU64::new(0)),
            cancellation: CancellationToken::new(),
            bindings: Arc::new(Mutex::new(None)),
            bindings_ready: Arc::new(Notify::new()),
            inputs: Arc::new(Mutex::new(HashMap::new())),
            input_ready: Arc::new(Notify::new()),
            finished: Arc::new(Notify::new()),
            exited: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            initialized_config: Value::Null,
        }
    }

    fn input(binding_id: Option<&str>) -> InputParams {
        InputParams {
            action: "touch".into(),
            owner: "pointer-owner".into(),
            sequence: 8,
            binding_id: binding_id.map(str::to_owned),
            value: serde_json::json!({"pointers":[{"x":0.5,"y":0.5}]}),
        }
    }

    fn bindings() -> Vec<Binding> {
        vec![
            Binding {
                binding_id: "device/a".into(),
                control_id: "device".into(),
                channel: Channel::A,
                generation: 2,
                config: empty_object(),
                active: true,
            },
            Binding {
                binding_id: "device/b".into(),
                control_id: "device".into(),
                channel: Channel::B,
                generation: 1,
                config: empty_object(),
                active: true,
            },
        ]
    }

    #[test]
    fn publishing_new_binding_clears_stale_channel_and_instance_input_but_keeps_other_channel() {
        let session = session();
        session.input(input(Some("device/a"))).unwrap();
        session.input(input(Some("device/b"))).unwrap();
        session.input(input(None)).unwrap();
        session.update_bindings(bindings(), &HashSet::from(["device/a".into()]));
        let pending = session.inputs.lock().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending.values().next().unwrap().0.binding_id.as_deref(),
            Some("device/b")
        );
        assert_eq!(pending.values().next().unwrap().0.sequence, 8);
        drop(pending);
        assert_eq!(
            session.bindings.lock().unwrap().as_ref().unwrap()[0].generation,
            2
        );
    }

    #[test]
    fn an_update_without_invalidated_bindings_preserves_all_pending_input() {
        let session = session();
        session.input(input(Some("device/a"))).unwrap();
        session.input(input(Some("device/b"))).unwrap();
        session.input(input(None)).unwrap();
        session.update_bindings(bindings(), &HashSet::new());
        assert_eq!(session.inputs.lock().unwrap().len(), 3);
    }
}
