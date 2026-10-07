use anyhow::{Context, Result, anyhow, bail};
use reqwest::blocking::{Client, ClientBuilder, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    time::Duration,
};

const BASE: &str = "https://wiesel.run/v1";
const MAX_REQUEST_BODY_BYTES: usize = 4_000_000;
const MAX_TOKENS: u32 = 2048;
const MAX_STREAM_EVENT_BYTES: usize = 1024 * 1024;
const MAX_STREAM_TEXT_BYTES: usize = 1024 * 1024;
const UNKNOWN_OUTCOME: &str =
    "The request outcome is unknown and may have been charged. Do not retry blindly.";
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
fn client_builder() -> ClientBuilder {
    // No cookie jar is configured. TLS certificate and hostname validation stay enabled.
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .danger_accept_invalid_certs(false)
        .danger_accept_invalid_hostnames(false)
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
}
fn client() -> Result<Client> {
    client_builder()
        .build()
        .map_err(|_| anyhow!("Cannot create the Wiesel HTTP client"))
}
fn send(request: RequestBuilder) -> Result<Response> {
    // Discard reqwest errors: even their source chains can contain URLs or credentials.
    request
        .send()
        .map_err(|_| anyhow!("Cannot complete the Wiesel request"))
}
#[derive(Debug)]
struct GatewayHttpError(reqwest::StatusCode);
impl std::fmt::Display for GatewayHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wiesel returned HTTP {}", self.0)
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
        .map_err(|_| anyhow!("Wiesel returned an invalid JSON response"))
}

/// Whether the UI must discard the rejected device credential and require login again.
pub fn is_unauthorized(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<GatewayHttpError>()
        .is_some_and(|error| error.0 == reqwest::StatusCode::UNAUTHORIZED)
}

/// Safe UI copy that never includes remote bodies, credentials, or raw transport errors.
pub fn failure_message(error: &anyhow::Error) -> &'static str {
    if let Some(error) = error.downcast_ref::<GatewayHttpError>() {
        return match error.0.as_u16() {
            401 => "Your session expired or was rejected. Log in again.",
            403 => "Access denied. Check your account permissions.",
            402 => "Insufficient allowance for this request.",
            409 => "This action was already submitted. Do not retry.",
            429 => "Too many requests. Wait before submitting another action.",
            _ => UNKNOWN_OUTCOME,
        };
    }
    match error.to_string().as_str() {
        "Model returned no text. Choose a text/chat model." => {
            "Model returned no text. Choose a text/chat model in Settings."
        }
        "Wiesel request body is too large" => "Input is too large. Shorten it before submitting.",
        _ => UNKNOWN_OUTCOME,
    }
}

/// Loads the model catalog using the current device Bearer token.
pub fn models(token: &str) -> Result<Vec<String>> {
    let value = checked(send(models_request(&client()?, token))?)?;
    parse_models(&value)
}
fn models_request(client: &Client, token: &str) -> RequestBuilder {
    client.get(format!("{BASE}/models")).bearer_auth(token)
}
fn parse_models(value: &Value) -> Result<Vec<String>> {
    let data = value["data"]
        .as_array()
        .context("Wiesel returned an invalid model catalog")?;
    let mut models: Vec<String> = data
        .iter()
        .filter_map(|model| model["id"].as_str())
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .collect();
    models.sort();
    models.dedup();
    if models.is_empty() {
        bail!("Wiesel returned no available models");
    }
    Ok(models)
}
#[derive(Serialize)]
struct CompletionRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    max_tokens: u32,
}

struct BoundedRequestBody(Vec<u8>);
impl Write for BoundedRequestBody {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_REQUEST_BODY_BYTES - self.0.len() {
            return Err(io::Error::other("Wiesel request body is too large"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn completion_body(model: &str, messages: &[Message], stream: bool) -> Result<Vec<u8>> {
    // Bound serialized bytes (including UTF-8 and escaping) during encoding, before any send.
    let mut body = BoundedRequestBody(Vec::new());
    // A closed schema allows only text/chat fields and exactly one integer token cap.
    // Its only possible serialization failure is exceeding the writer's byte limit.
    serde_json::to_writer(
        &mut body,
        &CompletionRequest {
            model,
            messages,
            stream,
            max_tokens: MAX_TOKENS,
        },
    )
    .map_err(|_| anyhow!("Wiesel request body is too large"))?;
    Ok(body.0)
}
fn completion_request(
    client: &Client,
    token: &str,
    model: &str,
    messages: &[Message],
    stream: bool,
) -> Result<RequestBuilder> {
    let body = completion_body(model, messages, stream)?;
    let request = client
        .post(format!("{BASE}/chat/completions"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    Ok(if stream {
        request.header(reqwest::header::ACCEPT, "text/event-stream")
    } else {
        request
    })
}

/// Completes a text/chat request authenticated with the current device token.
pub fn complete(token: &str, model: &str, messages: &[Message]) -> Result<String> {
    let value = checked(send(completion_request(
        &client()?,
        token,
        model,
        messages,
        false,
    )?)?)?;
    parse_completion(&value)
}
/// Streams text deltas without putting incomplete answers into conversation history.
pub fn complete_stream(
    token: &str,
    model: &str,
    messages: &[Message],
    on_delta: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let response = send(completion_request(
        &client()?,
        token,
        model,
        messages,
        true,
    )?)?;
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
        bail!("Wiesel returned an invalid streaming response");
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
    let mut event_bytes = 0;
    loop {
        line.clear();
        // Bound reads before allocating, even if a server sends no newline.
        let read = reader
            .by_ref()
            .take((MAX_STREAM_EVENT_BYTES - event_bytes + 1) as u64)
            .read_line(&mut line)
            .map_err(|_| anyhow!("Cannot read Wiesel stream"))?;
        if read == 0 {
            break;
        }
        event_bytes += read;
        if event_bytes > MAX_STREAM_EVENT_BYTES {
            bail!("Wiesel stream event is too large");
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
        event_bytes = 0;
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
            .map_err(|_| anyhow!("Wiesel returned an invalid streaming response"))?;
        data.clear();
        if value.get("error").is_some() {
            // Provider error bodies may contain private input; never expose them.
            bail!("Wiesel stream failed");
        }
        if value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
            == Some("length")
        {
            bail!("Model response was truncated before completion");
        }
        if let Some(delta) = value
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            && !delta.is_empty()
        {
            if text.len() + delta.len() > MAX_STREAM_TEXT_BYTES {
                bail!("Wiesel response is too large");
            }
            text.push_str(delta);
            on_delta(delta).map_err(|_| anyhow!("Streaming consumer closed before completion"))?;
        }
    }
    // An abruptly closed connection is not a successful, complete answer.
    bail!("Wiesel stream ended before completion")
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
        bail!("Model response was truncated before completion");
    }
    Ok(text.to_owned())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        net::{TcpListener, TcpStream},
        thread,
    };

    fn mock_response(response: String) -> (reqwest::Url, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_mock_request(&socket);
            socket.write_all(response.as_bytes()).unwrap();
            request
        });
        (
            format!("http://{address}/v1/models").parse().unwrap(),
            server,
        )
    }

    fn read_mock_request(socket: &TcpStream) -> String {
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut request = String::new();
        let mut content_length = 0;
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            request.push_str(&line);
            if line == "\r\n" {
                break;
            }
            if let Some(length) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_length = length.trim().parse::<usize>().unwrap();
            }
        }
        assert!(content_length <= MAX_REQUEST_BODY_BYTES);
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).unwrap();
        request.push_str(std::str::from_utf8(&body).unwrap());
        request
    }

    fn mock_client() -> Client {
        // Only local mocks allow plaintext; production clients remain HTTPS-only.
        client_builder()
            .https_only(false)
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    #[test]
    fn models_request_uses_fixed_backend_and_device_bearer_only() {
        let request = models_request(&client().unwrap(), "test-device-token")
            .build()
            .unwrap();
        assert_eq!(request.method(), reqwest::Method::GET);
        assert_eq!(request.url().as_str(), "https://wiesel.run/v1/models");
        assert_eq!(request.headers().len(), 1);
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer test-device-token"
        );
        assert!(request.body().is_none());
    }

    #[test]
    fn models_request_authenticates_and_parses_a_mock_catalog() {
        let (url, server) = mock_response(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"data\":[{\"id\":\"z/model\"},{\"id\":\"a/model\"}]}".into(),
        );
        let client = mock_client();
        let mut request = models_request(&client, "test-device-token")
            .build()
            .unwrap();
        *request.url_mut() = url;
        let value = checked(client.execute(request).unwrap()).unwrap();
        let wire = server.join().unwrap().to_ascii_lowercase();
        assert!(wire.starts_with("get /v1/models http/1.1\r\n"));
        assert!(wire.contains("\r\nauthorization: bearer test-device-token\r\n"));
        assert_eq!(parse_models(&value).unwrap(), vec!["a/model", "z/model"]);
    }

    #[test]
    fn completion_requests_allow_only_capped_text_chat_fields_and_safe_headers() {
        for stream in [false, true] {
            let request = completion_request(
                &client().unwrap(),
                "test-device-token",
                "test/model",
                &[
                    Message::new("system", "Be helpful"),
                    Message::new("user", "Hé 👋"),
                ],
                stream,
            )
            .unwrap()
            .build()
            .unwrap();
            assert_eq!(request.method(), reqwest::Method::POST);
            assert_eq!(
                request.url().as_str(),
                "https://wiesel.run/v1/chat/completions"
            );
            let headers = request.headers();
            assert_eq!(
                headers[reqwest::header::AUTHORIZATION],
                "Bearer test-device-token"
            );
            assert_eq!(headers[reqwest::header::CONTENT_TYPE], "application/json");
            assert_eq!(headers.len(), if stream { 3 } else { 2 });
            assert_eq!(headers.contains_key(reqwest::header::ACCEPT), stream);
            if stream {
                assert_eq!(headers[reqwest::header::ACCEPT], "text/event-stream");
            }
            let body = request.body().unwrap().as_bytes().unwrap();
            assert!(body.len() <= MAX_REQUEST_BODY_BYTES);
            assert_eq!(
                std::str::from_utf8(body)
                    .unwrap()
                    .matches("\"max_tokens\":")
                    .count(),
                1
            );
            // Exact whitelist excludes all provider-routing, billing and alternate token fields.
            assert_eq!(
                serde_json::from_slice::<Value>(body).unwrap(),
                json!({
                    "model": "test/model",
                    "messages": [{"role":"system", "content":"Be helpful"}, {"role":"user", "content":"Hé 👋"}],
                    "stream": stream,
                    "max_tokens": 2048
                })
            );
            let value: Value = serde_json::from_slice(body).unwrap();
            assert!(value["max_tokens"].as_u64().unwrap() <= 8192);
        }
    }

    #[test]
    fn completion_request_authenticates_and_preserves_text_on_mock_wire() {
        let (mut url, server) = mock_response(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"Hello\"}}]}".into(),
        );
        url.set_path("/v1/chat/completions");
        let client = mock_client();
        let mut request = completion_request(
            &client,
            "test-device-token",
            "test/model",
            &[Message::new("user", "Hi")],
            false,
        )
        .unwrap()
        .build()
        .unwrap();
        *request.url_mut() = url;
        let value = checked(client.execute(request).unwrap()).unwrap();
        let wire = server.join().unwrap();
        assert!(wire.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(wire.contains("authorization: Bearer test-device-token\r\n"));
        let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["messages"][0]["content"], "Hi");
        assert_eq!(parse_completion(&value).unwrap(), "Hello");
    }

    #[test]
    fn completion_body_enforces_exact_serialized_byte_limit_for_both_modes() {
        for stream in [false, true] {
            let overhead = completion_body("m", &[Message::new("user", "")], stream)
                .unwrap()
                .len();
            let input = "x".repeat(MAX_REQUEST_BODY_BYTES - overhead);
            assert_eq!(
                completion_body("m", &[Message::new("user", &input)], stream)
                    .unwrap()
                    .len(),
                MAX_REQUEST_BODY_BYTES
            );
            let error = completion_body("m", &[Message::new("user", format!("{input}x"))], stream)
                .unwrap_err();
            assert_eq!(error.to_string(), "Wiesel request body is too large");
            assert_eq!(
                failure_message(&error),
                "Input is too large. Shorten it before submitting."
            );
        }
    }

    #[test]
    fn completion_body_counts_json_escaping_not_raw_input_size() {
        let input = "\"".repeat(MAX_REQUEST_BODY_BYTES / 2);
        assert!(completion_body("m", &[Message::new("user", input)], false).is_err());
    }

    #[test]
    fn completion_body_counts_utf8_bytes_not_character_count() {
        let input = "é".repeat(MAX_REQUEST_BODY_BYTES / 2);
        assert!(completion_body("m", &[Message::new("user", input)], true).is_err());
    }

    #[test]
    fn client_does_not_follow_redirects_or_forward_device_token() {
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let location = format!("http://{}/redirected", destination.local_addr().unwrap());
        let (url, server) = mock_response(format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ));
        let response = send(mock_client().get(url).bearer_auth("test-device-token")).unwrap();
        server.join().unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            destination.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn client_does_not_store_or_send_response_cookies() {
        let client = mock_client();
        let (url, server) = mock_response("HTTP/1.1 200 OK\r\nSet-Cookie: test=value; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into());
        send(client.get(url)).unwrap();
        server.join().unwrap();
        let (url, server) = mock_response(
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        );
        send(client.get(url)).unwrap();
        assert!(
            !server
                .join()
                .unwrap()
                .to_ascii_lowercase()
                .contains("\r\ncookie:")
        );
    }

    #[test]
    fn production_client_rejects_plaintext_and_sanitizes_transport_error_chains() {
        // No connection is attempted: HTTPS-only validation rejects this URL locally.
        let error = send(client().unwrap().get("http://127.0.0.1:1/?private=fixture")).unwrap_err();
        assert_eq!(format!("{error:#}"), "Cannot complete the Wiesel request");
        assert_eq!(failure_message(&error), UNKNOWN_OUTCOME);
    }

    #[test]
    fn send_sanitizes_timeouts_and_warns_about_potential_charges() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            read_mock_request(&socket);
            thread::sleep(Duration::from_millis(500));
        });
        let client = client_builder()
            .https_only(false)
            .no_proxy()
            .timeout(Duration::from_millis(200))
            .build()
            .unwrap();
        let error = send(client.get(format!("http://{address}/v1/chat/completions"))).unwrap_err();
        server.join().unwrap();
        assert_eq!(format!("{error:#}"), "Cannot complete the Wiesel request");
        assert_eq!(failure_message(&error), UNKNOWN_OUTCOME);
    }

    #[test]
    fn checked_redacts_remote_error_bodies_and_preserves_unauthorized_status() {
        let (url, server) = mock_response(
            "HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\nprivate remote details".into(),
        );
        let error = checked(send(mock_client().get(url)).unwrap())
            .unwrap_err()
            .context("Loading models");
        server.join().unwrap();
        assert!(is_unauthorized(&error));
        assert!(!format!("{error:#}").contains("private remote details"));
        assert_eq!(
            failure_message(&error),
            "Your session expired or was rejected. Log in again."
        );
    }

    #[test]
    fn unauthorized_classification_does_not_clear_credentials_on_other_failures() {
        for status in [403, 402, 409, 429, 503] {
            let error = GatewayHttpError(reqwest::StatusCode::from_u16(status).unwrap()).into();
            assert!(!is_unauthorized(&error));
        }
        assert!(!is_unauthorized(&anyhow!("untrusted error")));
    }

    #[test]
    fn checked_redacts_invalid_success_response_details() {
        let (url, server) = mock_response(
            "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nprivate invalid JSON".into(),
        );
        let error = checked(send(mock_client().get(url)).unwrap()).unwrap_err();
        server.join().unwrap();
        assert_eq!(
            format!("{error:#}"),
            "Wiesel returned an invalid JSON response"
        );
        assert_eq!(failure_message(&error), UNKNOWN_OUTCOME);
    }

    #[test]
    fn streaming_accepts_events_up_to_one_mib_and_resets_the_limit() {
        let prefix = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}";
        let event = format!(
            "{prefix}{}\n\n",
            " ".repeat(MAX_STREAM_EVENT_BYTES - prefix.len() - 2)
        );
        let input = format!("{event}{event}data: [DONE]\n\n");
        assert_eq!(
            parse_stream(input.as_bytes(), |_| Ok(())).unwrap(),
            "HelloHello"
        );
    }

    #[test]
    fn streaming_event_limit_includes_metadata_and_comment_lines() {
        let input = format!(
            "{}data: [DONE]\n\n",
            ": comment\n".repeat(MAX_STREAM_EVENT_BYTES / 10 + 1)
        );
        let error = parse_stream(input.as_bytes(), |_| Ok(())).unwrap_err();
        assert_eq!(error.to_string(), "Wiesel stream event is too large");
    }

    #[test]
    fn streaming_keeps_total_text_bounded() {
        let chunk = "x".repeat(MAX_STREAM_TEXT_BYTES / 2);
        let event = format!(
            "data: {}\n\n",
            json!({"choices":[{"delta":{"content":chunk}}]})
        );
        let input = format!("{event}{event}{event}data: [DONE]\n\n");
        assert_eq!(
            parse_stream(input.as_bytes(), |_| Ok(()))
                .unwrap_err()
                .to_string(),
            "Wiesel response is too large"
        );
    }

    #[test]
    fn streaming_redacts_read_error_chains_and_warns_about_unknown_outcome() {
        struct BrokenReader;
        impl Read for BrokenReader {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("private transport detail"))
            }
        }
        let error = parse_stream(BufReader::new(BrokenReader), |_| Ok(())).unwrap_err();
        assert_eq!(format!("{error:#}"), "Cannot read Wiesel stream");
        assert_eq!(failure_message(&error), UNKNOWN_OUTCOME);
    }

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
            (401, "Your session expired or was rejected. Log in again."),
            (403, "Access denied. Check your account permissions."),
            (402, "Insufficient allowance for this request."),
            (409, "This action was already submitted. Do not retry."),
            (
                429,
                "Too many requests. Wait before submitting another action.",
            ),
            (503, UNKNOWN_OUTCOME),
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
            UNKNOWN_OUTCOME
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
        assert_eq!(
            failure_message(&parse_stream(input.as_bytes(), |_| Ok(())).unwrap_err()),
            UNKNOWN_OUTCOME
        );
    }

    #[test]
    fn streaming_rejects_truncated_answers() {
        let input = "data: {\"choices\":[{\"delta\":{\"content\":\"Half\"},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n\n";
        let error = parse_stream(input.as_bytes(), |_| Ok(())).unwrap_err();
        assert_eq!(failure_message(&error), UNKNOWN_OUTCOME);
    }

    #[test]
    fn streaming_rejects_oversized_events_before_decoding() {
        let input = format!("data: {}", "x".repeat(MAX_STREAM_EVENT_BYTES));
        assert_eq!(
            parse_stream(input.as_bytes(), |_| Ok(()))
                .unwrap_err()
                .to_string(),
            "Wiesel stream event is too large"
        );
    }

    #[test]
    fn streaming_delivers_delta_before_http_body_finishes() {
        use std::sync::mpsc;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let request = read_mock_request(&socket);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\n").unwrap();
            socket.flush().unwrap();
            // The rest of the body cannot arrive until the consumer saw the first delta.
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
            socket.write_all(b"data: [DONE]\n\n").unwrap();
            request
        });
        let client = mock_client();
        let mut request = completion_request(
            &client,
            "test-device-token",
            "test/model",
            &[Message::new("user", "Hi")],
            true,
        )
        .unwrap()
        .build()
        .unwrap();
        *request.url_mut() = format!("http://{address}/v1/chat/completions")
            .parse()
            .unwrap();
        let response = client.execute(request).unwrap();
        let result = parse_stream(BufReader::new(response), |_| {
            tx.send(()).unwrap();
            Ok(())
        });
        let wire = server.join().unwrap();
        assert!(wire.starts_with("POST /v1/chat/completions HTTP/1.1\r\n"));
        assert!(wire.contains("authorization: Bearer test-device-token\r\n"));
        assert!(wire.contains("accept: text/event-stream\r\n"));
        let body: Value = serde_json::from_str(wire.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["stream"], true);
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
