/// realtime_ws — Realtime API WebSocket client (trait abstraction)
///
/// Unifies protocol differences via the `RealtimeProtocol` trait.
/// Adding a new provider only requires implementing the trait and registering in `protocol_for`.
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{self, HeaderValue, Uri},
    },
    MaybeTlsStream, WebSocketStream,
};

// ============================================================
// Type definitions
// ============================================================

/// Provider identifier, used for DB storage and factory dispatch
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderType {
    OpenAI,
    Gemini,
}

impl ProviderType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "gemini" => Self::Gemini,
            _ => Self::OpenAI,
        }
    }
}

/// Unified events parsed from WebSocket messages
#[derive(Debug)]
pub enum WsEvent {
    /// Model audio chunk (base64)
    AudioDelta(String),
    /// Model audio output complete
    AudioDone,
    /// Model text transcript delta
    Transcript(String),
    /// Model text transcript complete (at end of a response segment)
    TranscriptDone(String),
    /// User speech recognition result
    InputTranscript(String),
    /// Tool call
    #[allow(dead_code)]
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    /// Model started responding (used to interrupt old audio)
    ResponseStart,
    /// Response round complete
    ResponseDone,
    /// Error
    Error(String),
    /// Other events (silently ignored)
    Other(#[allow(dead_code)] String),
}

struct ProviderDefaults {
    base_url: &'static str,
    default_model: &'static str,
    voice: &'static str,
    audio_format: &'static str,
    transcription_model: &'static str,
}

const OPENAI_DEFAULTS: ProviderDefaults = ProviderDefaults {
    base_url: "wss://api.openai.com/v1/realtime",
    default_model: "gpt-4o-realtime-preview-2024-12-17",
    voice: "alloy",
    audio_format: "pcm16",
    transcription_model: "whisper-1",
};

const GEMINI_DEFAULTS: ProviderDefaults = ProviderDefaults {
    base_url: "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
    default_model: "gemini-2.5-flash-native-audio-preview-12-2025",
    voice: "Aoede",
    audio_format: "pcm16",
    transcription_model: "",
};

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ============================================================
// RealtimeProtocol Trait — Unified protocol for all Realtime Providers
// ============================================================

pub trait RealtimeProtocol: Send + Sync {
    /// Whether function calling is supported
    fn supports_function_calling(&self) -> bool {
        true
    }

    /// Build the complete WebSocket connection URL
    fn build_connect_url(&self, base_url: &str, api_key: &str, model: &str) -> String;

    /// Log-friendly URL (hides sensitive information)
    fn build_log_url(&self, base_url: &str, api_key: &str, model: &str) -> String;

    /// Add authentication to WS request (e.g. headers). Default is no-op.
    fn apply_request_auth(
        &self,
        _request: &mut http::Request<()>,
        _api_key: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Build session initialization message (session.update / setup)
    /// `tools` contains the full set of tool schemas to register on the session.
    fn build_session_update(
        &self,
        instructions: &str,
        tools: &[Value],
        model: &str,
        voice: &str,
    ) -> Value;

    /// Build audio append message
    fn audio_append_msg(&self, base64_audio: &str) -> String;

    /// Parse server WS message into unified event list
    fn parse_events(&self, raw: &str) -> Vec<WsEvent>;

    /// Build inject-text-to-speech message
    fn inject_speech_msg(&self, text: &str) -> String;

    /// Build tool call result message
    fn function_call_output_msg(&self, call_id: &str, name: &str, output: &str) -> String;

    /// Build trigger-response message (providers that auto-respond return None)
    fn response_create_msg(&self) -> Option<String> {
        None
    }

    /// Build input audio buffer clear message (discard buffered echo on server)
    fn input_audio_clear_msg(&self) -> Option<String> {
        None
    }

    /// Whether to wait for setup acknowledgment (e.g. Gemini's setupComplete) before sending data
    fn requires_setup_ack(&self) -> bool {
        false
    }

    /// Build a conversation history injection message (for context after reconnection).
    /// Returns None if the provider doesn't support injecting conversation items.
    fn conversation_inject_msg(&self, role: &str, text: &str) -> Option<String> {
        let _ = (role, text);
        None
    }

    /// Check if a voice name is valid for this provider.
    fn is_valid_voice(&self, _voice: &str) -> bool {
        true // Default: accept all
    }

    /// Provide a prompt describing the supported voices for the agent's system instructions.
    fn supported_voices_prompt(&self) -> String {
        String::new()
    }
}

// ============================================================
// Factory
// ============================================================

/// Create the corresponding protocol implementation based on provider type string
pub fn protocol_for(provider_type: &str) -> Box<dyn RealtimeProtocol> {
    match ProviderType::from_str(provider_type) {
        ProviderType::OpenAI => Box::new(OpenAiProtocol),
        ProviderType::Gemini => Box::new(GeminiProtocol),
    }
}

// ============================================================
// Connection (using trait abstraction)
// ============================================================

/// Establish WebSocket connection, auto-detecting system proxy
pub async fn connect(
    protocol: &dyn RealtimeProtocol,
    api_key: &str,
    base_url: &str,
    model: &str,
) -> Result<WsStream, String> {
    let url = protocol.build_connect_url(base_url, api_key, model);
    let log_url = protocol.build_log_url(base_url, api_key, model);
    log::info!("[RealtimeWS] Connecting: {}", log_url);

    let uri: Uri = url.parse().map_err(|e| format!("Invalid URL: {e}"))?;
    let mut request = uri
        .into_client_request()
        .map_err(|e| format!("Request build error: {e}"))?;
    protocol.apply_request_auth(&mut request, api_key)?;

    if let Some(proxy_url) = detect_proxy(&url) {
        let timeout_duration = std::time::Duration::from_secs(10);
        match tokio::time::timeout(
            timeout_duration,
            connect_via_proxy(request, &url, &proxy_url),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err("WebSocket connection via proxy timed out after 10 seconds".to_string()),
        }
    } else {
        let timeout_duration = std::time::Duration::from_secs(10);
        match tokio::time::timeout(timeout_duration, connect_async(request)).await {
            Ok(Ok((ws_stream, _))) => {
                log::info!("[RealtimeWS] Connected successfully (direct)");
                Ok(ws_stream)
            }
            Ok(Err(e)) => Err(format!("WebSocket connect failed: {e}")),
            Err(_) => Err("WebSocket direct connection timed out after 10 seconds".to_string()),
        }
    }
}

// ============================================================
// System proxy detection and tunneling
// ============================================================

/// Detect system proxy: check env vars first, then Windows registry
fn detect_proxy(target_url: &str) -> Option<String> {
    let is_tls = target_url.starts_with("wss://") || target_url.starts_with("https://");

    // 1. Environment variables take priority
    let env_proxy = if is_tls {
        std::env::var("HTTPS_PROXY")
            .or_else(|_| std::env::var("https_proxy"))
            .or_else(|_| std::env::var("ALL_PROXY"))
            .or_else(|_| std::env::var("all_proxy"))
            .ok()
    } else {
        std::env::var("HTTP_PROXY")
            .or_else(|_| std::env::var("http_proxy"))
            .or_else(|_| std::env::var("ALL_PROXY"))
            .or_else(|_| std::env::var("all_proxy"))
            .ok()
    };

    let proxy = env_proxy
        .filter(|s| !s.is_empty())
        // 2. Fallback: Windows registry system proxy
        .or_else(read_windows_system_proxy)
        .filter(|s| !s.is_empty())?;

    log::info!("[RealtimeWS] Detected proxy: {}", proxy);

    // NO_PROXY / ProxyOverride exclusion check
    let no_proxy = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .ok()
        .or_else(read_windows_proxy_override);

    if let Some(no_proxy) = no_proxy {
        if let Some((host, _)) = parse_host_port(target_url) {
            for entry in no_proxy.split(';').flat_map(|s| s.split(',')) {
                let entry = entry.trim();
                if entry == "*" || entry == "<local>" {
                    // "<local>" only excludes hostnames without dots
                    if entry == "*" || !host.contains('.') {
                        return None;
                    }
                } else if host == entry
                    || host.ends_with(entry)
                    || host == entry.trim_start_matches('.')
                {
                    return None;
                }
            }
        }
    }

    Some(proxy)
}

/// Read Windows registry system proxy (Internet Settings)
#[cfg(target_os = "windows")]
fn read_windows_system_proxy() -> Option<String> {
    use std::process::Command;
    // Use reg query to read, avoiding extra crate dependency
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyEnable",
        ])
        .output()
        .ok()?;
    let enable_str = String::from_utf8_lossy(&output.stdout);
    // ProxyEnable REG_DWORD 0x1 means proxy is enabled
    let enabled = enable_str.contains("0x1");
    if !enabled {
        return None;
    }

    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyServer",
        ])
        .output()
        .ok()?;
    let server_str = String::from_utf8_lossy(&output.stdout);
    // Format: "    ProxyServer    REG_SZ    127.0.0.1:7890"
    let proxy_value = server_str
        .lines()
        .find(|l| l.contains("ProxyServer"))?
        .split_whitespace()
        .last()?
        .to_string();

    if proxy_value.is_empty() {
        return None;
    }

    // Registry value may be "http=host:port;https=host:port" or simple "host:port"
    if proxy_value.contains('=') {
        // Protocol-specific format, extract https or http
        for segment in proxy_value.split(';') {
            let segment = segment.trim();
            if let Some(stripped) = segment.strip_prefix("https=") {
                return Some(format!("http://{}", stripped));
            }
        }
        for segment in proxy_value.split(';') {
            let segment = segment.trim();
            if let Some(stripped) = segment.strip_prefix("http=") {
                return Some(format!("http://{}", stripped));
            }
        }
        None
    } else {
        // Simple host:port, add http:// prefix
        if proxy_value.starts_with("http://") || proxy_value.starts_with("https://") {
            Some(proxy_value)
        } else {
            Some(format!("http://{}", proxy_value))
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn read_windows_system_proxy() -> Option<String> {
    None
}

/// Read Windows registry ProxyOverride (similar to NO_PROXY)
#[cfg(target_os = "windows")]
fn read_windows_proxy_override() -> Option<String> {
    use std::process::Command;
    let output = Command::new("reg")
        .args([
            "query",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            "/v",
            "ProxyOverride",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .find(|l| l.contains("ProxyOverride"))
        .and_then(|l| l.split_whitespace().last())
        .map(|s| s.to_string())
}

#[cfg(not(target_os = "windows"))]
fn read_windows_proxy_override() -> Option<String> {
    None
}

/// Extract host and port from URL
fn parse_host_port(url: &str) -> Option<(String, u16)> {
    let uri: Uri = url.parse().ok()?;
    let host = uri.host()?.to_string();
    let default_port = match uri.scheme_str() {
        Some("wss") | Some("https") => 443,
        _ => 80,
    };
    Some((host, uri.port_u16().unwrap_or(default_port)))
}

/// Establish WebSocket connection via HTTP CONNECT proxy tunnel
async fn connect_via_proxy(
    request: http::Request<()>,
    target_url: &str,
    proxy_url: &str,
) -> Result<WsStream, String> {
    let (target_host, target_port) =
        parse_host_port(target_url).ok_or("Failed to parse target host:port")?;

    // Proxy URL format: http://host:port or host:port
    let proxy_addr = proxy_url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let (proxy_host, proxy_port) = if let Some(colon) = proxy_addr.rfind(':') {
        let host = &proxy_addr[..colon];
        let port: u16 = proxy_addr[colon + 1..].parse().unwrap_or(1080);
        (host.to_string(), port)
    } else {
        (proxy_addr.to_string(), 1080)
    };

    log::info!(
        "[RealtimeWS] Using proxy {}:{} → {}:{}",
        proxy_host,
        proxy_port,
        target_host,
        target_port
    );

    // 1. TCP connect to proxy server
    let mut stream = TcpStream::connect(format!("{}:{}", proxy_host, proxy_port))
        .await
        .map_err(|e| format!("Proxy TCP connect failed: {e}"))?;

    // 2. HTTP CONNECT to establish tunnel
    let connect_req = format!(
        "CONNECT {}:{} HTTP/1.1\r\nHost: {}:{}\r\n\r\n",
        target_host, target_port, target_host, target_port
    );
    stream
        .write_all(connect_req.as_bytes())
        .await
        .map_err(|e| format!("Proxy CONNECT write failed: {e}"))?;

    // 3. Read proxy response
    let mut buf = vec![0u8; 4096];
    let n = stream
        .read(&mut buf)
        .await
        .map_err(|e| format!("Proxy CONNECT read failed: {e}"))?;
    let response = String::from_utf8_lossy(&buf[..n]);

    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        return Err(format!(
            "Proxy CONNECT rejected: {}",
            response.lines().next().unwrap_or("")
        ));
    }

    log::info!("[RealtimeWS] Proxy tunnel established");

    // 4. Perform TLS + WebSocket handshake over the tunnel
    let (ws_stream, _) = client_async_tls_with_config(request, stream, None, None)
        .await
        .map_err(|e| format!("WebSocket handshake via proxy failed: {e}"))?;

    log::info!("[RealtimeWS] Connected successfully (via proxy)");
    Ok(ws_stream)
}

// ============================================================
// OpenAI Realtime implementation
// ============================================================

pub struct OpenAiProtocol;

impl RealtimeProtocol for OpenAiProtocol {
    fn build_connect_url(&self, base_url: &str, _api_key: &str, model: &str) -> String {
        let url = if base_url.is_empty() {
            OPENAI_DEFAULTS.base_url
        } else {
            base_url
        };
        let m = if model.is_empty() {
            OPENAI_DEFAULTS.default_model
        } else {
            model
        };
        format!("{}?model={}", url, urlencoding::encode(m))
    }

    fn build_log_url(&self, base_url: &str, _api_key: &str, model: &str) -> String {
        // OpenAI doesn't expose the key in the URL, reuse directly
        self.build_connect_url(base_url, "", model)
    }

    fn apply_request_auth(
        &self,
        request: &mut http::Request<()>,
        api_key: &str,
    ) -> Result<(), String> {
        let protocols = format!(
            "realtime, openai-insecure-api-key.{}, openai-beta.realtime-v1",
            api_key
        );
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            HeaderValue::from_str(&protocols).map_err(|e| format!("Header error: {e}"))?,
        );
        Ok(())
    }

    fn build_session_update(
        &self,
        instructions: &str,
        tools: &[Value],
        _model: &str,
        voice: &str,
    ) -> Value {
        build_openai_session(instructions, tools, voice)
    }

    fn audio_append_msg(&self, base64_audio: &str) -> String {
        json!({
            "type": "input_audio_buffer.append",
            "audio": base64_audio,
        })
        .to_string()
    }

    fn parse_events(&self, raw: &str) -> Vec<WsEvent> {
        let msg: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return vec![WsEvent::Other("unparseable".into())],
        };
        vec![parse_openai_event(&msg)]
    }

    fn inject_speech_msg(&self, text: &str) -> String {
        json!({
            "type": "response.create",
            "response": {
                "modalities": ["text", "audio"],
                "instructions": format!(
                    "Paraphrase the following content to the user in concise, natural spoken language. Speak in {}. No lists or markdown — just say the key points:\n\n{}",
                    crate::i18n::language_name(), text
                ),
            },
        })
        .to_string()
    }

    fn function_call_output_msg(&self, call_id: &str, _name: &str, output: &str) -> String {
        json!({
            "type": "conversation.item.create",
            "item": {
                "type": "function_call_output",
                "call_id": call_id,
                "output": output,
            }
        })
        .to_string()
    }

    fn response_create_msg(&self) -> Option<String> {
        Some(json!({ "type": "response.create" }).to_string())
    }

    fn input_audio_clear_msg(&self) -> Option<String> {
        Some(json!({ "type": "input_audio_buffer.clear" }).to_string())
    }

    fn conversation_inject_msg(&self, role: &str, text: &str) -> Option<String> {
        // OpenAI Realtime API supports conversation.item.create for injecting context
        let (api_role, content_type) = match role {
            "user" => ("user", "input_text"),
            "assistant" => ("assistant", "text"),
            _ => return None, // tool results not injected as context
        };
        Some(
            json!({
                "type": "conversation.item.create",
                "item": {
                    "type": "message",
                    "role": api_role,
                    "content": [{
                        "type": content_type,
                        "text": text,
                    }]
                }
            })
            .to_string(),
        )
    }

    fn is_valid_voice(&self, voice: &str) -> bool {
        const OPENAI_VOICES: &[&str] = &[
            "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse", "marin", "cedar",
        ];
        OPENAI_VOICES.contains(&voice)
    }

    fn supported_voices_prompt(&self) -> String {
        "Available voices for OpenAI: alloy, ash, ballad, coral, echo, sage, shimmer, verse, marin, cedar.".to_string()
    }
}

// ============================================================
// Gemini Multimodal Live implementation
// ============================================================

pub struct GeminiProtocol;

impl RealtimeProtocol for GeminiProtocol {
    fn build_connect_url(&self, base_url: &str, api_key: &str, _model: &str) -> String {
        let url = if base_url.is_empty() {
            GEMINI_DEFAULTS.base_url
        } else {
            base_url
        };
        format!("{}?key={}", url, api_key)
    }

    fn build_log_url(&self, base_url: &str, _api_key: &str, _model: &str) -> String {
        let url = if base_url.is_empty() {
            GEMINI_DEFAULTS.base_url
        } else {
            base_url
        };
        format!("{}?key=***", url)
    }

    fn is_valid_voice(&self, voice: &str) -> bool {
        ["Aoede", "Puck", "Charon", "Kore", "Fenrir", ""].contains(&voice)
    }

    fn supported_voices_prompt(&self) -> String {
        "Available voices for Gemini: Aoede (female), Puck (male), Charon (male), Kore (female), Fenrir (male).".to_string()
    }

    fn build_session_update(
        &self,
        instructions: &str,
        tools: &[Value],
        model: &str,
        voice: &str,
    ) -> Value {
        build_gemini_setup(instructions, tools, model, voice)
    }

    fn audio_append_msg(&self, base64_audio: &str) -> String {
        // Gemini input audio requires 16kHz, capture is at 24kHz, auto-downsample here
        let resampled = match STANDARD.decode(base64_audio) {
            Ok(pcm_bytes) => {
                let downsampled = resample_24k_to_16k(&pcm_bytes);
                STANDARD.encode(&downsampled)
            }
            Err(_) => base64_audio.to_string(),
        };
        json!({
            "realtimeInput": {
                "mediaChunks": [{
                    "mimeType": "audio/pcm;rate=16000",
                    "data": resampled,
                }]
            }
        })
        .to_string()
    }

    fn parse_events(&self, raw: &str) -> Vec<WsEvent> {
        let msg: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => return vec![WsEvent::Other("unparseable".into())],
        };
        parse_gemini_events(&msg)
    }

    fn inject_speech_msg(&self, text: &str) -> String {
        json!({
            "clientContent": {
                "turns": [{
                    "role": "user",
                    "parts": [{ "text": format!(
                        "[System] Paraphrase the following content to the user in concise, natural spoken language:\n\n{}", text
                    ) }]
                }],
                "turnComplete": true
            }
        })
        .to_string()
    }

    fn function_call_output_msg(&self, call_id: &str, name: &str, output: &str) -> String {
        json!({
            "toolResponse": {
                "functionResponses": [{
                    "id": call_id,
                    "name": name,
                    "response": { "result": output }
                }]
            }
        })
        .to_string()
    }

    // response_create_msg: Gemini auto-generates a reply after sending toolResponse, no manual trigger needed

    fn requires_setup_ack(&self) -> bool {
        true
    }

    // Gemini Multimodal Live doesn't support injecting conversation history items,
    // so conversation_inject_msg returns None (default).
}

// ============================================================
// Internal helper — OpenAI session construction
// ============================================================

fn build_openai_session(instructions: &str, tools: &[Value], voice: &str) -> Value {
    const OPENAI_VOICES: &[&str] = &[
        "alloy", "ash", "ballad", "coral", "echo", "sage", "shimmer", "verse", "marin", "cedar",
    ];
    let defaults = &OPENAI_DEFAULTS;
    let effective_voice = if !voice.is_empty() && OPENAI_VOICES.contains(&voice) {
        voice
    } else {
        if !voice.is_empty() {
            log::warn!(
                "[WS] Voice '{}' not supported by OpenAI, falling back to default",
                voice
            );
        }
        defaults.voice
    };

    let mut session = json!({
        "modalities": ["text", "audio"],
        "instructions": instructions,
        "voice": effective_voice,
        "input_audio_format": defaults.audio_format,
        "output_audio_format": defaults.audio_format,
        "input_audio_transcription": {
            "model": defaults.transcription_model,
            "language": whisper_language_code(),
        },
        "turn_detection": {
            "type": "server_vad",
            "threshold": 0.6,
            "silence_duration_ms": 500,
            "prefix_padding_ms": 300,
        },
    });

    if !tools.is_empty() {
        session["tools"] = json!(tools);
        session["tool_choice"] = json!("auto");
    }

    json!({
        "type": "session.update",
        "session": session,
    })
}

// ============================================================
// Internal helper — Gemini session construction
// ============================================================

fn build_gemini_setup(instructions: &str, tools: &[Value], model: &str, voice: &str) -> Value {
    let defaults = &GEMINI_DEFAULTS;
    let effective_model = if model.is_empty() {
        defaults.default_model
    } else {
        model
    };
    let model_path = if effective_model.starts_with("models/") {
        effective_model.to_string()
    } else {
        format!("models/{}", effective_model)
    };
    let effective_voice = if voice.is_empty() {
        defaults.voice
    } else {
        voice
    };

    let mut setup = json!({
        "model": model_path,
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": effective_voice,
                    }
                }
            }
        },
        "systemInstruction": {
            "parts": [{ "text": instructions }]
        },
        "outputAudioTranscription": {},
        "inputAudioTranscription": {},
    });

    if !tools.is_empty() {
        // Convert OpenAI-format tools to Gemini functionDeclarations format
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut decl = json!({
                    "name": tool["name"],
                    "description": tool["description"],
                });
                // Convert OpenAI parameter types to Gemini upper-case types
                if let Some(params) = tool.get("parameters") {
                    decl["parameters"] = convert_params_to_gemini(params);
                }
                decl
            })
            .collect();
        setup["tools"] = json!([{ "functionDeclarations": declarations }]);
    }

    json!({ "setup": setup })
}

/// Convert OpenAI-format JSON Schema parameters to Gemini's upper-case type format
fn convert_params_to_gemini(params: &Value) -> Value {
    let mut result = params.clone();
    // Convert "type" field to upper-case (e.g. "object" → "OBJECT", "string" → "STRING")
    if let Some(t) = result
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_uppercase())
    {
        result["type"] = json!(t);
    }
    // Recursively convert properties
    if let Some(props) = result.get("properties").cloned() {
        if let Some(obj) = props.as_object() {
            let mut new_props = serde_json::Map::new();
            for (key, val) in obj {
                new_props.insert(key.clone(), convert_params_to_gemini(val));
            }
            result["properties"] = Value::Object(new_props);
        }
    }
    result
}

// ============================================================
// Internal helper — Message parsing
// ============================================================

fn parse_openai_event(msg: &Value) -> WsEvent {
    let event_type = msg["type"].as_str().unwrap_or("");

    match event_type {
        "response.audio.delta" => {
            WsEvent::AudioDelta(msg["delta"].as_str().unwrap_or("").to_string())
        }
        "response.audio.done" => WsEvent::AudioDone,
        "response.audio_transcript.delta" => {
            WsEvent::Transcript(msg["delta"].as_str().unwrap_or("").to_string())
        }
        "response.audio_transcript.done" => {
            WsEvent::TranscriptDone(msg["transcript"].as_str().unwrap_or("").to_string())
        }
        "conversation.item.input_audio_transcription.completed" => {
            WsEvent::InputTranscript(msg["transcript"].as_str().unwrap_or("").to_string())
        }
        "response.function_call_arguments.done" => WsEvent::FunctionCall {
            call_id: msg["call_id"].as_str().unwrap_or("").to_string(),
            name: msg["name"].as_str().unwrap_or("").to_string(),
            arguments: msg["arguments"].as_str().unwrap_or("{}").to_string(),
        },
        "response.created" => WsEvent::ResponseStart,
        "response.done" => {
            let status = msg["response"]["status"].as_str().unwrap_or("completed");
            if status == "failed" {
                let detail = msg["response"]["status_details"]
                    .as_object()
                    .map(|o| format!("{}", Value::Object(o.clone())))
                    .unwrap_or_default();
                return WsEvent::Error(format!("Response failed: {}", detail));
            }
            if status == "cancelled" {
                log::debug!("[RealtimeWS] Response cancelled (user interrupt)");
            }
            WsEvent::ResponseDone
        }
        "error" => WsEvent::Error(
            msg["error"]["message"]
                .as_str()
                .unwrap_or("unknown error")
                .to_string(),
        ),
        other => WsEvent::Other(other.to_string()),
    }
}

fn parse_gemini_events(msg: &Value) -> Vec<WsEvent> {
    let mut events = vec![];

    // Connection established
    if msg.get("setupComplete").is_some() {
        return vec![WsEvent::Other("setupComplete".into())];
    }

    // Error response
    if let Some(err) = msg.get("error") {
        let code = err["code"].as_i64().unwrap_or(0);
        let message = err["message"].as_str().unwrap_or("unknown error");
        return vec![WsEvent::Error(format!(
            "Gemini error {}: {}",
            code, message
        ))];
    }

    // Tool call
    if let Some(tc) = msg.get("toolCall") {
        if let Some(calls) = tc["functionCalls"].as_array() {
            for call in calls {
                events.push(WsEvent::FunctionCall {
                    call_id: call["id"].as_str().unwrap_or("").to_string(),
                    name: call["name"].as_str().unwrap_or("").to_string(),
                    arguments: serde_json::to_string(call.get("args").unwrap_or(&json!({})))
                        .unwrap_or_else(|_| "{}".into()),
                });
            }
        }
        events.push(WsEvent::ResponseDone); // Implicit generation completion for tool calls
        return events;
    }

    // Server content (audio/text/completion markers/transcription)
    if let Some(sc) = msg.get("serverContent") {
        let mut handled = false;

        // Output audio transcription (assistant voice → text)
        if let Some(ot) = sc.get("outputTranscription") {
            if let Some(text) = ot["text"].as_str() {
                if !text.is_empty() {
                    events.push(WsEvent::Transcript(text.to_string()));
                }
            }
            handled = true;
        }

        // Input audio transcription (user voice → text)
        if let Some(it) = sc.get("inputTranscription") {
            if let Some(text) = it["text"].as_str() {
                if !text.is_empty() {
                    events.push(WsEvent::InputTranscript(text.to_string()));
                }
            }
            handled = true;
        }

        // Interruption handling: user speech interrupted model output
        if sc
            .get("interrupted")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            log::info!("[RealtimeWS] Gemini interrupted by user");
            return vec![WsEvent::ResponseStart, WsEvent::AudioDone];
        }

        let turn_complete = sc["turnComplete"].as_bool().unwrap_or(false);
        let generation_complete = sc["generationComplete"].as_bool().unwrap_or(false);

        if let Some(model_turn) = sc.get("modelTurn") {
            handled = true;
            if let Some(parts) = model_turn["parts"].as_array() {
                for part in parts {
                    if let Some(inline_data) = part.get("inlineData") {
                        let data = inline_data["data"].as_str().unwrap_or("").to_string();
                        if !data.is_empty() {
                            events.push(WsEvent::AudioDelta(data));
                        }
                    }
                    if let Some(text) = part["text"].as_str() {
                        if !text.is_empty() {
                            // Gemini native audio model's text part is chain-of-thought, not speech transcription
                            log::debug!(
                                "[RealtimeWS] Gemini thinking: {}",
                                &text[..text.len().min(100)]
                            );
                        }
                    }
                }
            }
        }

        if turn_complete || generation_complete {
            events.push(WsEvent::ResponseDone);
            handled = true;
        }

        // serverContent recognized but no emittable events (e.g. pure chain-of-thought text) — return silently
        if handled && events.is_empty() {
            return events; // Return empty vec, don't trigger "unknown"
        }
    }

    if events.is_empty() {
        events.push(WsEvent::Other("unknown".into()));
    }
    events
}

// ============================================================
// Internal helper — Audio downsampling
// ============================================================
// Internal helper — Whisper language code
// ============================================================

/// Map app locale to Whisper ISO-639-1 language code for input_audio_transcription.
/// Specifying the language avoids Whisper auto-detection errors (e.g. Chinese misidentified as Korean).
fn whisper_language_code() -> &'static str {
    match crate::i18n::get_locale().as_str() {
        "zh" => "zh",
        "en" => "en",
        _ => "en",
    }
}

// ============================================================
// Internal helper — Audio resampling
// ============================================================

/// PCM16 downsampling: 24kHz → 16kHz (3:2 ratio linear interpolation)
fn resample_24k_to_16k(data: &[u8]) -> Vec<u8> {
    let num_samples = data.len() / 2;
    if num_samples < 3 {
        return data.to_vec();
    }
    let output_samples = num_samples * 2 / 3;
    let mut output = Vec::with_capacity(output_samples * 2);

    for i in 0..output_samples {
        let pos = i as f64 * 1.5; // 24k/16k = 1.5
        let idx = pos as usize;
        let frac = pos - idx as f64;

        let s0 = if idx * 2 + 1 < data.len() {
            i16::from_le_bytes([data[idx * 2], data[idx * 2 + 1]]) as f64
        } else {
            0.0
        };
        let s1 = if (idx + 1) * 2 + 1 < data.len() {
            i16::from_le_bytes([data[(idx + 1) * 2], data[(idx + 1) * 2 + 1]]) as f64
        } else {
            s0
        };

        let sample = (s0 * (1.0 - frac) + s1 * frac) as i16;
        output.extend_from_slice(&sample.to_le_bytes());
    }

    output
}
