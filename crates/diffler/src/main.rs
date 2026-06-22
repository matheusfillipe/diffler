use std::path::Path;

use clap::{Parser, Subcommand};
use diffler::app::{self, App, CiRequest, Flow};
use diffler::event::AppEvent;
use diffler::{ci, clipboard, config, editor, event, mcp, ui, watch};
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

    /// UI theme: github-dark, github-light, or dracula (overrides `ui.theme`)
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
            set_mouse_capture(true);
            install_mouse_panic_hook();
            let result = run(terminal, app).await;
            set_mouse_capture(false);
            ratatui::restore();
            result
        }
    }
}

/// Toggle SGR mouse reporting (crossterm uses mode 1006, which tmux forwards to
/// apps that request it). Best-effort: a terminal without mouse support just
/// ignores it.
fn set_mouse_capture(on: bool) {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    let mut out = std::io::stdout();
    let _ = if on {
        crossterm::execute!(out, EnableMouseCapture)
    } else {
        crossterm::execute!(out, DisableMouseCapture)
    };
}

/// Chain mouse-disable ahead of the existing (ratatui screen-restore) panic
/// hook, so a crash doesn't leave the terminal emitting mouse escape codes.
fn install_mouse_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        set_mouse_capture(false);
        previous(info);
    }));
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
                let _ = mcp::write_endpoint(&app.review.repo_root, handle.port);
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
        if let Some(text) = app.pending_clipboard.take() {
            // OSC52 addresses the terminal emulator, not the screen: it must
            // bypass ratatui's buffer and go out raw, right after the draw so
            // it cannot interleave with one. The native CLI pipe (on a blocking
            // thread, since it spawns a process) covers terminals without OSC52.
            use std::io::Write as _;
            let mut out = std::io::stdout();
            out.write_all(clipboard::osc52(&text).as_bytes())?;
            out.flush()?;
            tokio::task::spawn_blocking(move || clipboard::native_copy(&text));
        }
        if let Some(request) = app.pending_editor.take() {
            // the event pump must release the tty before the editor gets
            // it, or both end up reading the same keystrokes
            events.abort();
            let _ = (&mut events).await;
            set_mouse_capture(false);
            ratatui::restore();
            let editor::EditorRequest { cmd, purpose } = request;
            let outcome = tokio::task::spawn_blocking(move || editor::run(&cmd))
                .await
                .map_err(|err| err.to_string())
                .and_then(|result| result.map_err(|err| err.to_string()));
            terminal = ratatui::init();
            set_mouse_capture(true);
            terminal.clear()?;
            events = event::spawn_event_loop(tx.clone());
            app.editor_finished(purpose, outcome);
            continue;
        }
        if let Some(app::GitOp { label, argv }) = app.pending_git.take() {
            // the terminal stays up: spawn the process on a blocking thread
            // with a tx clone so the result returns as an event and the loop
            // keeps drawing the "running …" status meanwhile
            let repo_root = app.review.repo_root.clone();
            let tx = tx.clone();
            tokio::task::spawn_blocking(move || {
                let _ = tx.send(run_git(&label, &argv, &repo_root));
            });
            continue;
        }
        if let Some(request) = app.pending_ci.take() {
            // service the CI provider call off-thread; the result returns as an
            // event so the active CI screen stays live without blocking the loop
            if let Some(detected) = app.ci_detected() {
                let provider = ci::build_provider(
                    &detected,
                    &app.review.repo_root,
                    app.head.branch.as_deref(),
                );
                let tx = tx.clone();
                tokio::spawn(async move {
                    let _ = tx.send(run_ci_request(provider, request).await);
                });
            }
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
        mcp::clear_endpoint(&app.review.repo_root);
    }
    drop(watcher);
    Ok(())
}

/// Service a CI provider request off the event loop, turning the result (or
/// error) into the matching [`AppEvent`] the loop folds back into the screen.
async fn run_ci_request(
    provider: Box<dyn crate::ci::CiProvider + Send>,
    request: CiRequest,
) -> AppEvent {
    match request {
        CiRequest::Runs => match provider.list_runs(30).await {
            Ok(runs) => AppEvent::CiRuns(runs),
            Err(err) => AppEvent::CiError(err.to_string()),
        },
        CiRequest::Pr => AppEvent::CiPr(provider.current_pr().await.unwrap_or(None)),
        CiRequest::Detail(id) => match provider.run_detail(&id).await {
            Ok(detail) => AppEvent::CiRunDetail(detail),
            Err(err) => AppEvent::CiError(err.to_string()),
        },
        CiRequest::Extras(id) => match provider.run_extras(&id).await {
            Ok(extras) => AppEvent::CiExtras(extras),
            Err(err) => AppEvent::CiError(err.to_string()),
        },
        CiRequest::Log { run, job, offset } => match provider.job_log(&run, &job, offset).await {
            Ok(chunk) => AppEvent::CiLog {
                text: chunk.text,
                steps: chunk.steps,
                next_offset: chunk.next_offset,
                done: chunk.done,
            },
            Err(err) => AppEvent::CiError(err.to_string()),
        },
    }
}

/// Run a network git op as a child process in `repo_root`, capturing its
/// combined output. Runs on a blocking thread; the result is an [`AppEvent`]
/// the run loop folds back into the status bar. Shelling to the user's `git`
/// keeps credentials entirely out of diffler.
fn run_git(label: &str, argv: &[String], repo_root: &Path) -> AppEvent {
    let Some((program, rest)) = argv.split_first() else {
        return AppEvent::GitDone {
            label: label.to_owned(),
            ok: false,
            output: "empty command".to_owned(),
        };
    };
    match std::process::Command::new(program)
        .args(rest)
        .current_dir(repo_root)
        .output()
    {
        Ok(out) => {
            let mut output = String::from_utf8_lossy(&out.stderr).into_owned();
            output.push_str(&String::from_utf8_lossy(&out.stdout));
            AppEvent::GitDone {
                label: label.to_owned(),
                ok: out.status.success(),
                output,
            }
        }
        Err(err) => AppEvent::GitDone {
            label: label.to_owned(),
            ok: false,
            output: format!("cannot run {program}: {err}"),
        },
    }
}
