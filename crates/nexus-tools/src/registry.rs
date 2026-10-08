use anyhow::Result;
use nexus_core::{ContentPart, Message, ToolSpec};
use nexus_guard::{Action, LoopGuard};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub call_id: String,
    pub name: String,
    pub output: String,
    /// 图片查看类调用产生的多模态注入（回传给模型的 vision 内容）。
    pub vision_parts: Vec<ContentPart>,
}

/// 工具执行注册表：名字 -> 异步闭包。
pub struct ToolRegistry {
    pub specs: Vec<ToolSpec>,
    handlers: std::collections::HashMap<String, BoxedHandler>,
    pub guard: Arc<Mutex<LoopGuard>>,
    pub timeout_secs: u64,
    pub workdir: PathBuf,
}

type BoxedHandler = Box<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send>> + Send + Sync>;

fn handler<F, Fut>(f: F) -> BoxedHandler
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<String>> + Send + 'static,
{
    Box::new(move |args| Box::pin(f(args)))
}

impl ToolRegistry {
    pub fn new(timeout_secs: u64, max_identical: usize, max_images: usize, workdir: PathBuf) -> Self {
        let mut reg = Self {
            specs: vec![],
            handlers: Default::default(),
            guard: Arc::new(Mutex::new(LoopGuard::new(max_identical, max_images))),
            timeout_secs,
            workdir,
        };
        reg.install_builtins();
        reg
    }

    fn install_builtins(&mut self) {
        // bash
        self.push(
            nexus_core::ToolSpec::new(
                "bash",
                "Run a bash command (non-interactive). Timeout per config. Output truncated to 16KB.",
                serde_json::json!({
                    "type": "object",
                    "properties": { "command": { "type": "string" } },
                    "required": ["command"]
                }),
            ),
            handler(|args| async move {
                let v: serde_json::Value = serde_json::from_str(&args)?;
                let cmd = v.get("command").and_then(|c| c.as_str()).ok_or_else(|| anyhow::anyhow!("missing command"))?;
                crate::bash::run_bash(cmd, 600, None).await
            }),
        );
        // fs ops
        for spec in crate::fsops::tool_specs() {
            let name = spec.name.clone();
            match name.as_str() {
                "read_file" => self.push(
                    spec,
                    handler(|args| async move {
                        let v: serde_json::Value = serde_json::from_str(&args)?;
                        crate::fsops::read_file(v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?)
                    }),
                ),
                "write_file" => self.push(
                    spec,
                    handler(|args| async move {
                        let v: serde_json::Value = serde_json::from_str(&args)?;
                        crate::fsops::write_file(
                            v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?,
                            v.get("content").and_then(|c| c.as_str()).ok_or_else(|| anyhow::anyhow!("missing content"))?,
                        )
                    }),
                ),
                "edit_file" => self.push(
                    spec,
                    handler(|args| async move {
                        let v: serde_json::Value = serde_json::from_str(&args)?;
                        crate::fsops::edit_file(
                            v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?,
                            v.get("old_string").and_then(|c| c.as_str()).ok_or_else(|| anyhow::anyhow!("missing old_string"))?,
                            v.get("new_string").and_then(|c| c.as_str()).ok_or_else(|| anyhow::anyhow!("missing new_string"))?,
                        )
                    }),
                ),
                "list_dir" => self.push(
                    spec,
                    handler(|args| async move {
                        let v: serde_json::Value = serde_json::from_str(&args)?;
                        crate::fsops::list_dir(v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?)
                    }),
                ),
                _ => {}
            }
        }
        // image tools
        for spec in crate::image::tool_specs() {
            let name = spec.name.clone();
            match name.as_str() {
                "image_info" => self.push(
                    spec,
                    handler(|args| async move {
                        let v: serde_json::Value = serde_json::from_str(&args)?;
                        crate::image::image_info(v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?)
                    }),
                ),
                "image_view" => self.push(spec, handler(|_args| async move { Ok("image loaded into vision context".to_string()) })),
                _ => {}
            }
        }
        // audio
        for spec in crate::speech::tool_specs() {
            self.push(
                spec,
                handler(|args| async move {
                    let v: serde_json::Value = serde_json::from_str(&args)?;
                    let p = v.get("path").and_then(|p| p.as_str()).ok_or_else(|| anyhow::anyhow!("missing path"))?;
                    crate::speech::audio_to_content(p)?;
                    Ok("audio attached to context".to_string())
                }),
            );
        }
        // memory_write / memory_recall 由 agent crate 挂接（需要 store 所有权），此处留接口说明。
    }

    fn push(&mut self, spec: ToolSpec, h: BoxedHandler) {
        let name = spec.name.clone();
        self.specs.push(spec);
        self.handlers.insert(name, h);
    }

    /// 执行一个调用：先过 LoopGuard，再限时执行。
    pub async fn execute(&self, call: &nexus_core::ToolCall, session: &[Message]) -> ToolResult {
        let action = {
            let mut g = self.guard.lock().await;
            g.check(&call.name, &call.arguments, session)
        };
        match action {
            Action::Deny => {
                return ToolResult { call_id: call.id.clone(), name: call.name.clone(), output: nexus_guard::LoopGuard::deny_message(&call.name), vision_parts: vec![] };
            }
            Action::Warn => {
                // 放行但附加提醒（学 gateway 软手段：先提醒不硬拒）
                let out = format!("[loop-guard warning] this call pattern repeats; proceed only if truly needed.");
                return ToolResult { call_id: call.id.clone(), name: call.name.clone(), output: out, vision_parts: vec![] };
            }
            Action::Allow => {}
        }

        let handler = match self.handlers.get(&call.name) {
            Some(h) => h,
            None => {
                return ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    output: format!("unknown tool: {}", call.name),
                    vision_parts: vec![],
                };
            }
        };

        let fut = handler(call.arguments.clone());
        let result = tokio::time::timeout(std::time::Duration::from_secs(self.timeout_secs), fut).await;

        let output = match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => format!("tool error: {}", e),
            Err(_) => format!("tool timeout after {}s", self.timeout_secs),
        };

        // image_view 产生 vision 注入
        let vision_parts = if call.name == "image_view" {
            match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                Ok(v) => match v.get("path").and_then(|p| p.as_str()) {
                    Some(path) => match crate::image::to_data_url(path) {
                        Ok(url) => vec![ContentPart::ImageUrl { image_url: nexus_core::ImageUrl { url } }],
                        Err(e) => vec![],
                    },
                    None => vec![],
                },
                Err(_) => vec![],
            }
        } else if call.name == "audio_attach" {
            match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                Ok(v) => match v.get("path").and_then(|p| p.as_str()) {
                    Some(path) => match crate::speech::audio_to_content(path) {
                        Ok(part) => vec![part],
                        Err(_) => vec![],
                    },
                    None => vec![],
                },
                Err(_) => vec![],
            }
        } else {
            vec![]
        };

        ToolResult { call_id: call.id.clone(), name: call.name.clone(), output, vision_parts }
    }
}
