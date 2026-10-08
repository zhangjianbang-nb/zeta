use anyhow::Result;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// 受控 bash 执行：超时硬顶 + 输出截断。禁止交互命令（无 tty）。
pub async fn run_bash(cmd: &str, timeout_secs: u64, cwd: Option<&str>) -> Result<String> {
    let mut command = Command::new("bash");
    command.arg("-lc").arg(cmd).stdin(Stdio::null());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let output = timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| anyhow::anyhow!("bash timeout after {}s: {}", timeout_secs, truncate_cmd(cmd)))?
        .map_err(|e| anyhow::anyhow!("spawn bash: {}", e))?;

    let mut out = String::new();
    out.push_str(&String::from_utf8_lossy(&output.stdout));
    let err = String::from_utf8_lossy(&output.stderr);
    if !err.is_empty() {
        out.push_str(&format!("\n[stderr]\n{}", err));
    }
    if !output.status.success() {
        out.push_str(&format!("\n[exit code: {}]", output.status.code().unwrap_or(-1)));
    }
    Ok(truncate_output(out))
}

fn truncate_cmd(c: &str) -> String {
    if c.len() > 120 {
        format!("{}…", &c[..120])
    } else {
        c.to_string()
    }
}

/// 输出截断到 ~16KB（防止把巨量输出塞进上下文）。
fn truncate_output(s: String) -> String {
    const MAX: usize = 16 * 1024;
    if s.len() <= MAX {
        return s;
    }
    let mut cut = MAX;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n…[truncated {} bytes total]", &s[..cut], s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_and_captures() {
        let out = run_bash("echo hello", 10, None).await.unwrap();
        assert!(out.contains("hello"));
    }

    #[tokio::test]
    async fn timeout_kills() {
        let r = run_bash("sleep 5", 1, None).await;
        assert!(r.is_err());
    }

    #[test]
    fn truncates_long_output() {
        let big = "x".repeat(20 * 1024);
        let t = truncate_output(big);
        assert!(t.contains("[truncated"));
    }
}
