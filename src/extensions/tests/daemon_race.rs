//! Concurrent `zdaemon start` on a STALE socket must yield exactly ONE live
//! daemon. Without the start lock, two starters could each unlink the stale
//! socket and bind their own — leaving two daemons (duplicated autonomy + a
//! leaked process). Here we plant a stale socket, fire many starts at once, and
//! prove only one survives.

use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git");

fn ping(sock: &Path) -> bool {
    use std::io::{BufRead, BufReader, Write};
    match std::os::unix::net::UnixStream::connect(sock) {
        Ok(mut s) => {
            let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
            if s.write_all(b"STATUS\n").is_err() || s.flush().is_err() {
                return false;
            }
            let mut line = String::new();
            matches!(BufReader::new(&s).read_line(&mut line), Ok(n) if n > 0)
        }
        Err(_) => false,
    }
}

#[test]
fn concurrent_start_on_stale_socket_yields_one_daemon() {
    let root = std::env::temp_dir().join(format!("zvcs-drace-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let home = home.canonicalize().unwrap();
    let sock = home.join("zvcs.sock");

    // Plant a STALE socket: bind then drop the listener. std leaves the socket
    // file on disk, but nothing is listening — exactly the post-hard-kill state.
    {
        let l = UnixListener::bind(&sock).unwrap();
        drop(l);
    }
    assert!(sock.exists(), "stale socket must exist");
    assert!(!ping(&sock), "stale socket must not answer");

    // Fire many starters at once, all pointed at the same home/socket.
    let mut kids: Vec<Child> = (0..6)
        .map(|_| {
            Command::new(BIN)
                .args(["zdaemon", "start"])
                .current_dir(&root)
                .env("ZVCS_HOME", &home)
                .spawn()
                .unwrap()
        })
        .collect();

    // Give the losers time to bail ("already running") and exit; the winner blocks
    // in its accept loop and stays alive.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !ping(&sock) {
        std::thread::sleep(Duration::from_millis(50));
    }
    // A moment more so every loser has observed the winner and exited.
    std::thread::sleep(Duration::from_millis(800));

    let mut alive = 0;
    for k in &mut kids {
        if k.try_wait().unwrap().is_none() {
            alive += 1;
        }
    }

    // Tear down before asserting so a failure doesn't leak daemons.
    let _ = Command::new(BIN).args(["zdaemon", "stop"]).current_dir(&root).env("ZVCS_HOME", &home).status();
    for k in &mut kids {
        let _ = k.kill();
        let _ = k.wait();
    }
    let _ = std::fs::remove_dir_all(&root);

    assert_eq!(alive, 1, "exactly one daemon must survive the stale-socket start race (found {alive})");
}
