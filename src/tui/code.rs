use crate::QueryArgs;
use crate::{
    github::code,
    github::code::{ContentClient, SearchItem},
    github::Github,
    tui::preview::PreviewCache,
};
use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{FutureExt as _, StreamExt as _};
use nucleo::{
    pattern::{CaseMatching, Normalization},
    Nucleo,
};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Style, Stylize},
    widgets::{Block, Paragraph, Row, Table, TableState},
    DefaultTerminal, Frame,
};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc::{self, Receiver};

use super::input::LineInput;

pub struct App {
    event_stream: EventStream,
    exit: bool,
    table_state: TableState,
    content_client: ContentClient,
    preview_cache: PreviewCache,
    nucleo: Nucleo<SearchItem>,
    nucleo_rx: Receiver<()>,
    line_input: LineInput,

    // When an item is selected, this is set to now+<small_timeout>.
    // If this elapses before selecting a new item, we will request a preview.
    // This debounces preview requests when quickly scrolling.
    preview_deadline: Option<Instant>,
}

impl App {
    pub fn new(github: Github, cli: QueryArgs) -> Result<Self> {
        let (nucleo_tx, nucleo_rx) = mpsc::channel(1);
        let nucleo = Nucleo::new(
            nucleo::Config::DEFAULT,
            Arc::new(move || {
                // if there's already a value in the channel, we've already got a pending redraw
                let _ = nucleo_tx.try_send(());
            }),
            None,
            1,
        );
        let injector = nucleo.injector();
        code::search_code(
            github.clone(),
            &cli.query,
            cli.pages,
            Arc::new(move |result| {
                injector.push(result, |item, columns| {
                    columns[0] = format!("{} {}", item.path, item.repository.full_name).into()
                });
            }),
        );

        Ok(Self {
            event_stream: EventStream::default(),
            exit: false,
            table_state: TableState::default().with_selected(Some(0)),
            content_client: ContentClient::new(github),
            preview_cache: PreviewCache::new(),
            nucleo,
            nucleo_rx,
            line_input: LineInput::default(),
            preview_deadline: None,
        })
    }

    /// runs the application's main loop until the user quits
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.nucleo.tick(10);
            self.handle_events().await?;
        }
        Ok(())
    }

    async fn maybe_request_preview(&mut self) -> Result<()> {
        let snap = self.nucleo.snapshot();
        let Some(item) = self
            .table_state
            .selected()
            .and_then(|idx| snap.get_matched_item(idx.try_into().unwrap()))
            .map(|item| item.data)
        else {
            tracing::trace!("No item matched for preview");
            return Ok(());
        };

        if self.preview_cache.contains(&item.url) {
            tracing::trace!("Item preview already cached: {}", item.url);
            return Ok(());
        }

        // First time selecting this item, insert a placeholder and request content
        tracing::debug!("Requesting preview for: {}", item.url);
        self.preview_cache.insert_placeholder(item.url.clone());
        self.content_client.get_content(item.clone()).await
    }

    fn start_preview_timer(&mut self) {
        // TODO: only start if we need a new preview, to avoid extra redraws
        tracing::trace!("Starting preview timer");
        self.preview_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_millis(100));
    }

    fn draw(&mut self, frame: &mut Frame) {
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

        let snap = self.nucleo.snapshot();
        if snap.matched_item_count() > 0 {
            let table = Table::new(
                snap.matched_items(0..snap.matched_item_count())
                    .map(|item| {
                        Row::new(vec![
                            item.data.repository.full_name.as_str(),
                            item.data.path.as_str(),
                        ])
                    }),
                &[Constraint::Max(32), Constraint::Fill(1)],
            )
            .row_highlight_style(Style::new().italic())
            .highlight_symbol(">");
            frame.render_stateful_widget(table, search_area, &mut self.table_state);
        }

        let idx = match self.table_state.selected() {
            Some(idx) => idx,
            None => {
                self.table_state.select(Some(0));
                0
            }
        };

        let preview_fragments = snap
            .get_matched_item(idx.try_into().unwrap())
            .and_then(|item| self.preview_cache.get(&item.data.url))
            .cloned()
            .unwrap_or_default();

        let preview_areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                preview_fragments
                    .iter()
                    .take(3) // TODO: split into as many as can fit the space
                    .map(|frag| Constraint::Length(frag.lines.len() as u16)),
            )
            .split(preview_area);

        for (area, frag) in preview_areas.iter().zip(preview_fragments) {
            let preview = Paragraph::new(frag).block(Block::bordered());
            frame.render_widget(preview, *area);
        }
    }

    /// updates the application's state based on user input
    async fn handle_events(&mut self) -> Result<()> {
        tracing::trace!("Awaiting event");

        let await_preview = async {
            self.preview_deadline
                .map(|when| tokio::time::sleep_until(when.into()))
        };

        tokio::select! {
            event = self.event_stream.next().fuse() => {
                tracing::debug!("Handling terminal event");
                let event = event.context("Event stream closed")??;
                match event {
                    Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event)
                    }
                    _ => {}
                };
            },
            Some((item, content)) = self.content_client.recv_content() => {
                tracing::debug!("Handling file content event");
                self.process_content(item, content)?;
            }
            Some(()) = self.nucleo_rx.recv() => {
                tracing::debug!("Redrawing for nucleo update");
            }
            Some(_) = await_preview => {
                tracing::trace!("Preview timer elapsed");
                self.preview_deadline = None;
                self.maybe_request_preview().await?;
            }
        }
        Ok(())
    }

    fn process_content(&mut self, item: SearchItem, content: String) -> Result<()> {
        self.preview_cache.insert(item, &content)
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match self.line_input.handle_key_event(key_event) {
            super::input::InputResult::Unhandled => {}
            super::input::InputResult::Handled => return,
            super::input::InputResult::PatternChanged => {
                self.nucleo.pattern.reparse(
                    0,
                    self.line_input.pattern(),
                    CaseMatching::Smart,
                    Normalization::Smart,
                    true,
                );
            }
        }
        match key_event.code {
            KeyCode::Esc => {
                tracing::debug!("Exit requested");
                self.exit = true;
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                tracing::debug!("Exit requested");
                self.exit = true;
            }
            KeyCode::Char('p') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.table_state.selected().unwrap_or_default() == 0 {
                    tracing::debug!("Selecting last");
                    self.table_state.select_last();
                } else {
                    tracing::debug!("Selecting previous");
                    self.table_state.select_previous()
                }
            }
            KeyCode::Char('n') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                tracing::debug!("Selecting next");
                self.start_preview_timer();
                self.table_state.select_next()
            }
            _ => {}
        }
    }
}
