//! Zeta TUI — opencode 风格界面（v2）。

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use nexus_agent::{Agent, AgentEvent};
use nexus_core::config::Config;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

const ACCENT: Color = Color::Rgb(126, 231, 135);
const WARN: Color = Color::Rgb(224, 175, 104);
const FAIL: Color = Color::Rgb(240, 113, 120);
const MUTED: Color = Color::Rgb(110, 118, 129);
const THINK: Color = Color::Rgb(90, 98, 110);
const PROMPT: char = '\u{276f}';

pub async fn run_tui(cfg: Config) -> Result<()> {
    let mut terminal = ratatui::init();
    terminal.clear()?;
    let res = tui_loop(cfg, &mut terminal).await;
    ratatui::restore();
    res
}

#[derive(Default)]
struct UiState {
    log: Vec<Line<'static>>,
    input: String,
    status: String,
    metrics: nexus_agent::Metrics,
    running: bool,
    scroll: u16,
    auto_scroll: bool,
    cancel: bool,
}

impl UiState {
    fn push(&mut self, line: Line<'static>) {
        self.log.push(line);
        if self.log.len() > 2000 {
            self.log.drain(..500);
        }
        if self.auto_scroll {
            self.scroll = 0;
        }
    }

    fn info(&mut self, s: String) {
        self.push(Line::from(Span::styled(s, Style::default().fg(MUTED))));
    }

    fn user(&mut self, s: &str) {
        self.push(Line::from(Span::styled(
            format!("{} {}", PROMPT, s),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )));
    }
}

enum TaskCtrl {
    Cancel,
}

async fn tui_loop(cfg: Config, terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    let mut st = UiState::default();
    let mut events_rx: Option<mpsc::UnboundedReceiver<AgentEvent>> = None;
    let mut ctrl_tx: Option<mpsc::UnboundedSender<TaskCtrl>> = None;
    let mut event_stream = EventStream::new();
    let mut quit = false;

    st.info(format!("zeta v{} | {} @ {}", env!("CARGO_PKG_VERSION"), cfg.provider.model, cfg.provider.base_url));

    while !quit {
        let mut submitted = false;

        if let Some(rx) = events_rx.as_mut() {
            tokio::select! {
                maybe = event_stream.next() => term_event(maybe, &mut st, &mut quit, &mut submitted),
                maybe = rx.recv() => match maybe {
                    Some(evt) => apply_event(evt, &mut st),
                    None => { events_rx = None; st.running = false; st.status = "ready".into(); }
                },
            }
        } else {
            tokio::select! {
                maybe = event_stream.next() => term_event(maybe, &mut st, &mut quit, &mut submitted),
            }
        }

        if st.cancel {
            st.cancel = false;
            if let Some(tx) = ctrl_tx.take() {
                let _ = tx.send(TaskCtrl::Cancel);
            }
            st.running = false;
            st.status = "cancelled".into();
            st.info("[cancelled by user]".to_string());
        }

        if submitted && !st.running {
            let prompt = st.input.trim().to_string();
            if !prompt.is_empty() {
                st.input.clear();
                st.user(&prompt);
                st.running = true;
                st.status = "running".into();
                let (etx, erx) = mpsc::unbounded_channel::<AgentEvent>();
                let (ctx, crx) = mpsc::unbounded_channel::<TaskCtrl>();
                events_rx = Some(erx);
                ctrl_tx = Some(ctx);
                let cfg2 = cfg.clone();
                tokio::spawn(async move {
                    if let Err(e) = spawn_and_forward(cfg2, prompt, etx, crx).await {
                        tracing::error!("task: {:#}", e);
                    }
                });
            }
        }

        draw(terminal, &st)?;
    }
    Ok(())
}

fn term_event(
    maybe: Option<Result<Event, std::io::Error>>,
    st: &mut UiState,
    quit: &mut bool,
    submitted: &mut bool,
) {
    match maybe {
        Some(Ok(Event::Key(k))) if k.kind == KeyEventKind::Press => {
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                *quit = true;
            } else if k.code == KeyCode::Esc && st.running {
                st.cancel = true;
            } else if k.code == KeyCode::Up {
                st.auto_scroll = false;
                st.scroll = st.scroll.saturating_add(3);
            } else if k.code == KeyCode::Down {
                st.scroll = st.scroll.saturating_sub(3);
                if st.scroll == 0 {
                    st.auto_scroll = true;
                }
            } else if k.code == KeyCode::Enter {
                *submitted = !st.running && !st.input.trim().is_empty();
            } else if k.code == KeyCode::Backspace {
                st.input.pop();
            } else if let KeyCode::Char(c) = k.code {
                st.input.push(c);
            }
        }
        Some(Ok(_)) => {}
        Some(Err(_)) => {}
        None => {}
    }
}

async fn spawn_and_forward(
    cfg: Config,
    prompt: String,
    tx: mpsc::UnboundedSender<AgentEvent>,
    mut ctrl: mpsc::UnboundedReceiver<TaskCtrl>,
) -> Result<()> {
    let agent_tx = tx.clone();
    let run = async {
        let mut agent = Agent::new(cfg, agent_tx).await?;
        agent.run_task(&prompt).await
    };
    tokio::select! {
        r = run => {
            match r {
                Ok(_) => { let _ = tx.send(AgentEvent::Done { reason: "task finished".into() }); Ok(()) }
                Err(e) => {
                    let _ = tx.send(AgentEvent::Error { message: format!("{:#}", e), retry_after_secs: 0 });
                    let _ = tx.send(AgentEvent::Done { reason: "task failed".into() });
                    Err(e)
                }
            }
        }
        _ = ctrl.recv() => {
            let _ = tx.send(AgentEvent::Done { reason: "cancelled".into() });
            Ok(())
        }
    }
}

fn apply_event(evt: AgentEvent, st: &mut UiState) {
    match evt {
        AgentEvent::Delta(t) => {
            for line in t.lines() {
                st.push(Line::from(Span::raw(line.to_string())));
            }
        }
        AgentEvent::ReasoningDelta(t) => {
            if !t.trim().is_empty() {
                st.push(Line::from(Span::styled(t, Style::default().fg(THINK))));
            }
        }
        AgentEvent::ToolStart { name, args } => {
            let brief: String = args.chars().take(72).collect();
            st.push(Line::from(Span::styled(
                format!("  + {} {}", name, brief),
                Style::default().fg(WARN),
            )));
        }
        AgentEvent::ToolEnd { name, output_preview } => {
            let prev: String = output_preview.replace('\n', " / ").chars().take(110).collect();
            st.push(Line::from(Span::styled(
                format!("  = {} {}", name, prev),
                Style::default().fg(WARN),
            )));
        }
        AgentEvent::Acceptance { passed, detail } => {
            if passed {
                st.push(Line::from(Span::styled(
                    "  [acceptance PASSED] evidence replayed, exit 0",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )));
            } else {
                st.push(Line::from(Span::styled(
                    "  [acceptance FAILED] fix and resubmit",
                    Style::default().fg(FAIL).add_modifier(Modifier::BOLD),
                )));
                st.push(Line::from(Span::styled(detail, Style::default().fg(FAIL))));
            }
        }
        AgentEvent::Status { metrics } => {
            st.metrics = metrics;
        }
        AgentEvent::Done { reason } => {
            st.push(Line::from(Span::styled(format!("  [done] {}", reason), Style::default().fg(ACCENT))));
            st.running = false;
            st.status = "ready".into();
        }
        AgentEvent::Error { message, retry_after_secs } => {
            let m: String = message.chars().take(180).collect();
            st.push(Line::from(Span::styled(
                format!("  [error, retry {}s] {}", retry_after_secs, m),
                Style::default().fg(FAIL),
            )));
            st.status = format!("retrying {}s", retry_after_secs);
        }
    }
}

fn draw(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, st: &UiState) -> Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

        let header = Line::from(vec![
            Span::styled(" zeta ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                format!(" {:.0} tok/s | cache {:.0}% | step {} | mem {} MB ", st.metrics.tok_per_sec, st.metrics.cache_hit_rate * 100.0, st.metrics.step, st.metrics.mem_used_mb),
                Style::default().fg(MUTED),
            ),
        ]);
        f.render_widget(Paragraph::new(header), chunks[0]);

        let visible = chunks[1].height.saturating_sub(2) as usize;
        let total = st.log.len();
        let end = total.saturating_sub(st.scroll as usize);
        let start = end.saturating_sub(visible);
        let lines: Vec<Line> = if start < end { st.log[start..end].to_vec() } else { vec![] };
        let log_block = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded);
        f.render_widget(Paragraph::new(lines).block(log_block).wrap(Wrap { trim: false }), chunks[1]);

        let (label, color) = if st.running {
            (" running - Esc to cancel ", WARN)
        } else {
            (" ask anything ", ACCENT)
        };
        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(color))
            .title(Span::styled(label, Style::default().fg(color)));
        let body = if st.running { st.input.clone() } else { format!("{} {}", PROMPT, st.input) };
        f.render_widget(Paragraph::new(body).block(input_block), chunks[2]);

        let left = format!(" {} | memories {} | err {}", st.status, st.metrics.memories, st.metrics.errors);
        let right = " Enter submit | Esc cancel | Ctrl+C quit ";
        let sb = Line::from(vec![
            Span::styled(left, Style::default().fg(MUTED)),
            Span::styled(right, Style::default().fg(THINK)),
        ]);
        f.render_widget(Paragraph::new(sb), chunks[3]);
    })?;
    Ok(())
}
