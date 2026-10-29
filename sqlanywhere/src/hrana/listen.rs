//! Commits announced by `sqld`, read from its `/beta/listen` server-sent
//! events: one stream per table, taken together.

use std::sync::Mutex;

use futures::stream::{self, BoxStream, SelectAll};
use futures::{FutureExt, StreamExt};

use crate::connection::Listener;
use crate::hrana::connection::HttpConnection;
use crate::hrana::hyper::{ByteStream, HttpSender};
use crate::hrana::HranaError;
use crate::{Error, Result};

/// Something happened to a table: a commit touched it, or the server dropped
/// announcements and the listener must look for itself.
type Announcements = BoxStream<'static, Result<()>>;

pub(super) async fn listen(
    conn: &HttpConnection<HttpSender>,
    tables: &[String],
) -> Result<Box<dyn Listener>> {
    if tables.is_empty() {
        return Err(Error::Misuse("listen needs at least one table".into()));
    }
    let (base, query) = match conn.base_url().rfind('?') {
        Some(i) => conn.base_url().split_at(i),
        None => (conn.base_url(), ""),
    };
    let base = base.trim_end_matches('/');
    let mut streams = SelectAll::new();
    for table in tables {
        let mut url = format!("{base}/beta/listen?table={}", percent_encode(table));
        if let Some(rest) = query.strip_prefix('?').filter(|q| !q.is_empty()) {
            url.push('&');
            url.push_str(rest);
        }
        // Once this returns, the server has subscribed: a commit after it is
        // announced.
        let body = conn
            .sender()
            .get_stream(&url, conn.auth())
            .await
            .map_err(|e| Error::Hrana(Box::new(e)))?;
        streams.push(announcements(body));
    }
    Ok(Box::new(SseListener {
        streams: Mutex::new(streams),
    }))
}

struct SseListener {
    // Only ever reached through `&mut self`, so never locked: the mutex makes
    // the listener `Sync`, which the streams are not.
    streams: Mutex<SelectAll<Announcements>>,
}

#[async_trait::async_trait]
impl Listener for SseListener {
    async fn changed(&mut self) -> Result<()> {
        let streams = self
            .streams
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match streams.next().await {
            Some(Ok(())) => {}
            Some(Err(e)) => return Err(e),
            None => return Err(closed()),
        }
        // Take what else has already arrived along with it.
        while let Some(Some(next)) = streams.next().now_or_never() {
            next?;
        }
        Ok(())
    }
}

fn closed() -> Error {
    Error::Hrana(Box::new(HranaError::StreamClosed(
        "sqld stopped announcing commits".into(),
    )))
}

/// The announcements in one server-sent event stream.
fn announcements(body: ByteStream) -> Announcements {
    let state = (body, Vec::new(), 0usize);
    stream::unfold(state, |(mut body, mut buf, mut ready)| async move {
        loop {
            if ready > 0 {
                ready -= 1;
                return Some((Ok(()), (body, buf, ready)));
            }
            match body.next().await {
                Some(Ok(chunk)) => {
                    buf.extend(chunk.iter().filter(|b| **b != b'\r'));
                    ready += take_events(&mut buf);
                }
                Some(Err(e)) => {
                    let err = Error::Hrana(Box::new(HranaError::Http(e.to_string())));
                    return Some((Err(err), (body, Vec::new(), 0)));
                }
                None => return None,
            }
        }
    })
    .boxed()
}

/// Remove the complete events from `buf` and count those that announce
/// something: `changes`, and `error`, which the server sends when a listener
/// fell behind and announcements were lost. Comments (keep-alives) and other
/// events count for nothing. `buf` holds no carriage returns.
fn take_events(buf: &mut Vec<u8>) -> usize {
    let mut count = 0;
    while let Some(end) = buf.windows(2).position(|w| w == b"\n\n") {
        let event: Vec<u8> = buf.drain(..end + 2).collect();
        let event = String::from_utf8_lossy(&event);
        let name = event
            .lines()
            .find_map(|line| line.strip_prefix("event:"))
            .map(str::trim);
        if matches!(name, Some("changes") | Some("error")) {
            count += 1;
        }
    }
    count
}

fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_counted_when_complete() {
        let mut buf = b"event: changes\ndata: {\"insert\":1}\n\n: keep-alive\n\nevent: chan".to_vec();
        assert_eq!(take_events(&mut buf), 1);
        assert_eq!(buf, b"event: chan");
        buf.extend_from_slice(b"ges\ndata: {}\n\nevent: error\ndata: some changes were lost\n\n");
        assert_eq!(take_events(&mut buf), 2);
        assert!(buf.is_empty());
        buf.extend_from_slice(b"data: no event name\n\nevent: other\n\n");
        assert_eq!(take_events(&mut buf), 0);
    }

    #[test]
    fn table_names_are_encoded() {
        assert_eq!(percent_encode("todo"), "todo");
        assert_eq!(percent_encode("my table&x=1"), "my%20table%26x%3D1");
        assert_eq!(percent_encode("bø"), "b%C3%B8");
    }

    #[tokio::test]
    async fn announcements_follow_the_chunks() {
        let chunks: Vec<std::io::Result<bytes::Bytes>> = vec![
            Ok("event: changes\r\ndata: {}\r\n".into()),
            Ok("\r\n:keep-alive\r\n\r\nevent: changes\r\ndata: {}\r\n\r\nevent: changes\n".into()),
            Ok("data: {}\n\n".into()),
        ];
        let body: ByteStream = Box::new(stream::iter(chunks));
        let got: Vec<_> = announcements(body).collect().await;
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|r| r.is_ok()));
    }
}
