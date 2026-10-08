use std::time::{Duration, Instant};

/// 24h 长跑看门狗：连续失败退避 + 卡死检测 + 心跳。
/// 不杀进程，只报告状态并建议动作（退出码由 cli 决定，方便 systemd Restart=always 拉起）。
pub struct Watchdog {
    started: Instant,
    consecutive_failures: u32,
    max_consecutive_failures: u32,
    last_heartbeat: Instant,
    backoff: Duration,
}

impl Watchdog {
    pub fn new(max_consecutive_failures: u32) -> Self {
        Self {
            started: Instant::now(),
            consecutive_failures: 0,
            max_consecutive_failures,
            last_heartbeat: Instant::now(),
            backoff: Duration::from_secs(5),
        }
    }

    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.backoff = Duration::from_secs(5);
        self.heartbeat();
    }

    pub fn record_failure(&mut self) -> WatchdogVerdict {
        self.consecutive_failures += 1;
        // 指数退避：5s → 10s → 20s → 40s → 80s（封顶 5min）
        self.backoff = (self.backoff * 2).min(Duration::from_secs(300));
        if self.consecutive_failures >= self.max_consecutive_failures {
            WatchdogVerdict::GiveUp
        } else {
            WatchdogVerdict::RetryAfter(self.backoff)
        }
    }

    pub fn heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
    }

    pub fn stalled_for(&self) -> Duration {
        self.last_heartbeat.elapsed()
    }

    pub fn uptime(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn backoff(&self) -> Duration {
        self.backoff
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogVerdict {
    RetryAfter(Duration),
    GiveUp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_then_give_up() {
        let mut w = Watchdog::new(3);
        match w.record_failure() {
            WatchdogVerdict::RetryAfter(d) => assert_eq!(d, Duration::from_secs(10)),
            _ => panic!("expected retry"),
        }
        match w.record_failure() {
            WatchdogVerdict::RetryAfter(d) => assert_eq!(d, Duration::from_secs(20)),
            _ => panic!("expected retry"),
        }
        assert_eq!(w.record_failure(), WatchdogVerdict::GiveUp);
    }

    #[test]
    fn success_resets_backoff() {
        let mut w = Watchdog::new(5);
        let _ = w.record_failure();
        w.record_success();
        assert_eq!(w.backoff(), Duration::from_secs(5));
        assert_eq!(w.stalled_for().as_secs(), 0);
    }
}
