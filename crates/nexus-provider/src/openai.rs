//! GLM-5.3-Flash（及一切 vLLM/OpenAI 兼容后端）客户端。
//!
//! 协议要点（按本机 GLM53 实测约定）：
//! - POST {base_url}/chat/completions，`stream: true` 走 SSE；
//! - `usage.prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` 是 GLM/vLLM 扩展字段，透传给 cache 统计；
//! - 思考档位走顶层 `reasoning_effort`（vLLM chat template kwargs 由网关或引擎映射）；
//! - 工具用标准 `tools: [{type:"function", function:{name,description,parameters}}]`。

use anyhow::{Context, Result};
use futures::StreamExt;
use nexus_core::config::ProviderConfig;
use nexus_core::{Content, ContentPart, Message, ToolSpec};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct StreamDelta {
    pub text: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<ToolCallAcc>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub struct ToolCallAcc {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments_delta: String,
}

#[derive(Debug, Clone)]
pub struct CompletionResult {
    pub text: String,
    pub reasoning: String,
    pub tool_calls: Vec<nexus_core::ToolCall>,
    pub usage: Usage,
}

pub struct OpenAiClient {
    http: reqwest::Client,
    cfg: ProviderConfig,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
}

fn message_to_json(m: &Message) -> serde_json::Value {
    let mut obj = serde_json::json!({ "role": m.role });
    match &m.content {
        Some(Content::Text(t)) => {
            obj["content"] = serde_json::Value::String(t.clone());
        }
        Some(Content::Parts(parts)) => {
            obj["content"] = serde_json::Value::Array(
                parts.iter().map(|p| match p {
                    ContentPart::Text { text } => serde_json::json!({ "type": "text", "text": text }),
                    ContentPart::ImageUrl { image_url } => {
                        serde_json::json!({ "type": "image_url", "image_url": { "url": image_url.url } })
                    }
                    ContentPart::InputAudio { input_audio } => {
                        serde_json::json!({ "type": "input_audio", "input_audio": { "data": input_audio.data, "format": input_audio.format } })
                    }
                })
                .collect(),
            );
        }
        None => {}
    }
    if let Some(tcs) = &m.tool_calls {
        obj["tool_calls"] = serde_json::Value::Array(
            tcs.iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id, "type": "function",
                        "function": { "name": tc.name, "arguments": tc.arguments }
                    })
                })
                .collect(),
        );
    }
    if let Some(id) = &m.tool_call_id {
        obj["tool_call_id"] = serde_json::Value::String(id.clone());
    }
    if let Some(n) = &m.name {
        obj["name"] = serde_json::Value::String(n.clone());
    }
    obj
}

fn tool_spec_to_json(t: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": { "name": t.name, "description": t.description, "parameters": t.parameters }
    })
}

impl OpenAiClient {
    pub fn new(cfg: ProviderConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(1300)) // 学 zcode：长 prefill 不撞 600s
            .build()?;
        Ok(Self { http, cfg })
    }

    /// 流式补全。回调收到每个 delta；返回聚合结果。
    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&[ToolSpec]>,
        mut on_delta: impl FnMut(&StreamDelta),
    ) -> Result<CompletionResult> {
        let mut req = ChatRequest {
            model: self.cfg.model.clone(),
            messages: messages.iter().map(message_to_json).collect(),
            tools: tools.map(|ts| ts.iter().map(tool_spec_to_json).collect()),
            tool_choice: tools.map(|_| serde_json::json!("auto")),
            stream: true,
            stream_options: Some(serde_json::json!({ "include_usage": true })),
            reasoning_effort: Some(self.cfg.reasoning_effort.clone()),
            max_tokens: Some(32768),
        };
        if self.cfg.reasoning_effort == "off" {
            req.reasoning_effort = None;
        }

        let url = format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.cfg.api_key)
            .json(&req)
            .send()
            .await
            .with_context(|| format!("POST {}", url))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            // 429/5xx 交给上层 watchdog 退避重试
            anyhow::bail!("provider error {}: {}", status, truncate(&body, 2000));
        }

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut finish_reason: Option<String> = None;
        let mut usage: Option<Usage> = None;
        let mut acc: std::collections::BTreeMap<usize, nexus_core::ToolCall> = std::collections::BTreeMap::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.with_context(|| "stream chunk")?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..pos + 1).collect();
                let line = line.trim_end();
                let payload = if let Some(rest) = line.strip_prefix("data: ") {
                    rest
                } else {
                    continue;
                };
                if payload.trim() == "[DONE]" {
                    continue;
                }
                let v: serde_json::Value = match serde_json::from_str(payload) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                // 心跳/注释行已滤；处理 choices
                if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
                    for ch in choices {
                        let delta = ch.get("delta");
                        if let Some(d) = delta {
                            if let Some(c) = d.get("content").and_then(|c| c.as_str()) {
                                if !c.is_empty() {
                                    text.push_str(c);
                                }
                            }
                            if let Some(r) = d.get("reasoning_content").and_then(|c| c.as_str()) {
                                if !r.is_empty() {
                                    reasoning.push_str(r);
                                }
                            }
                            if let Some(tcs) = d.get("tool_calls").and_then(|t| t.as_array()) {
                                for tc in tcs {
                                    let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                                    let entry = acc.entry(idx).or_insert_with(|| nexus_core::ToolCall {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
                                        if !id.is_empty() {
                                            entry.id = id.to_string();
                                        }
                                    }
                                    if let Some(f) = tc.get("function") {
                                        if let Some(n) = f.get("name").and_then(|n| n.as_str()) {
                                            if !n.is_empty() {
                                                entry.name.push_str(n);
                                            }
                                        }
                                        if let Some(a) = f.get("arguments").and_then(|a| a.as_str()) {
                                            entry.arguments.push_str(a);
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(fr) = ch.get("finish_reason").and_then(|f| f.as_str()) {
                            finish_reason = Some(fr.to_string());
                        }
                    }
                }
                if let Some(u) = v.get("usage") {
                    usage = Some(Usage {
                        prompt_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                        completion_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                        cached_tokens: u
                            .get("prompt_cache_hit_tokens")
                            .or_else(|| u.get("prompt_tokens_details").and_then(|d| d.get("cached_tokens")))
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                    });
                }
                let sd = StreamDelta {
                    text: None,
                    reasoning: None,
                    tool_calls: vec![],
                    finish_reason: finish_reason.clone(),
                    usage,
                };
                on_delta(&sd);
            }
        }

        Ok(CompletionResult {
            text,
            reasoning,
            tool_calls: acc.into_values().collect(),
            usage: usage.unwrap_or_default(),
        })
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
