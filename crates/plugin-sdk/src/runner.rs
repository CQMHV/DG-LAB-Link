use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::protocol::{Message, read_message_sync, write_message};
use crate::*;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, PluginError>>>>>;

/// Implement one input-source instance. The SDK owns transport and ticks;
/// application-specific input, audio and waveform algorithms stay in the plugin.
#[async_trait]
pub trait Plugin: Send + 'static {
    async fn migrate(
        &mut self,
        params: MigrateParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let _ = context;
        Ok(params.config)
    }
    async fn initialize(
        &mut self,
        params: InitializeParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError>;
    async fn configure(
        &mut self,
        params: ConfigureParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError>;
    async fn bindings(
        &mut self,
        bindings: Vec<Binding>,
        context: &PluginContext,
    ) -> Result<Value, PluginError>;
    async fn action(
        &mut self,
        params: ActionParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError>;
    async fn ui(
        &mut self,
        params: UiParams,
        context: &PluginContext,
    ) -> Result<UiDocument, PluginError>;
    async fn input(
        &mut self,
        params: InputParams,
        context: &PluginContext,
    ) -> Result<Value, PluginError> {
        let _ = (params, context);
        Err(PluginError::new(
            "unsupported_action",
            "插件不支持该持续输入",
        ))
    }
    async fn tick(&mut self, context: &PluginContext) -> Result<(), PluginError> {
        let _ = context;
        Ok(())
    }
    async fn shutdown(&mut self, context: &PluginContext) {
        let _ = context;
    }
}

#[derive(Clone)]
pub struct PluginContext {
    sender: mpsc::Sender<Message>,
    frames: Arc<Mutex<HashMap<String, FrameNotification>>>,
    frame_ready: Arc<Notify>,
    pending: Pending,
    sequence: Arc<AtomicU64>,
    request_id: Arc<AtomicU64>,
}

impl PluginContext {
    /// Latest-wins mailbox: a slow pipe never accumulates old waveform frames.
    pub fn emit_frame(
        &self,
        binding_id: impl Into<String>,
        generation: u64,
        frame: Frame,
    ) -> Result<(), PluginError> {
        frame.validate()?;
        let binding_id = binding_id.into();
        let mut frames = self
            .frames
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !frames.contains_key(&binding_id) && frames.len() >= MAX_BINDINGS {
            return Err(PluginError::new("queue_busy", "插件波形绑定数量已达上限"));
        }
        frames.insert(
            binding_id.clone(),
            FrameNotification {
                binding_id,
                generation,
                sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
                frame,
            },
        );
        drop(frames);
        self.frame_ready.notify_one();
        Ok(())
    }

    pub fn emit_for_bindings(&self, bindings: &[Binding], frame: Frame) -> Result<(), PluginError> {
        for binding in bindings.iter().filter(|binding| binding.active) {
            self.emit_frame(&binding.binding_id, binding.generation, frame)?;
        }
        Ok(())
    }

    pub fn status(&self, value: Value) -> Result<(), PluginError> {
        self.sender
            .try_send(Message::Notification {
                method: "status".into(),
                params: value,
            })
            .map_err(|_| PluginError::new("queue_busy", "插件状态队列已满或关闭"))
    }

    /// Call a typed core business command serialized as JSON. Device writes must
    /// contain an explicit controlId just as GUI, CLI and MCP calls do.
    pub async fn business_call(&self, command: Value) -> Result<Value, PluginError> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if pending.len() >= 16 {
                return Err(PluginError::new("queue_busy", "插件业务请求过多"));
            }
            pending.insert(id, sender);
        }
        if self
            .sender
            .try_send(Message::Request {
                id,
                method: "core.call".into(),
                params: command,
            })
            .is_err()
        {
            self.pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&id);
            return Err(PluginError::new("queue_busy", "插件请求队列已满或关闭"));
        }
        let result = tokio::time::timeout(Duration::from_secs(15), receiver).await;
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&id);
        match result {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PluginError::new("plugin_disconnected", "核心连接已关闭")),
            Err(_) => Err(PluginError::new(
                "request_timeout",
                "核心业务请求超时，未自动重试",
            )),
        }
    }
}

pub async fn run_plugin<P: Plugin>(mut plugin: P) -> Result<(), PluginError> {
    let (sender, mut writer_queue) = mpsc::channel::<Message>(64);
    let (incoming, mut request_queue) = mpsc::channel::<Message>(64);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let context = PluginContext {
        sender: sender.clone(),
        frames: Arc::new(Mutex::new(HashMap::new())),
        frame_ready: Arc::new(Notify::new()),
        pending: pending.clone(),
        sequence: Arc::new(AtomicU64::new(0)),
        request_id: Arc::new(AtomicU64::new(0)),
    };
    let cancellation = CancellationToken::new();
    let writer_context = context.clone();
    let writer_cancel = cancellation.clone();
    let writer = tokio::spawn(async move {
        let mut output = tokio::io::stdout();
        loop {
            tokio::select! {
                biased;
                Some(message) = writer_queue.recv() => {
                    tokio::time::timeout(Duration::from_secs(1), write_message(&mut output, &message)).await
                        .map_err(|_| PluginError::new("plugin_io", "插件输出管道超时"))??;
                }
                _ = writer_cancel.cancelled() => break,
                _ = writer_context.frame_ready.notified() => {
                    let frames = {
                        let mut mailbox = writer_context.frames.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                        std::mem::take(&mut *mailbox)
                    };
                    for frame in frames.into_values() {
                        let message = Message::Notification { method: "frame".into(), params: serde_json::to_value(frame).expect("frame is serializable") };
                        tokio::time::timeout(Duration::from_secs(1), write_message(&mut output, &message)).await
                            .map_err(|_| PluginError::new("plugin_io", "插件输出管道超时"))??;
                    }
                }
            }
        }
        Ok::<(), PluginError>(())
    });
    let (reader_finished, reader) = oneshot::channel();
    std::thread::Builder::new()
        .name("plugin-ipc-input".into())
        .spawn(move || {
            let result = (|| {
                let input = std::io::stdin();
                let mut input = input.lock();
                loop {
                    let message = read_message_sync(&mut input)?;
                    match message {
                        Some(Message::Response { id, result, error }) => {
                            if let Some(sender) = pending
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .remove(&id)
                            {
                                let _ = sender.send(error.map_or(Ok(result), Err));
                            }
                        }
                        Some(message) => {
                            if incoming.try_send(message).is_err() {
                                return Err(PluginError::new(
                                    "queue_busy",
                                    "核心请求超过插件处理容量",
                                ));
                            }
                        }
                        None => break,
                    }
                }
                Ok::<(), PluginError>(())
            })();
            let _ = reader_finished.send(result);
        })
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))?;
    let mut ticker = tokio::time::interval(Duration::from_millis(FRAME_INTERVAL_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut initialized = false;
    let mut reader = reader;
    let mut writer = writer;
    let result = loop {
        tokio::select! {
            biased;
            result = &mut reader => break result.map_err(|error| PluginError::new("plugin_io", error.to_string())).and_then(|result| result),
            result = &mut writer => break result.map_err(|error| PluginError::new("plugin_io", error.to_string())).and_then(|result| result),
            Some(message) = request_queue.recv() => {
                let (id, method, params) = match message {
                    Message::Request { id, method, params } => (Some(id), method, params),
                    Message::Notification { method, params } => (None, method, params),
                    Message::Response { .. } => continue,
                };
                let shutdown = method == "shutdown";
                let response = if !initialized && method != "initialize" && method != "migrate" && !shutdown {
                    Err(PluginError::new("not_initialized", "插件尚未初始化"))
                } else {
                    match method.as_str() {
                        "migrate" => match serde_json::from_value(params) {
                            Ok(params) => plugin.migrate(params, &context).await,
                            Err(error) => Err(invalid_params(error)),
                        },
                        "initialize" => match serde_json::from_value::<InitializeParams>(params) {
                            Ok(params) if params.protocol_version == PROTOCOL_VERSION && !initialized => {
                                match plugin.initialize(params, &context).await {
                                    Ok(value) => { initialized = true; Ok(value) }
                                    Err(error) => Err(error),
                                }
                            }
                            Ok(_) => Err(PluginError::new("protocol_incompatible", "插件协议版本不兼容或重复初始化")),
                            Err(error) => Err(invalid_params(error)),
                        },
                        "configure" => match serde_json::from_value(params) {
                            Ok(params) => plugin.configure(params, &context).await,
                            Err(error) => Err(invalid_params(error)),
                        },
                        "bindings" => match serde_json::from_value::<Vec<Binding>>(params) {
                            Ok(bindings) if bindings.len() <= MAX_BINDINGS => plugin.bindings(bindings, &context).await,
                            Ok(_) => Err(PluginError::new("invalid_params", "绑定数量超过上限")),
                            Err(error) => Err(invalid_params(error)),
                        },
                        "action" => match serde_json::from_value(params) {
                            Ok(params) => plugin.action(params, &context).await,
                            Err(error) => Err(invalid_params(error)),
                        },
                        "ui" => match serde_json::from_value(params) {
                            Ok(params) => plugin.ui(params, &context).await.and_then(|document| {
                                document.validate()?;
                                serde_json::to_value(document).map_err(invalid_params)
                            }),
                            Err(error) => Err(invalid_params(error)),
                        },
                        "input" => match serde_json::from_value(params) {
                            Ok(params) => plugin.input(params, &context).await,
                            Err(error) => Err(invalid_params(error)),
                        },
                        "shutdown" => { plugin.shutdown(&context).await; Ok(Value::Null) },
                        _ => Err(PluginError::new("method_not_found", "未知插件方法")),
                    }
                };
                if let Some(id) = id {
                    let (result, error) = match response { Ok(value) => (value, None), Err(error) => (Value::Null, Some(error)) };
                    if sender.send(Message::Response { id, result, error }).await.is_err() {
                        break Err(PluginError::new("plugin_disconnected", "插件输出管道已关闭"));
                    }
                }
                if shutdown { break Ok(()); }
            }
            _ = ticker.tick(), if initialized => {
                if let Err(error) = plugin.tick(&context).await {
                    let _ = context.status(serde_json::json!({"lastError":error}));
                }
            }
        }
    };
    cancellation.cancel();
    if !writer.is_finished() {
        let _ = tokio::time::timeout(Duration::from_secs(1), &mut writer).await;
        writer.abort();
    }
    context
        .pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    result
}

fn invalid_params(error: serde_json::Error) -> PluginError {
    PluginError::new("invalid_params", error.to_string())
}
