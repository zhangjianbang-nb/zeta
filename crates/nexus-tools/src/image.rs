use anyhow::{bail, Result};
use base64::Engine;
use std::path::Path;

/// 图片工具：
/// - image_info：纯脚本检查（尺寸/格式），不消耗视觉窗口 —— 多图任务优先走这个；
/// - image_view：把图片作为 data-url 注入会话（受 LoopGuard 图预算管控）。
pub fn image_info(path: &str) -> Result<String> {
    let p = Path::new(path);
    if !p.exists() {
        bail!("not found: {}", path);
    }
    let bytes = std::fs::read(p)?;
    let (w, h, format) = sniff_png(&bytes)
        .or_else(|| sniff_jpeg(&bytes))
        .unwrap_or((0, 0, "unknown".to_string()));
    Ok(format!("file={} format={} size={}x{} bytes={}", path, format, w, h, bytes.len()))
}

fn sniff_png(b: &[u8]) -> Option<(u32, u32, String)> {
    if b.len() < 24 || &b[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
    let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
    Some((w, h, "png".into()))
}

fn sniff_jpeg(b: &[u8]) -> Option<(u32, u32, String)> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    // 扫 SOF0/2 段取尺寸
    let mut i = 2;
    while i < b.len() - 9 {
        if b[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = b[i + 1];
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xC8 && marker != 0xCC {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
            return Some((w, h, "jpeg".into()));
        }
        let seg_len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        i += 2 + seg_len;
    }
    None
}

/// 转 data-url（供 vision 注入）。格式嗅探失败默认按 png。
pub fn to_data_url(path: &str) -> Result<String> {
    let bytes = std::fs::read(path)?;
    let mime = if sniff_png(&bytes).is_some() {
        "image/png"
    } else if sniff_jpeg(&bytes).is_some() {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") {
        "image/webp"
    } else {
        bail!("not a recognizable image: {}", path);
    };
    Ok(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(bytes)))
}

pub fn tool_specs() -> Vec<nexus_core::ToolSpec> {
    vec![
        nexus_core::ToolSpec::new(
            "image_info",
            "Script-level image check (format, dimensions, bytes). Costs NO vision window; prefer this for multi-image tasks.",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
        nexus_core::ToolSpec::new(
            "image_view",
            "Load an image into the conversation as a vision input. Counts against the image budget (max 4 in context).",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
    ]
}
