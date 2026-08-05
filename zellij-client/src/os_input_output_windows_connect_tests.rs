use super::setup_ipc;
use interprocess::local_socket::prelude::*;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use zellij_utils::consts::{ipc_bind, ipc_bind_reply, ipc_connect, ipc_connect_reply};

#[test]
fn slow_reply_accept_yields_exactly_one_reply_connection() {
    let path = std::env::temp_dir().join(format!(
        "zellij-slow-reply-accept-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
    ));
    let _main_listener = ipc_bind(&path).unwrap();
    let reply_listener = ipc_bind_reply(&path).unwrap();
    let _occupies_the_only_reply_instance = ipc_connect_reply(&path).unwrap();
    let main_stream = ipc_connect(&path).unwrap();

    let _client = {
        let path = path.clone();
        std::thread::spawn(move || setup_ipc(main_stream, &path))
    };
    std::thread::sleep(Duration::from_millis(1300));

    let (accepted_tx, accepted_rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in reply_listener.incoming() {
            if accepted_tx.send(stream.map(|_| ())).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_millis(1000);
    let accepted: Vec<_> = std::iter::from_fn(|| {
        accepted_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    })
    .collect();
    let _ = std::fs::remove_file(&path);

    let client_connections = accepted.len().saturating_sub(1);
    assert_eq!(
        client_connections, 1,
        "one attach opened {client_connections} reply connections: {accepted:?}"
    );
}
