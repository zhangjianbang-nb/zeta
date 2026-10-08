use anyhow::{Context, Result};
use nexus_core::config::Config;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 一条长期记忆。tag 用于域分组（project/feedback/lesson/...）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub tag: String,
    pub text: String,
    pub created_at: String,
}

/// JSONL 持久化 + BM25 召回（学 zcode memory 机制：MEMORY.md 索引 + topic 文件）。
pub struct MemoryStore {
    path: PathBuf,
    entries: Vec<Memory>,
    index: crate::bm25::Bm25,
}

impl MemoryStore {
    pub fn open(cfg: &Config) -> Result<Self> {
        let dir = cfg.data_dir.join("memory");
        fs::create_dir_all(&dir)?;
        let path = dir.join("memories.jsonl");
        let mut entries = Vec::new();
        if path.exists() {
            let text = fs::read_to_string(&path)?;
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str::<Memory>(line) {
                    Ok(m) => entries.push(m),
                    Err(_) => continue, // 跳过损坏行，不因单行炸整体（24h 鲁棒性）
                }
            }
        }
        let mut index = crate::bm25::Bm25::new();
        for m in &entries {
            index.add(&format!("{} {}", m.tag, m.text));
        }
        Ok(Self { path, entries, index })
    }

    /// 写入并落盘（append + flush，崩溃安全）。
    pub fn write(&mut self, tag: &str, text: &str) -> Result<Memory> {
        let m = Memory {
            id: uuid::Uuid::new_v4().to_string(),
            tag: tag.into(),
            text: text.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
        };
        use std::io::Write;
        let mut f = fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        f.write_all(serde_json::to_string(&m)?.as_bytes())?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        self.index.add(&format!("{} {}", m.tag, m.text));
        self.entries.push(m.clone());
        Ok(m)
    }

    /// BM25 召回 top_k 条。
    pub fn recall(&self, query: &str, top_k: usize) -> Vec<&Memory> {
        if self.entries.is_empty() {
            return vec![];
        }
        self.index
            .search(query, top_k)
            .into_iter()
            .filter_map(|i| self.entries.get(i))
            .collect::<Vec<_>>()
    }

    /// 把召回结果注入 system 上下文（zcode 风格：每次任务开工时带相关记忆）。
    pub fn recall_prompt(&self, query: &str, top_k: usize) -> String {
        let hits = self.recall(query, top_k);
        if hits.is_empty() {
            return String::new();
        }
        let mut s = String::from("[long-term memory recall]\n");
        for h in hits {
            s.push_str(&format!("- ({}) {}\n", h.tag, h.text));
        }
        s
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_cfg() -> (Config, tempdir::TempDirGuard) {
        let mut cfg = Config::default();
        let g = tempdir::TempDirGuard::new();
        cfg.data_dir = g.path.clone();
        (cfg, g)
    }

    // 简易临时目录（避免额外依赖）
    mod tempdir {
        use super::*;
        use std::path::PathBuf;

        pub struct TempDirGuard {
            pub path: PathBuf,
        }

        impl TempDirGuard {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!("nexus-test-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Self { path: p }
            }
        }

        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }
    }

    #[test]
    fn write_recall_roundtrip() -> Result<()> {
        let (cfg, _guard) = tmp_cfg();
        let mut store = MemoryStore::open(&cfg).with_context(|| "open store")?;
        store.write("lesson", "不要重读被驱逐的图片")?;
        store.write("project", "nexus harness 用 rust 编写")?;
        assert_eq!(store.len(), 2);
        let hits = store.recall("图片 死循环", 1);
        assert!(!hits.is_empty());
        assert!(hits[0].text.contains("图片"));
        Ok(())
    }
}
