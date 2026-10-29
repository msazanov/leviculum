//! The Python LXMF peer and the in-process Rust daemon loquax attaches to,
//! shared by `python_interop.rs` and `home_node.rs`.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;

/// Host-wide listener-port allocator, shared with the `mvr`, `rnsd_interop`
/// and `discovery_autoconnect` suites. Included by path rather than copied:
/// the module's whole point is that there is ONE counter per host, and a
/// private one here would walk the same numbers as everyone else's.
#[path = "../../../leviculum-std/tests/support/port_alloc.rs"]
#[allow(dead_code)]
mod port_alloc;

pub const LNMSG: &str = env!("CARGO_BIN_EXE_loquax");

/// `scripts/test_daemon.py`, relative to this package.
fn daemon_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../scripts/test_daemon.py")
}

/// A running Python RNS daemon with an LXMF router, and its JSON-RPC port.
pub struct PythonPeer {
    pub process: std::process::Child,
    pub rns_port: u16,
    pub cmd_port: u16,
    /// The daemon's TMPDIR, so the `rns_test_*` config directory it makes
    /// there goes with this handle rather than with the daemon, which removes
    /// its own only when its main loop ends and a `kill` never lets it (730).
    /// Last, so it is removed after `Drop::drop` has killed the daemon.
    _scratch: tempfile::TempDir,
}

impl PythonPeer {
    pub fn start() -> Self {
        let rns_port = port_alloc::free_tcp_port();
        let cmd_port = port_alloc::free_tcp_port();
        let scratch = tempfile::Builder::new()
            .prefix("rns_test_")
            .tempdir()
            .expect("the Python daemon's TMPDIR");
        let mut command = Command::new("python3");
        command
            .env("TMPDIR", scratch.path())
            .arg(daemon_script())
            .args(["--rns-port", &rns_port.to_string()])
            .args(["--cmd-port", &cmd_port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // Supervised so the Python daemon dies with this test binary rather
        // than outliving a failed run and holding its ports.
        let mut process =
            leviculum_std::process::spawn_supervised(command).expect("spawn the Python daemon");

        let stdout = process.stdout.take().expect("stdout is piped");
        let mut reader = BufReader::new(stdout);
        let mut ready = String::new();
        loop {
            ready.clear();
            let read = reader
                .read_line(&mut ready)
                .expect("read the daemon stdout");
            assert!(read > 0, "the Python daemon exited before saying READY");
            if ready.starts_with("READY ") {
                break;
            }
        }
        // The daemon falls back to a kernel-allocated command port when the
        // requested one was taken, and reports the bound one on the READY
        // line; trusting the requested number would talk to nothing.
        let bound: Vec<&str> = ready.split_whitespace().collect();
        let cmd_port = bound[2].parse::<u16>().expect("the READY line's cmd port");

        let peer = Self {
            process,
            rns_port,
            cmd_port,
            _scratch: scratch,
        };
        assert_eq!(
            peer.rpc("ping", serde_json::json!({})),
            serde_json::json!("pong"),
            "the Python daemon must answer on its command port"
        );
        peer
    }

    /// One JSON-RPC call: connect, write, half-close, read the answer.
    pub fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let response = self.rpc_answer(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} failed: {response}"
        );
        response
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// One JSON-RPC call whose error is an answer too: the result, or the
    /// error's text.
    pub fn try_rpc(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let response = self.rpc_answer(method, params);
        match response.get("error") {
            Some(error) => Err(error.to_string()),
            None => Ok(response
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null)),
        }
    }

    fn rpc_answer(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        use std::net::TcpStream;
        let mut stream = TcpStream::connect(("127.0.0.1", self.cmd_port))
            .expect("connect to the Python command port");
        let request = serde_json::json!({ "method": method, "params": params });
        stream
            .write_all(request.to_string().as_bytes())
            .expect("write the command");
        stream
            .shutdown(std::net::Shutdown::Write)
            .expect("half-close");
        let mut response = Vec::new();
        std::io::Read::read_to_end(&mut stream, &mut response).expect("read the answer");
        serde_json::from_slice(&response).expect("the answer is JSON")
    }

    /// Bring up the Python LXMF router and return its delivery address.
    pub fn lxmf_init(&self) -> String {
        self.rpc("lxmf_init", serde_json::json!({}))
            .get("delivery_hash")
            .and_then(|v| v.as_str())
            .expect("lxmf_init returns a delivery hash")
            .to_string()
    }

    pub fn lxmf_announce(&self) {
        self.rpc("lxmf_announce", serde_json::json!({}));
    }

    pub fn received(&self) -> Vec<serde_json::Value> {
        match self.rpc("lxmf_get_received", serde_json::json!({})) {
            serde_json::Value::Array(items) => items,
            _ => Vec::new(),
        }
    }
}

impl Drop for PythonPeer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// The Rust daemon `loquax` attaches to, plus everything it needs alive.
pub struct Mesh {
    pub daemon: ReticulumNode,
    pub instance: String,
    pub home: tempfile::TempDir,
    _storage: tempfile::TempDir,
}

impl Mesh {
    pub async fn start(python_rns_port: u16) -> Self {
        // Unique per process AND per call: two tests in one binary run in
        // parallel threads and would otherwise fight over one abstract socket.
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let instance = format!(
            "loquax-interop-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let storage = tempfile::tempdir().expect("daemon storage");
        let python: SocketAddr = format!("127.0.0.1:{python_rns_port}")
            .parse()
            .expect("loopback address");

        let mut daemon = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .share_instance(true)
            .instance_name(instance.clone())
            .add_tcp_client(python)
            .storage_path(storage.path().to_path_buf())
            .build_sync()
            .expect("build the Rust daemon");
        daemon.start().await.expect("start the Rust daemon");
        // Let the abstract Unix socket listener and the TCP link come up
        // before a client connects; the same wait `lnomad`'s integ test uses.
        tokio::time::sleep(Duration::from_millis(500)).await;

        Self {
            daemon,
            instance,
            home: tempfile::tempdir().expect("loquax state dir"),
            _storage: storage,
        }
    }

    /// Run `loquax send` against this mesh with the mesh's own state dir.
    pub async fn send(&self, args: Vec<String>, stdin: Vec<u8>) -> Sent {
        let home = self.home.path().to_path_buf();
        self.run("send", home, args, stdin).await
    }

    /// Run one `loquax` subcommand against this mesh, on a blocking thread so
    /// the daemon's runtime keeps serving the IPC socket while the child
    /// talks to it. `home` is the client's state directory — the mailbox
    /// tests run two clients (a sender and a recipient) against one mesh,
    /// and each needs its own identity and seen store.
    ///
    /// The child writes its structured event log into the state directory and
    /// [`Sent::log`] carries it back, so a failing assertion prints the state
    /// sequence that produced it rather than a bare exit code. That is what the
    /// log is for (`docs/src/concepts/loquax-architecture.md` §7).
    pub async fn run(
        &self,
        subcommand: &'static str,
        home: PathBuf,
        args: Vec<String>,
        stdin: Vec<u8>,
    ) -> Sent {
        let instance = self.instance.clone();
        let log = home.join(format!("events-{subcommand}-{}.log", args.join("-")));
        let read_log = log.clone();
        let sent = tokio::task::spawn_blocking(move || {
            let mut command = Command::new(LNMSG);
            command
                .arg(subcommand)
                .args(&args)
                .args(["--instance", &instance])
                .env("LNMSG_HOME", &home)
                .env("LEVICULUM_EVENT_LOG", &log)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Supervised for the same reason the Python daemon is: a client
            // that outlives a killed run keeps talking to the shared instance.
            let mut child =
                leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
            child
                .stdin
                .take()
                .expect("stdin is piped")
                .write_all(&stdin)
                .expect("write the body");
            let output = child.wait_with_output().expect("wait for loquax");
            Sent {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                log: String::new(),
            }
        })
        .await
        .expect("the loquax child thread");
        Sent {
            log: std::fs::read_to_string(&read_log).unwrap_or_default(),
            ..sent
        }
    }
}

/// One `loquax send` run, as a script and a debugger see it.
pub struct Sent {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// The child's structured event log, for failure messages.
    pub log: String,
}

impl Sent {
    /// The message id, from the `id=` field of this run's `LNMSG_ENQUEUED`
    /// line. Since 2026-08-21 the id is not on stdout, and this is the
    /// documented way to get it back out of a run.
    pub fn enqueued_id(&self) -> String {
        let line = self
            .log
            .lines()
            .find(|line| line.starts_with("LNMSG_ENQUEUED "))
            .unwrap_or_else(|| panic!("no LNMSG_ENQUEUED line in this run: {self}"));
        let id = line
            .split_whitespace()
            .find_map(|token| token.strip_prefix("id="))
            .unwrap_or_else(|| panic!("no id= field in {line}"));
        assert_eq!(
            id.len(),
            64,
            "an LXMF message id is 32 bytes of hex: {id:?}"
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "the id must be hex: {id:?}"
        );
        id.to_string()
    }
}

impl std::fmt::Display for Sent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "exit {:?}\nstdout: {:?}\nstderr: {}\nevents:\n{}",
            self.code, self.stdout, self.stderr, self.log
        )
    }
}
