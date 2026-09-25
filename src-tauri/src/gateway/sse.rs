use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::{Stream, StreamExt};
use tokio::sync::oneshot;

use crate::error::{AppError, AppResult};

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

/// 上游 SSE 解析：按字节累积（`BytesMut`），凑齐一整行（`\n`）才解码。
///
/// TCP 分帧与 UTF-8 字符边界毫无关系——一个多字节字符（CJK、emoji）可能被切成两个 chunk，
/// 按 chunk 解码会把半个字符当乱码吞掉。按行解码后，不完整的尾字节自然留在缓冲等下一个
/// chunk；行内保证是完整 UTF-8（`from_utf8_lossy` 只兜极端异常，如上游真的发了坏字节）。
pub fn parse_sse_stream<S>(stream: S) -> impl Stream<Item = AppResult<SseFrame>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::stream! {
        futures_util::pin_mut!(stream);
        let mut buffer = BytesMut::new();
        let mut frame = SseFrame::default();

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    buffer.extend_from_slice(&bytes);
                    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                        let line_bytes = buffer.split_to(position + 1);
                        // 去掉行尾换行（含 \r\n 的 \r）再解码。
                        let mut end = line_bytes.len() - 1;
                        if end > 0 && line_bytes[end - 1] == b'\r' {
                            end -= 1;
                        }
                        let line = String::from_utf8_lossy(&line_bytes[..end]);
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

/// 首帧限时（B1）：只给上游响应的**第一个数据帧**设上限。
///
/// 上游接受请求、回了响应头，却迟迟不吐第一个字节（排队卡死、上游假死）时，流会永久悬住
/// ——客户端一直干等，明细也永远落不了库。第一帧到了就不再限时：流中途慢吐是合法的，
/// 加总超时会把正在正常输出长文的流掐断。
///
/// 超时以 `Err` 项进入流中，与上游断流走同一条处理路径（记账 + 跨协议时报错给客户端）。
pub fn first_frame_timeout<S>(stream: S, timeout: Duration) -> impl Stream<Item = AppResult<SseFrame>>
where
    S: Stream<Item = AppResult<SseFrame>>,
{
    async_stream::stream! {
        futures_util::pin_mut!(stream);
        match tokio::time::timeout(timeout, stream.next()).await {
            Ok(Some(frame)) => yield frame,
            Ok(None) => return,
            Err(_elapsed) => {
                yield Err(AppError::Message(format!(
                    "上游 {} 秒内未返回首帧（超时）",
                    timeout.as_secs()
                )));
                return;
            }
        }
        while let Some(frame) = stream.next().await {
            yield frame;
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
