//! `zellij wait` — block until a pane's output matches a pattern.
//!
//! Built entirely on the pane-render subscription that backs `zellij subscribe`, so it needs
//! nothing from the server that is not already there. `subscribe` reports an unresolvable pane
//! id as a `LogError`, which this inherits.

use std::io::Write;
use std::path::PathBuf;
use std::process;
use std::str::FromStr;
use std::thread;
use std::time::Duration;

use regex::Regex;
use zellij_utils::{
    cli::WaitCli,
    data::PaneId,
    ipc::{ClientToServerMsg, ServerToClientMsg},
};

use crate::os_input_output::ClientOsApi;

const MATCHED: i32 = 0;
const TIMED_OUT: i32 = 1;
const FAILED: i32 = 2;

/// Blocks until the pane's contents match, the pane closes, or the timeout elapses.
/// Returns the process exit status.
pub fn start_wait_client(os_input: Box<dyn ClientOsApi>, session_name: &str, wait_cli: WaitCli) -> i32 {
    let matcher = match Matcher::new(&wait_cli) {
        Ok(matcher) => matcher,
        Err(e) => {
            eprintln!("{e}");
            return FAILED;
        },
    };
    let pane_id = match PaneId::from_str(&wait_cli.pane_id) {
        Ok(pane_id) => pane_id,
        Err(e) => {
            eprintln!("Invalid pane ID '{}': {}", wait_cli.pane_id, e);
            return FAILED;
        },
    };

    let zellij_ipc_pipe: PathBuf = crate::resolve_session_ipc_pipe(session_name);
    os_input.connect_to_server(&*zellij_ipc_pipe);
    // The subscription delivers the pane's current contents before any new render, so a
    // pattern that already matched before we connected is still seen.
    os_input.send_to_server(ClientToServerMsg::SubscribeToPaneRenders {
        pane_ids: vec![pane_id],
        scrollback: Some(0),
        ansi: false,
    });

    // recv_from_server has no deadline of its own, and a pane that never writes again would
    // park this thread forever, so the timeout is enforced from the outside.
    if let Some(timeout) = wait_cli.timeout.map(Duration::from_millis) {
        let pattern = matcher.description();
        thread::spawn(move || {
            thread::sleep(timeout);
            eprintln!("Timed out after {}ms waiting for {}", timeout.as_millis(), pattern);
            process::exit(TIMED_OUT);
        });
    }

    loop {
        match os_input.recv_from_server() {
            Some((
                ServerToClientMsg::PaneRenderUpdate {
                    viewport,
                    scrollback,
                    ..
                },
                _,
            )) => {
                let mut lines = scrollback.unwrap_or_default();
                lines.extend(viewport);
                let contents = lines.join("\n");
                if matcher.matches(&contents) {
                    if wait_cli.print_match {
                        let stdout = std::io::stdout();
                        let mut stdout = stdout.lock();
                        for line in lines.iter().filter(|line| matcher.matches(line)) {
                            let _ = writeln!(stdout, "{line}");
                        }
                        let _ = stdout.flush();
                    }
                    os_input.send_to_server(ClientToServerMsg::ClientExited);
                    return MATCHED;
                }
            },
            Some((ServerToClientMsg::SubscribedPaneClosed { .. }, _)) => {
                eprintln!("Pane {pane_id} closed before {} matched", matcher.description());
                return FAILED;
            },
            Some((ServerToClientMsg::LogError { lines }, _)) => {
                for line in lines {
                    eprintln!("{line}");
                }
                return FAILED;
            },
            Some((ServerToClientMsg::Exit { .. }, _)) | None => {
                eprintln!("Session '{session_name}' ended before {} matched", matcher.description());
                return FAILED;
            },
            _ => {},
        }
    }
}

enum Matcher {
    Regex(Regex),
    Literal(String),
}

impl Matcher {
    fn new(wait_cli: &WaitCli) -> Result<Self, String> {
        match (&wait_cli.regex, &wait_cli.match_text) {
            (Some(pattern), _) => Regex::new(pattern)
                .map(Matcher::Regex)
                .map_err(|e| format!("Invalid --regex '{pattern}': {e}")),
            (None, Some(literal)) => Ok(Matcher::Literal(literal.clone())),
            (None, None) => Err("Pass either --regex or --match".to_owned()),
        }
    }

    /// Matched against the pane's scrollback and viewport joined with newlines, so a pattern
    /// may span what the terminal happens to have wrapped.
    fn matches(&self, contents: &str) -> bool {
        match self {
            Matcher::Regex(regex) => regex.is_match(contents),
            Matcher::Literal(literal) => contents.contains(literal),
        }
    }

    fn description(&self) -> String {
        match self {
            Matcher::Regex(regex) => format!("/{}/", regex.as_str()),
            Matcher::Literal(literal) => format!("'{literal}'"),
        }
    }
}
