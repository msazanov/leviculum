//! The Python LXMF peer: `scripts/test_daemon.py`, a stock RNS instance
//! with an LXMF router, driven over its JSON-RPC port. Shared by loquax's
//! suites (through `python.rs`) and loquax-android's facade test, which
//! includes this file alone because it has no `loquax` binary.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Host-wide listener-port allocator, shared with the `mvr`, `rnsd_interop`
/// and `discovery_autoconnect` suites. Included by path rather than copied:
/// the module's whole point is that there is ONE counter per host, and a
/// private one here would walk the same numbers as everyone else's.
#[path = "../../../leviculum-std/tests/support/port_alloc.rs"]
#[allow(dead_code)]
pub mod port_alloc;

/// `scripts/test_daemon.py`, relative to this file, which every including
/// package reaches from a sibling directory of `loquax`.
fn daemon_script() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/test_daemon.py"
    ))
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
