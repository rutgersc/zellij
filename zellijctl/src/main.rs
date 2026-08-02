use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

use zellij_client::cli_client::start_cli_client;
use zellij_client::os_input_output::get_cli_client_os_input;
use zellij_utils::cli::CliAction;
use zellij_utils::input::actions::Action;
use zellij_utils::sessions;

/// A drop-in for the handful of `zellij` subcommands `mux` shells out to,
/// dispatched over the same IPC socket the real client uses. See the manifest
/// for why a separate small binary exists.
#[derive(Parser)]
#[clap(name = "zellijctl", version)]
struct Cli {
    /// Target session. Defaults to $ZELLIJ_SESSION_NAME. Must precede the subcommand.
    #[clap(long, value_parser)]
    session: Option<String>,

    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Mirrors `zellij list-sessions`.
    ListSessions {
        #[clap(short, long)]
        no_formatting: bool,
        #[clap(short, long)]
        short: bool,
        #[clap(short, long)]
        reverse: bool,
    },
    /// Mirrors `zellij delete-session`.
    DeleteSession {
        name: String,
        #[clap(short, long)]
        force: bool,
    },
    /// Mirrors `zellij action <…>` — sends one action to a running session.
    Action {
        #[clap(subcommand)]
        action: CliAction,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::ListSessions {
            no_formatting,
            short,
            reverse,
        } => sessions::list_sessions(no_formatting, short, reverse),
        Cmd::DeleteSession { name, force } => sessions::delete_session(&name, force),
        Cmd::Action { action } => send_action(cli.session, action),
    }
}

fn send_action(requested_session: Option<String>, cli_action: CliAction) {
    let session = requested_session
        .or_else(|| std::env::var("ZELLIJ_SESSION_NAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            eprintln!("zellijctl: no session (pass --session or set ZELLIJ_SESSION_NAME)");
            process::exit(1);
        });

    let get_current_dir = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let actions = Action::actions_from_cli(cli_action, Box::new(get_current_dir), None)
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            process::exit(2);
        });

    let os_input = get_cli_client_os_input().unwrap_or_else(|e| {
        eprintln!("zellijctl: failed to acquire client os input: {e}");
        process::exit(2);
    });
    start_cli_client(Box::new(os_input), &session, actions);
}
