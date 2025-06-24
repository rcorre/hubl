use super::input::LineInput;
use crate::github::issues::{self, Issue};
use crate::github::Github;
use crate::QueryArgs;
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
use tokio::sync::mpsc::{self, Receiver};

pub struct App {
    event_stream: EventStream,
    exit: bool,
    table_state: TableState,
    nucleo: Nucleo<Issue>,
    nucleo_rx: Receiver<()>,
    line_input: LineInput,
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
        issues::search_issues(
            github.clone(),
            &cli.to_query(),
            cli.pages,
            Arc::new(move |result| {
                injector.push(result, |item, columns| {
                    columns[0] = item.title.clone().into();
                });
            }),
        );

        Ok(Self {
            event_stream: EventStream::default(),
            exit: false,
            table_state: TableState::default().with_selected(Some(0)),
            nucleo,
            nucleo_rx,
            line_input: LineInput::default(),
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
                            // item.data.number.to_string().as_str(),
                            item.data.title.as_str(),
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

        let Some(item) = snap.get_matched_item(idx as u32) else {
            return;
        };

        let preview = Paragraph::new(item.data.body.as_str()).block(Block::bordered());
        frame.render_widget(preview, preview_area);
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
                        self.handle_key_event(key_event)
                    }
                    _ => {}
                };
            },
            Some(()) = self.nucleo_rx.recv() => {
                tracing::debug!("Redrawing for nucleo update");
            }
        }
        Ok(())
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
                self.table_state.select_next()
            }
            _ => {}
        }
    }
}
