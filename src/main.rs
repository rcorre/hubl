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
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState},
    DefaultTerminal, Frame,
};
use std::collections::HashMap;
use std::{io::Cursor, sync::Arc};
use syntect::{
    easy::HighlightLines,
    highlighting::{self, Theme, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};
use tokio::sync::mpsc::{self, Receiver};
use tracing_error::ErrorLayer;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _, Layer as _};

const ANSI_THEME: &[u8] = include_bytes!("ansi.tmTheme");

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
    content_cache: HashMap<String, Text<'static>>, // url->content
    nucleo: Nucleo<SearchItem>,
    nucleo_rx: Receiver<()>,
    pattern: String,
    cursor_pos: usize,

    syntax: SyntaxSet,
    theme: Theme,
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

        let mut theme_cursor = Cursor::new(ANSI_THEME);
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
            syntax: SyntaxSet::load_defaults_newlines(),
            theme: ThemeSet::load_from_reader(&mut theme_cursor).expect("Loading theme"),
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
            .and_then(|item| self.content_cache.get(&item.data.url))
            .cloned()
            .unwrap_or_default();

        let preview = Paragraph::new(text).block(Block::bordered());
        frame.render_widget(preview, preview_area);
    }

    /// updates the application's state based on user input
    async fn handle_events(&mut self) -> Result<()> {
        tracing::trace!("Awaiting event");
        // TODO: eliminate clone here
        if let Some(item) = self.selected_item().cloned() {
            if !self.content_cache.contains_key(&item.url) {
                // First time selecting this item, insert a placeholder and request content
                tracing::debug!("Requesting content for {}", item.path);
                self.content_cache
                    .insert(item.url.clone(), "<fetching...>".into());
                self.content_client.get_content(item.clone()).await?;
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
            Some((item, content)) = self.content_client.rx.recv() => {
                tracing::debug!("Handling file content event");
                self.process_content(item, content);
            }
            Some(()) = self.nucleo_rx.recv() => {
                tracing::debug!("Redrawing for nucleo update");
            }
        }
        Ok(())
    }

    fn to_ansi_color(color: highlighting::Color) -> Option<ratatui::style::Color> {
        if color.a == 0 {
            // Themes can specify one of the user-configurable terminal colors by
            // encoding them as #RRGGBBAA with AA set to 00 (transparent) and RR set
            // to the 8-bit color palette number. The built-in themes ansi, base16,
            // and base16-256 use this.
            Some(match color.r {
                // For the first 8 colors, use the Color enum to produce ANSI escape
                // sequences using codes 30-37 (foreground) and 40-47 (background).
                // For example, red foreground is \x1b[31m. This works on terminals
                // without 256-color support.
                0x00 => ratatui::style::Color::Black,
                0x01 => ratatui::style::Color::Red,
                0x02 => ratatui::style::Color::Green,
                0x03 => ratatui::style::Color::Yellow,
                0x04 => ratatui::style::Color::Blue,
                0x05 => ratatui::style::Color::Magenta,
                0x06 => ratatui::style::Color::Cyan,
                0x07 => ratatui::style::Color::White,
                // For all other colors, use Fixed to produce escape sequences using
                // codes 38;5 (foreground) and 48;5 (background). For example,
                // bright red foreground is \x1b[38;5;9m. This only works on
                // terminals with 256-color support.
                //
                // TODO: When ansi_term adds support for bright variants using codes
                // 90-97 (foreground) and 100-107 (background), we should use those
                // for values 0x08 to 0x0f and only use Fixed for 0x10 to 0xff.
                n => ratatui::style::Color::Indexed(n),
            })
        } else if color.a == 1 {
            // Themes can specify the terminal's default foreground/background color
            // (i.e. no escape sequence) using the encoding #RRGGBBAA with AA set to
            // 01. The built-in theme ansi uses this.
            None
        } else {
            Some(ratatui::style::Color::Rgb(color.r, color.g, color.b))
        }
    }

    // Convert syntect highlighting to ANSI terminal colors
    // See https://github.com/trishume/syntect/issues/309
    // Borrowed from https://github.com/sxyazi/yazi/pull/460/files
    fn to_line_widget(regions: Vec<(highlighting::Style, &str)>) -> Line<'static> {
        let mut line = Line::default();
        for (style, s) in regions {
            let mut modifier = ratatui::style::Modifier::empty();
            if style.font_style.contains(highlighting::FontStyle::BOLD) {
                modifier |= ratatui::style::Modifier::BOLD;
            }
            if style.font_style.contains(highlighting::FontStyle::ITALIC) {
                modifier |= ratatui::style::Modifier::ITALIC;
            }
            if style
                .font_style
                .contains(highlighting::FontStyle::UNDERLINE)
            {
                modifier |= ratatui::style::Modifier::UNDERLINED;
            }

            line.push_span(Span {
                content: s.to_string().into(),
                style: ratatui::style::Style {
                    fg: Self::to_ansi_color(style.foreground),
                    // bg: Self::to_ansi_color(style.background),
                    add_modifier: modifier,
                    ..Default::default()
                },
            })
        }

        line
    }

    fn process_content(&mut self, item: SearchItem, content: String) {
        tracing::debug!("Caching content for: {}", item.url);
        let syntax = std::path::Path::new(&item.path)
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| self.syntax.find_syntax_by_extension(ext))
            .or_else(|| {
                content
                    .lines()
                    .next()
                    .and_then(|line| self.syntax.find_syntax_by_first_line(line))
            })
            .unwrap_or_else(|| self.syntax.find_syntax_plain_text());
        let mut h = HighlightLines::new(syntax, &self.theme);

        let mut text = Text::default();
        for line in LinesWithEndings::from(&content) {
            let ranges = h.highlight_line(line, &self.syntax).unwrap();
            text.push_line(Self::to_line_widget(ranges))
        }
        self.content_cache.insert(item.url, text);
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
