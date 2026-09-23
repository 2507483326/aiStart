use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tokio::sync::oneshot;

use crate::error::AppResult;

pub fn parse_sse_stream<S>(stream: S) -> impl Stream<Item = AppResult<(String, String)>>
where
    S: Stream<Item = reqwest::Result<Bytes>>,
{
    async_stream::stream! {
        futures_util::pin_mut!(stream);
        let mut buffer = String::new();
        let mut event = String::new();
        let mut data = String::new();

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
                            if !data.is_empty() {
                                yield Ok((std::mem::take(&mut event), std::mem::take(&mut data)));
                            }
                            event.clear();
                            data.clear();
                            continue;
                        }
                        if let Some(rest) = line.strip_prefix("event:") {
                            event = rest.trim().to_string();
                        } else if let Some(rest) = line.strip_prefix("data:") {
                            if !data.is_empty() {
                                data.push('\n');
                            }
                            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
                        }
                    }
                }
                Err(error) => {
                    yield Err(error.into());
                    return;
                }
            }
        }

        if !data.is_empty() {
            yield Ok((event, data));
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
