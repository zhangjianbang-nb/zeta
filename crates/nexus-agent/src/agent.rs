//! Agent 主循环：单任务执行器（供 CLI 与 TUI 共用）。

use anyhow::Result;
use nexus_core::cache::{compact_session, CachePolicy, CacheStats};
use nexus_core::config::Config;
use nexus_core::{ContentPart, Message, ToolCall};
use nexus_guard::watchdog::{Watchdog, WatchdogVerdict};
use nexus_memory::MemoryStore;
use nexus_provider::openai::OpenAiClient;
use nexus_tools::ToolRegistry;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub enum AgentEvent {
    Delta(String),
    ReasoningDelta(String),
    ToolStart { name: String, args: String },
    ToolEnd { name: String, output_preview: String },
    Acceptance { passed: bool, detail: String },
    Status { metrics: crate::metrics::Metrics },
    Done { reason: String },
    Error { message: String, retry_after_secs: u64 },
}

pub struct Agent {
    cfg: Config,
    client: OpenAiClient,
    registry: ToolRegistry,
    memory: Arc<Mutex<MemoryStore>>,
    journal: nexus_core::journal::Journal,
    session: nexus_core::session::Session,
    cache: CacheStats,
    pub event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
}

fn tool_result_msg(id: &str, text: String) -> Message {
    Message::tool_result(id, text)
}

impl Agent {
    pub async fn new(cfg: Config, event_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>) -> Result<Self> {
        let client = OpenAiClient::new(cfg.provider.clone())?;
        let memory = Arc::new(Mutex::new(MemoryStore::open(&cfg)?));
        let journal = nexus_core::journal::Journal::open(&cfg.data_dir.join("sessions"), "current")?;
        let mut session = if cfg.auto_resume && journal.path().exists() {
            journal.load().unwrap_or_else(|_| nexus_core::session::Session::new())
        } else {
            nexus_core::session::Session::new()
        };
        if session.messages.is_empty() {
            session.messages.push(Message::system(&cfg.provider.system_prompt));
        }
        let registry = ToolRegistry::new(
            cfg.tool_timeout_secs,
            cfg.max_identical_calls,
            cfg.max_images_in_context,
            cfg.data_dir.clone(),
        );
        Ok(Self { cfg, client, registry, memory, journal, session, cache: CacheStats::default(), event_tx })
    }

    pub fn push_user(&mut self, msg: Message) -> Result<()> {
        self.session.messages.push(msg);
        let last = self.session.messages.last().unwrap().clone();
        self.journal.append(&last)?;
        Ok(())
    }

    fn is_memory_tool(name: &str) -> bool {
        name == "memory_write" || name == "memory_recall"
    }

    async fn run_memory_tool(&self, call: &ToolCall) -> nexus_tools::ToolResult {
        let v: serde_json::Value = serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
        let out = if call.name == "memory_write" {
            let tag = v.get("tag").and_then(|t| t.as_str()).unwrap_or("note");
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let mut mem = self.memory.lock().await;
            match mem.write(tag, text) {
                Ok(_) => "memory saved".to_string(),
                Err(e) => format!("memory error: {}", e),
            }
        } else {
            let query = v.get("query").and_then(|t| t.as_str()).unwrap_or("");
            let mem = self.memory.lock().await;
            mem.recall_prompt(query, 8)
        };
        nexus_tools::ToolResult { call_id: call.id.clone(), name: call.name.clone(), output: out, vision_parts: vec![] }
    }

    /// 执行一个工具调用（含 memory/验收路由）。
    async fn dispatch(&self, call: &ToolCall) -> nexus_tools::ToolResult {
        if Self::is_memory_tool(&call.name) {
            return self.run_memory_tool(call).await;
        }
        self.registry.execute(call, &self.session.messages).await
    }

    /// 长任务主循环。
    pub async fn run_task(&mut self, user_text: &str) -> Result<String> {
        let recall = {
            let mem = self.memory.lock().await;
            mem.recall_prompt(user_text, 5)
        };
        if !recall.is_empty() {
            self.session.messages.push(Message::system(recall));
        }
        self.push_user(Message::user(user_text))?;

        let mut watchdog = Watchdog::new(self.cfg.max_consecutive_failures as u32);
        let specs = self.all_specs();
        let policy = CachePolicy::default();
        let mut final_text = String::new();
        let mut last_tok_per_sec: f64 = 0.0;

        for step in 0..self.cfg.max_steps {
            let mut metrics = crate::metrics::Metrics {
                step,
                cache_hit_rate: self.cache.hit_rate(),
                prompt_tokens: self.cache.prompt_tokens,
                ..Default::default()
            };
            let (gpu_used, gpu_total, gpu_temp) = crate::metrics::Metrics::read_gpu();
            metrics.gpu_used_mb = gpu_used;
            metrics.gpu_total_mb = gpu_total;
            metrics.gpu_temp = gpu_temp;
            metrics.memories = { self.memory.lock().await.len() };
            metrics.tok_per_sec = last_tok_per_sec;
            let _ = self.event_tx.send(AgentEvent::Status { metrics });

            compact_session(&mut self.session, &policy);

            let call_start = std::time::Instant::now();
            let result = self.call_with_backoff(&specs, &mut watchdog).await?;
            let elapsed = call_start.elapsed().as_secs_f64();
            self.cache.record(result.usage.prompt_tokens, result.usage.cached_tokens);
            if elapsed > 0.0 && result.usage.completion_tokens > 0 {
                last_tok_per_sec = result.usage.completion_tokens as f64 / elapsed;
            }
            if !result.reasoning.is_empty() {
                let _ = self.event_tx.send(AgentEvent::ReasoningDelta(result.reasoning.clone()));
            }
            if !result.text.is_empty() {
                let _ = self.event_tx.send(AgentEvent::Delta(result.text.clone()));
            }

            if result.tool_calls.is_empty() {
                final_text = result.text.clone();
                let msg = Message::assistant_text(result.text.clone());
                self.session.messages.push(msg.clone());
                self.journal.append(&msg)?;
                let _ = self.event_tx.send(AgentEvent::Done { reason: "no more tool calls".into() });
                return Ok(final_text);
            }

            let ac_msg = Message::assistant_tool_calls(result.tool_calls.clone());
            self.session.messages.push(ac_msg.clone());
            self.journal.append(&ac_msg)?;

            for call in result.tool_calls.clone() {
                let _ = self.event_tx.send(AgentEvent::ToolStart { name: call.name.clone(), args: call.arguments.clone() });

                if call.name == "submit_acceptance" {
                    let report = self.handle_acceptance(&call).await?;
                    let passed = report.verdict == crate::acceptance::Verdict::Pass;
                    let detail = report.replay.join("\n").chars().take(2000).collect();
                    let _ = self.event_tx.send(AgentEvent::Acceptance { passed, detail });
                    let serialized = serde_json::to_string(&report)?;
                    let msg = tool_result_msg(&call.id, serialized);
                    self.session.messages.push(msg.clone());
                } else {
                    let tr = self.dispatch(&call).await;
                    if tr.vision_parts.is_empty() {
                        let msg = tool_result_msg(&call.id, tr.output.clone());
                        self.session.messages.push(msg.clone());
                    } else {
                        let msg = tool_result_msg(&call.id, tr.output.clone());
                        self.session.messages.push(msg.clone());
                        let mut parts = vec![ContentPart::Text { text: format!("[result of {}]", call.name) }];
                        parts.extend(tr.vision_parts.clone());
                        let vmsg = Message::user_parts(parts);
                        self.session.messages.push(vmsg.clone());
                    }
                    let preview: String = tr.output.chars().take(300).collect();
                    let _ = self.event_tx.send(AgentEvent::ToolEnd { name: call.name.clone(), output_preview: preview });
                }
                let last = self.session.messages.last().unwrap().clone();
                self.journal.append(&last)?;
            }

            watchdog.record_success();
        }

        let _ = self.event_tx.send(AgentEvent::Done { reason: "max steps reached".into() });
        Ok(final_text)
    }

    async fn call_with_backoff(
        &mut self,
        specs: &[nexus_core::ToolSpec],
        watchdog: &mut Watchdog,
    ) -> Result<nexus_provider::openai::CompletionResult> {
        loop {
            let specs_ref = Some(specs);
            match self.client.chat_stream(&self.session.messages, specs_ref, &mut |_: &nexus_provider::openai::StreamDelta| {}).await {
                Ok(r) => return Ok(r),
                Err(e) => match watchdog.record_failure() {
                    WatchdogVerdict::RetryAfter(d) => {
                        let _ = self.event_tx.send(AgentEvent::Error {
                            message: format!("{}", e),
                            retry_after_secs: d.as_secs(),
                        });
                        tokio::time::sleep(d).await;
                    }
                    WatchdogVerdict::GiveUp => {
                        anyhow::bail!("giving up after {} consecutive failures: {}", self.cfg.max_consecutive_failures, e);
                    }
                },
            }
        }
    }

    async fn handle_acceptance(&self, call: &ToolCall) -> Result<crate::acceptance::AcceptanceReport> {
        let v: serde_json::Value = serde_json::from_str(&call.arguments)?;
        let task = v.get("task").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let criteria: Vec<String> = v
            .get("criteria")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let verify: Vec<String> = v
            .get("verify")
            .and_then(|c| c.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        Ok(crate::acceptance::evaluate(&task, criteria, verify).await)
    }

    fn all_specs(&self) -> Vec<nexus_core::ToolSpec> {
        let mut specs = self.registry.specs.clone();
        specs.push(crate::acceptance::submit_acceptance_spec());
        specs.push(nexus_core::ToolSpec::new(
            "memory_write",
            "Persist a long-term memory entry (facts, lessons, decisions). Survives across sessions.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "tag": { "type": "string", "description": "category e.g. project/lesson/decision" },
                    "text": { "type": "string" }
                },
                "required": ["tag", "text"]
            }),
        ));
        specs.push(nexus_core::ToolSpec::new(
            "memory_recall",
            "Search long-term memory (BM25). Returns top matches.",
            serde_json::json!({
                "type": "object",
                "properties": { "query": { "type": "string" } },
                "required": ["query"]
            }),
        ));
        specs
    }
}
