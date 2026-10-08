use nexus_core::msg::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Warn,
    Deny,
}

/// 防无限循环护栏（学 zcode 三类事故：周期型读图回路 / 重复工具调用 / arguments 退化）。
///
/// 判据三条：
/// 1. 完全相同 (工具, 参数指纹) 超过 max_identical 次 → Deny
/// 2. 滑动窗口内出现 2 到 4 周期模式（A,B,A,B…）→ Warn，连续 Warn 超限 → Deny
/// 3. 图片预算：session 内图数已达上限 → 图片类调用 Deny（防 [image evicted] 死循环）
#[derive(Debug)]
pub struct LoopGuard {
    counts: Vec<(String, u64, u32)>,
    recent: Vec<(String, u64)>,
    warn_streak: usize,
    max_identical: usize,
    max_images: usize,
}

const WINDOW: usize = 8;
const MAX_WARN_STREAK: usize = 2;

fn fingerprint(args: &str) -> u64 {
    // FNV-1a：够用、无依赖、稳定。
    let bytes = args.as_bytes();
    let mut h: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= u64::from(bytes[i]);
        h = h.wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

impl LoopGuard {
    pub fn new(max_identical: usize, max_images: usize) -> Self {
        Self { counts: Vec::new(), recent: Vec::new(), warn_streak: 0, max_identical, max_images }
    }

    /// 在执行工具调用前询问。Deny 时配 deny_message 作为 tool result 回给模型。
    pub fn check(&mut self, tool: &str, args: &str, session: &[Message]) -> Action {
        let fp = fingerprint(args);
        let existing = self.counts.iter().position(|(t, f, _n)| t == tool && f == &fp);
        let idx = match existing {
            Some(i) => i,
            None => {
                self.counts.push((tool.to_string(), fp, 0u32));
                self.counts.len() - 1
            }
        };
        self.counts[idx].2 = self.counts[idx].2.saturating_add(1);
        if self.counts[idx].2 as usize > self.max_identical {
            return Action::Deny;
        }

        self.recent.push((tool.to_string(), fp));
        if self.recent.len() > WINDOW {
            self.recent.remove(0);
        }
        if self.detect_cycle() {
            self.warn_streak += 1;
            if self.warn_streak > MAX_WARN_STREAK {
                return Action::Deny;
            }
            return Action::Warn;
        }
        self.warn_streak = 0;

        if tool == "image_view" || tool == "read_image" {
            let mut n_images = 0usize;
            for m in session {
                n_images += m.image_urls().len();
            }
            if n_images >= self.max_images {
                return Action::Deny;
            }
        }

        Action::Allow
    }

    fn detect_cycle(&self) -> bool {
        let n = self.recent.len();
        if n < 4 {
            return false;
        }
        let mut p = 2usize;
        while p <= 4 {
            if n >= p * 2 {
                let tail = &self.recent[n - p * 2..];
                if tail[0..p] == tail[p..2 * p] {
                    return true;
                }
            }
            p += 1;
        }
        false
    }

    pub fn deny_message(tool: &str) -> String {
        let detail = format!(
            "Tool {} was blocked: you are repeating the same call. Stop, use the info you already got, persist it via memory_write, or change approach.",
            tool
        );
        let obj = serde_json::json!({ "error": "loop guard", "detail": detail });
        obj.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::msg::Message;

    fn empty_session() -> Vec<Message> {
        Vec::new()
    }

    #[test]
    fn identical_calls_denied_after_limit() {
        let mut g = LoopGuard::new(3, 4);
        let s = empty_session();
        assert_eq!(g.check("bash", "ls", &s), Action::Allow);
        assert_eq!(g.check("bash", "ls", &s), Action::Allow);
        assert_eq!(g.check("bash", "ls", &s), Action::Allow);
        assert_eq!(g.check("bash", "ls", &s), Action::Deny);
    }

    #[test]
    fn abab_cycle_warns_then_denies() {
        let mut g = LoopGuard::new(50, 99);
        let s = empty_session();
        let seq = ["a.rs", "b.rs", "a.rs", "b.rs", "a.rs", "b.rs"];
        let mut actions = Vec::new();
        for f in seq.iter() {
            actions.push(g.check("read", f, &s));
        }
        assert!(actions.contains(&Action::Warn));
        assert_eq!(actions.last().unwrap().clone(), Action::Deny);
    }

    #[test]
    fn image_budget_blocks_view_when_full() {
        let mut g = LoopGuard::new(10, 4);
        let mut parts = Vec::new();
        let mut k = 0;
        while k < 4 {
            parts.push(nexus_core::ContentPart::ImageUrl {
                image_url: nexus_core::ImageUrl { url: format!("data:image/png;base64,img{}", k) },
            });
            k += 1;
        }
        let s = vec![Message::user_parts(parts)];
        assert_eq!(g.check("image_view", "x", &s), Action::Deny);
    }
}
