use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read},
    time::Duration,
};

const BASE: &str = "https://ai-gateway.vercel.sh/v1";
const MAX_STREAM_EVENT_BYTES: usize = 256 * 1024;
const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}
impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}
fn client() -> Result<Client> {
    Ok(Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .build()?)
}
#[derive(Debug)]
struct GatewayHttpError(reqwest::StatusCode);
impl std::fmt::Display for GatewayHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AI Gateway returned HTTP {}", self.0)
    }
}
impl std::error::Error for GatewayHttpError {}

fn checked(response: Response) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        // Classify before decoding: outages often return HTML, not JSON.
        // Neither remote response bodies nor credentials belong in diagnostics.
        return Err(GatewayHttpError(status).into());
    }
    response
        .json()
        .context("Gateway returned an invalid JSON response")
}

/// Safe, actionable UI copy; full error chains stay in diagnostic logs.
pub fn failure_message(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<GatewayHttpError>() {
        return match error.0.as_u16() {
            401 | 403 => "API key rejected. Reconnect in Settings.",
            402 => "AI Gateway credit is unavailable. Check your Gateway account.",
            429 => "AI Gateway rate limit reached. Try again shortly.",
            500..=599 => "AI Gateway is unavailable. Try again.",
            _ => "AI Gateway request failed. Check your model and retry.",
        };
    }
    if let Some(error) = error.downcast_ref::<reqwest::Error>() {
        if error.is_timeout() {
            return "AI Gateway timed out. Try again.";
        }
        if error.is_connect() {
            return "Cannot reach AI Gateway. Check your connection.";
        }
        if error.is_decode() {
            return "AI Gateway returned an invalid response. Retry.";
        }
    }
    let message = error.to_string();
    match message.as_str() {
        "Model returned no text. Choose a text/chat model." => {
            "Model returned no text. Choose a text/chat model in Settings."
        }
        "Model returned an empty response" => "Model returned an empty response. Retry.",
        "Response was truncated. Try a shorter input or a different model." => {
            "Response was truncated. Shorten your input or choose another model."
        }
        _ => "AI Gateway request failed. Check your connection and retry.",
    }
}
pub fn authenticate(key: &str) -> Result<()> {
    let client = client()?;
    // Models is public; credits is authenticated and verifies the key without generating text.
    checked(
        client
            .get(format!("{BASE}/credits"))
            .bearer_auth(key)
            .send()
            .context("Cannot reach AI Gateway")?,
    )?;
    Ok(())
}
pub fn models() -> Result<Vec<String>> {
    let value = checked(
        client()?
            .get(format!("{BASE}/models"))
            .send()
            .context("Cannot load AI Gateway models")?,
    )?;
    parse_models(&value)
}
fn parse_models(value: &Value) -> Result<Vec<String>> {
    let data = value["data"]
        .as_array()
        .context("Gateway returned an invalid model catalog")?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        bail!("AI Gateway returned no available models");
    }
    Ok(models)
}
pub fn complete(key: &str, model: &str, messages: &[Message]) -> Result<String> {
    let value = checked(
        client()?
            .post(format!("{BASE}/chat/completions"))
            .bearer_auth(key)
            .json(&json!({"model": model, "messages": messages, "stream": false}))
            .send()
            .context("Cannot reach AI Gateway")?,
    )?;
    parse_completion(&value)
}
/// Streams text deltas without putting incomplete answers into conversation history.
pub fn complete_stream(
    key: &str,
    model: &str,
    messages: &[Message],
    on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let response = client()?
        .post(format!("{BASE}/chat/completions"))
        .bearer_auth(key)
        .header(reqwest::header::ACCEPT, "text/event-stream")
        .json(&json!({"model": model, "messages": messages, "stream": true}))
        .send()
        .context("Cannot reach AI Gateway")?;
    if !response.status().is_success() {
        return Err(GatewayHttpError(response.status()).into());
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .unwrap_or_default();
    if !content_type
        .trim()
        .eq_ignore_ascii_case("text/event-stream")
    {
        bail!("Gateway returned an invalid streaming response");
    }
    parse_stream(BufReader::new(response), on_delta)
}

fn parse_stream(
    mut reader: impl BufRead,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let mut text = String::new();
    let mut data = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        // Bound reads before allocating, even if a server sends no newline.
        let read = reader
            .by_ref()
            .take((MAX_STREAM_EVENT_BYTES + 1) as u64)
            .read_line(&mut line)
            .context("Cannot read AI Gateway stream")?;
        if read == 0 {
            break;
        }
        if line.len() + data.len() > MAX_STREAM_EVENT_BYTES {
            bail!("AI Gateway stream event is too large");
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if !line.is_empty() {
            // SSE comments, event names and IDs carry no completion text.
            if let Some(value) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
            continue;
        }
        if data.is_empty() {
            continue;
        }
        if data.trim() == "[DONE]" {
            if text.trim().is_empty() {
                bail!("Model returned an empty response");
            }
            return Ok(text);
        }
        let value: Value = serde_json::from_str(&data)
            .context("Gateway returned an invalid streaming response")?;
        data.clear();
        if value.get("error").is_some() {
            // Provider error bodies may contain private input; never expose them.
            bail!("AI Gateway stream failed");
        }
        if value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            == Some("length")
        {
            bail!("Response was truncated. Try a shorter input or a different model.");
        }
        if let Some(delta) = value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            && !delta.is_empty()
        {
            if text.len() + delta.len() > MAX_STREAM_TEXT_BYTES {
                bail!("AI Gateway response is too large");
            }
            text.push_str(delta);
            on_delta(delta)?;
        }
    }
    // An abruptly closed connection is not a successful, complete answer.
    bail!("AI Gateway stream ended before completion")
}

fn parse_completion(value: &Value) -> Result<String> {
    let text = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .context("Model returned no text. Choose a text/chat model.")?;
    if text.trim().is_empty() {
        bail!("Model returned an empty response");
    }
    if value
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        bail!("Response was truncated. Try a shorter input or a different model.");
    }
    Ok(text.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_catalog_is_sorted_deduplicated_and_validated() {
        assert_eq!(
            parse_models(
                &json!({"data":[{"id":"z/model"},{"id":"a/model"},{"id":"z/model"},{"id":""},{}]})
            )
            .unwrap(),
            vec!["a/model", "z/model"]
        );
        assert!(parse_models(&json!({"data":[]})).is_err());
        assert!(parse_models(&json!({})).is_err());
    }
    #[test]
    fn http_failure_messages_are_safe_and_actionable() {
        let cases = [
            (401, "API key rejected. Reconnect in Settings."),
            (403, "API key rejected. Reconnect in Settings."),
            (
                402,
                "AI Gateway credit is unavailable. Check your Gateway account.",
            ),
            (429, "AI Gateway rate limit reached. Try again shortly."),
            (503, "AI Gateway is unavailable. Try again."),
        ];
        for (status, message) in cases {
            let error = anyhow::Error::new(GatewayHttpError(
                reqwest::StatusCode::from_u16(status).unwrap(),
            ));
            assert_eq!(failure_message(&error), message);
        }
    }
    #[test]
    fn arbitrary_remote_details_are_not_used_as_ui_copy() {
        assert_eq!(
            failure_message(&anyhow::anyhow!("untrusted remote body")),
            "AI Gateway request failed. Check your connection and retry."
        );
    }
    #[test]
    fn streaming_emits_text_in_order_and_ignores_metadata() {
        let input = concat!(
            ": heartbeat\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hé\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"llo 👋\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data: {\"choices\":[],\"usage\":{}}\r\n\r\n",
            "data: [DONE]\r\n\r\n",
        );
        let mut deltas = Vec::new();
        // Tiny buffers split even multibyte UTF-8 characters across reads.
        let text = parse_stream(BufReader::with_capacity(1, input.as_bytes()), |delta| {
            deltas.push(delta.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            (text, deltas),
            ("Héllo 👋".into(), vec!["Hé".into(), "llo 👋".into()])
        );
    }

    #[test]
    fn streaming_supports_multiline_data() {
        let input = "event: message\ndata: {\"choices\":\ndata: [{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n";
        assert_eq!(parse_stream(input.as_bytes(), |_| Ok(())).unwrap(), "Hello");
    }

    #[test]
    fn streaming_rejects_disconnects_after_partial_text() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"Half\"}}]}\n\n";
        assert!(parse_stream(input.as_bytes(), |_| Ok(())).is_err());
    }

    #[test]
    fn streaming_rejects_truncated_answers() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"Half\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
        let error = parse_stream(input.as_bytes(), |_| Ok(())).unwrap_err();
        assert_eq!(
            failure_message(&error),
            "Response was truncated. Shorten your input or choose another model."
        );
    }

    #[test]
    fn streaming_rejects_oversized_events_before_decoding() {
        let input = format!("data: {}", "x".repeat(MAX_STREAM_EVENT_BYTES));
        assert_eq!(
            parse_stream(input.as_bytes(), |_| Ok(()))
                .unwrap_err()
                .to_string(),
            "AI Gateway stream event is too large"
        );
    }

    #[test]
    fn streaming_delivers_delta_before_http_body_finishes() {
        use std::{io::Write, net::TcpListener, sync::mpsc, thread};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = BufReader::new(socket.try_clone().unwrap());
            loop {
                let mut line = String::new();
                request.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n").unwrap();
            socket.flush().unwrap();
            // The rest of the body cannot arrive until the consumer saw the first delta.
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
            socket.write_all(b"data: [DONE]\n\n").unwrap();
        });
        let response = Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .unwrap();
        let result = parse_stream(BufReader::new(response), |_| {
            tx.send(()).unwrap();
            Ok(())
        });
        server.join().unwrap();
        assert_eq!(result.unwrap(), "Hello");
    }

    #[test]
    fn streaming_rejects_empty_answers() {
        assert!(parse_stream("data: [DONE]\n\n".as_bytes(), |_| Ok(())).is_err());
    }

    #[test]
    fn streaming_rejects_invalid_json() {
        assert!(parse_stream("data: not json\n\n".as_bytes(), |_| Ok(())).is_err());
    }

    #[test]
    fn streaming_redacts_provider_errors() {
        let error = parse_stream(
            "data: {\"error\":{\"message\":\"private input\"}}\n\n".as_bytes(),
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(!format!("{error:#}").contains("private input"));
    }

    #[test]
    fn streaming_stops_when_consumer_closes() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\ndata: [DONE]\n\n";
        assert!(parse_stream(input.as_bytes(), |_| bail!("closed")).is_err());
    }

    #[test]
    fn parses_text() {
        assert_eq!(
            parse_completion(&json!({"choices":[{"message":{"content":"Hello"}}]})).unwrap(),
            "Hello"
        );
    }
    #[test]
    fn rejects_missing_or_truncated_text() {
        assert!(parse_completion(&json!({"choices":[]})).is_err());
        assert!(
            parse_completion(
                &json!({"choices":[{"message":{"content":"half"},"finish_reason":"length"}]})
            )
            .is_err()
        );
    }
}
