//! Desktop browser authentication against the fixed Wiesel backend.
//! Keep one Attempt in UI memory, drop/cancel it when replacing or signing out, and pass
//! raw (not URL-parser-normalized) deep-link text to consume_callback. Blocking HTTP
//! belongs on a worker thread. Nothing in this module logs secrets or remote errors.

use std::{
    io::Read,
    time::{Duration, Instant},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{
    StatusCode,
    blocking::{Client, ClientBuilder, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

pub use crate::settings::DeviceCredential;
use crate::settings::{CredentialError, is_base64url_byte, utc_now_ms};

/// The only production origin for browser login and device API requests.
pub const BASE_URL: &str = "https://wiesel.run";
/// Monotonic lifetime of a browser login attempt.
pub const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: u64 = 16 * 1024;

/// Sanitized, non-secret auth failures. Raw HTTP/URL/JSON errors are never retained.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("Cannot obtain secure randomness")]
    Randomness,
    #[error("No active desktop login attempt")]
    Inactive,
    #[error("Desktop login attempt has expired")]
    Expired,
    #[error("Invalid desktop login callback")]
    Callback,
    #[error("Desktop login callback does not match the active attempt")]
    StateMismatch,
    #[error("Cannot initialize secure HTTP client")]
    Client,
    #[error("Desktop request failed; start a new login rather than retrying an exchange")]
    Network,
    #[error("Desktop service rejected the request (HTTP {0})")]
    HttpStatus(u16),
    #[error("Invalid desktop service response")]
    Response,
    #[error(transparent)]
    Credential(#[from] CredentialError),
}

struct AttemptSecrets {
    state: Zeroizing<String>,
    verifier: Zeroizing<String>,
}

/// An in-memory, single-use PKCE attempt. No Debug/Serialize/Clone implementations.
pub struct Attempt {
    created_at: Instant,
    secrets: Option<AttemptSecrets>,
}

impl Attempt {
    /// Generate independent state and verifier using 32 OS-CSPRNG bytes each.
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self {
            created_at: Instant::now(),
            secrets: Some(AttemptSecrets {
                state: random_secret()?,
                verifier: random_secret()?,
            }),
        })
    }

    /// HTTPS browser URL containing only challenge, state and S256 method (no verifier).
    /// Also enforces timeout; do not log this URL because it contains the state.
    pub fn login_url(&mut self) -> Result<String, AuthError> {
        self.ensure_active_at(Instant::now())?;
        let secrets = self.secrets.as_ref().ok_or(AuthError::Inactive)?;
        Ok(format!(
            "{BASE_URL}/desktop/login?code_challenge={}&state={}&code_challenge_method=S256",
            challenge(&secrets.verifier),
            secrets.state.as_str()
        ))
    }

    /// Check the monotonic deadline, clearing secrets if elapsed. Poll while login is open.
    pub fn expired(&mut self) -> bool {
        self.expired_at(Instant::now())
    }

    fn expired_at(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.created_at) >= ATTEMPT_TIMEOUT {
            self.cancel();
            return true;
        }
        false
    }

    fn ensure_active_at(&mut self, now: Instant) -> Result<(), AuthError> {
        if self.expired_at(now) {
            return Err(AuthError::Expired);
        }
        if self.secrets.is_none() {
            return Err(AuthError::Inactive);
        }
        Ok(())
    }

    /// Cancel and zeroize state/verifier. Later callbacks cannot reactivate this attempt.
    pub fn cancel(&mut self) {
        self.secrets = None;
    }

    /// Invalidate this attempt before generating its replacement, even if randomness fails.
    pub fn replace(&mut self) -> Result<(), AuthError> {
        self.cancel();
        *self = Self::new()?;
        Ok(())
    }

    /// Validate raw callback grammar and state, then consume before any network exchange.
    /// Malformed/mismatching callbacks leave an unexpired attempt usable; a matching one
    /// transfers its secrets exactly once. Reject unsolicited callbacks by keeping the
    /// sole Attempt in an Option and using consume_callback below when it is absent.
    pub fn consume_callback(&mut self, raw: &str) -> Result<ExchangeRequest, AuthError> {
        self.consume_at(raw, Instant::now())
    }

    fn consume_at(&mut self, raw: &str, now: Instant) -> Result<ExchangeRequest, AuthError> {
        self.ensure_active_at(now)?;
        let (code, state) = parse_callback(raw)?;
        let secrets = self.secrets.as_ref().ok_or(AuthError::Inactive)?;
        if !bool::from(secrets.state.as_bytes().ct_eq(state.as_bytes())) {
            return Err(AuthError::StateMismatch);
        }
        let secrets = self.secrets.take().ok_or(AuthError::Inactive)?;
        Ok(ExchangeRequest {
            code,
            state: secrets.state,
            code_verifier: secrets.verifier,
        })
    }
}

/// Validate against the sole active attempt; None rejects unsolicited callbacks.
/// On a matching callback the Option is cleared, preventing replay even before exchange.
pub fn consume_callback(
    active: &mut Option<Attempt>,
    raw: &str,
) -> Result<ExchangeRequest, AuthError> {
    let attempt = active.as_mut().ok_or(AuthError::Inactive)?;
    let result = attempt.consume_callback(raw);
    if attempt.secrets.is_none() {
        *active = None;
    }
    result
}

fn random_secret() -> Result<Zeroizing<String>, AuthError> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| AuthError::Randomness)?;
    Ok(Zeroizing::new(URL_SAFE_NO_PAD.encode(bytes.as_ref())))
}

fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

// Compare the raw authority/path rather than using Url: parsers normalize case, dot
// segments, whitespace and escapes. Such transformations must not expand this grammar.
fn parse_callback(raw: &str) -> Result<(Zeroizing<String>, Zeroizing<String>), AuthError> {
    if raw.len() > 1024 {
        return Err(AuthError::Callback);
    }
    let query = raw
        .strip_prefix("wiesel://auth/callback?")
        .ok_or(AuthError::Callback)?;
    let mut code = None;
    let mut state = None;
    for field in query.split('&') {
        let (name, value) = field.split_once('=').ok_or(AuthError::Callback)?;
        let name = decode_query_once(name)?;
        let value = decode_query_once(value)?;
        match name.as_str() {
            "code" if code.is_none() && valid_base64url(&value, 43, 43) => code = Some(value),
            "state" if state.is_none() && valid_base64url(&value, 43, 128) => state = Some(value),
            _ => return Err(AuthError::Callback),
        }
    }
    Ok((
        code.ok_or(AuthError::Callback)?,
        state.ok_or(AuthError::Callback)?,
    ))
}

fn valid_base64url(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.len()) && value.bytes().all(is_base64url_byte)
}

fn decode_query_once(raw: &str) -> Result<Zeroizing<String>, AuthError> {
    let mut value = Zeroizing::new(String::with_capacity(raw.len()));
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        let decoded = if byte == b'%' {
            let hi = bytes
                .next()
                .and_then(hex_digit)
                .ok_or(AuthError::Callback)?;
            let lo = bytes
                .next()
                .and_then(hex_digit)
                .ok_or(AuthError::Callback)?;
            hi * 16 + lo
        } else {
            byte
        };
        // No form-urlencoded '+' substitution, controls, Unicode, separators or second
        // decoding. Only names and base64url values occur in this callback contract.
        if !is_base64url_byte(decoded) {
            return Err(AuthError::Callback);
        }
        value.push(char::from(decoded));
    }
    Ok(value)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Single-use exchange material. Intentionally not Debug, Clone or Serialize.
/// It can only be constructed by a successful callback and is consumed by exchange.
pub struct ExchangeRequest {
    code: Zeroizing<String>,
    code_verifier: Zeroizing<String>,
    state: Zeroizing<String>,
}

#[derive(Serialize)]
struct ExchangeBody<'a> {
    code: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExchangeResponse {
    access_token: Zeroizing<String>,
    token_type: String,
    expires_at: i64,
    device_id: uuid::Uuid,
}

/// Backend plan projection, not a local authorization decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    Free,
    Starter,
    Unlimited,
}

/// Device status projection returned by the service (never used to authorize locally).
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct DeviceStatus {
    pub plan: Plan,
    #[serde(rename = "virtualCredits")]
    pub virtual_credits: i64,
}

/// Blocking, redirect-disabled, timeout-bound HTTPS client. No refresh or cookie jar.
/// Production endpoints are fixed; only private unit-test construction permits loopback.
pub struct DesktopClient {
    client: Client,
    #[cfg(test)]
    mock_origin: Option<String>,
}

fn client_builder() -> ClientBuilder {
    Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .no_proxy()
}

impl DesktopClient {
    /// Build a TLS-only client with redirects and automatic retries disabled.
    pub fn new() -> Result<Self, AuthError> {
        Ok(Self {
            client: client_builder().build().map_err(|_| AuthError::Client)?,
            #[cfg(test)]
            mock_origin: None,
        })
    }

    fn endpoint(&self, path: &str) -> String {
        #[cfg(test)]
        if let Some(origin) = &self.mock_origin {
            return format!("{origin}{path}");
        }
        format!("{BASE_URL}{path}")
    }

    /// POST the one-use exchange, without cookies/Origin/Authorization/query parameters.
    /// Consumes the request on all outcomes. Network ambiguity requires a fresh login,
    /// never an exchange retry. Underlying HTTP buffers are not guaranteed zeroized.
    pub fn exchange(&self, request: ExchangeRequest) -> Result<DeviceCredential, AuthError> {
        let response = self
            .client
            .post(self.endpoint("/api/desktop/exchange"))
            .json(&ExchangeBody {
                code: &request.code,
                code_verifier: &request.code_verifier,
                state: &request.state,
            })
            .send()
            .map_err(|_| AuthError::Network)?;
        require_status(&response, StatusCode::OK)?;
        let body = response_body(response)?;
        let raw: ExchangeResponse =
            serde_json::from_slice(&body).map_err(|_| AuthError::Response)?;
        if raw.token_type != "Bearer" {
            return Err(AuthError::Response);
        }
        Ok(DeviceCredential::validated(
            raw.access_token,
            raw.expires_at,
            raw.device_id,
            utc_now_ms()?,
        )?)
    }

    /// GET status with a valid device bearer. Plan/credits are informational only.
    pub fn device_status(&self, credential: &DeviceCredential) -> Result<DeviceStatus, AuthError> {
        credential.ensure_valid()?;
        let response = self
            .client
            .get(self.endpoint("/api/device/status"))
            .bearer_auth(credential.access_token())
            .send()
            .map_err(|_| AuthError::Network)?;
        require_status(&response, StatusCode::OK)?;
        serde_json::from_slice(&response_body(response)?).map_err(|_| AuthError::Response)
    }

    /// POST sign-out with bearer and {}. Separately delete local Keychain credentials
    /// even on remote failure. Any 2xx response is accepted; redirects are never followed.
    pub fn sign_out(&self, credential: &DeviceCredential) -> Result<(), AuthError> {
        credential.ensure_valid()?;
        let response = self
            .client
            .post(self.endpoint("/api/device/sign-out"))
            .bearer_auth(credential.access_token())
            .json(&serde_json::json!({}))
            .send()
            .map_err(|_| AuthError::Network)?;
        if !response.status().is_success() {
            return Err(AuthError::HttpStatus(response.status().as_u16()));
        }
        Ok(())
    }
}

fn require_status(response: &Response, required: StatusCode) -> Result<(), AuthError> {
    if response.status() != required {
        return Err(AuthError::HttpStatus(response.status().as_u16()));
    }
    Ok(())
}

fn response_body(response: Response) -> Result<Zeroizing<Vec<u8>>, AuthError> {
    let mut body = Zeroizing::new(Vec::new());
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|_| AuthError::Network)?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(AuthError::Response);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, sync::mpsc, thread};

    fn fixture_attempt() -> Attempt {
        Attempt {
            created_at: Instant::now(),
            secrets: Some(AttemptSecrets {
                state: Zeroizing::new("s".repeat(43)),
                verifier: Zeroizing::new("v".repeat(43)),
            }),
        }
    }

    fn callback(state: &str) -> String {
        format!(
            "wiesel://auth/callback?code={}&state={state}",
            "c".repeat(43)
        )
    }

    fn request() -> ExchangeRequest {
        fixture_attempt()
            .consume_callback(&callback(&"s".repeat(43)))
            .unwrap()
    }

    fn credential() -> DeviceCredential {
        DeviceCredential::validated(
            Zeroizing::new(format!("wd_{}", "t".repeat(43))),
            utc_now_ms().unwrap() + 60_000,
            uuid::Uuid::parse_str("01234567-89ab-4def-8123-456789abcdef").unwrap(),
            utc_now_ms().unwrap(),
        )
        .unwrap()
    }

    fn exchange_json() -> serde_json::Value {
        serde_json::json!({
            "access_token": format!("wd_{}", "t".repeat(43)),
            "token_type": "Bearer",
            "expires_at": utc_now_ms().unwrap() + 60_000,
            "device_id": "01234567-89ab-4def-8123-456789abcdef",
        })
    }

    struct Mock {
        client: DesktopClient,
        captured: mpsc::Receiver<Zeroizing<String>>,
        task: thread::JoinHandle<()>,
    }

    // A single local HTTP request/response. No production requests or Keychain access.
    fn mock(status: u16, headers: &str, body: &str) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status} Mock\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
            body.len()
        );
        let (tx, captured) = mpsc::channel();
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut input = Zeroizing::new(Vec::new());
            let mut byte = [0_u8; 1];
            while !input.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                input.push(byte[0]);
            }
            let head = std::str::from_utf8(&input).unwrap();
            let length = head
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            let offset = input.len();
            input.resize(offset + length, 0);
            stream.read_exact(&mut input[offset..]).unwrap();
            tx.send(Zeroizing::new(String::from_utf8(input.to_vec()).unwrap()))
                .unwrap();
            stream.write_all(response.as_bytes()).unwrap();
        });
        Mock {
            client: DesktopClient {
                client: client_builder().https_only(false).build().unwrap(),
                mock_origin: Some(origin),
            },
            captured,
            task,
        }
    }

    fn finish(mock: Mock) -> Zeroizing<String> {
        let input = mock.captured.recv_timeout(Duration::from_secs(5)).unwrap();
        mock.task.join().unwrap();
        input
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn attempt_new_uses_independent_32_byte_urlsafe_secrets() {
        let attempt = Attempt::new().unwrap();
        let secrets = attempt.secrets.as_ref().unwrap();
        assert!(valid_base64url(&secrets.state, 43, 43));
        assert!(valid_base64url(&secrets.verifier, 43, 43));
        assert!(secrets.state.as_str() != secrets.verifier.as_str());
        assert_eq!(URL_SAFE_NO_PAD.decode(&*secrets.state).unwrap().len(), 32);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&*secrets.verifier).unwrap().len(),
            32
        );
    }

    #[test]
    fn login_url_has_fixed_origin_only_required_fields_and_no_verifier() {
        let mut attempt = fixture_attempt();
        let url = attempt.login_url().unwrap();
        assert!(
            url == format!(
                "https://wiesel.run/desktop/login?code_challenge={}&state={}&code_challenge_method=S256",
                challenge(&"v".repeat(43)),
                "s".repeat(43)
            )
        );
        assert!(!url.contains(&"v".repeat(43)));
    }

    #[test]
    fn callback_parser_rejects_normalization_authority_and_path_bypasses() {
        let query = format!("?code={}&state={}", "c".repeat(43), "s".repeat(43));
        for prefix in [
            "WIESEL://auth/callback",
            "wiesel://AUTH/callback",
            "wiesel://user@auth/callback",
            "wiesel://auth:1/callback",
            "wiesel://auth:/callback",
            "wiesel:///auth/callback",
            "wiesel://auth/callback/",
            "wiesel://auth/./callback",
            "wiesel://auth/a/../callback",
            "wiesel://auth/%63allback",
            "wiesel://auth\\callback",
            " wiesel://auth/callback",
            "\nwiesel://auth/callback",
            "wiesel://auth/call\tback",
            "wiesel://auth/callback#fragment",
        ] {
            assert!(matches!(
                parse_callback(&format!("{prefix}{query}")),
                Err(AuthError::Callback)
            ));
        }
    }

    #[test]
    fn callback_parser_rejects_duplicate_unknown_missing_and_invalid_fields() {
        let valid = callback(&"s".repeat(43));
        for raw in [
            format!("{valid}&code={}", "c".repeat(43)),
            format!("{valid}&%73tate={}", "s".repeat(43)),
            format!("{valid}&extra=x"),
            format!("{valid}&"),
            format!("{valid}#"),
            format!("{valid}#fragment"),
            format!("{valid}&state"),
            "wiesel://auth/callback".into(),
            "wiesel://auth/callback?".into(),
            format!("wiesel://auth/callback?code={}", "c".repeat(43)),
            callback(&"s".repeat(42)),
            callback(&"s".repeat(129)),
            format!(
                "wiesel://auth/callback?code={}&state={}",
                "c".repeat(44),
                "s".repeat(43)
            ),
        ] {
            assert!(matches!(parse_callback(&raw), Err(AuthError::Callback)));
        }
    }

    #[test]
    fn callback_parser_decodes_valid_percent_encoding_exactly_once() {
        let raw = format!(
            "wiesel://auth/callback?%63ode=%63{}&state=%73{}",
            "c".repeat(42),
            "s".repeat(42)
        );
        let (code, state) = parse_callback(&raw).unwrap();
        assert!(code.as_str() == "c".repeat(43));
        assert!(state.as_str() == "s".repeat(43));
    }

    #[test]
    fn callback_parser_rejects_bad_encoding_plus_unicode_and_second_decode() {
        for suffix in [
            "%", "%2", "%GG", "%252D", "+", "=", "/", "%00", "%0A", "%26", "%3D", "%FF", "%C3%A9",
            "é", " ",
        ] {
            let raw = callback(&format!("{}{suffix}", "s".repeat(42)));
            assert!(matches!(parse_callback(&raw), Err(AuthError::Callback)));
        }
    }

    #[test]
    fn callback_parser_accepts_state_length_boundaries_and_field_order() {
        for length in [43, 128] {
            let raw = format!(
                "wiesel://auth/callback?state={}&code={}",
                "s".repeat(length),
                "c".repeat(43)
            );
            assert!(parse_callback(&raw).is_ok());
        }
    }

    #[test]
    fn attempt_mismatch_and_malformed_callbacks_do_not_consume_matching_attempt() {
        let mut attempt = fixture_attempt();
        assert!(matches!(
            attempt.consume_callback(&callback(&"x".repeat(43))),
            Err(AuthError::StateMismatch)
        ));
        assert!(matches!(
            attempt.consume_callback("invalid"),
            Err(AuthError::Callback)
        ));
        assert!(attempt.consume_callback(&callback(&"s".repeat(43))).is_ok());
    }

    #[test]
    fn attempt_matching_callback_consumes_secrets_before_exchange_and_rejects_replay() {
        let mut attempt = fixture_attempt();
        let raw = callback(&"s".repeat(43));
        let exchange = attempt.consume_callback(&raw).unwrap();
        assert!(attempt.secrets.is_none());
        assert!(matches!(
            attempt.consume_callback(&raw),
            Err(AuthError::Inactive)
        ));
        assert!(exchange.code_verifier.as_str() == "v".repeat(43));
    }

    #[test]
    fn callback_without_active_attempt_is_rejected_and_success_clears_option() {
        let mut active = None;
        let raw = callback(&"s".repeat(43));
        assert!(matches!(
            consume_callback(&mut active, &raw),
            Err(AuthError::Inactive)
        ));
        active = Some(fixture_attempt());
        assert!(consume_callback(&mut active, &raw).is_ok());
        assert!(active.is_none());
    }

    #[test]
    fn attempt_timeout_is_monotonic_exclusive_and_clears_secrets() {
        let mut attempt = fixture_attempt();
        let before = attempt.created_at + ATTEMPT_TIMEOUT - Duration::from_nanos(1);
        assert!(!attempt.expired_at(before));
        let deadline = attempt.created_at + ATTEMPT_TIMEOUT;
        assert!(matches!(
            attempt.consume_at(&callback(&"s".repeat(43)), deadline),
            Err(AuthError::Expired)
        ));
        assert!(attempt.secrets.is_none());
    }

    #[test]
    fn attempt_expired_login_url_clears_secrets() {
        let mut attempt = fixture_attempt();
        attempt.created_at -= ATTEMPT_TIMEOUT;
        assert!(matches!(attempt.login_url(), Err(AuthError::Expired)));
        assert!(attempt.expired());
        assert!(attempt.secrets.is_none());
    }

    #[test]
    fn attempt_cancel_invalidates_callback_and_login_url() {
        let mut attempt = fixture_attempt();
        attempt.cancel();
        assert!(matches!(
            attempt.consume_callback(&callback(&"s".repeat(43))),
            Err(AuthError::Inactive)
        ));
        assert!(matches!(attempt.login_url(), Err(AuthError::Inactive)));
        assert!(attempt.secrets.is_none());
    }

    #[test]
    fn attempt_replacement_invalidates_previous_state() {
        let mut attempt = fixture_attempt();
        attempt.replace().unwrap();
        assert!(matches!(
            attempt.consume_callback(&callback(&"s".repeat(43))),
            Err(AuthError::StateMismatch)
        ));
    }

    #[test]
    fn exchange_posts_exact_json_without_forbidden_headers_or_query() {
        let mock = mock(
            200,
            "Content-Type: application/json\r\n",
            &exchange_json().to_string(),
        );
        let credential = mock.client.exchange(request()).unwrap();
        assert_eq!(
            credential.device_id().to_string(),
            "01234567-89ab-4def-8123-456789abcdef"
        );
        let input = finish(mock);
        let (head, body) = input.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /api/desktop/exchange HTTP/1.1\r\n"));
        let head = head.to_ascii_lowercase();
        for forbidden in ["authorization:", "origin:", "cookie:"] {
            assert!(!head.contains(forbidden));
        }
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(
            json == serde_json::json!({"code": "c".repeat(43), "code_verifier": "v".repeat(43), "state": "s".repeat(43)})
        );
    }

    #[test]
    fn exchange_validates_error_status_before_body_and_sanitizes_errors() {
        let mock = mock(401, "", "remote secret-bearing error");
        let result = mock.client.exchange(request());
        assert!(matches!(result, Err(AuthError::HttpStatus(401))));
        assert_eq!(
            result.err().unwrap().to_string(),
            "Desktop service rejected the request (HTTP 401)"
        );
        finish(mock);
    }

    #[test]
    fn exchange_rejects_redirect_without_following_it() {
        let mock = mock(
            302,
            "Location: /redirected?secret=fixture\r\n",
            &exchange_json().to_string(),
        );
        assert!(matches!(
            mock.client.exchange(request()),
            Err(AuthError::HttpStatus(302))
        ));
        finish(mock);
    }

    #[test]
    fn exchange_rejects_invalid_credential_schema_and_lifetime() {
        let now = utc_now_ms().unwrap();
        let changes = [
            ("access_token", serde_json::json!("provider-key")),
            (
                "access_token",
                serde_json::json!(format!("wd_{}=", "t".repeat(42))),
            ),
            (
                "access_token",
                serde_json::json!(format!("wd_{}", "t".repeat(44))),
            ),
            ("token_type", serde_json::json!("bearer")),
            ("expires_at", serde_json::json!(now - 1)),
            (
                "expires_at",
                serde_json::json!(now + crate::settings::MAX_CREDENTIAL_LIFETIME_MS + 60_000),
            ),
            ("expires_at", serde_json::json!("tomorrow")),
            ("expires_at", serde_json::json!(now as f64 + 60_000.5)),
            ("device_id", serde_json::json!("not-a-uuid")),
            ("extra", serde_json::json!(true)),
        ];
        for (field, value) in changes {
            let mut body = exchange_json();
            body[field] = value;
            let mock = mock(200, "", &body.to_string());
            assert!(mock.client.exchange(request()).is_err());
            finish(mock);
        }
    }

    #[test]
    fn exchange_rejects_missing_duplicate_and_malformed_json_fields() {
        let mut missing = exchange_json();
        missing.as_object_mut().unwrap().remove("state");
        missing.as_object_mut().unwrap().remove("access_token");
        let valid = exchange_json().to_string();
        let duplicate = format!("{{\"token_type\":\"Bearer\",{}", &valid[1..]);
        for body in [
            "{".to_owned(),
            "null".to_owned(),
            missing.to_string(),
            duplicate,
        ] {
            let mock = mock(200, "", &body);
            assert!(matches!(
                mock.client.exchange(request()),
                Err(AuthError::Response)
            ));
            finish(mock);
        }
    }

    #[test]
    fn exchange_rejects_oversized_success_response() {
        let mock = mock(200, "", &" ".repeat(MAX_RESPONSE_BYTES as usize + 1));
        assert!(matches!(
            mock.client.exchange(request()),
            Err(AuthError::Response)
        ));
        finish(mock);
    }

    #[test]
    fn exchange_network_ambiguity_is_not_retried() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0_u8; 4096];
            assert!(stream.read(&mut buf).unwrap() > 0);
            drop(stream);
            listener.set_nonblocking(true).unwrap();
            thread::sleep(Duration::from_millis(300));
            assert!(
                matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
        });
        let client = DesktopClient {
            client: client_builder().https_only(false).build().unwrap(),
            mock_origin: Some(origin),
        };
        assert!(matches!(
            client.exchange(request()),
            Err(AuthError::Network)
        ));
        task.join().unwrap();
    }

    #[test]
    fn device_status_uses_bearer_and_parses_plan_integer_projection() {
        for plan in ["free", "starter", "unlimited"] {
            let mock = mock(
                200,
                "",
                &format!("{{\"plan\":\"{plan}\",\"virtualCredits\":42}}"),
            );
            assert_eq!(
                mock.client
                    .device_status(&credential())
                    .unwrap()
                    .virtual_credits,
                42
            );
            let input = finish(mock);
            assert!(input.starts_with("GET /api/device/status HTTP/1.1\r\n"));
            assert!(
                input
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer wd_{}", "t".repeat(43)))
            );
        }
    }

    #[test]
    fn device_status_rejects_invalid_plan_and_noninteger_credits() {
        for body in [
            "{\"plan\":\"paid\",\"virtualCredits\":1}",
            "{\"plan\":\"free\",\"virtualCredits\":1.5}",
            "{\"plan\":\"free\",\"virtualCredits\":\"42\"}",
        ] {
            let mock = mock(200, "", body);
            assert!(matches!(
                mock.client.device_status(&credential()),
                Err(AuthError::Response)
            ));
            finish(mock);
        }
    }

    #[test]
    fn device_endpoints_reject_status_errors_and_redirects() {
        let mock_status = mock(307, "Location: /redirected\r\n", "{}");
        assert!(matches!(
            mock_status.client.device_status(&credential()),
            Err(AuthError::HttpStatus(307))
        ));
        finish(mock_status);
        let mock_signout = mock(401, "", "remote secret-bearing error");
        assert!(matches!(
            mock_signout.client.sign_out(&credential()),
            Err(AuthError::HttpStatus(401))
        ));
        finish(mock_signout);
    }

    #[test]
    fn sign_out_posts_bearer_and_empty_object() {
        let mock = mock(204, "", "");
        mock.client.sign_out(&credential()).unwrap();
        let input = finish(mock);
        assert!(input.starts_with("POST /api/device/sign-out HTTP/1.1\r\n"));
        assert!(
            input
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer wd_{}", "t".repeat(43)))
        );
        assert!(input.ends_with("\r\n\r\n{}"));
    }

    #[test]
    fn sign_out_rejects_redirect_without_following_it() {
        let mock = mock(308, "Location: /redirected\r\n", "");
        assert!(matches!(
            mock.client.sign_out(&credential()),
            Err(AuthError::HttpStatus(308))
        ));
        finish(mock);
    }

    #[test]
    fn device_endpoints_reject_expired_in_memory_credentials_before_network() {
        let mut credential = credential();
        // Constructed valid, then simulate wall-clock passage without sleeping.
        credential = DeviceCredential::validated(
            Zeroizing::new(credential.access_token().to_owned()),
            utc_now_ms().unwrap() - 1,
            credential.device_id(),
            utc_now_ms().unwrap() - 60_000,
        )
        .unwrap();
        let client = DesktopClient::new().unwrap();
        assert!(matches!(
            client.device_status(&credential),
            Err(AuthError::Credential(CredentialError::Expired))
        ));
        assert!(matches!(
            client.sign_out(&credential),
            Err(AuthError::Credential(CredentialError::Expired))
        ));
    }

    #[test]
    fn exchange_waiting_for_response_obeys_client_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buf = [0_u8; 4096];
            assert!(stream.read(&mut buf).unwrap() > 0);
            thread::sleep(Duration::from_millis(200));
        });
        let client = DesktopClient {
            client: client_builder()
                .https_only(false)
                .timeout(Duration::from_millis(50))
                .build()
                .unwrap(),
            mock_origin: Some(origin),
        };
        assert!(matches!(
            client.exchange(request()),
            Err(AuthError::Network)
        ));
        task.join().unwrap();
    }

    #[test]
    fn production_client_has_fixed_https_endpoints() {
        let client = DesktopClient::new().unwrap();
        assert_eq!(
            client.endpoint("/api/device/status"),
            "https://wiesel.run/api/device/status"
        );
        assert!(client.client.get("http://127.0.0.1:1").build().is_ok());
        // Building is permitted by reqwest; execution rejects the scheme before connecting.
        assert!(
            client
                .client
                .get("http://127.0.0.1:1")
                .send()
                .unwrap_err()
                .is_builder()
        );
    }
}
