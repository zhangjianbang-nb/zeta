use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// 读文件（文本）。二进制文件拒绝并提示用 bash。
pub fn read_file(path: &str) -> Result<String> {
    let p = Path::new(path);
    if !p.exists() {
        bail!("file not found: {}", path);
    }
    let bytes = std::fs::read(p)?;
    match std::str::from_utf8(&bytes) {
        Ok(text) => {
            // 大文件截断
            if text.len() > 64 * 1024 {
                return Ok(format!("{}\n…[truncated at 64KB of {} bytes]", &text[..safe_cut(&text, 64 * 1024)], text.len()));
            }
            Ok(text.to_string())
        }
        Err(_) => bail!("binary file, use bash (xxd/od) instead: {}", path),
    }
}

/// 写文件（父目录自动创建）。
pub fn write_file(path: &str, content: &str) -> Result<String> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(p, content)?;
    Ok(format!("wrote {} bytes to {}", content.len(), path))
}

/// 列目录（一层）。
pub fn list_dir(path: &str) -> Result<String> {
    let mut entries: Vec<String> = vec![];
    for e in std::fs::read_dir(Path::new(path))? {
        let e = e?;
        let meta = e.metadata()?;
        let kind = if meta.is_dir() { "d" } else { "f" };
        entries.push(format!("{} {:>10} {}", kind, meta.len(), e.file_name().to_string_lossy()));
    }
    entries.sort();
    Ok(entries.join("\n"))
}

/// 补丁式编辑：old 必须唯一出现，替换为 new（学 zcode Edit 语义，防误伤）。
pub fn edit_file(path: &str, old: &str, new: &str) -> Result<String> {
    let text = std::fs::read_to_string(path)?;
    let hits = text.matches(old).count();
    if hits == 0 {
        bail!("old_string not found in {}", path);
    }
    if hits > 1 {
        bail!("old_string matches {} times, must be unique", hits);
    }
    std::fs::write(path, text.replacen(old, new, 1))?;
    Ok(format!("edited {}", path))
}

fn safe_cut(s: &str, max: usize) -> usize {
    let mut i = max;
    while i < s.len() && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn tool_specs() -> Vec<nexus_core::ToolSpec> {
    vec![
        nexus_core::ToolSpec::new(
            "read_file",
            "Read a UTF-8 text file. Returns content (truncated at 64KB).",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "absolute or cwd-relative path" } },
                "required": ["path"]
            }),
        ),
        nexus_core::ToolSpec::new(
            "write_file",
            "Write content to a file (parent dirs auto-created). Overwrites.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        nexus_core::ToolSpec::new(
            "edit_file",
            "Replace a unique substring in in a file. old_string must match exactly once.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        ),
        nexus_core::ToolSpec::new(
            "list_dir",
            "List one directory level with sizes.",
            serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        ),
    ]
}
