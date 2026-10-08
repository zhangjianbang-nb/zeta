//! Zeta TUI v3 — opencode 风格极简界面。

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

const ACCENT: Color = Color::Rgb(140, 235, 150);
const WARN: Color = Color::Rgb(190, 160, 110);
const FAIL: Color = Color::Rgb(235, 120, 125);
const MUTED: Color = Color::Rgb(105, 112, 122);
const FAINT: Color = Color::Rgb(70, 76, 84);
const PROMPT: char = '\u{276f}';
const DOT: char = '\u{2b1a}';

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
    tool_line: Option<usize>,
}

impl UiState {
    fn push(&mut self, line: Line<'static>) {
        self.log.push(line);
        if self.log.len() > 3000 {
            self.log.drain(..1000);
        }
        if self.auto_scroll {
            self.scroll = 0;
        }
    }

    fn blank(&mut self) {
        self.push(Line::from(Span::raw("")));
    }

    fn tool_start(&mut self, name: &str, args: &str) {
        let brief: String = args.replace('\n', " ").chars().take(60).collect();
        self.push(Line::from(Span::styled(
            format!("{} {} {}", DOT, name, brief),
            Style::default().fg(WARN),
        )));
        self.tool_line = Some(self.log.len() - 1);
    }

    fn tool_end(&mut self, name: &str, preview: &str) {
        let prev: String = preview.replace('\n', " ").chars().take(90).collect();
        let line = Line::from(Span::styled(
            format!("{} {} \u{00b7} {}", DOT, name, prev),
            Style::default().fg(WARN),
        ));
        match self.tool_line.take() {
            Some(i) if i < self.log.len() => self.log[i] = line,
            _ => self.push(line),
        }
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

    st.push(Line::from(Span::styled(
        format!("{} {} @ {}", PROMPT, cfg.provider.model, cfg.provider.base_url),
        Style::default().fg(FAINT),
    )));

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
            st.push(Line::from(Span::styled("cancelled", Style::default().fg(FAIL))));
            st.blank();
        }

        if submitted && !st.running {
            let prompt = st.input.trim().to_string();
            if !prompt.is_empty() {
                st.input.clear();
                st.push(Line::from(Span::styled(
                    format!("{} {}", PROMPT, prompt),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )));
                st.running = true;
                st.status = "working".into();
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
    if let Some(Ok(Event::Key(k))) = maybe {
        if k.kind != KeyEventKind::Press {
            return;
        }
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            *quit = true;
        } else if k.code == KeyCode::Esc && st.running {
            st.cancel = true;
        } else if k.code == KeyCode::Up {
            st.auto_scroll = false;
            st.scroll = st.scroll.saturating_add(4);
        } else if k.code == KeyCode::Down {
            st.scroll = st.scroll.saturating_sub(4);
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
        r = run => match r {
            Ok(_) => Ok(()),
            Err(e) => {
                let _ = tx.send(AgentEvent::Error { message: format!("{:#}", e), retry_after_secs: 0 });
                let _ = tx.send(AgentEvent::Done { reason: "failed".into() });
                Err(e)
            }
        },
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
        AgentEvent::ReasoningDelta(_) => {}
        AgentEvent::ToolStart { name, args } => st.tool_start(&name, &args),
        AgentEvent::ToolEnd { name, output_preview } => st.tool_end(&name, &output_preview),
        AgentEvent::Acceptance { passed, detail } => {
            if passed {
                st.push(Line::from(Span::styled(
                    "\u{2713} acceptance passed (evidence replayed, exit 0)",
                    Style::default().fg(ACCENT),
                )));
            } else {
                let d: String = detail.replace('\n', " ").chars().take(140).collect();
                st.push(Line::from(Span::styled(
                    format!("\u{2717} acceptance failed - {}", d),
                    Style::default().fg(FAIL),
                )));
            }
        }
        AgentEvent::Status { metrics } => st.metrics = metrics,
        AgentEvent::Done { reason } => {
            st.tool_line = None;
            if reason != "no more tool calls" {
                let c = if reason == "cancelled" { FAIL } else { ACCENT };
                st.push(Line::from(Span::styled(reason, Style::default().fg(c))));
            }
            st.blank();
            st.running = false;
            st.status = "ready".into();
        }
        AgentEvent::Error { message, retry_after_secs } => {
            let m: String = message.replace('\n', " ").chars().take(150).collect();
            st.push(Line::from(Span::styled(
                format!("error, retry {}s: {}", retry_after_secs, m),
                Style::default().fg(FAIL),
            )));
        }
    }
}

fn draw(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, st: &UiState) -> Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let rows = Layout::vertical([Constraint::Min(4), Constraint::Length(3), Constraint::Length(1)]).split(area);

        let visible = rows[0].height as usize;
        let total = st.log.len();
        let end = total.saturating_sub(st.scroll as usize);
        let start = end.saturating_sub(visible);
        let mut lines: Vec<Line> = if start < end { st.log[start..end].to_vec() } else { vec![] };
        for l in lines.iter_mut() {
            l.spans.insert(0, Span::raw("  "));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[0]);

        let (label, accent) = if st.running {
            (" working - esc to cancel ", WARN)
        } else {
            (" ", ACCENT)
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if st.running { FAINT } else { accent }))
            .title(Span::styled(label, Style::default().fg(FAINT)));
        let body = format!("{} {}", PROMPT, st.input);
        f.render_widget(Paragraph::new(body).block(block), rows[1]);

        let m = &st.metrics;
        let left = format!(
            " {} | cache {:.0}% | {:.0} tok/s | {} memories",
            st.status, m.cache_hit_rate * 100.0, m.tok_per_sec, m.memories
        );
        let right = " enter submit \u{00b7} esc cancel \u{00b7} ^c quit ";
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(left, Style::default().fg(MUTED)),
                Span::styled(right, Style::default().fg(FAINT)),
            ])),
            rows[2],
        );
    })?;
    Ok(())
}
