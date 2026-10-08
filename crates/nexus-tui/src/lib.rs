//! opencode 风格 TUI：底部输入框 + 滚动事件日志 + 状态栏。

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use futures::StreamExt;
use nexus_agent::{Agent, AgentEvent};
use nexus_core::config::Config;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

pub async fn run_tui(cfg: Config) -> Result<()> {
    let mut terminal = ratatui::init();
    let res = tui_loop(cfg, &mut terminal).await;
    ratatui::restore();
    res
}

async fn tui_loop(cfg: Config, terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut input = String::new();
    let mut log: Vec<Line> = vec![];
    let mut running_task: Option<String> = None;
    let mut status = String::from("ready");
    let mut cache_rate: f64 = 0.0;
    let mut step: usize = 0;
    let mut events_rx: Option<mpsc::UnboundedReceiver<AgentEvent>> = None;
    let mut event_stream = EventStream::new();
    let mut quit = false;

    while !quit {
        let mut submitted = false;

        if let Some(rx) = events_rx.as_mut() {
            tokio::select! {
                maybe_evt = event_stream.next() => {
                    match maybe_evt {
                        Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                            if k.code == KeyCode::Esc {
                                quit = true;
                            } else {
                                submitted = handle_key(k.code, &mut input, running_task.is_none());
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => anyhow::bail!("event stream: {}", e),
                        None => return Ok(()),
                    }
                }
                maybe_evt = rx.recv() => {
                    match maybe_evt {
                        Some(evt) => apply_event(evt, &mut log, &mut status, &mut cache_rate, &mut step, &mut running_task),
                        None => { events_rx = None; running_task = None; status = String::from("ready"); }
                    }
                }
            }
        } else {
            tokio::select! {
                maybe_evt = event_stream.next() => {
                    match maybe_evt {
                        Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
                            if k.code == KeyCode::Esc {
                                quit = true;
                            } else {
                                submitted = handle_key(k.code, &mut input, true);
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(e)) => anyhow::bail!("event stream: {}", e),
                        None => return Ok(()),
                    }
                }
            }
        }

        if submitted && running_task.is_none() {
            let prompt = input.trim().to_string();
            input.clear();
            running_task = Some(prompt.clone());
            log.push(Line::from(Span::styled(prompt.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
            let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
            events_rx = Some(rx);
            let cfg2 = cfg.clone();
            tokio::spawn(async move {
                if let Err(e) = spawn_and_forward(cfg2, prompt, tx).await {
                    tracing::error!("agent task: {}", e);
                }
            });
        }

        terminal.draw(|f| draw_ui(f, &input, &log, &status, cache_rate, step, running_task.as_deref()))?;
    }
    Ok(())
}

/// 启动 agent：事件走 tx；结束后发 Done。
async fn spawn_and_forward(cfg: Config, prompt: String, tx: mpsc::UnboundedSender<AgentEvent>) -> Result<()> {
    let r = async {
        let mut agent = Agent::new(cfg, tx.clone()).await?;
        agent.run_task(&prompt).await
    }
    .await;
    match r {
        Ok(_) => {
            let _ = tx.send(AgentEvent::Done { reason: String::from("task finished") });
            Ok(())
        }
        Err(e) => {
            let _ = tx.send(AgentEvent::Error { message: format!("{}", e), retry_after_secs: 0 });
            let _ = tx.send(AgentEvent::Done { reason: String::from("task failed") });
            Err(e)
        }
    }
}

/// 返回 true 表示提交了一个 prompt。
fn handle_key(code: KeyCode, input: &mut String, can_submit: bool) -> bool {
    match code {
        KeyCode::Enter => can_submit && !input.trim().is_empty(),
        KeyCode::Char(c) => {
            input.push(c);
            false
        }
        KeyCode::Backspace => {
            input.pop();
            false
        }
        _ => false,
    }
}

fn apply_event(
    evt: AgentEvent,
    log: &mut Vec<Line<'static>>,
    status: &mut String,
    cache_rate: &mut f64,
    step: &mut usize,
    running: &mut Option<String>,
) {
    match evt {
        AgentEvent::Delta(t) => log.push(Line::from(Span::raw(t))),
        AgentEvent::ReasoningDelta(t) => log.push(Line::from(Span::styled(t, Style::default().fg(Color::DarkGray)))),
        AgentEvent::ToolStart { name, .. } => {
            let s = format!("⚙ {}", name);
            log.push(Line::from(Span::styled(s, Style::default().fg(Color::Yellow))));
        }
        AgentEvent::ToolEnd { name, output_preview } => {
            let s = format!("⚙ {} → {}", name, output_preview.replace('\n', " | "));
            log.push(Line::from(Span::styled(s.chars().take(200).collect::<String>(), Style::default().fg(Color::Yellow))));
        }
        AgentEvent::Acceptance { passed, detail } => {
            if passed {
                log.push(Line::from(Span::styled("acceptance PASSED", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))));
            } else {
                log.push(Line::from(Span::styled("acceptance FAILED", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))));
                log.push(Line::from(Span::styled(detail, Style::default().fg(Color::Red))));
            }
        }
        AgentEvent::Status { metrics } => {
            *step = metrics.step;
            *cache_rate = metrics.cache_hit_rate;
        }
        AgentEvent::Done { reason } => {
            let s = format!("— done: {}", reason);
            log.push(Line::from(Span::styled(s, Style::default().fg(Color::Green))));
            *running = None;
            *status = String::from("ready");
        }
        AgentEvent::Error { message, retry_after_secs } => {
            let s = format!("! error, retry after {}s: {}", retry_after_secs, message);
            log.push(Line::from(Span::styled(s.chars().take(300).collect::<String>(), Style::default().fg(Color::Red))));
            *status = format!("retrying in {}s", retry_after_secs);
        }
    }
}

fn draw_ui(
    f: &mut Frame,
    input: &str,
    log: &[Line<'static>],
    status: &str,
    cache_rate: f64,
    step: usize,
    running: Option<&str>,
) {
    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(3), Constraint::Length(1)]).split(f.area());

    let log_block = Block::default().borders(Borders::ALL).title(" nexus - multimodal agent (Esc to quit) ");
    let para = Paragraph::new(log.to_vec()).block(log_block).wrap(Wrap { trim: false });
    f.render_widget(para, chunks[0]);

    let title = match running {
        Some(_) => " running... (wait for current task) ",
        None => " input (Enter to run) ",
    };
    let input_block = Block::default().borders(Borders::ALL).title(title);
    let input_para = Paragraph::new(input.to_string()).block(input_block);
    f.render_widget(input_para, chunks[1]);

    let status_text = format!(" step {} | cache hit {:.0}% | {}", step, cache_rate * 100.0, status);
    f.render_widget(Paragraph::new(Line::from(status_text)), chunks[2]);
}
