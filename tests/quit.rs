//! `/quit` must stop the whole process, not just the web server.
//!
//! Regression test: the web server used to shut down on its own broadcast
//! channel without ever requesting a shutdown of the subsystem tree, leaving
//! a headless process behind that still held the radar sockets and could
//! only be killed by hand.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const EXIT_TIMEOUT: Duration = Duration::from_secs(15);

/// Start a server on an ephemeral port and return it with that port.
///
/// `--parent` with our own pid keeps the server on the loopback interface and
/// off mDNS, so a test run neither exposes a port to the network nor
/// advertises itself, and orphans die with the test process.
fn start_server() -> (Child, u16) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mayara-server"))
        .args(["--port", "0", "--parent", &std::process::id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start mayara-server");

    // The startup line reports the bound address, which is where `--port 0`
    // actually landed.
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(addr) = line.split("web server on ").nth(1)
                && let Some(port) = addr.split(':').nth(1)
                && let Some(port) = port.split_whitespace().next()
                && let Ok(port) = port.parse::<u16>()
            {
                let _ = tx.send(port);
                break;
            }
        }
    });

    let port = rx
        .recv_timeout(STARTUP_TIMEOUT)
        .expect("server did not report a listening port");

    (child, port)
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[tokio::test]
async fn quit_stops_the_whole_process() {
    let (mut child, port) = start_server();

    let response = reqwest::get(format!("http://127.0.0.1:{}/quit", port))
        .await
        .expect("GET /quit failed");
    assert!(response.status().is_success());

    match wait_for_exit(&mut child, EXIT_TIMEOUT) {
        Some(status) => assert!(status.success(), "server exited with {}", status),
        None => {
            let _ = child.kill();
            panic!("server still running after /quit");
        }
    }
}
