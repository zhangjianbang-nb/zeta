use crate::msg::Message;
use serde::{Deserialize, Serialize};

/// 会话：id + 消息序列。journal 是它的持久化形式。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            messages: vec![],
        }
    }

    /// 估算 prompt 字符量（cache 指标用，非精确 token）。
    pub fn approx_chars(&self) -> usize {
        self.messages.iter().map(|m| m.text().len() + 8).sum::<usize>() + self.messages.iter().map(|m| m.image_urls().len() * 1024).sum::<usize>()
    }

    /// 崩溃恢复：若最后一条 assistant 带 tool_calls 但对应 tool 结果缺失，补"interrupted"结果。
    pub fn repair_dangling_tool_calls(&mut self) -> usize {
        let mut repaired = 0;
        let mut i = 0;
        while i < self.messages.len() {
            let m = self.messages[i].clone();
            if let Some(calls) = m.tool_calls.clone() {
                let answered: Vec<String> = self.messages[i + 1..]
                    .iter()
                    .take_while(|n| n.role == crate::msg::Role::Tool)
                    .filter_map(|n| n.tool_call_id.clone())
                    .collect();
                for c in calls {
                    if !answered.contains(&c.id) {
                        self.messages.insert(i + 1 + answered.len(), Message::tool_result(c.id.clone(), "{\"error\":\"interrupted by crash, tool never ran\"}"));
                        repaired += 1;
                    }
                }
            }
            i += 1;
        }
        repaired
    }
}

use crate::msg::Role;
