//! Zeta TUI v4 — 照 opencode 构图：ASCII banner 居中 + 左竖线输入框 + 全指标状态栏。

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use nexus_agent::{Agent, AgentEvent};
use nexus_core::config::Config;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use tokio::sync::mpsc;

const ACCENT: Color = Color::Rgb(140, 235, 150);
const WARN: Color = Color::Rgb(190, 160, 110);
const FAIL: Color = Color::Rgb(235, 120, 125);
const MUTED: Color = Color::Rgb(105, 112, 122);
const FAINT: Color = Color::Rgb(70, 76, 84);
const BAR: Color = Color::Rgb(120, 130, 140);
const PROMPT: char = '\u{276f}';

const BANNER: &[&str] = &[
    "\u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2584} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580}",
    "\u{2588}  \u{2588} \u{2588}  \u{2588} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}  \u{2588} \u{2588}    \u{2588}  \u{2588} \u{2588}  \u{2588} \u{2588}\u{2580}\u{2580}",
    "\u{2588}\u{2580}\u{2580}\u{2588} \u{2588}\u{2580}\u{2580}\u{2588} \u{2588}    \u{2588}\u{2580}\u{2580}\u{2584} \u{2588}\u{2580}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580}\u{2580}",
];

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
    started: Option<std::time::Instant>,
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
        let brief: String = args.replace('\n', " ").chars().take(56).collect();
        self.push(Line::from(Span::styled(
            format!("\u{25b8} {} {}", name, brief),
            Style::default().fg(WARN),
        )));
        self.tool_line = Some(self.log.len() - 1);
    }
    fn tool_end(&mut self, name: &str, preview: &str) {
        let prev: String = preview.replace('\n', " ").chars().take(88).collect();
        let line = Line::from(Span::styled(
            format!("\u{25b8} {} \u{00b7} {}", name, prev),
            Style::default().fg(WARN),
        ));
        match self.tool_line.take() {
            Some(i) if i < self.log.len() => self.log[i] = line,
            _ => self.push(line),
        }
    }
    fn user(&mut self, s: &str) {
        self.blank();
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
        }

        if submitted && !st.running {
            let prompt = st.input.trim().to_string();
            if !prompt.is_empty() {
                st.input.clear();
                st.user(&prompt);
                st.running = true;
                st.status = "working".into();
                st.started = Some(std::time::Instant::now());
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

        draw(terminal, &st, &cfg)?;
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
                let d: String = detail.replace('\n', " ").chars().take(120).collect();
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
            st.running = false;
            st.status = "ready".into();
        }
        AgentEvent::Error { message, retry_after_secs } => {
            let m: String = message.replace('\n', " ").chars().take(120).collect();
            st.push(Line::from(Span::styled(
                format!("error, retry {}s: {}", retry_after_secs, m),
                Style::default().fg(FAIL),
            )));
        }
    }
}

fn draw(terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>, st: &UiState, cfg: &Config) -> Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let rows = Layout::vertical([
            Constraint::Min(3),    // log / banner
            Constraint::Length(4), // 输入（左竖线三行）
            Constraint::Length(2), // 键位提示 + 空隙
            Constraint::Length(1), // 状态栏
        ])
        .split(area);

        if st.log.is_empty() {
            // 空闲态：居中 banner + 模型行（opencode 同款构图）
            let mut lines: Vec<Line> = vec![];
            for b in BANNER {
                lines.push(Line::from(Span::styled(*b, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))));
            }
            lines.push(Line::from(Span::raw("")));
            lines.push(Line::from(Span::styled(
                format!("  zeta v{} \u{00b7} {} \u{00b7} rust harness", env!("CARGO_PKG_VERSION"), cfg.provider.model),
                Style::default().fg(FAINT),
            )));
            f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rows[0]);
        } else {
            let visible = rows[0].height as usize;
            let total = st.log.len();
            let end = total.saturating_sub(st.scroll as usize);
            let start = end.saturating_sub(visible);
            let lines: Vec<Line> = if start < end { st.log[start..end].to_vec() } else { vec![] };
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), rows[0]);
        }

        // 输入区：左竖线 ┃ + 内容 + 底部 ╹▀▀▀（opencode 同款）
        let vbar = Span::styled("\u{2503} ", Style::default().fg(BAR));
        let mut input_lines: Vec<Line> = vec![
            Line::from(Span::styled("\u{2503}", Style::default().fg(BAR))),
        ];
        if st.running {
            input_lines.push(Line::from(vec![
                vbar.clone(),
                Span::styled(" working... (esc to cancel)", Style::default().fg(WARN)),
            ]));
        } else {
            input_lines.push(Line::from(vec![
                vbar.clone(),
                Span::styled(" ", Style::default()),
                Span::styled(st.input.clone(), Style::default()),
            ]));
        }
        input_lines.push(Line::from(Span::styled(
            "\u{2579}\u{2580}\u{2580}".to_string() + &"\u{2580}".repeat(60),
            Style::default().fg(BAR),
        )));
        f.render_widget(Paragraph::new(input_lines), rows[1]);

        // 键位提示（输入框下，靠左缩进）
        let hint = if st.running {
            "  tab agents  esc cancel  ctrl+c quit"
        } else {
            "  enter submit  esc cancel  ctrl+c quit"
        };
        f.render_widget(Paragraph::new(Span::styled(hint, Style::default().fg(FAINT))), rows[2]);

        // 全指标状态栏
        let m = st.metrics;
        let secs = st.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let left = format!(
            " {} \u{00b7} cache {:.0}% \u{00b7} {:.0} tok/s \u{00b7} prompt {} tok \u{00b7} compacted {} \u{00b7} sub {} \u{00b7} cron {} \u{00b7} mem {} \u{00b7} err {} \u{00b7} temp {:.1} \u{00b7} ram {}/{} MB",
            st.status,
            m.cache_hit_rate * 100.0,
            m.tok_per_sec,
            m.prompt_chars / 2,
            m.compacted_tokens,
            m.subagents,
            m.scheduled,
            m.memories,
            m.errors,
            m.temperature,
            m.mem_used_mb,
            m.mem_total_mb,
        );
        let right = format!(" up {:02}:{:02}:{:02} ", secs / 3600, (secs % 3600) / 60, secs % 60);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(left, Style::default().fg(MUTED)),
                Span::styled(right, Style::default().fg(FAINT)),
            ])),
            rows[3],
        );
    })?;
    Ok(())
}
