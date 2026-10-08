use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioData {
    pub data: String,
    pub format: String,
}

/// 多模态内容片段：文本 / 图片 / 语音。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
    InputAudio { input_audio: AudioData },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self { role: Role::System, content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: None, name: None }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: None, name: None }
    }

    pub fn user_parts(parts: Vec<ContentPart>) -> Self {
        Self { role: Role::User, content: Some(Content::Parts(parts)), tool_calls: None, tool_call_id: None, name: None }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: None, name: None }
    }

    pub fn assistant_tool_calls(calls: Vec<ToolCall>) -> Self {
        Self { role: Role::Assistant, content: None, tool_calls: Some(calls), tool_call_id: None, name: None }
    }

    pub fn tool_result(call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: Some(Content::Text(text.into())), tool_calls: None, tool_call_id: Some(call_id.into()), name: None }
    }

    pub fn text(&self) -> String {
        match &self.content {
            Some(Content::Text(t)) => t.clone(),
            Some(Content::Parts(parts)) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    ContentPart::ImageUrl { .. } => Some("[image]".into()),
                    ContentPart::InputAudio { .. } => Some("[audio]".into()),
                })
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        }
    }

    /// 图片 data-url / http-url 列表（用于图预算统计）。
    pub fn image_urls(&self) -> Vec<String> {
        match &self.content {
            Some(Content::Parts(parts)) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::ImageUrl { image_url } => Some(image_url.url.clone()),
                    _ => None,
                })
                .collect(),
            _ => vec![],
        }
    }
}
