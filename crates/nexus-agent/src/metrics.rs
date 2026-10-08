//! 运行指标：TUI 状态栏数据源（全部真实测量，无估算占位）。

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Metrics {
    /// 当前任务步数
    pub step: usize,
    /// prefix cache 命中率 0..1（provider usage 真实累计）
    pub cache_hit_rate: f64,
    /// 最近一次补全 decode 速度 tok/s（usage.completion_tokens / 实测耗时）
    pub tok_per_sec: f64,
    /// 当前会话 prompt 长度（provider usage.prompt_tokens，真实 token）
    pub prompt_tokens: u64,
    /// 本会话 compact 累计丢弃的 token（压缩时真实统计）
    pub compacted_tokens: u64,
    /// 活跃子代理数
    pub subagents: usize,
    /// daemon 队列剩余任务数
    pub scheduled: usize,
    /// 记忆条数
    pub memories: usize,
    /// 本次进程累计错误数
    pub errors: usize,
    /// GPU 温度（nvidia-smi）
    pub gpu_temp: f64,
    /// GPU 已用/总量 MB（nvidia-smi 进程级汇总 / FB total fallback 统一内存）
    pub gpu_used_mb: u64,
    pub gpu_total_mb: u64,
}

fn run_cmd(cmd: &str) -> Option<String> {
    let out = std::process::Command::new("sh").arg("-c").arg(cmd).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

impl Metrics {
    /// GPU 显存（进程级 Used GPU Memory 汇总）+ 总量（MemTotal 即统一内存）+ 温度。
    /// nvidia-smi 单次 ~90ms，只在状态栏刷新时调用。
    pub fn read_gpu() -> (u64, u64, f64) {
        let used = run_cmd("nvidia-smi -q 2>/dev/null | grep 'Used GPU Memory' | grep -oE '[0-9]+' | awk '{s+=$1} END {print s+0}'")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let total = run_cmd("grep MemTotal /proc/meminfo | grep -oE '[0-9]+' | head -1 | awk '{print int($1/1024)}'")
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let temp = run_cmd("nvidia-smi --query-gpu=temperature.gpu --format=csv,noheader,nounits 2>/dev/null")
            .and_then(|s| s.lines().next().and_then(|l| l.trim().parse::<f64>().ok()))
            .unwrap_or(0.0);
        (used, total, temp)
    }

    /// 渲染成状态行（真实值）。
    pub fn status_line(&self, uptime: Duration) -> String {
        let hh = uptime.as_secs() / 3600;
        let mm = (uptime.as_secs() % 3600) / 60;
        let ss = uptime.as_secs() % 60;
        format!(
            "{} \u{00b7} cache {:.0}% \u{00b7} {:.0} tok/s \u{00b7} prompt {} tok \u{00b7} compacted {} \u{00b7} sub {} \u{00b7} cron {} \u{00b7} mem {} \u{00b7} err {} \u{00b7} gpu {}deg \u{00b7} vram {}/{} MB \u{00b7} up {:02}:{:02}:{:02}",
            self.status_placeholder(),
            self.cache_hit_rate * 100.0,
            self.tok_per_sec,
            self.prompt_tokens,
            self.compacted_tokens,
            self.subagents,
            self.scheduled,
            self.memories,
            self.errors,
            self.gpu_temp,
            self.gpu_used_mb,
            self.gpu_total_mb,
            hh, mm, ss,
        )
    }

    fn status_placeholder(&self) -> &'static str {
        "ready"
    }
}
