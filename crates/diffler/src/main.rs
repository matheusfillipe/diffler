use clap::{Parser, Subcommand};
use diffler::app::{App, Flow};
use diffler::{config, editor, event, mcp, ui, watch};
use diffler_core::review::Review;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

/// Terminal code review for AI coding agents.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Cli {
    /// Path to the repository to review (defaults to the current directory)
    #[arg(default_value = ".")]
    path: String,

    /// MCP server port (overrides config key `mcp.port`)
    #[arg(long, global = true)]
    port: Option<u16>,

    /// Disable the MCP server (overrides config key `mcp.enabled`)
    #[arg(long, global = true)]
    no_mcp: bool,

    /// UI theme name (overrides config key `ui.theme`)
    #[arg(long, global = true)]
    theme: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Inspect configuration
    Config {
        /// Print the merged effective config with per-key origins
        #[arg(long)]
        dump: bool,
    },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();
    let repo = diffler_core::repo::discover(std::path::Path::new(&cli.path));

    let overrides = config::CliOverrides {
        port: cli.port,
        mcp_enabled: cli.no_mcp.then_some(false),
        theme: cli.theme.clone(),
    };
    let loaded = config::load(repo.as_deref().ok(), &overrides)?;

    match cli.command {
        Some(Command::Config { dump: true }) => print_config_dump(&loaded),
        Some(Command::Config { dump: false }) => Err(color_eyre::eyre::eyre!(
            "nothing to do: try `diffler config --dump`"
        )),
        None => {
            // fail before touching the terminal so the error stays readable
            let review = Review::open_with_context(&repo?, loaded.config.ui.context_lines)?;
            let app = App::new(review, loaded);
            let terminal = ratatui::init();
            let result = run(terminal, app).await;
            ratatui::restore();
            result
        }
    }
}

// stdout is correct for a non-TUI subcommand; the workspace print_stdout lint
// exists to keep the TUI screen intact, which never starts on this path
#[allow(clippy::print_stdout)]
fn print_config_dump(loaded: &config::LoadedConfig) -> color_eyre::Result<()> {
    print!("{}", config::render_dump(loaded)?);
    Ok(())
}

async fn run(mut terminal: DefaultTerminal, mut app: App) -> color_eyre::Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut events = event::spawn_event_loop(tx.clone());
    // a missing watcher is not fatal: the app falls back to periodic polling
    let git_dir = app
        .review
        .vcs
        .git_dir()
        .unwrap_or_else(|_| app.review.repo_root.join(".git"));
    let watcher = watch::spawn_watcher(&app.review.repo_root, &git_dir, tx.clone()).ok();
    if let Some(handle) = &watcher {
        app.watcher_healthy = Some(handle.healthy.clone());
    }
    let mcp = if app.config.mcp.enabled {
        match mcp::spawn_mcp(tx.clone(), app.feedback_tx.subscribe(), app.config.mcp.port).await {
            Ok(handle) => {
                app.mcp_port = Some(handle.port);
                // show the connect hint as the initial message only when no
                // startup warning is already occupying the one-shot slot
                if app.message.is_none() {
                    app.info(mcp::connect_hint(handle.port));
                }
                Some(handle)
            }
            Err(err) => {
                app.error(format!("mcp server failed to start: {err}"));
                None
            }
        }
    } else {
        None
    };
    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;
        if let Some(sequence) = app.pending_osc.take() {
            // an OSC52 sequence addresses the terminal emulator, not the
            // screen: it must bypass ratatui's buffer and go out raw,
            // right after the draw so it cannot interleave with one
            use std::io::Write as _;
            let mut out = std::io::stdout();
            out.write_all(sequence.as_bytes())?;
            out.flush()?;
        }
        if let Some(request) = app.pending_editor.take() {
            // the event pump must release the tty before the editor gets
            // it, or both end up reading the same keystrokes
            events.abort();
            let _ = (&mut events).await;
            ratatui::restore();
            let editor::EditorRequest { cmd, purpose } = request;
            let outcome = tokio::task::spawn_blocking(move || editor::run(&cmd))
                .await
                .map_err(|err| err.to_string())
                .and_then(|result| result.map_err(|err| err.to_string()));
            terminal = ratatui::init();
            terminal.clear()?;
            events = event::spawn_event_loop(tx.clone());
            app.editor_finished(purpose, outcome);
            continue;
        }
        let Some(event) = rx.recv().await else { break };
        if app.handle(event) == Flow::Quit {
            break;
        }
    }
    events.abort();
    if let Some(mcp) = mcp {
        mcp.handle.abort();
    }
    drop(watcher);
    Ok(())
}
