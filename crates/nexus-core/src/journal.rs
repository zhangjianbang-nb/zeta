use crate::session::Session;
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// append-only JSONL 会话日志：每条消息一行，崩溃只丢最后一行，重启可恢复。
pub struct Journal {
    path: std::path::PathBuf,
}

impl Journal {
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn open(dir: &Path, session_id: &str) -> Result<Self> {
        fs::create_dir_all(dir)?;
        Ok(Self { path: dir.join(format!("{}.jsonl", session_id)) })
    }

    /// 追加一条消息并 fsync（24h 长跑的关键：崩溃不丢上下文）。
    pub fn append(&self, msg: &crate::msg::Message) -> Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(msg)?;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        Ok(())
    }

    /// 从 journal 恢复会话（崩溃恢复路径）。
    pub fn load(&self) -> Result<Session> {
        let text = fs::read_to_string(&self.path).with_context(|| format!("read {}", self.path.display()))?;
        let mut s = Session { id: self.path.file_stem().unwrap_or_default().to_string_lossy().to_string(), ..Default::default() };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let msg: crate::msg::Message = serde_json::from_str(line).with_context(|| format!("corrupt line in {}", self.path.display()))?;
            s.messages.push(msg);
        }
        s.repair_dangling_tool_calls();
        Ok(s)
    }
}
