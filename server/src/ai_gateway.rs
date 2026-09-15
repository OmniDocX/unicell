//! Server-only OpenAI-compatible model gateway for U AI.
//!
//! The browser never receives the upstream base URL or credential. Runtime configuration is
//! resolved from server environment variables or ignored local files, and the client payload is
//! reduced to a validated message list before it is forwarded. Product policy is capability-first:
//! large workbook context and deep reasoning are allowed when they improve correctness.

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_AI_BASE: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const DEFAULT_AI_MODEL: &str = "qwen3.7-flash";
const MAX_MESSAGES: usize = 96;
const MAX_MESSAGE_CHARS: usize = 2_000_000;
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
// A successful chat response is normally far below this bound (the server also caps output
// tokens).  Keep a hard byte limit because a malicious/misconfigured upstream can otherwise make
// the blocking gateway buffer an unbounded response before JSON parsing.
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

const BASE_NAMES: &[&str] = &["UNICELL_AI_BASE"];
const MODEL_NAMES: &[&str] = &["UNICELL_AI_MODEL"];
const KEY_NAMES: &[&str] = &["UNICELL_AI_KEY"];
#[derive(Clone)]
pub struct AiRuntimeConfig {
    base: String,
    model: String,
    key: Option<String>,
    source: String,
    thinking: bool,
}

fn first_value(values: &BTreeMap<String, String>, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        values
            .get(*name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn parse_local_config(path: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (name, raw) = line.split_once('=')?;
            let name = name.trim();
            if !name.starts_with("UNI") {
                return None;
            }
            let value = raw
                .trim()
                .trim_matches(|character| character == '"' || character == '\'')
                .to_string();
            (!value.is_empty()).then(|| (name.to_string(), value))
        })
        .collect()
}

fn push_candidate(candidates: &mut Vec<(PathBuf, String)>, path: PathBuf, source: &str) {
    if !candidates.iter().any(|(existing, _)| existing == &path) {
        candidates.push((path, source.to_string()));
    }
}

fn local_candidates() -> Vec<(PathBuf, String)> {
    if let Some(path) = std::env::var_os("UNICELL_AI_CONFIG_PATH").map(PathBuf::from) {
        return vec![(path, "configured-file".into())];
    }

    let mut candidates = Vec::new();
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    push_candidate(&mut candidates, current.join(".env.local"), "unicell-local");
    if current.file_name().and_then(|name| name.to_str()) == Some("server") {
        if let Some(project) = current.parent() {
            push_candidate(&mut candidates, project.join(".env.local"), "unicell-local");
        }
    }

    candidates
}

fn environment_values() -> BTreeMap<String, String> {
    BASE_NAMES
        .iter()
        .chain(MODEL_NAMES)
        .chain(KEY_NAMES)
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|value| ((*name).to_string(), value))
        })
        .collect()
}

fn resolve_config(
    environment: &BTreeMap<String, String>,
    locals: &[(BTreeMap<String, String>, String)],
) -> AiRuntimeConfig {
    let local_value = |names: &[&str]| {
        locals
            .iter()
            .find_map(|(values, source)| first_value(values, names).map(|value| (value, source)))
    };
    let env_key = first_value(environment, KEY_NAMES);
    let (local_key, local_source) = local_value(KEY_NAMES)
        .map(|(value, source)| (Some(value), source.clone()))
        .unwrap_or_else(|| (None, "missing".into()));
    AiRuntimeConfig {
        base: first_value(environment, BASE_NAMES)
            .or_else(|| local_value(BASE_NAMES).map(|(value, _)| value))
            .unwrap_or_else(|| DEFAULT_AI_BASE.to_string()),
        model: first_value(environment, MODEL_NAMES)
            .or_else(|| local_value(MODEL_NAMES).map(|(value, _)| value))
            .unwrap_or_else(|| DEFAULT_AI_MODEL.to_string()),
        key: env_key.clone().or(local_key),
        source: if env_key.is_some() {
            "server-env".into()
        } else {
            local_source
        },
        // Product policy: all DashScope-compatible model calls keep deep thinking enabled.
        // This is intentionally not configurable so deployments cannot silently disable it.
        thinking: true,
    }
}

pub fn runtime_config() -> AiRuntimeConfig {
    let environment = environment_values();
    let locals = local_candidates()
        .into_iter()
        .map(|(path, source)| (parse_local_config(&path), source))
        .filter(|(values, _)| !values.is_empty())
        .collect::<Vec<_>>();
    resolve_config(&environment, &locals)
}

fn provider_name(base: &str) -> &'static str {
    if base.contains("dashscope.aliyuncs.com") {
        "DashScope OpenAI-compatible"
    } else if base.contains("api.openai.com") {
        "OpenAI"
    } else {
        "OpenAI-compatible"
    }
}

/// Safe browser-visible status. The upstream URL and key are intentionally absent.
pub fn public_status() -> Value {
    let config = runtime_config();
    json!({
        "configured": config.key.is_some(),
        "model": config.model,
        "provider": provider_name(&config.base),
        "source": config.source,
        "protocol": "openai-chat-completions",
        "streaming": false,
        "thinking": config.thinking,
    })
}

fn chat_endpoint(base: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base).map_err(|_| "Invalid AI base URL")?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(host)) => host.is_loopback(),
        Some(url::Host::Ipv6(host)) => host.is_loopback(),
        None => false,
    };
    if base.trim() != base
        || base.chars().any(char::is_control)
        || base.contains('\\')
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || base
            .split_once("://")
            .is_some_and(|(_, rest)| rest.split('/').next().unwrap_or("").contains('@'))
        || url.fragment().is_some()
        || url.query().is_some()
        || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
    {
        return Err("AI base URL requires HTTPS (HTTP only for loopback), without userinfo, query or fragment".into());
    }
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/chat/completions") {
        path.to_string()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&path);
    Ok(url.to_string())
}

fn validated_messages(body: &[u8]) -> Result<Vec<Value>, String> {
    let payload: Value = serde_json::from_slice(body).map_err(|_| "AI 请求必须是 JSON 对象")?;
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .ok_or("AI 请求缺少 messages")?;
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(format!("AI 消息数必须在 1..={MAX_MESSAGES} 之间"));
    }
    let mut total_chars = 0usize;
    let mut clean = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .filter(|role| matches!(*role, "system" | "user" | "assistant"))
            .ok_or_else(|| format!("第 {} 条 AI 消息 role 无效", index + 1))?;
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("第 {} 条 AI 消息 content 必须是字符串", index + 1))?;
        total_chars = total_chars.saturating_add(content.chars().count());
        if total_chars > MAX_MESSAGE_CHARS {
            return Err(format!("AI 上下文超过 {MAX_MESSAGE_CHARS} 字符上限"));
        }
        clean.push(json!({ "role": role, "content": content }));
    }
    Ok(clean)
}

fn upstream_error(status: reqwest::StatusCode) -> String {
    // Upstream bodies are untrusted and may echo credentials or private prompt content.  Keep
    // provider diagnostics in server logs/observability rather than reflecting them to clients.
    format!("AI 上游 HTTP {}", status.as_u16())
}

fn ensure_json_mode_instruction(messages: &mut Vec<Value>) {
    let mentions_json = messages.iter().any(|message| {
        message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|content| content.to_ascii_lowercase().contains("json"))
    });
    if !mentions_json {
        messages.insert(
            0,
            json!({
                "role": "system",
                "content": "Return one valid JSON object. Put the natural-language answer in a message field."
            }),
        );
    }
}

fn normalized_usage(usage: Option<&Value>, request_characters: usize) -> Value {
    let token = |primary: &str, fallback: &str| {
        usage
            .and_then(|value| value.get(primary).or_else(|| value.get(fallback)))
            .and_then(Value::as_u64)
    };
    let prompt_tokens = token("prompt_tokens", "input_tokens");
    let completion_tokens = token("completion_tokens", "output_tokens");
    let total_tokens = token("total_tokens", "total_tokens").or_else(|| {
        prompt_tokens
            .zip(completion_tokens)
            .map(|(input, output)| input + output)
    });
    json!({
        "providerReported": usage.is_some_and(Value::is_object),
        "promptTokens": prompt_tokens,
        "completionTokens": completion_tokens,
        "totalTokens": total_tokens,
        "requestCharacters": request_characters,
        "raw": usage.cloned().unwrap_or(Value::Null),
    })
}

/// Calls the configured OpenAI-compatible Chat Completions endpoint and returns a normalized JSON
/// response. The credential never enters the returned value.
pub fn chat(body: &[u8]) -> Result<Value, String> {
    let mut messages = validated_messages(body)?;
    // DashScope (and some other OpenAI-compatible providers) rejects
    // `response_format: json_object` unless the conversation explicitly asks
    // for JSON.  The UniCell system prompt already does, but keep the public
    // gateway self-contained for small direct API clients as well.
    ensure_json_mode_instruction(&mut messages);
    let request_characters = messages
        .iter()
        .filter_map(|message| message.get("content").and_then(Value::as_str))
        .map(|content| content.chars().count())
        .sum::<usize>();
    let config = runtime_config();
    let key = config.key.ok_or_else(|| {
        "AI_NOT_CONFIGURED: Set UNICELL_AI_KEY in the server environment".to_string()
    })?;
    let endpoint = chat_endpoint(&config.base)?;
    let mut payload = json!({
        "model": config.model,
        "messages": messages,
        "stream": false,
        "temperature": 0.15,
        "max_tokens": 16_384,
        "response_format": { "type": "json_object" },
    });
    // Product policy requires deep thinking on every DashScope-compatible model call. Keep the
    // provider-specific field out of arbitrary OpenAI-compatible servers.
    if config.base.contains("dashscope.aliyuncs.com") {
        payload["enable_thinking"] = Value::Bool(true);
    }
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| format!("无法创建 AI 客户端：{error}"))?;
    let response = client
        .post(endpoint)
        .bearer_auth(key)
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .map_err(|_| "AI 上游连接失败，请检查服务端网络与模型配置".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(upstream_error(status));
    }
    let response = response;
    let mut bytes = Vec::new();
    response
        .take((MAX_UPSTREAM_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取 AI 上游响应：{error}"))?;
    if bytes.len() > MAX_UPSTREAM_RESPONSE_BYTES {
        return Err("AI 上游响应超过安全上限".into());
    }
    let upstream: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("AI 上游返回了无效 JSON：{error}"))?;
    let choice = upstream
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .ok_or("AI 上游没有返回 choices")?;
    let content = choice
        .pointer("/message/content")
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
        .ok_or("AI 上游没有返回 message.content")?;
    let mut result = Map::new();
    result.insert("ok".into(), Value::Bool(true));
    result.insert("model".into(), Value::String(config.model));
    result.insert("content".into(), Value::String(content.to_string()));
    result.insert(
        "finishReason".into(),
        choice.get("finish_reason").cloned().unwrap_or(Value::Null),
    );
    result.insert(
        "usage".into(),
        normalized_usage(upstream.get("usage"), request_characters),
    );
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn unicell_environment_has_highest_priority() {
        let environment = values(&[
            ("UNICELL_AI_BASE", "https://cell.example/v1"),
            ("UNICELL_AI_MODEL", "cell-model"),
            ("UNICELL_AI_KEY", "cell-secret"),
        ]);
        let local = vec![(
            values(&[
                ("UNICELL_AI_BASE", "https://ppt.example/v1"),
                ("UNICELL_AI_MODEL", "local-model"),
                ("UNICELL_AI_KEY", "local-secret"),
            ]),
            "unicell-local".into(),
        )];
        let config = resolve_config(&environment, &local);
        assert_eq!(config.base, "https://cell.example/v1");
        assert_eq!(config.model, "cell-model");
        assert_eq!(config.key.as_deref(), Some("cell-secret"));
        assert_eq!(config.source, "server-env");
        assert!(config.thinking);
    }

    #[test]
    fn local_config_is_used_without_client_secret_exposure() {
        let local = vec![(
            values(&[
                ("UNICELL_AI_BASE", "https://ppt.example/v1"),
                ("UNICELL_AI_MODEL", "shared-model"),
                ("UNICELL_AI_KEY", "shared-secret"),
            ]),
            "unicell-local".into(),
        )];
        let config = resolve_config(&BTreeMap::new(), &local);
        assert_eq!(config.model, "shared-model");
        assert_eq!(config.source, "unicell-local");
        let visible = json!({
            "configured": config.key.is_some(),
            "model": config.model,
            "source": config.source,
        })
        .to_string();
        assert!(!visible.contains("shared-secret"));
    }

    #[test]
    fn chat_endpoint_requires_https_except_loopback() {
        assert_eq!(
            chat_endpoint("https://example.test/v1/").unwrap(),
            "https://example.test/v1/chat/completions"
        );
        assert!(chat_endpoint("http://127.0.0.1:11434/v1").is_ok());
        assert!(chat_endpoint("http://remote.example/v1").is_err());
    }

    #[test]
    fn validates_and_reduces_messages() {
        let input = br#"{
            "model":"client-must-not-control-this",
            "messages":[{"role":"system","content":"Return JSON"},{"role":"user","content":"hello"}],
            "extra":"not-forwarded"
        }"#;
        let messages = validated_messages(input).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"], "hello");
        assert!(validated_messages(br#"{"messages":[{"role":"tool","content":"x"}]}"#).is_err());
    }

    #[test]
    fn json_mode_instruction_is_added_only_when_needed() {
        let mut plain = vec![json!({"role":"user","content":"hello"})];
        ensure_json_mode_instruction(&mut plain);
        assert_eq!(plain.len(), 2);
        assert_eq!(plain[0]["role"], "system");
        assert!(plain[0]["content"].as_str().unwrap().contains("JSON"));

        let mut explicit = vec![json!({"role":"user","content":"return Json please"})];
        ensure_json_mode_instruction(&mut explicit);
        assert_eq!(explicit.len(), 1);
    }

    #[test]
    fn provider_usage_is_normalized_without_inventing_tokens() {
        let usage = json!({
            "prompt_tokens": 120,
            "completion_tokens": 30,
            "total_tokens": 150,
            "cached_tokens": 20
        });
        let normalized = normalized_usage(Some(&usage), 480);
        assert_eq!(normalized["providerReported"], true);
        assert_eq!(normalized["promptTokens"], 120);
        assert_eq!(normalized["completionTokens"], 30);
        assert_eq!(normalized["totalTokens"], 150);
        assert_eq!(normalized["requestCharacters"], 480);
        assert_eq!(normalized["raw"]["cached_tokens"], 20);

        let missing = normalized_usage(None, 41);
        assert_eq!(missing["providerReported"], false);
        assert!(missing["totalTokens"].is_null());
        assert_eq!(missing["requestCharacters"], 41);
    }

    #[test]
    fn thinking_is_globally_enabled_and_cannot_be_disabled() {
        let default_config = resolve_config(&BTreeMap::new(), &[]);
        assert!(default_config.thinking);
        let attempted_override = resolve_config(&values(&[("UNICELL_AI_THINKING", "false")]), &[]);
        assert!(attempted_override.thinking);
    }
}
