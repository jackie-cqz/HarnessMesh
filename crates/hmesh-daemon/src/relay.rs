//! A relay on this computer between an agent and its custom model endpoint.
//!
//! Some OpenAI-compatible gateways send replies that strict clients reject:
//! one sends a final streamed chunk whose `delta.role` is an empty string,
//! and OpenCode's AI SDK then fails the whole turn after the reply has
//! arrived. OpenCode on a custom endpoint therefore talks to this relay: each
//! request goes on to the endpoint with the real key, and replies come back
//! unchanged except for such fields. The agent process receives only a token
//! for this relay, never the key.
//!
//! The relay listens on 127.0.0.1 only and starts with the first agent that
//! needs it. Requests without a known token are refused before anything is
//! sent on; redirects are not followed.
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use hmesh_runtime::service::{Protocol, Relayed};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

/// The path prefix the relay serves; the rest of the path follows the endpoint.
const PREFIX: &str = "/relay";
/// Largest request body passed on (requests may carry images).
const REQUEST_LIMIT: usize = 64 * 1024 * 1024;
/// Largest non-streamed JSON reply that is checked; larger ones pass unchanged.
const JSON_LIMIT: usize = 32 * 1024 * 1024;
/// Longest streamed line that is checked; longer ones pass unchanged.
const LINE_LIMIT: usize = 4 * 1024 * 1024;
/// Request headers that describe this hop or carry credentials for it.
const DROPPED_REQUEST: &[&str] = &[
    "host",
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "cookie",
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    // Compressed replies could not be checked.
    "accept-encoding",
];
const DROPPED_RESPONSE: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
    "set-cookie",
];

#[derive(Clone)]
struct Route {
    endpoint: String,
    key: String,
    protocol: Protocol,
}

#[derive(Clone)]
struct Shared {
    routes: Arc<Mutex<HashMap<String, Route>>>,
    remote: reqwest::Client,
    local: reqwest::Client,
}

pub struct ModelRelay {
    server: Mutex<Option<(SocketAddr, tokio::task::JoinHandle<()>)>>,
    routes: Arc<Mutex<HashMap<String, Route>>>,
    shutdown: watch::Receiver<bool>,
}

impl ModelRelay {
    pub fn new(shutdown: watch::Receiver<bool>) -> Self {
        Self {
            server: Mutex::new(None),
            routes: Arc::default(),
            shutdown,
        }
    }

    /// The relay address and token an agent uses for this endpoint and key.
    /// The same endpoint, key and protocol keep their token, so agents already running
    /// are not cut off; must be called inside the daemon's runtime.
    pub fn route(&self, endpoint: &str, key: &str, protocol: Protocol) -> anyhow::Result<Relayed> {
        let address = self.address()?;
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| anyhow::anyhow!("model relay lock poisoned"))?;
        let token = match routes
            .iter()
            .find(|(_, r)| r.endpoint == endpoint && r.key == key && r.protocol == protocol)
        {
            Some((token, _)) => token.clone(),
            None => {
                let token = format!("hmr-{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
                routes.insert(
                    token.clone(),
                    Route {
                        endpoint: endpoint.to_owned(),
                        key: key.to_owned(),
                        protocol,
                    },
                );
                token
            }
        };
        Ok(Relayed {
            url: format!("http://{address}{PREFIX}"),
            token,
        })
    }

    /// The relay's address, starting it when it is not running: at first
    /// use, or after it stopped or could not start.
    fn address(&self) -> anyhow::Result<SocketAddr> {
        if *self.shutdown.borrow() {
            anyhow::bail!("the daemon is stopping");
        }
        let mut server = self
            .server
            .lock()
            .map_err(|_| anyhow::anyhow!("model relay lock poisoned"))?;
        if let Some((address, task)) = server.as_ref()
            && !task.is_finished()
        {
            return Ok(*address);
        }
        let (address, task) = self
            .start()
            .map_err(|e| anyhow::anyhow!("the local model relay could not start: {e}"))?;
        *server = Some((address, task));
        Ok(address)
    }

    fn start(&self) -> anyhow::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let client = |local: bool| {
            let builder = reqwest::Client::builder()
                // A redirect would carry the key to an address nobody chose.
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(30));
            if local { builder.no_proxy() } else { builder }.build()
        };
        let shared = Shared {
            routes: self.routes.clone(),
            remote: client(false)?,
            local: client(true)?,
        };
        let app = Router::new().fallback(forward).with_state(shared);
        let mut shutdown = self.shutdown.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                result = axum::serve(listener, app) => {
                    if let Err(error) = result {
                        tracing::error!(%error, "model relay stopped");
                    }
                }
                _ = shutdown.wait_for(|stop| *stop) => {}
            }
        });
        tracing::info!(%address, "model relay listening");
        Ok((address, task))
    }
}

fn refuse(status: StatusCode, message: &str) -> Response {
    let body = json!({"error":{"message":message,"type":"harnessmesh_relay"}});
    (status, axum::Json(body)).into_response()
}

async fn forward(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let Some(rest) = uri
        .path()
        .strip_prefix(PREFIX)
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
    else {
        return refuse(StatusCode::NOT_FOUND, "not a HarnessMesh relay path");
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        })
        .map(|(_, token)| token.trim().to_owned());
    let anthropic = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    let route = shared.routes.lock().ok().and_then(|routes| {
        match (bearer.as_deref(), anthropic) {
            (Some(token), None) => routes
                .get(token)
                .filter(|r| r.protocol != Protocol::Anthropic),
            (None, Some(token)) => routes
                .get(token)
                .filter(|r| r.protocol == Protocol::Anthropic),
            _ => None,
        }
        .cloned()
    });
    let Some(route) = route else {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "HarnessMesh relay: unknown token; restart the agent from HarnessMesh",
        );
    };
    let mut target = format!("{}{rest}", route.endpoint.trim_end_matches('/'));
    if let Some(query) = uri.query() {
        target.push('?');
        target.push_str(query);
    }
    // The path only extends the configured endpoint: never another address.
    let (Ok(url), Ok(base)) = (
        reqwest::Url::parse(&target),
        reqwest::Url::parse(&route.endpoint),
    ) else {
        return refuse(
            StatusCode::BAD_GATEWAY,
            "the model service address is not valid",
        );
    };
    if url.origin() != base.origin() {
        return refuse(StatusCode::BAD_REQUEST, "not a path of the model service");
    }
    let body = match axum::body::to_bytes(body, REQUEST_LIMIT).await {
        Ok(body) => body,
        Err(_) => return refuse(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
    };
    let client = if crate::service_test::is_loopback(&route.endpoint) {
        &shared.local
    } else {
        &shared.remote
    };
    let mut request = client.request(method, url);
    for (name, value) in &headers {
        if !DROPPED_REQUEST.contains(&name.as_str()) {
            request = request.header(name, value);
        }
    }
    if !route.key.is_empty() {
        request = match route.protocol {
            Protocol::Anthropic => request.header("x-api-key", &route.key),
            _ => request.bearer_auth(&route.key),
        };
    }
    if !body.is_empty() {
        request = request.body(body);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            let reason = if error.is_timeout() {
                "no answer in time"
            } else if error.is_connect() {
                "could not connect"
            } else {
                "the request failed"
            };
            return refuse(
                StatusCode::BAD_GATEWAY,
                &format!("HarnessMesh relay could not reach the model service: {reason}"),
            );
        }
    };
    let status = response.status();
    let mut out = Response::builder().status(status.as_u16());
    let reply_headers = response.headers().clone();
    for (name, value) in &reply_headers {
        if !DROPPED_RESPONSE.contains(&name.as_str()) {
            out = out.header(name, value);
        }
    }
    let fix = if route.protocol == Protocol::OpenAi {
        Fix::for_reply(&reply_headers)
    } else {
        Fix::Raw
    };
    out.body(Body::from_stream(relay_body(response, fix)))
        .unwrap_or_else(|_| refuse(StatusCode::BAD_GATEWAY, "unusable reply"))
}

fn relay_body(
    response: reqwest::Response,
    fix: Fix,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> + Send + 'static {
    futures_util::stream::unfold(
        (Some(response), fix),
        |(mut response, mut fix)| async move {
            let current = response.as_mut()?;
            loop {
                match current.chunk().await {
                    Ok(Some(chunk)) => {
                        let out = fix.push(&chunk);
                        if !out.is_empty() {
                            return Some((Ok(out), (response, fix)));
                        }
                    }
                    Ok(None) => {
                        let out = fix.finish();
                        return (!out.is_empty()).then(|| (Ok(out), (None, fix)));
                    }
                    Err(error) => {
                        return Some((Err(std::io::Error::other(error)), (None, fix)));
                    }
                }
            }
        },
    )
}

/// How a reply is checked on its way back.
enum Fix {
    /// Server-sent events: each complete `data:` line is checked.
    Stream(Vec<u8>),
    /// A JSON reply, checked once it is complete.
    Json(Vec<u8>, bool),
    /// Passed through unchanged.
    Raw,
}

impl Fix {
    fn for_reply(headers: &HeaderMap) -> Self {
        let encoded = headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| !v.trim().eq_ignore_ascii_case("identity"));
        let kind = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if encoded {
            Fix::Raw
        } else if kind.starts_with("text/event-stream") {
            Fix::Stream(Vec::new())
        } else if kind.starts_with("application/json") {
            Fix::Json(Vec::new(), false)
        } else {
            Fix::Raw
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Bytes {
        match self {
            Fix::Raw => Bytes::copy_from_slice(chunk),
            Fix::Json(buffer, overflow) => {
                if *overflow {
                    return Bytes::copy_from_slice(chunk);
                }
                buffer.extend_from_slice(chunk);
                if buffer.len() > JSON_LIMIT {
                    *overflow = true;
                    return Bytes::from(std::mem::take(buffer));
                }
                Bytes::new()
            }
            Fix::Stream(pending) => {
                pending.extend_from_slice(chunk);
                let Some(end) = pending.iter().rposition(|b| *b == b'\n') else {
                    if pending.len() > LINE_LIMIT {
                        return Bytes::from(std::mem::take(pending));
                    }
                    return Bytes::new();
                };
                let rest = pending.split_off(end + 1);
                let complete = std::mem::replace(pending, rest);
                let mut out = Vec::with_capacity(complete.len());
                for line in complete.split_inclusive(|b| *b == b'\n') {
                    out.extend_from_slice(&fix_line(line));
                }
                Bytes::from(out)
            }
        }
    }

    fn finish(&mut self) -> Bytes {
        match self {
            Fix::Raw => Bytes::new(),
            Fix::Json(buffer, overflow) => {
                let buffer = std::mem::take(buffer);
                if *overflow {
                    return Bytes::from(buffer);
                }
                Bytes::from(fixed(&buffer).unwrap_or(buffer))
            }
            Fix::Stream(pending) => Bytes::from(fix_line(&std::mem::take(pending)).into_owned()),
        }
    }
}

/// One streamed line, with its line ending if it has one.
fn fix_line(line: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    let body_end = line
        .iter()
        .rposition(|b| *b != b'\n' && *b != b'\r')
        .map_or(0, |i| i + 1);
    let (content, ending) = line.split_at(body_end);
    let Some(payload) = content.strip_prefix(b"data:") else {
        return line.into();
    };
    let payload = payload.strip_prefix(b" ").unwrap_or(payload);
    match fixed(payload) {
        Some(json) => {
            let mut out = b"data: ".to_vec();
            out.extend_from_slice(&json);
            out.extend_from_slice(ending);
            out.into()
        }
        None => line.into(),
    }
}

/// The corrected JSON text, when the text is JSON and needed correcting.
fn fixed(text: &[u8]) -> Option<Vec<u8>> {
    let mut value: Value = serde_json::from_slice(text).ok()?;
    if !fix_value(&mut value) {
        return None;
    }
    serde_json::to_vec(&value).ok()
}

/// Correct fields strict clients reject; true when anything changed. An
/// empty `role` in a choice's `delta` or `message` is dropped: the field is
/// optional, and an empty value means nothing.
fn fix_value(value: &mut Value) -> bool {
    let mut changed = false;
    if let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) {
        for choice in choices {
            for part in ["delta", "message"] {
                if let Some(fields) = choice.get_mut(part).and_then(Value::as_object_mut)
                    && fields.get("role").and_then(Value::as_str) == Some("")
                {
                    fields.remove("role");
                    changed = true;
                }
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(fix: &mut Fix, chunks: &[&str]) -> String {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(&fix.push(chunk.as_bytes()));
        }
        out.extend_from_slice(&fix.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn an_empty_role_in_a_streamed_chunk_is_dropped_and_everything_else_passes() {
        let last = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"role":"","content":"","reasoning_content":""},"finish_reason":"stop"}]}"#;
        let first = r#"data: {"id":"c1","choices":[{"index":0,"delta":{"role":"assistant","content":"你好"}}]}"#;
        let stream = format!(": keep-alive\n\n{first}\n\n{last}\r\n\r\ndata: [DONE]\n\n");
        // Split everywhere, including inside a multi-byte character.
        let bytes = stream.as_bytes();
        let mut fix = Fix::Stream(Vec::new());
        let mut out = Vec::new();
        for piece in bytes.chunks(7) {
            out.extend_from_slice(&fix.push(piece));
        }
        out.extend_from_slice(&fix.finish());
        let out = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines[0], ": keep-alive");
        assert_eq!(lines[2], first, "an assistant role is kept byte for byte");
        let fixed: Value =
            serde_json::from_str(lines[4].strip_prefix("data: ").unwrap().trim_end()).unwrap();
        assert_eq!(
            fixed["choices"][0]["delta"],
            json!({"content":"","reasoning_content":""})
        );
        assert_eq!(fixed["choices"][0]["finish_reason"], "stop");
        assert!(lines[4].ends_with('\r'), "the line ending is kept");
        assert!(out.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn a_json_reply_with_an_empty_role_is_corrected_once_complete() {
        let reply = r#"{"choices":[{"index":0,"message":{"role":"","content":"hi"}}]}"#;
        let mut fix = Fix::Json(Vec::new(), false);
        let out: Value =
            serde_json::from_str(&run(&mut fix, &[&reply[..10], &reply[10..]])).unwrap();
        assert_eq!(out["choices"][0]["message"], json!({"content":"hi"}));
        let fine = r#"{"choices":[{"message":{"role":"assistant","content":"hi"}}]}"#;
        assert_eq!(run(&mut Fix::Json(Vec::new(), false), &[fine]), fine);
        assert_eq!(
            run(&mut Fix::Json(Vec::new(), false), &["not json"]),
            "not json"
        );
    }

    /// Events reach the agent as the service sends them, not when the reply
    /// ends; the service receives the real key, never the relay token.
    #[tokio::test]
    async fn replies_stream_back_as_they_arrive() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", upstream.local_addr().unwrap());
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let service = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut head = Vec::new();
            let mut buffer = [0u8; 4096];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buffer).await.unwrap();
                assert!(n > 0, "request ended early");
                head.extend_from_slice(&buffer[..n]);
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"a\"}}]}\n\n")
                .await
                .unwrap();
            released.await.unwrap();
            stream
                .write_all(b"data: {\"choices\":[{\"delta\":{\"role\":\"\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
                .await
                .unwrap();
            String::from_utf8_lossy(&head).to_ascii_lowercase()
        });
        let (_stop, stopped) = watch::channel(false);
        let relay = ModelRelay::new(stopped);
        let route = relay
            .route(&base, "sk-unit-0123456789", Protocol::OpenAi)
            .unwrap();
        assert_eq!(
            relay
                .route(&base, "sk-unit-0123456789", Protocol::OpenAi)
                .unwrap(),
            route
        );
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut reply = client
            .get(format!("{}/chat/completions", route.url))
            .bearer_auth(&route.token)
            .send()
            .await
            .unwrap();
        // The first event arrives while the service still holds the rest.
        let first = tokio::time::timeout(Duration::from_secs(10), reply.chunk())
            .await
            .expect("first event held back")
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&first).contains(r#""content":"a""#));
        release.send(()).unwrap();
        let mut rest = Vec::new();
        while let Some(chunk) = reply.chunk().await.unwrap() {
            rest.extend_from_slice(&chunk);
        }
        let rest = String::from_utf8(rest).unwrap();
        assert!(
            !rest.contains(r#""role":"""#) && rest.ends_with("data: [DONE]\n\n"),
            "{rest}"
        );
        let head = service.await.unwrap();
        assert!(
            head.contains("authorization: bearer sk-unit-0123456789"),
            "{head}"
        );
        assert!(!head.contains(&route.token.to_ascii_lowercase()), "{head}");
    }

    #[test]
    fn unchecked_replies_pass_unchanged() {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, "text/event-stream".parse().unwrap());
        headers.insert(header::CONTENT_ENCODING, "gzip".parse().unwrap());
        assert!(matches!(Fix::for_reply(&headers), Fix::Raw));
        headers.remove(header::CONTENT_ENCODING);
        assert!(matches!(Fix::for_reply(&headers), Fix::Stream(_)));
        let odd = "data: {\"choices\":[{\"delta\":{\"role\":\"\"}}]"; // no line ending, not JSON
        assert_eq!(run(&mut Fix::Stream(Vec::new()), &[odd]), odd);
        assert_eq!(run(&mut Fix::Raw, &["a", "b"]), "ab");
    }
}
