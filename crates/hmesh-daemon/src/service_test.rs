//! Connection tests for an agent's model service, run only when its owner
//! asks. The service is first asked for its model list, which costs nothing;
//! only when it cannot list them is one minimal request sent. A rejected key
//! is reported at once without a second request. The key never appears in
//! the result.
use hmesh_core::service::{ServiceTestResult, TESTED_BY_MODELS, TESTED_BY_REQUEST};
use hmesh_runtime::service::{KeyHeader, Protocol, TestTarget};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const BODY_LIMIT: usize = 4 * 1024 * 1024;
const MAX_LISTED: usize = 500;
const SNIPPET: usize = 200;
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// The whole test, both requests included, ends before the owner's own
/// request to the daemon gives up (15 seconds).
const BUDGET: Duration = Duration::from_secs(13);

enum Answer {
    Status(u16, Vec<u8>),
    Unreachable(String),
}

fn client(base: &str) -> reqwest::Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        // A redirect would carry the key to an address nobody chose.
        .redirect(reqwest::redirect::Policy::none());
    // Proxies configured for this computer apply to remote services only.
    let local = reqwest::Url::parse(base)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.trim_matches(['[', ']']).to_owned()))
        .is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if local { builder.no_proxy() } else { builder }.build()
}

async fn call(
    client: &reqwest::Client,
    target: &TestTarget,
    url: &str,
    body: Option<Value>,
    deadline: Instant,
) -> Answer {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Answer::Unreachable("no answer in time".to_owned());
    }
    let mut request = match &body {
        Some(body) => client.post(url).json(body),
        None => client.get(url),
    }
    .timeout(left);
    request = match target.header {
        KeyHeader::Bearer => request.bearer_auth(&target.key),
        KeyHeader::XApiKey => request.header("x-api-key", &target.key),
    };
    if target.protocol == Protocol::Anthropic {
        request = request.header("anthropic-version", ANTHROPIC_VERSION);
    }
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(e) => {
            let reason = if e.is_timeout() {
                "no answer in time".to_owned()
            } else if e.is_connect() {
                "could not connect".to_owned()
            } else {
                "the request failed".to_owned()
            };
            return Answer::Unreachable(reason);
        }
    };
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len() + chunk.len() > BODY_LIMIT {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Answer::Status(status, bytes)
}

/// Model names from an OpenAI- or Anthropic-style list.
fn listed(bytes: &[u8]) -> Option<Vec<String>> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let rows = value
        .get("data")
        .or_else(|| value.get("models"))?
        .as_array()?;
    let mut names: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.get("id")
                .or_else(|| row.get("name"))
                .and_then(Value::as_str)
                .or_else(|| row.as_str())
        })
        .filter(|name| !name.is_empty() && name.len() <= 256)
        .map(str::to_owned)
        .collect();
    names.dedup();
    names.truncate(MAX_LISTED);
    Some(names)
}

/// The start of a service's answer with the key removed.
fn snippet(key: &str, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes).replace(key, "[REDACTED]");
    let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut cut: String = text.chars().take(SNIPPET).collect();
    if cut.len() < text.len() {
        cut.push('…');
    }
    cut
}
fn detail(key: &str, status: u16, bytes: &[u8]) -> String {
    let text = snippet(key, bytes);
    if text.is_empty() {
        format!("HTTP {status}")
    } else {
        format!("HTTP {status}: {text}")
    }
}

pub(crate) async fn run(target: TestTarget) -> ServiceTestResult {
    let started = Instant::now();
    let deadline = started + BUDGET;
    let finish = |ok: bool, method: Option<&str>, endpoint: String, models, message: String| {
        ServiceTestResult {
            ok,
            method: method.map(str::to_owned),
            endpoint,
            latency_ms: started.elapsed().as_millis() as u64,
            models,
            message: message.replace(&target.key, "[REDACTED]"),
        }
    };
    let (list_url, request_url, request_body) = match target.protocol {
        Protocol::OpenAi => (
            format!("{}/models", target.base),
            format!("{}/chat/completions", target.base),
            target.model.as_ref().map(|model| {
                json!({"model":model,"messages":[{"role":"user","content":"ping"}],"max_tokens":1})
            }),
        ),
        // The Responses API accepts no fewer than 16 output tokens.
        Protocol::Responses => (
            format!("{}/models", target.base),
            format!("{}/responses", target.base),
            target
                .model
                .as_ref()
                .map(|model| json!({"model":model,"input":"ping","max_output_tokens":16})),
        ),
        Protocol::Anthropic => (
            format!("{}/v1/models?limit=1000", target.base),
            format!("{}/v1/messages", target.base),
            target.model.as_ref().map(|model| {
                json!({"model":model,"max_tokens":1,"messages":[{"role":"user","content":"ping"}]})
            }),
        ),
    };
    let client = match client(&target.base) {
        Ok(client) => client,
        Err(_) => {
            return finish(
                false,
                None,
                list_url,
                vec![],
                "could not prepare the request".into(),
            );
        }
    };
    let why_not_listed = match call(&client, &target, &list_url, None, deadline).await {
        Answer::Unreachable(reason) => {
            return finish(false, None, list_url, vec![], reason);
        }
        Answer::Status(status @ (401 | 403), bytes) => {
            let message = format!(
                "the service rejected the key ({})",
                detail(&target.key, status, &bytes)
            );
            return finish(false, Some(TESTED_BY_MODELS), list_url, vec![], message);
        }
        Answer::Status(status, bytes) if (200..300).contains(&status) => match listed(&bytes) {
            Some(models) => {
                let mut message = format!("connected; the service lists {} models", models.len());
                if let Some(model) = &target.model
                    && !models.is_empty()
                    && !models.contains(model)
                {
                    message.push_str(&format!("; {model} is not among them"));
                }
                return finish(true, Some(TESTED_BY_MODELS), list_url, models, message);
            }
            None => format!("HTTP {status} without a model list"),
        },
        Answer::Status(status, bytes) => detail(&target.key, status, &bytes),
    };
    let Some(body) = request_body else {
        return finish(
            false,
            None,
            list_url,
            vec![],
            format!(
                "the service did not list its models ({why_not_listed}); enter a model name and test again"
            ),
        );
    };
    let model = target.model.clone().unwrap_or_default();
    match call(&client, &target, &request_url, Some(body), deadline).await {
        Answer::Unreachable(reason) => finish(false, None, request_url, vec![], reason),
        Answer::Status(status, _) if (200..300).contains(&status) => finish(
            true,
            Some(TESTED_BY_REQUEST),
            request_url,
            vec![],
            format!("connected; {model} answered a minimal request"),
        ),
        Answer::Status(status, bytes) => finish(
            false,
            Some(TESTED_BY_REQUEST),
            request_url,
            vec![],
            format!(
                "{model} did not answer a minimal request ({})",
                detail(&target.key, status, &bytes)
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_lists_in_either_format_are_read() {
        assert_eq!(
            listed(br#"{"object":"list","data":[{"id":"a"},{"id":"b"}]}"#),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(
            listed(br#"{"data":[{"id":"claude-x","type":"model"}],"has_more":false}"#),
            Some(vec!["claude-x".to_owned()])
        );
        assert_eq!(
            listed(br#"{"models":[{"name":"local"}]}"#),
            Some(vec!["local".to_owned()])
        );
        assert_eq!(listed(b"<html>not found</html>"), None);
    }
    #[test]
    fn answers_never_repeat_the_key() {
        let shown = detail(
            "sk-secret-0123456789",
            401,
            b"bad key sk-secret-0123456789\n",
        );
        assert_eq!(shown, "HTTP 401: bad key [REDACTED]");
    }
}
