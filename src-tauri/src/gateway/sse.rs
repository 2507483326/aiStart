use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::sync::oneshot;

use crate::error::AppResult;

/// 上游 SSE 的一帧。
///
/// `raw` 是这一帧的**原文**（`\r\n` 归一为 `\n`），同协议直通要把它一字不改地发给客户端。
/// `data` 只是拼好的副本（多行 `data:` 用 `\n` 连接），**不能拿它重排回去**：会丢注释行，
/// 并把多行 `data:` 压成一行——压出来的换行会直接破坏 SSE 分帧（客户端读到半截 JSON，
/// 后半截没有 `data:` 前缀、被当成未知行丢掉）。
#[derive(Debug, Clone, Default)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
    pub raw: String,
}

pub fn parse_sse_stream<S>(stream: S) -> impl Stream<Item = AppResult<SseFrame>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::stream! {
        futures_util::pin_mut!(stream);
        let mut buffer = String::new();
        let mut frame = SseFrame::default();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.push_str(&String::from_utf8_lossy(&bytes));
                    while let Some(position) = buffer.find('\n') {
                        let mut line = buffer[..position].to_string();
                        buffer.drain(..=position);
                        if line.ends_with('\r') {
                            line.pop();
                        }
                        if line.is_empty() {
                            // 空行 = 一帧结束。纯注释帧（`: ping` 心跳）也照样发出去：
                            // 直通时它就是客户端本该收到的东西，丢掉等于替上游改流。
                            if !frame.data.is_empty() || !frame.raw.is_empty() {
                                frame.raw.push('\n');
                                yield Ok(std::mem::take(&mut frame));
                            }
                            continue;
                        }
                        frame.raw.push_str(&line);
                        frame.raw.push('\n');
                        if let Some(rest) = line.strip_prefix("event:") {
                            frame.event = rest.trim().to_string();
                        } else if let Some(rest) = line.strip_prefix("data:") {
                            if !frame.data.is_empty() {
                                frame.data.push('\n');
                            }
                            frame.data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                        }
                    }
                }
                Err(error) => {
                    yield Err(error.into());
                    return;
                }
            }
        }

        if !frame.data.is_empty() || !frame.raw.is_empty() {
            yield Ok(frame);
        }
    }
}

pub fn encode_channel_event(event: &str, data: &str) -> String {
    if event.is_empty() {
        format!("data: {data}\n\n")
    } else {
        format!("event: {event}\ndata: {data}\n\n")
    }
}

pub fn shutdown_channel() -> (oneshot::Sender<()>, oneshot::Receiver<()>) {
    oneshot::channel()
}
