//! 验收机制：任务完成声明必须带证据，证据必须可编程复核。
//!
//! 设计（对应 system prompt 的 ACCEPTANCE PROTOCOL）：
//! 1. 模型声明完成时必须调用 `submit_acceptance` 工具，给出 criteria 与 evidence；
//! 2. harness 对 evidence 里每条 `verify:<bash>` 命令真实重放；
//! 3. 重放全部 exit 0 → PASS；任一失败 → 返回失败详情给模型继续干（不许糊弄过关）。

use anyhow::Result;
use nexus_core::{Message, ToolSpec};
use nexus_tools::bash::run_bash;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceReport {
    pub task: String,
    pub criteria: Vec<String>,
    /// 每条形如 "bash command" 的可重放验证（harness 真实执行）。
    pub verify: Vec<String>,
    pub verdict: Verdict,
    #[serde(default)]
    pub replay: Vec<String>,
}

pub fn submit_acceptance_spec() -> ToolSpec {
    ToolSpec::new(
        "submit_acceptance",
        "Declare task completion. MUST be called before telling the user the task is done. \
         Provide acceptance criteria and shell commands whose exit code 0 proves each criterion. \
         The harness replays them; failures are reported back and the task continues.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": { "type": "string" },
                "criteria": { "type": "array", "items": { "type": "string" } },
                "verify": { "type": "array", "items": { "type": "string" }, "description": "shell commands; exit 0 = pass" }
            },
            "required": ["task", "criteria", "verify"]
        }),
    )
}

/// 重放 verify 命令，产出报告。
pub async fn evaluate(task: &str, criteria: Vec<String>, verify: Vec<String>) -> AcceptanceReport {
    let mut replay = vec![];
    let mut verdict = Verdict::Pass;
    for cmd in &verify {
        match run_bash(cmd, 600, None).await {
            Ok(out) => {
                let passed = !out.contains("[exit code:");
                if !passed {
                    verdict = Verdict::Fail;
                }
                replay.push(format!("$ {}\n→ {}", cmd, if passed { "PASS" } else { "FAIL" }));
                replay.push(out);
            }
            Err(e) => {
                verdict = Verdict::Fail;
                replay.push(format!("$ {}\n→ ERROR: {}", cmd, e));
            }
        }
    }
    AcceptanceReport { task: task.to_string(), criteria, verify, verdict, replay }
}

/// 构造回给模型的验收反馈消息。
pub fn feedback_message(report: &AcceptanceReport) -> Message {
    let text = match report.verdict {
        Verdict::Pass => {
            let mut s = String::from("[acceptance PASSED]\n");
            for line in &report.replay {
                s.push_str(line);
                s.push('\n');
            }
            s.push_str("Evidence verified. You may now report completion to the user, including: what was done / how verified / remaining risks.");
            s
        }
        Verdict::Fail => {
            let mut s = String::from("[acceptance FAILED]\nYour verification commands did not all pass. Fix and resubmit.\n");
            for line in &report.replay {
                s.push_str(line);
                s.push('\n');
            }
            s
        }
    };
    Message::user(text)
}
