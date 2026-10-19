//! Local Ollama backend. Requests go only to the fixed loopback API, never
//! leave this Mac and carry no Wiesel credential.
use crate::gateway::Message;
use anyhow::Result;
use reqwest::blocking::{Client, Response};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::Duration,
};

const BASE: &str = "http://127.0.0.1:11434/api";
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STREAM_LINE_BYTES: usize = 1024 * 1024;
const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
/// GUI apps do not inherit a shell PATH, so look where installers put Ollama.
const INSTALL_PATHS: [&str; 4] = [
    "/Applications/Ollama.app",
    "/usr/local/bin/ollama",
    "/opt/homebrew/bin/ollama",
    "Applications/Ollama.app",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModel {
    pub name: String,
    /// Thinking models are asked not to think; their reasoning is never shown.
    pub thinking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    NotInstalled,
    NotRunning,
    Running {
        version: String,
        models: Vec<LocalModel>,
    },
}
impl Status {
    pub fn models(&self) -> &[LocalModel] {
        match self {
            Self::Running { models, .. } => models,
            _ => &[],
        }
    }
    pub fn thinking(&self, model: &str) -> bool {
        self.models()
            .iter()
            .any(|local| local.name == model && local.thinking)
    }
}

/// Safe, user-facing copy. Local server bodies are never surfaced verbatim.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OllamaError(pub(crate) &'static str);

pub fn failure_message(error: &anyhow::Error) -> Option<&'static str> {
    error.downcast_ref::<OllamaError>().map(|error| error.0)
}

fn fail(message: &'static str) -> anyhow::Error {
    OllamaError(message).into()
}

fn client(timeout: Duration) -> Result<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .timeout(timeout)
        .build()
        .map_err(|_| fail("Cannot create the Ollama HTTP client."))
}

/// Probes the local server first; a reachable server counts even without a
/// known install location (e.g. a custom build).
// In-process fixtures release status explicitly instead of probing the network.
#[cfg_attr(test, allow(dead_code))]
pub fn status() -> Status {
    match running() {
        Ok(status) => status,
        Err(_) if installed(std::env::var_os("HOME").map(PathBuf::from).as_deref()) => {
            Status::NotRunning
        }
        Err(_) => Status::NotInstalled,
    }
}

fn installed(home: Option<&Path>) -> bool {
    INSTALL_PATHS.iter().any(|path| {
        let path = Path::new(path);
        if path.is_absolute() {
            path.exists()
        } else {
            home.is_some_and(|home| home.join(path).exists())
        }
    })
}

fn running() -> Result<Status> {
    let client = client(Duration::from_secs(5))?;
    let version = get_json(&client, "version")?;
    let tags = get_json(&client, "tags")?;
    Ok(Status::Running {
        version: version["version"].as_str().unwrap_or_default().to_owned(),
        models: parse_models(&tags),
    })
}

fn get_json(client: &Client, path: &str) -> Result<Value> {
    let response = client
        .get(format!("{BASE}/{path}"))
        .send()
        .map_err(|_| fail("Ollama is not running."))?;
    if !response.status().is_success() {
        return Err(fail("Ollama returned an error."));
    }
    read_json(response)
}

fn read_json(response: Response) -> Result<Value> {
    let mut bytes = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("Cannot read the Ollama response."))?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(fail("Ollama response is too large."));
    }
    serde_json::from_slice(&bytes).map_err(|_| fail("Ollama returned an invalid response."))
}

/// Chat-capable models only. Older servers omit capabilities; keep those models.
fn parse_models(value: &Value) -> Vec<LocalModel> {
    let mut models: Vec<LocalModel> = value["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let name = model["name"].as_str()?.trim();
            let capabilities = model["capabilities"].as_array();
            let has = |capability: &str| {
                capabilities.is_some_and(|all| all.iter().any(|c| c.as_str() == Some(capability)))
            };
            (!name.is_empty() && (capabilities.is_none() || has("completion"))).then(|| {
                LocalModel {
                    name: name.to_owned(),
                    thinking: has("thinking"),
                }
            })
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    models.dedup_by(|a, b| a.name == b.name);
    models
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    think: Option<bool>,
}

fn chat(model: &str, messages: &[Message], stream: bool, thinking: bool) -> Result<Response> {
    // Loading a large model on first use can take minutes.
    let client = client(Duration::from_secs(600))?;
    let response = client
        .post(format!("{BASE}/chat"))
        .json(&ChatRequest {
            model,
            messages,
            stream,
            think: thinking.then_some(false),
        })
        .send()
        .map_err(|error| {
            fail(if error.is_connect() {
                "Ollama is not running. Start Ollama, or turn it off in Settings."
            } else if error.is_timeout() {
                "Ollama took too long to respond."
            } else {
                "Cannot complete the Ollama request."
            })
        })?;
    match response.status().as_u16() {
        200..=299 => Ok(response),
        404 => Err(fail(
            "This model is no longer in Ollama. Choose another in Settings.",
        )),
        _ => Err(fail("Ollama could not complete the request.")),
    }
}

pub fn complete(model: &str, messages: &[Message], thinking: bool) -> Result<String> {
    let value = read_json(chat(model, messages, false, thinking)?)?;
    let text = message_text(&value)?;
    finished(&value, text.to_owned())
}

pub fn complete_stream(
    model: &str,
    messages: &[Message],
    thinking: bool,
    on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    parse_stream(
        BufReader::new(chat(model, messages, true, thinking)?),
        on_delta,
    )
}

fn message_text(value: &Value) -> Result<&str> {
    if value.get("error").is_some() {
        return Err(fail("Ollama could not complete the request."));
    }
    value
        .pointer("/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| fail("Ollama returned an invalid response."))
}

fn finished(value: &Value, text: String) -> Result<String> {
    if value["done_reason"].as_str() == Some("length") {
        return Err(fail("Model response was truncated before completion."));
    }
    if text.trim().is_empty() {
        return Err(fail("Model returned an empty response."));
    }
    Ok(text)
}

/// Ollama streams newline-delimited JSON objects; the last one has `done: true`.
fn parse_stream(
    mut reader: impl BufRead,
    mut on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let mut text = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        // Bound reads before allocating, even if the server sends no newline.
        let read = reader
            .by_ref()
            .take(MAX_STREAM_LINE_BYTES as u64 + 1)
            .read_line(&mut line)
            .map_err(|_| fail("Cannot read the Ollama stream."))?;
        if read == 0 {
            return Err(fail("Ollama stream ended before completion."));
        }
        if read > MAX_STREAM_LINE_BYTES {
            return Err(fail("Ollama response is too large."));
        }
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(&line)
            .map_err(|_| fail("Ollama returned an invalid response."))?;
        let delta = message_text(&value)?;
        if !delta.is_empty() {
            if text.len() + delta.len() > MAX_STREAM_TEXT_BYTES {
                return Err(fail("Ollama response is too large."));
            }
            text.push_str(delta);
            on_delta(delta).map_err(|_| fail("Streaming consumer closed before completion."))?;
        }
        if value["done"].as_bool() == Some(true) {
            return finished(&value, text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream(lines: &[Value]) -> String {
        lines.iter().map(|line| format!("{line}\n")).collect()
    }

    #[test]
    fn models_keep_chat_capable_and_legacy_entries_sorted() {
        let tags = json!({"models": [
            {"name": "qwen3.5:0.8b", "capabilities": ["completion", "thinking"]},
            {"name": "nomic-embed-text", "capabilities": ["embedding"]},
            {"name": "llama3:8b"},
            {"name": " "},
            {"name": "llama3:8b"},
        ]});
        assert_eq!(
            parse_models(&tags),
            vec![
                LocalModel {
                    name: "llama3:8b".into(),
                    thinking: false
                },
                LocalModel {
                    name: "qwen3.5:0.8b".into(),
                    thinking: true
                },
            ]
        );
        assert!(parse_models(&json!({})).is_empty());
    }

    #[test]
    fn thinking_models_are_asked_not_to_think() {
        let body = |thinking: bool| {
            serde_json::to_value(ChatRequest {
                model: "m",
                messages: &[Message::new("user", "hi")],
                stream: true,
                think: thinking.then_some(false),
            })
            .unwrap()
        };
        assert_eq!(body(true)["think"], json!(false));
        assert!(body(false).get("think").is_none());
    }

    #[test]
    fn stream_forwards_deltas_until_done() {
        let input = stream(&[
            json!({"message": {"role": "assistant", "content": "Hel"}, "done": false}),
            json!({"message": {"role": "assistant", "content": "lo"}, "done": false}),
            json!({"message": {"role": "assistant", "content": ""}, "done": true, "done_reason": "stop"}),
        ]);
        let mut deltas = Vec::new();
        let text = parse_stream(input.as_bytes(), |delta| {
            deltas.push(delta.to_owned());
            Ok(())
        })
        .unwrap();
        assert_eq!(text, "Hello");
        assert_eq!(deltas, ["Hel", "lo"]);
    }

    #[test]
    fn stream_rejects_errors_truncation_and_early_end() {
        let message = |input: String| {
            failure_message(&parse_stream(input.as_bytes(), |_| Ok(())).unwrap_err()).unwrap()
        };
        assert_eq!(
            message(stream(&[json!({"error": "secret prompt echo"})])),
            "Ollama could not complete the request."
        );
        assert_eq!(
            message(stream(&[
                json!({"message": {"content": "a"}, "done": true, "done_reason": "length"})
            ])),
            "Model response was truncated before completion."
        );
        assert_eq!(
            message(stream(&[
                json!({"message": {"content": "a"}, "done": false})
            ])),
            "Ollama stream ended before completion."
        );
        assert_eq!(
            message(stream(&[
                json!({"message": {"content": " "}, "done": true})
            ])),
            "Model returned an empty response."
        );
    }

    #[test]
    fn install_detection_checks_home_relative_app() {
        let home = std::env::temp_dir().join(format!(
            "wiesel-ollama-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("Applications/Ollama.app")).unwrap();
        assert!(installed(Some(&home)));
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    #[ignore = "requires a running local Ollama with at least one chat model"]
    fn live_ollama_round_trip() {
        let status = status();
        let model = status.models().first().expect("an installed chat model");
        let messages = [Message::new("user", "Reply with the single word: ready")];
        let mut deltas = 0;
        let text = complete_stream(&model.name, &messages, model.thinking, |_| {
            deltas += 1;
            Ok(())
        })
        .unwrap();
        assert!(deltas > 0 && !text.trim().is_empty());
        assert!(
            !complete(&model.name, &messages, model.thinking)
                .unwrap()
                .trim()
                .is_empty()
        );
    }
}
