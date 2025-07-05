use super::input::LineInput;
use super::preview::MarkdownHighlighter;
use crate::github::issues::{self, Issue};
use crate::github::Github;
use crate::QueryArgs;
use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{FutureExt as _, StreamExt as _};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Style, Stylize},
    widgets::{Block, Paragraph, Row, Table, TableState},
    DefaultTerminal, Frame,
};
use tokio::sync::mpsc::{self, Receiver, Sender};

pub struct App {
    event_stream: EventStream,
    exit: bool,
    table_state: TableState,
    issues: Vec<Issue>,
    tx: Sender<u32>,
    rx: Receiver<Issue>,
    line_input: LineInput,
    highlighter: MarkdownHighlighter,
}

impl App {
    pub fn new(github: Github, cli: QueryArgs) -> Result<Self> {
        let (req_tx, req_rx) = mpsc::channel(16);
        let (resp_tx, resp_rx) = mpsc::channel(16);
        issues::search_issues(github.clone(), &cli.to_query(), req_rx, resp_tx);

        Ok(Self {
            event_stream: EventStream::default(),
            exit: false,
            table_state: TableState::default().with_selected(Some(0)),
            line_input: LineInput::default(),
            highlighter: MarkdownHighlighter::default(),
            issues: Vec::new(),
            tx: req_tx,
            rx: resp_rx,
        })
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        self.tx.send(32).await?; // TODO: pick size based on visible rows
        while !self.exit {
            terminal.draw(|frame| self.draw(frame).unwrap())?;
            self.handle_events().await?;
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) -> Result<()> {
        tracing::debug!("Drawing");
        let [search_area, preview_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(frame.area());

        frame.render_widget(Block::bordered(), search_area);

        let [input_area, search_area] = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Length(2), Constraint::Fill(1)])
            .margin(1) // to account for the border we draw around everything
            .areas(search_area);

        self.line_input.draw(frame, input_area);

        if self.issues.is_empty() {
            return Ok(());
        }

        let table = Table::new(
            self.issues
                .iter()
                .map(|i| Row::new(vec![i.number.to_string(), i.title.clone()])),
            &[Constraint::Max(8), Constraint::Fill(1)],
        )
        .row_highlight_style(Style::new().italic())
        .highlight_symbol(">");
        frame.render_stateful_widget(table, search_area, &mut self.table_state);

        let idx = match self.table_state.selected() {
            Some(idx) => idx,
            None => {
                self.table_state.select(Some(0));
                0
            }
        };

        let item = &self.issues[idx];

        let preview = Paragraph::new(self.highlighter.highlight(item.body.as_str())?)
            .block(Block::bordered());
        frame.render_widget(preview, preview_area);
        Ok(())
    }

    /// updates the application's state based on user input
    async fn handle_events(&mut self) -> Result<()> {
        tracing::trace!("Awaiting event");

        tokio::select! {
            event = self.event_stream.next().fuse() => {
                tracing::debug!("Handling terminal event");
                let event = event.context("Event stream closed")??;
                match event {
                    Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event).await?
                    }
                    _ => {}
                };
            },
            Some(issue) = self.rx.recv() => {
                tracing::debug!("Pushing issue into list");
                self.issues.push(issue);
            }
        }
        Ok(())
    }

    async fn handle_key_event(&mut self, key_event: KeyEvent) -> Result<()> {
        match key_event.code {
            KeyCode::Esc => {
                tracing::debug!("Exit requested");
                self.exit = true;
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                tracing::debug!("Exit requested");
                self.exit = true;
            }
            KeyCode::Char('k') => {
                tracing::debug!("Selecting previous");
                self.table_state.select_previous()
            }
            KeyCode::Char('j') => {
                if self
                    .table_state
                    .selected()
                    .is_some_and(|i| i >= self.issues.len() - 1)
                {
                    tracing::debug!("Requesting more items");
                    self.tx.send(32).await?; // TODO: request size based on visible rows
                } else {
                    tracing::debug!("Selecting next");
                    self.table_state.select_next()
                }
            }
            _ => {}
        }
        Ok(())
    }
}
