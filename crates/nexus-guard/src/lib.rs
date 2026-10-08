pub mod loopguard;
pub mod watchdog;

pub use loopguard::{Action, LoopGuard};
pub use watchdog::{Watchdog, WatchdogVerdict};
