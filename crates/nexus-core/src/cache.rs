use serde::{Deserialize, Serialize};

/// 缓存命中指标：来自 provider usage（prompt_cache_hit_tokens 之类）。
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct CacheStats {
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    pub requests: u64,
}

impl CacheStats {
    pub fn hit_rate(&self) -> f64 {
        if self.prompt_tokens == 0 {
            0.0
        } else {
            self.cached_tokens as f64 / self.prompt_tokens as f64
        }
    }

    pub fn record(&mut self, prompt: u64, cached: u64) {
        self.prompt_tokens += prompt;
        self.cached_tokens += cached;
        self.requests += 1;
    }
}

/// 前缀缓存策略（学 zcode 的教训）：
/// - system prompt 与工具 schema 固定在最前 → 引擎 prefix cache 稳定命中；
/// - 历史只追加不重写 → 前缀单调增长；
/// - 上下文压缩走"截断最老的中间轮次"，不动头部与尾部（尾部是 cache 断点）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachePolicy {
    /// 超过该字符数的会话触发压缩（粗估，~2.5 chars/token）。
    pub compact_above_chars: usize,
    /// 压缩后保留最近多少条消息（尾部）。
    pub keep_recent: usize,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self { compact_above_chars: 600_000, keep_recent: 40 }
    }
}

/// 就地压缩：保留 system 头部 + 最近 keep_recent 条，中间替换为一条摘要占位。
/// 关键：头部与尾部不动 → prefix cache 在压缩后仍能从尾部继续命中。
pub fn compact_session(session: &mut crate::session::Session, policy: &CachePolicy) -> bool {
    let n = session.messages.len();
    if session.approx_chars() <= policy.compact_above_chars || n <= policy.keep_recent + 2 {
        return false;
    }
    let head_is_system = session.messages.first().map(|m| matches!(m.role, crate::msg::Role::System)).unwrap_or(false);
    let head = if head_is_system { 1 } else { 0 };
    let cut_start = head;
    let cut_end = n - policy.keep_recent;
    let dropped_n = cut_end - cut_start;
    let dropped_chars: usize = session.messages[cut_start..cut_end].iter().map(|m| m.text().len()).sum();
    let summary = format!(
        "[compacted] {} earlier messages dropped ({} chars). Key facts were persisted to memory by the agent before this point.",
        dropped_n, dropped_chars
    );
    session.messages.splice(cut_start..cut_end, vec![crate::msg::Message::assistant_text(summary)]);
    true
}
