use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use futures::{FutureExt as _, StreamExt as _};
use hubl::github::{Github, SearchItem, SearchResponse};
use nucleo::Nucleo;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Style, Stylize},
    symbols::border,
    text::{Line, Text},
    widgets::{Block, List, ListDirection, ListState, Paragraph, StatefulWidget, Widget},
    DefaultTerminal, Frame,
};
use serde::Deserialize;
use std::io;
use std::sync::Arc;
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

// #[tokio::main]
// async fn main() -> Result<()> {

//     // let cli = Cli::parse();

//     // let token = get_auth_token()?;

//     // let client = reqwest::Client::new();
//     // let req = client
//     //     .request(reqwest::Method::GET, "https://api.github.com/search/code")
//     //     .query(&[("q", &cli.query)])
//     //     .bearer_auth(token)
//     //     .header(
//     //         reqwest::header::ACCEPT,
//     //         "application/vnd.github.v3.text-match+json",
//     //     )
//     //     .header(reqwest::header::USER_AGENT, env!("CARGO_PKG_NAME"))
//     //     .build()?;
//     // log::debug!("sending request: {req:?}");
//     // let resp: SearchResponse = client.execute(req).await?.json().await?;

//     // log::trace!("response: {resp:?}");

//     let mut nucleo = Nucleo::new(nucleo::Config::DEFAULT, Arc::new(redraw), None, 1);

//     let injector = nucleo.injector();
//     injector.push("foo", |item, columns| {});
//     nucleo.tick(10);
//     let snap = nucleo.snapshot();
//     for item in snap.matched_items(0..snap.matched_item_count()) {
//         eprintln!("{}", item.data);
//     }

//     // let input = "aaaaa\nbbbb\nccc".to_string();

//     // for item in selected_items.iter() {
//     //     println!("{}", item.output());
//     // }
//     Ok(())
// }

pub struct App {
    event_stream: EventStream,
    exit: bool,
    search_response: SearchResponse,
    list_state: ListState,
    search_recv: Receiver<SearchResponse>,
}

impl App {
    pub async fn new(github: Github) -> Self {
        Self {
            // search_response: github.search_code("foo").await.unwrap(),
            search_response: SearchResponse { items: vec![] },
            search_recv: github.search_code("foo"),
            event_stream: EventStream::default(),
            exit: false,
            list_state: ListState::default().with_selected(Some(0)),
        }
    }

    /// runs the application's main loop until the user quits
    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.exit {
            tracing::trace!("Drawing");
            terminal.draw(|frame| self.draw(frame))?;
            tracing::trace!("Awaiting event");
            self.handle_events().await?;
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let list = List::new(
            self.search_response
                .items
                .iter()
                .map(|item| item.path.as_str()),
        )
        .block(Block::bordered().title("List"))
        .style(Style::new().white())
        .highlight_style(Style::new().italic())
        .highlight_symbol(">");

        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(frame.area());

        frame.render_stateful_widget(list, layout[0], &mut self.list_state);
    }

    /// updates the application's state based on user input
    async fn handle_events(&mut self) -> Result<()> {
        tokio::select! {
            event = self.event_stream.next().fuse() => {
                let event = event.unwrap()?;
                match event {
                    Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event)
                    }
                    _ => {}
                };
            },
            res = self.search_recv.recv() => {
                self.process_search_result(res)?;
            }
        }
        Ok(())
    }

    fn process_search_result(&mut self, res: Option<SearchResponse>) -> Result<()> {
        let Some(mut res) = res else {
            return Ok(());
        };

        self.search_response.items.append(&mut res.items);
        Ok(())
    }

    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event.code {
            KeyCode::Char('k') => self.list_state.select_previous(),
            KeyCode::Char('j') => self.list_state.select_next(),
            KeyCode::Char('q') => self.exit(),
            _ => {}
        }
    }

    fn exit(&mut self) {
        self.exit = true;
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
    let mut terminal = ratatui::init();
    let github = Github::new(get_auth_token()?);
    let app_result = App::new(github).await.run(&mut terminal).await;
    ratatui::restore();
    Ok(app_result?)
}
