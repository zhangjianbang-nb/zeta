//! 运行指标：TUI 状态栏数据源（学 zcode 状态栏五要素并扩展）。

use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Metrics {
    /// 当前任务步数
    pub step: usize,
    /// prefix cache 命中率 0..1
    pub cache_hit_rate: f64,
    /// 最近一次补全 decode 速度 tok/s
    pub tok_per_sec: f64,
    /// 当前 prompt 估算字符量（÷2.5≈token）
    pub prompt_chars: usize,
    /// 本会话 compact 累计丢弃的 token 估算
    pub compacted_tokens: usize,
    /// 活跃子代理数（后台任务占位：当前=0，扩展点）
    pub subagents: usize,
    /// daemon 队列剩余任务数
    pub scheduled: usize,
    /// 记忆条数
    pub memories: usize,
    /// 本次进程累计错误数
    pub errors: usize,
    /// provider 温度
    pub temperature: f64,
    /// 统一内存 used/total MB（GB10 无独立显存，读 MemAvailable）
    pub mem_used_mb: u64,
    pub mem_total_mb: u64,
}

impl Metrics {
    pub fn read_system_memory() -> (u64, u64) {
        let (mut total, mut avail) = (0u64, 0u64);
        if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
            for line in text.lines() {
                if let Some(v) = line.strip_prefix("MemTotal:") {
                    total = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0) / 1024;
                }
                if let Some(v) = line.strip_prefix("MemAvailable:") {
                    avail = v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0) / 1024;
                }
            }
        }
        (total.saturating_sub(avail), total)
    }

    /// 渲染成单行状态串（TUI 与 CLI 共用）。
    pub fn status_line(&self, uptime: Duration) -> String {
        let hh = uptime.as_secs() / 3600;
        let mm = (uptime.as_secs() % 3600) / 60;
        let ss = uptime.as_secs() % 60;
        format!(
            "up {:02}:{:02}:{:02} | tok/s {:.0} | cache {:.0}% | prompt {}ch (~{} tok) | compacted {} tok | sub {} | cron {} | mem {} | err {} | temp {:.1} | ram {}/{} MB",
            hh, mm, ss,
            self.tok_per_sec,
            self.cache_hit_rate * 100.0,
            self.prompt_chars,
            self.prompt_chars / 2 + 500,
            self.compacted_tokens,
            self.subagents,
            self.scheduled,
            self.memories,
            self.errors,
            self.temperature,
            self.mem_used_mb,
            self.mem_total_mb,
        )
    }
}
