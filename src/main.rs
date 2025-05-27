use anyhow::Result;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{FutureExt as _, StreamExt as _};
use hubl::{
    github::{ContentClient, Github, SearchItem},
    preview::PreviewCache,
};
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
use std::time::Instant;
use tokio::sync::mpsc::{self, Receiver};
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

fn get_auth_token() -> Result<String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token"]);
    tracing::debug!("executing auth command: {cmd:?}");
    let output = cmd.output()?;
    Ok(core::str::from_utf8(&output.stdout)?.trim().to_string())
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Query to search.
    query: String,
}

pub struct App {
    event_stream: EventStream,
    exit: bool,
    table_state: TableState,
    content_client: ContentClient,
    preview_cache: PreviewCache,
    nucleo: Nucleo<SearchItem>,
    nucleo_rx: Receiver<()>,
    pattern: String,
    cursor_pos: usize,

    // When an item is selected, this is set to now+<small_timeout>.
    // If this elapses before selecting a new item, we will request a preview.
    // This debounces preview requests when quickly scrolling.
    preview_deadline: Option<Instant>,
}

impl App {
    pub fn new(github: Github, query: &str) -> Self {
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
        github.search_code(
            query,
            Arc::new(move |result| {
                injector.push(result, |item, columns| {
                    columns[0] = format!("{} {}", item.path, item.repository.full_name).into()
                });
            }),
        );

        Self {
            event_stream: EventStream::default(),
            exit: false,
            table_state: TableState::default().with_selected(Some(0)),
            content_client: ContentClient::new(github),
            preview_cache: PreviewCache::new(),
            nucleo,
            nucleo_rx,
            pattern: String::new(),
            cursor_pos: 0,
            preview_deadline: None,
        }
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

        let text = snap
            .get_matched_item(idx.try_into().unwrap())
            .and_then(|item| self.preview_cache.get(&item.data.url))
            .cloned()
            .unwrap_or_default();

        let preview = Paragraph::new(text).block(Block::bordered());
        frame.render_widget(preview, preview_area);
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
                let event = event.unwrap()?;
                match event {
                    Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event)
                    }
                    _ => {}
                };
            },
            Some((item, content)) = self.content_client.rx.recv() => {
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
        self.preview_cache.insert(item.url, item.path, &content)
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
                self.start_preview_timer();
                self.table_state.select_next()
            }
            KeyCode::Left => {
                tracing::debug!("Moving cursor left");
                self.start_preview_timer();
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

pub fn initialize_logging() -> Result<()> {
    let xdg_dirs = xdg::BaseDirectories::with_prefix(env!("CARGO_PKG_NAME"));
    let log_path = xdg_dirs.place_cache_file("log.txt")?;
    let log_file = std::fs::File::create(log_path)?;
    let file_subscriber = tracing_subscriber::fmt::layer()
        .with_file(true)
        .with_line_number(true)
        .with_writer(log_file)
        .with_target(false)
        .with_ansi(false)
        .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env());
    tracing_subscriber::registry()
        .with(file_subscriber)
        .with(ErrorLayer::default())
        .init();
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    initialize_logging()?;
    let cli = Cli::parse();
    let mut terminal = ratatui::init();
    crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::SetCursorStyle::BlinkingBar
    )?;
    let github = Github::new("https://api.github.com".to_string(), get_auth_token()?);
    let app_result = App::new(github, &cli.query).run(&mut terminal).await;
    ratatui::restore();
    app_result
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
        let github = Github::new("".to_string(), "".to_string());
        let mut app = App::new(github, "");

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
        let github = Github::new("".to_string(), "".to_string());
        let mut app = App::new(github, "");

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
        let github = Github::new("".to_string(), "".to_string());
        let mut app = App::new(github, "");

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
        let github = Github::new("".to_string(), "".to_string());
        let mut app = App::new(github, "");

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
