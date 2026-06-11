mod ui;

use std::io::Stdout;
use std::time::Duration;

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Terminal;
use ratatui::prelude::CrosstermBackend;

/// Terminal code review for AI coding agents.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to the repository to review (defaults to the current directory)
    #[arg(default_value = ".")]
    path: String,
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let args = Args::parse();
    let repo = diffler_core::repo::discover(std::path::Path::new(&args.path)).ok();

    let terminal = ratatui::init();
    let result = run(terminal, repo.as_deref());
    ratatui::restore();
    result
}

fn run(
    mut terminal: Terminal<CrosstermBackend<Stdout>>,
    repo: Option<&std::path::Path>,
) -> color_eyre::Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, repo))?;
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('q')
        {
            return Ok(());
        }
    }
}
