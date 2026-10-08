use serde::{Deserialize, Serialize};

/// 工具声明（OpenAI function-calling 格式的 parameters JSON Schema）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolSpec {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self { name: name.into(), description: description.into(), parameters }
    }
}
