use anyhow::{bail, Result};
use base64::Engine;
use std::path::Path;

/// 语音输入：读取本地音频文件转 base64（OpenAI input_audio 协议）。
/// TTS/ASR 由 provider 侧模型承担（GLM 系列 omni 端点或外部 whisper 服务），
/// harness 只负责编码与注入。
pub fn audio_to_content(path: &str) -> Result<nexus_core::ContentPart> {
    let p = Path::new(path);
    if !p.exists() {
        bail!("audio not found: {}", path);
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_lowercase();
    let fmt = match ext.as_str() {
        "wav" => "wav",
        "mp3" => "mp3",
        "flac" => "flac",
        "ogg" | "opus" => "opus",
        _ => bail!("unsupported audio format: {}", ext),
    };
    let bytes = std::fs::read(p)?;
    if bytes.len() > 20 * 1024 * 1024 {
        bail!("audio too large (>20MB): {}", path);
    }
    Ok(nexus_core::ContentPart::InputAudio {
        input_audio: nexus_core::AudioData {
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            format: fmt.into(),
        },
    })
}

pub fn tool_specs() -> Vec<nexus_core::ToolSpec> {
    vec![nexus_core::ToolSpec::new(
        "audio_attach",
        "Attach a local audio file (wav/mp3/flac/opus, <20MB) to the conversation as speech input.",
        serde_json::json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )]
}
