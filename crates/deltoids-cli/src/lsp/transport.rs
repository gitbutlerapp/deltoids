//! The production adapter at the transport seam: a language server
//! child process spoken to over its stdin and stdout.

use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lsp_server::{Connection, Message};

use super::config::ServerSpec;

/// A connection to a server, plus its process when it has one.
pub(crate) struct Link {
    pub(crate) connection: Connection,
    pub(crate) process: Option<Child>,
    /// The last line the process wrote to stderr: why it stopped, when
    /// it stops early.
    pub(crate) last_error: Arc<Mutex<Option<String>>>,
    /// Disconnects once stderr is fully read.
    pub(crate) stderr_closed: Option<crossbeam_channel::Receiver<()>>,
}

impl Link {
    /// The last line the process wrote to stderr, once it is fully read
    /// (waiting up to 200 ms for that).
    pub(super) fn last_error(&self) -> Option<String> {
        if let Some(closed) = &self.stderr_closed {
            let _ = closed.recv_timeout(Duration::from_millis(200));
        }
        self.last_error
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Wait for the process to exit until `deadline`, then kill it with
    /// everything it started.
    pub(super) fn finish(&mut self, deadline: Instant) {
        let Some(child) = self.process.as_mut() else {
            return;
        };
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        kill_group(child);
        let _ = child.wait();
    }
}

/// Kill `child` and every process in its group: the server and what it
/// started. Elsewhere, the server alone.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = libc::pid_t::try_from(child.id()) {
        // SAFETY: `kill` only sends a signal; a negative pid names the
        // group the server leads (see `process_group(0)` in `spawn`).
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        return;
    }
    let _ = child.kill();
}

/// Start `spec`'s program in `root`. One thread reads messages from its
/// stdout and one writes to its stdin; when the process exits, the
/// connection's receiver disconnects.
pub(crate) fn spawn(spec: &ServerSpec, root: &Path) -> io::Result<Link> {
    let (program, args) = spec
        .command
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "empty command"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Lead a process group of its own, so killing the server also kills
    // what it started (proc-macro servers, `cargo check`, `tsserver`).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let last_error = Arc::new(Mutex::new(None));
    let last = Arc::clone(&last_error);
    let (closing, stderr_closed) = crossbeam_channel::bounded::<()>(0);
    thread::spawn(move || {
        let _closing = closing;
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let line = line.trim();
            if !line.is_empty() {
                *last.lock().unwrap_or_else(|err| err.into_inner()) = Some(line.to_string());
            }
        }
    });

    let (to_client, from_server) = crossbeam_channel::unbounded();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        while let Ok(Some(message)) = Message::read(&mut reader) {
            if to_client.send(message).is_err() {
                break;
            }
        }
    });
    let (to_server, from_client) = crossbeam_channel::unbounded::<Message>();
    thread::spawn(move || {
        for message in from_client {
            if message.write(&mut stdin).is_err() {
                break;
            }
        }
    });
    Ok(Link {
        connection: Connection {
            sender: to_server,
            receiver: from_server,
        },
        process: Some(child),
        last_error,
        stderr_closed: Some(stderr_closed),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Whether process `pid` exists (a zombie counts as gone).
    fn alive(pid: &str) -> bool {
        let status = Command::new("ps")
            .args(["-o", "stat=", "-p", pid])
            .output()
            .unwrap();
        let stat = String::from_utf8_lossy(&status.stdout);
        !stat.trim().is_empty() && !stat.trim().starts_with('Z')
    }

    #[test]
    fn a_server_that_will_not_exit_is_killed_with_what_it_started() {
        let spec = ServerSpec {
            command: ["sh", "-c", "sleep 300 & echo $! >&2; wait"]
                .map(String::from)
                .to_vec(),
            settings: None,
        };
        let mut link = spawn(&spec, Path::new("/")).unwrap();
        let started = Instant::now() + Duration::from_secs(5);
        let child = loop {
            let line = link.last_error.lock().unwrap().clone();
            if let Some(pid) = line {
                break pid;
            }
            assert!(
                Instant::now() < started,
                "the server never started its child"
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert!(alive(&child));

        link.finish(Instant::now());

        let gone = Instant::now() + Duration::from_secs(2);
        while alive(&child) {
            assert!(Instant::now() < gone, "the server's child outlived it");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
