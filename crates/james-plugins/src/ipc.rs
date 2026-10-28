//! IPC with the plugin host: length-prefixed JSON frames over a Unix stream.
//!
//! Each frame is a 4-byte big-endian length followed by a UTF-8 JSON object:
//!
//! - `{"t":"req","id":1,"method":"…","params":…}` – request, answered by exactly one response
//! - `{"t":"res","id":1,"result":…}` / `{"t":"res","id":1,"error":"…"}` – response
//! - `{"t":"evt","name":"…","data":…}` – one-way event
//!
//! Both sides send requests and events. The protocol version is exchanged with the first
//! request (`init`), see [`PROTOCOL_VERSION`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// Version of the IPC protocol; bumped on incompatible changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Upper bound for one frame (protects against runaway or malicious lengths).
pub const MAX_FRAME: usize = 16 << 20;

/// Frames waiting to be written; events beyond this are dropped.
const OUTBOX: usize = 8192;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum Frame {
    Req {
        id: u64,
        method: String,
        #[serde(default)]
        params: Value,
    },
    Res {
        id: u64,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        result: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Evt {
        name: String,
        #[serde(default)]
        data: Value,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("plugin host is not connected")]
    Closed,
    #[error("plugin host did not answer in time")]
    Timeout,
    #[error("{0}")]
    Remote(String),
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Frame>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {len} bytes exceeds the limit"),
        ));
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> std::io::Result<()> {
    let body = serde_json::to_vec(frame).map_err(std::io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = Vec::with_capacity(body.len() + 4);
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(&body);
    writer.write_all(&buf).await
}

/// Receives requests and events from the other side.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn request(&self, method: &str, params: Value) -> Result<Value, String>;
    async fn event(&self, name: &str, data: Value);
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// Sending side of a connection; cheap to clone.
#[derive(Clone)]
pub struct Peer {
    outbox: mpsc::Sender<Frame>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl Peer {
    /// Sends a request and waits for the response.
    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, IpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        let frame = Frame::Req {
            id,
            method: method.to_owned(),
            params,
        };
        // Sending counts towards the timeout too (the other side may have stopped reading).
        let exchange = async {
            if self.outbox.send(frame).await.is_err() {
                return Err(IpcError::Closed);
            }
            match rx.await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(IpcError::Remote(error)),
                Err(_) => Err(IpcError::Closed),
            }
        };
        let result = tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(IpcError::Timeout));
        if result.is_err() {
            self.pending.lock().remove(&id);
        }
        result
    }

    /// Sends an event; returns `false` if it was dropped (closed or overloaded).
    pub fn notify(&self, name: &str, data: Value) -> bool {
        self.outbox
            .try_send(Frame::Evt {
                name: name.to_owned(),
                data,
            })
            .is_ok()
    }

    pub fn is_closed(&self) -> bool {
        self.outbox.is_closed()
    }
}

/// Runs a connection: returns the peer and a future completing when the connection ends.
/// At most `max_requests` incoming requests are handled at once; further frames are not read
/// until one finishes.
pub fn connect<S>(
    stream: S,
    handler: Arc<dyn Handler>,
    max_requests: usize,
) -> (Peer, impl std::future::Future<Output = ()>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (outbox, mut queue) = mpsc::channel::<Frame>(OUTBOX);
    let pending: Pending = Arc::default();
    let peer = Peer {
        outbox: outbox.clone(),
        pending: pending.clone(),
        next_id: Arc::new(AtomicU64::new(1)),
    };
    let slots = Arc::new(tokio::sync::Semaphore::new(max_requests.max(1)));
    let run = async move {
        let write = async {
            while let Some(frame) = queue.recv().await {
                if let Err(e) = write_frame(&mut writer, &frame).await {
                    tracing::debug!(error = %e, "plugin host connection write failed");
                    break;
                }
            }
        };
        let read = async {
            loop {
                let frame = match read_frame(&mut reader).await {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(error = %e, "invalid frame from plugin host");
                        break;
                    }
                };
                match frame {
                    Frame::Res { id, result, error } => {
                        if let Some(tx) = pending.lock().remove(&id) {
                            let _ = tx.send(match error {
                                Some(error) => Err(error),
                                None => Ok(result),
                            });
                        }
                    }
                    Frame::Req { id, method, params } => {
                        let Ok(permit) = slots.clone().acquire_owned().await else {
                            break;
                        };
                        let handler = handler.clone();
                        let outbox = outbox.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            let frame = match handler.request(&method, params).await {
                                Ok(result) => Frame::Res {
                                    id,
                                    result,
                                    error: None,
                                },
                                Err(error) => Frame::Res {
                                    id,
                                    result: Value::Null,
                                    error: Some(error),
                                },
                            };
                            let _ = outbox.send(frame).await;
                        });
                    }
                    Frame::Evt { name, data } => handler.event(&name, data).await,
                }
            }
        };
        tokio::select! {
            _ = write => {}
            _ = read => {}
        }
        // Fail everything still waiting.
        pending.lock().clear();
    };
    (peer, run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Echo;

    #[async_trait]
    impl Handler for Echo {
        async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
            match method {
                "echo" => Ok(params),
                _ => Err(format!("unknown method {method}")),
            }
        }
        async fn event(&self, _: &str, _: Value) {}
    }

    #[test]
    fn frames_use_the_documented_shape() {
        let frame = Frame::Res {
            id: 3,
            result: Value::Null,
            error: Some("x".into()),
        };
        assert_eq!(
            serde_json::to_value(&frame).unwrap(),
            json!({"t": "res", "id": 3, "error": "x"})
        );
        let parsed: Frame = serde_json::from_value(json!({"t": "evt", "name": "log"})).unwrap();
        assert_eq!(
            parsed,
            Frame::Evt {
                name: "log".into(),
                data: Value::Null
            }
        );
    }

    #[tokio::test]
    async fn requests_round_trip_both_ways() {
        let (a, b) = tokio::io::duplex(1024);
        let (left, run_left) = connect(a, Arc::new(Echo), 4);
        let (right, run_right) = connect(b, Arc::new(Echo), 4);
        tokio::spawn(run_left);
        tokio::spawn(run_right);
        let t = Duration::from_secs(1);
        assert_eq!(
            left.request("echo", json!([1, 2]), t).await.unwrap(),
            json!([1, 2])
        );
        assert_eq!(right.request("echo", json!("x"), t).await.unwrap(), json!("x"));
        assert!(matches!(
            left.request("nope", Value::Null, t).await,
            Err(IpcError::Remote(e)) if e.contains("unknown method")
        ));
    }

    #[tokio::test]
    async fn oversized_frames_are_rejected() {
        let mut data = Vec::new();
        data.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
        let mut reader = &data[..];
        assert!(read_frame(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn closing_fails_pending_requests() {
        let (a, b) = tokio::io::duplex(1024);
        let (peer, run) = connect(a, Arc::new(Echo), 4);
        let task = tokio::spawn(run);
        drop(b);
        let result = peer.request("echo", Value::Null, Duration::from_secs(5)).await;
        assert!(matches!(result, Err(IpcError::Closed)), "{result:?}");
        task.await.unwrap();
    }
}
