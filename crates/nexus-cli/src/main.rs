//! nexus — 多模态 agent harness 入口。
//!
//! 子命令：
//!   nexus tui                 opencode 风格交互界面
//!   nexus run "任务"          跑单个任务
//!   nexus daemon "任务清单"   24h 无人值守：逐任务执行 + 崩溃自愈 + watchdog 退避
//!   nexus rsi "改动说明"      自我升级（过四道门：fmt/test/build + 审计）
//!   nexus memory query        查记忆

use anyhow::Result;
use clap::{Parser, Subcommand};
use nexus_core::config::Config;
use nexus_core::Message;

#[derive(Parser)]
#[command(name = "nexus", version, about = "Rust multimodal agent harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// opencode 风格 TUI
    Tui,
    /// 执行单个任务
    Run { prompt: String },
    /// 24h 无人值守模式：从文件读任务清单逐个执行
    Daemon { task_file: String },
    /// 自我升级
    Rsi { summary: String },
    /// 查询长期记忆
    Memory {
        #[arg(default_value = "")]
        query: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cfg = Config::load()?;
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Tui => nexus_tui::run_tui(cfg).await,
        Cmd::Run { prompt } => run_once(cfg, &prompt).await,
        Cmd::Daemon { task_file } => daemon(cfg, &task_file).await,
        Cmd::Rsi { summary } => {
            let repo = std::env::var("NEXUS_REPO").unwrap_or_else(|_| "/home/whu/workspace/nexus-harness".into());
            let mut mem = nexus_memory::MemoryStore::open(&cfg)?;
            let rsi = nexus_agent::RsiController::new(std::path::PathBuf::from(repo));
            match rsi.upgrade_gate(&mut mem, &summary).await {
                Ok(msg) => {
                    println!("RSI OK: {}", msg);
                    Ok(())
                }
                Err(e) => {
                    eprintln!("RSI FAILED: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Cmd::Memory { query } => {
            let mem = nexus_memory::MemoryStore::open(&cfg)?;
            if query.is_empty() {
                println!("{} memories", mem.len());
            } else {
                for m in mem.recall(&query, 10) {
                    println!("[{}] {}", m.tag, m.text);
                }
            }
            Ok(())
        }
    }
}

async fn run_once(cfg: Config, prompt: &str) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let cfg2 = cfg.clone();
    let prompt_owned = prompt.to_string();
    let handle = tokio::spawn(async move {
        let mut agent = nexus_agent::Agent::new(cfg2, tx).await?;
        agent.run_task(&prompt_owned).await
    });
    while let Some(evt) = rx.recv().await {
        print_event(&evt);
    }
    let _ = handle
        .await
        .map_err(|e| anyhow::anyhow!("join: {}", e))?
        ;
    Ok(())
}

/// 24h 自愈守护：逐任务串行执行；单任务失败不退出（记 memory，跳过继续）；
/// 整个 daemon 崩了由外部 systemd Restart=always 拉起，journal+memory 保证续跑。
async fn daemon(cfg: Config, task_file: &str) -> Result<()> {
    let tasks = std::fs::read_to_string(task_file)?;
    let list: Vec<String> = tasks
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    println!("[daemon] {} tasks loaded from {}", list.len(), task_file);

    let mut mem = nexus_memory::MemoryStore::open(&cfg)?;
    let mut done = 0usize;
    let mut failed = 0usize;

    for (i, task) in list.iter().enumerate() {
        println!("[daemon] ({}/{}) {}", i + 1, list.len(), task);
        let r = run_once(cfg.clone(), task).await;
        match r {
            Ok(_) => {
                done += 1;
                let _ = mem.write("daemon", &format!("task {} done: {}", i + 1, task));
            }
            Err(e) => {
                failed += 1;
                eprintln!("[daemon] task {} failed: {}", i + 1, e);
                let _ = mem.write("daemon", &format!("task {} FAILED: {} — {}", i + 1, task, e));
                // 失败不退出：继续下一个任务（24h 铁律）
            }
        }
    }

    println!("[daemon] finished: {} done, {} failed", done, failed);
    Ok(())
}

fn print_event(evt: &nexus_agent::AgentEvent) {
    use nexus_agent::AgentEvent;
    match evt {
        AgentEvent::Delta(t) => print!("{}", t),
        AgentEvent::ReasoningDelta(_) => {} // 静默（verbose 可开）
        AgentEvent::ToolStart { name, args } => {
            eprintln!("\n⚙ {}", name);
            if args.len() < 200 {
                eprintln!("  {}", args.replace('\n', " "));
            }
        }
        AgentEvent::ToolEnd { name, output_preview } => {
            eprintln!("  {} → {}", name, output_preview.replace('\n', " "));
        }
        AgentEvent::Acceptance { passed, detail } => {
            if *passed {
                eprintln!("\n✓ acceptance PASSED");
            } else {
                eprintln!("\n✗ acceptance FAILED\n{}", detail);
            }
        }
        AgentEvent::Status { metrics } => {
            eprintln!("[step {} | cache {:.0}%]", metrics.step, metrics.cache_hit_rate * 100.0);
        }
        AgentEvent::Done { reason } => eprintln!("\n— done: {}", reason),
        AgentEvent::Error { message, retry_after_secs } => {
            eprintln!("! error, retry in {}s: {}", retry_after_secs, message);
        }
    }
}
