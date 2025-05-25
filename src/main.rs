use anyhow::Result;
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::{FutureExt as _, StreamExt as _};
use hubl::github::{ContentClient, Github, SearchItem};
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
use std::collections::HashMap;
use std::{collections::hash_map::Entry, sync::Arc};
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
    content_cache: HashMap<String, String>, // url->content
    nucleo: Nucleo<SearchItem>,
    nucleo_rx: Receiver<()>,
    pattern: String,
    cursor_pos: usize,
}

impl App {
    pub async fn new(github: Github, query: &str) -> Self {
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
            content_cache: HashMap::new(),
            nucleo,
            nucleo_rx,
            pattern: String::new(),
            cursor_pos: 0,
        }
    }

    /// runs the application's main loop until the user quits
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.exit {
            self.nucleo.tick(10);
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events().await?;
        }
        Ok(())
    }

    fn selected_item(&self) -> Option<&SearchItem> {
        self.table_state
            .selected()
            .and_then(|idx| {
                self.nucleo
                    .snapshot()
                    .get_matched_item(idx.try_into().unwrap())
            })
            .map(|item| item.data)
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

        let idx = match self.table_state.selected() {
            Some(idx) => idx,
            None => {
                self.table_state.select(Some(0));
                0
            }
        };

        let content = snap
            .get_matched_item(idx.try_into().unwrap())
            .and_then(|item| self.content_cache.get(&item.data.url))
            .map(|s| s.as_str())
            .unwrap_or("");

        let preview = Paragraph::new(content).block(Block::bordered());
        frame.render_widget(preview, preview_area);
    }

    /// updates the application's state based on user input
    async fn handle_events(&mut self) -> Result<()> {
        tracing::trace!("Awaiting event");
        if let Some(item) = self.selected_item() {
            let url = item.url.clone();
            // First time selecting this item, insert a placeholder and request content
            if let Entry::Vacant(entry) = self.content_cache.entry(url.clone()) {
                tracing::debug!("Requesting content for {url}");
                entry.insert("<fetching...>".into());
                self.content_client.get_content(url).await?;
            }
        }
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
            Some((url, content)) = self.content_client.rx.recv() => {
                tracing::debug!("Handling file content event");
                self.process_content(url, content);
            }
            Some(()) = self.nucleo_rx.recv() => {
                tracing::debug!("Redrawing for nucleo update");
            }
        }
        Ok(())
    }

    fn process_content(&mut self, url: String, content: String) {
        tracing::debug!("Caching content for: {url}");
        self.content_cache.insert(url, content);
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
                tracing::debug!("Selecting previous");
                self.table_state.select_previous()
            }
            KeyCode::Char('n') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                tracing::debug!("Selecting next");
                self.table_state.select_next()
            }
            KeyCode::Char('w') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(idx) = self.pattern.rfind(char::is_whitespace) {
                    self.pattern.truncate(idx);
                    tracing::debug!("Truncated pattern to {}", self.pattern);
                } else {
                    self.pattern.clear();
                    tracing::debug!("Cleared pattern");
                    self.cursor_pos = 0;
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
                let Some(char) = self.pattern.pop() else {
                    return;
                };
                self.cursor_pos -= 1;
                tracing::debug!(
                    "Popped char from pattern: {char:?}, new pattern: {}",
                    self.pattern
                );
                self.nucleo.pattern.reparse(
                    0,
                    &self.pattern,
                    CaseMatching::Smart,
                    Normalization::Smart,
                    false,
                );
            }
            KeyCode::Char(c) => {
                self.pattern.push(c);
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
    let app_result = App::new(github, &cli.query).await.run(&mut terminal).await;
    ratatui::restore();
    app_result
}
