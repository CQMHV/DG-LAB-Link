use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::PluginError;

pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Native stdin uses a dedicated OS thread so cancelling a Tokio stdin read
/// cannot keep the executor alive during plugin shutdown.
pub fn read_message_sync<R: std::io::Read>(reader: &mut R) -> Result<Option<Message>, PluginError> {
    let mut prefix = [0_u8; 4];
    match reader.read_exact(&mut prefix[..1]) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(PluginError::new("plugin_io", error.to_string())),
    }
    reader
        .read_exact(&mut prefix[1..])
        .map_err(|error| PluginError::new("invalid_message", error.to_string()))?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(PluginError::new(
            "message_too_large",
            "插件 IPC 消息大小无效",
        ));
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| PluginError::new("invalid_message", error.to_string()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        #[serde(default)]
        result: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<PluginError>,
    },
    Notification {
        method: String,
        params: Value,
    },
}

pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Message>, PluginError> {
    let mut prefix = [0_u8; 4];
    match reader.read_exact(&mut prefix[..1]).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(PluginError::new("plugin_io", error.to_string())),
    }
    reader
        .read_exact(&mut prefix[1..])
        .await
        .map_err(|error| PluginError::new("invalid_message", error.to_string()))?;
    let length = u32::from_le_bytes(prefix) as usize;
    if length == 0 || length > MAX_MESSAGE_BYTES {
        return Err(PluginError::new(
            "message_too_large",
            "插件 IPC 消息大小无效",
        ));
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| PluginError::new("invalid_message", error.to_string()))
}

pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), PluginError> {
    let bytes = serde_json::to_vec(message)
        .map_err(|error| PluginError::new("invalid_message", error.to_string()))?;
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE_BYTES {
        return Err(PluginError::new(
            "message_too_large",
            "插件 IPC 消息超过 1 MiB",
        ));
    }
    writer
        .write_all(&(bytes.len() as u32).to_le_bytes())
        .await
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| PluginError::new("plugin_io", error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framing_roundtrip_and_oversized_prefix() {
        let (mut first, mut second) = tokio::io::duplex(4096);
        let expected = Message::Request {
            id: 7,
            method: "ui".into(),
            params: serde_json::json!({"surface":"control"}),
        };
        write_message(&mut first, &expected).await.unwrap();
        let actual = read_message(&mut second).await.unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        first
            .write_all(&((MAX_MESSAGE_BYTES + 1) as u32).to_le_bytes())
            .await
            .unwrap();
        assert_eq!(
            read_message(&mut second).await.unwrap_err().code,
            "message_too_large"
        );
    }
}
