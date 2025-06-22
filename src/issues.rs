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
    layout::{Constraint, Direction, Layout, Position},
    style::{Style, Stylize},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState},
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
    pattern: String,
    cursor_pos: usize,
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
            &cli.query,
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
            pattern: String::new(),
            cursor_pos: 0,
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

        let input =
            Paragraph::new(self.pattern.as_str()).block(Block::new().borders(Borders::BOTTOM));
        frame.render_widget(input, input_area);
        frame.set_cursor_position(Position::new(
            input_area.x + self.cursor_pos as u16,
            input_area.y,
        ));

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
            KeyCode::Left => {
                tracing::debug!("Moving cursor left");
                self.cursor_pos = self.cursor_pos.saturating_sub(1)
            }
            KeyCode::Right => {
                tracing::debug!("Moving cursor right");
                self.cursor_pos = (self.cursor_pos + 1).min(self.pattern.len())
            }
            KeyCode::Char('w') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                tracing::debug!(
                    "Deleting word from '{}' at {}",
                    self.pattern,
                    self.cursor_pos
                );
                let (s, rest) = self.pattern.split_at(self.cursor_pos);
                if let Some(idx) = s.trim_end().rfind(char::is_whitespace) {
                    self.cursor_pos = idx + 1;
                    self.pattern = s[0..=idx].to_owned() + rest;
                    tracing::debug!("Truncated pattern to {}", self.pattern);
                } else {
                    self.pattern = rest.into();
                    self.cursor_pos = 0;
                    tracing::debug!("Cleared pattern");
                }
                self.nucleo.pattern.reparse(
                    0,
                    &self.pattern,
                    CaseMatching::Smart,
                    Normalization::Smart,
                    false,
                );
            }
            KeyCode::Backspace => {
                if self.cursor_pos == 0 {
                    return;
                };
                self.cursor_pos -= 1;
                let c = self.pattern.remove(self.cursor_pos);
                tracing::debug!("Removed '{c}' from pattern, new pattern: {}", self.pattern);
                self.nucleo.pattern.reparse(
                    0,
                    &self.pattern,
                    CaseMatching::Smart,
                    Normalization::Smart,
                    false,
                );
            }
            KeyCode::Char(c) => {
                self.pattern.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
                tracing::debug!("Updated filter pattern: {}", self.pattern);
                self.nucleo.pattern.reparse(
                    0,
                    &self.pattern,
                    CaseMatching::Smart,
                    Normalization::Smart,
                    true,
                );
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key_event(KeyCode::Char(c).into());
        }
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_input() {
        let github = Github {
            host: "".to_string(),
            token: "".to_string(),
        };
        let mut app = App::new(github, QueryArgs::default()).unwrap();

        assert_eq!(app.pattern, "");
        assert_eq!(app.cursor_pos, 0);

        input(&mut app, "abc");
        assert_eq!(app.pattern, "abc");
        assert_eq!(app.cursor_pos, 3);

        app.handle_key_event(KeyCode::Backspace.into());
        assert_eq!(app.pattern, "ab");
        assert_eq!(app.cursor_pos, 2);

        app.handle_key_event(KeyCode::Backspace.into());
        assert_eq!(app.pattern, "a");
        assert_eq!(app.cursor_pos, 1);

        app.handle_key_event(KeyCode::Backspace.into());
        assert_eq!(app.pattern, "");
        assert_eq!(app.cursor_pos, 0);

        app.handle_key_event(KeyCode::Backspace.into());
        assert_eq!(app.pattern, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_delete_word() {
        let github = Github {
            host: "".to_string(),
            token: "".to_string(),
        };
        let mut app = App::new(github, QueryArgs::default()).unwrap();

        input(&mut app, "abc def ghi");
        assert_eq!(app.pattern, "abc def ghi");
        assert_eq!(app.cursor_pos, 11);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "abc def ");
        assert_eq!(app.cursor_pos, 8);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "abc ");
        assert_eq!(app.cursor_pos, 4);

        input(&mut app, "    ");
        assert_eq!(app.pattern, "abc     ");
        assert_eq!(app.cursor_pos, 8);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "");
        assert_eq!(app.cursor_pos, 0);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "");
        assert_eq!(app.cursor_pos, 0);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_cursor_movement() {
        let github = Github {
            host: "".to_string(),
            token: "".to_string(),
        };
        let mut app = App::new(github, QueryArgs::default()).unwrap();

        input(&mut app, "abc def ghi");
        assert_eq!(app.pattern, "abc def ghi");
        assert_eq!(app.cursor_pos, 11);

        app.handle_key_event(KeyCode::Left.into());
        assert_eq!(app.cursor_pos, 10);

        for _ in 0..4 {
            app.handle_key_event(KeyCode::Left.into());
        }
        assert_eq!(app.cursor_pos, 6);

        for _ in 0..8 {
            app.handle_key_event(KeyCode::Left.into());
        }
        assert_eq!(app.cursor_pos, 0);

        for _ in 0..8 {
            app.handle_key_event(KeyCode::Right.into());
        }
        assert_eq!(app.cursor_pos, 8);

        for _ in 0..8 {
            app.handle_key_event(KeyCode::Right.into());
        }
        assert_eq!(app.cursor_pos, 11);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_cursor_input() {
        let github = Github {
            host: "".to_string(),
            token: "".to_string(),
        };
        let mut app = App::new(github, QueryArgs::default()).unwrap();

        input(&mut app, "abc def ghi");
        assert_eq!(app.pattern, "abc def ghi");
        assert_eq!(app.cursor_pos, 11);

        for _ in 0..4 {
            app.handle_key_event(KeyCode::Left.into());
        }
        assert_eq!(app.cursor_pos, 7);

        input(&mut app, "bar");
        assert_eq!(app.pattern, "abc defbar ghi");
        assert_eq!(app.cursor_pos, 10);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, "abc  ghi");
        assert_eq!(app.cursor_pos, 4);

        app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.pattern, " ghi");
        assert_eq!(app.cursor_pos, 0);
    }
}
