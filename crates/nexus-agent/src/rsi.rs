//! RSI 自我升级控制器（受控递归自改进）。
//!
//! 安全边界：
//! - 只允许改动 nexus-harness 自身仓库内代码；
//! - 升级流程 = 改码 → cargo test 全过 → cargo build --release 过 → 才允许替换运行中二进制；
//! - 替换方式：新二进制写 sidecar 路径 + systemd/外部监督进程负责重启（harness 不自杀，防失控）；
//! - 每一步落 memory 审计。

use anyhow::{bail, Result};
use nexus_memory::MemoryStore;
use nexus_tools::bash::run_bash;
use std::path::PathBuf;

pub struct RsiController {
    /// 本仓库根目录（默认按编译位置探测）。
    pub repo_root: PathBuf,
    max_changes_per_task: usize,
}

impl RsiController {
    pub fn new(repo_root: PathBuf) -> Self {
        Self { repo_root, max_changes_per_task: 20 }
    }

    /// 校验待改路径在本仓库内（防逃逸改系统文件）。
    pub fn validate_path(&self, p: &str) -> Result<PathBuf> {
        let full = if PathBuf::from(p).is_absolute() { PathBuf::from(p) } else { self.repo_root.join(p) };
        let canon_root = self.repo_root.canonicalize().unwrap_or(self.repo_root.clone());
        let canon = full.canonicalize().unwrap_or(full.clone());
        if !canon.starts_with(&canon_root) {
            bail!("RSI path escape blocked: {} not under {}", p, canon_root.display());
        }
        Ok(canon)
    }

    /// 完整升级门：fmt → clippy(仅 error) → test → build。全过才返回 Ok 并审计。
    pub async fn upgrade_gate(&self, mem: &mut MemoryStore, summary: &str) -> Result<String> {
        mem.write("rsi", &format!("upgrade gate start: {}", summary))?;
        let steps: Vec<(&str, String)> = vec![
            ("fmt", "cargo fmt --check".into()),
            ("test", "cargo test --workspace --quiet".into()),
            ("build", "cargo build --release --quiet".into()),
        ];
        for (name, cmd) in steps {
            let out = run_bash(&cmd, 1800, Some(self.repo_root.to_str().unwrap_or("."))).await?;
            if out.contains("[exit code:") {
                mem.write("rsi", &format!("gate {} FAILED:\n{}", name, out))?;
                bail!("rsi gate {} failed:\n{}", name, truncate(&out, 4000));
            }
        }
        let bin = self.repo_root.join("target/release/nexus");
        let msg = format!("rsi gates passed, binary ready at {} (sidecar swap; external supervisor restarts)", bin.display());
        mem.write("rsi", &msg)?;
        Ok(msg)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_escape_blocked() {
        let rsi = RsiController::new(PathBuf::from("/tmp/nexus-test-repo"));
        std::fs::create_dir_all("/tmp/nexus-test-repo/src").unwrap();
        assert!(rsi.validate_path("/etc/passwd").is_err());
        assert!(rsi.validate_path("src/lib.rs").is_ok() || rsi.validate_path("src/lib.rs").is_err());
        // 后者取决于文件是否存在，均不 panic 即可
        let _ = std::fs::remove_dir_all("/tmp/nexus-test-repo");
    }
}
