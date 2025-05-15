use anyhow::{Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
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

fn get_auth_token() -> Result<String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token"]);
    log::debug!("executing auth command: {cmd:?}");
    let output = cmd.output()?;
    Ok(core::str::from_utf8(&output.stdout)?.trim().to_string())
}

#[derive(clap::Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Query to search.
    query: String,
}

fn redraw() {}

// #[tokio::main]
// async fn main() -> Result<()> {
//     env_logger::init();

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
    github: Github,
    exit: bool,
    search_response: SearchResponse,
    list_state: ListState,
}

impl App {
    pub async fn new(github: Github) -> Self {
        Self {
            exit: false,
            // search_response: github.search_code("foo").await.unwrap(),
            search_response: SearchResponse {
                items: vec![
                    SearchItem {
                        url: "".into(),
                        path: "foo".into(),
                        repository: hubl::github::SearchRepository {
                            full_name: "".into(),
                        },
                    },
                    SearchItem {
                        url: "".into(),
                        path: "bar".into(),
                        repository: hubl::github::SearchRepository {
                            full_name: "".into(),
                        },
                    },
                ],
            },
            github,
            list_state: ListState::default().with_selected(Some(0)),
        }
    }

    /// runs the application's main loop until the user quits
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;
            self.handle_events()?;
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
    fn handle_events(&mut self) -> io::Result<()> {
        match event::read()? {
            // it's important to check that the event is a key press event as
            // crossterm also emits key release and repeat events on Windows.
            Event::Key(key_event) if key_event.kind == KeyEventKind::Press => {
                self.handle_key_event(key_event)
            }
            _ => {}
        };
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

#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    let github = Github::new(get_auth_token()?);
    let app_result = App::new(github).await.run(&mut terminal);
    ratatui::restore();
    Ok(app_result?)
}
