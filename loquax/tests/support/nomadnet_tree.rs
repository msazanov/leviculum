//! A small NomadNet tree for the tests that run the import end to end
//! (1162): one conversation with a named peer, a sent and a received
//! message, and one blocked peer, in NomadNet's format
//! (`loquax/src/import/nomadnet.rs`). Shared by `tui_snapshot.rs` and
//! `loquax-android/tests/facade.rs`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm};
use leviculum_lxmf::msgpack;
use leviculum_std::Identity;

/// The bodies the tree holds, in time order.
pub const SENT: &str = "nomad hin";
pub const RECEIVED: &str = "nomad zurueck";
/// The peer's name in NomadNet's directory.
pub const NAME: &str = "Nomade";
/// The peer NomadNet ignores.
pub const BLOCKED: [u8; 16] = [0xbb; 16];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn address(identity: &Identity) -> [u8; 16] {
    loquax::engine::delivery_address(identity).expect("an address")
}

/// LXMF's `packed_container()`, as NomadNet writes each message.
fn container(packed: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, 2);
    msgpack::string(&mut out, "state");
    msgpack::int(&mut out, 0x08);
    msgpack::string(&mut out, "lxmf_bytes");
    msgpack::bin(&mut out, packed);
    out
}

fn message(from: &Identity, to: [u8; 16], ts: f64, body: &str) -> Lxm {
    Lxm::create(
        to,
        address(from),
        from,
        ts,
        Vec::new(),
        body.as_bytes().to_vec(),
        Vec::new(),
        DeliveryMethod::Direct,
    )
    .expect("a message")
}

/// Write the tree of `own` under `root`, talking to `peer`: its root.
pub fn write(root: &Path, own: &Identity, peer: &Identity) -> PathBuf {
    let storage = root.join("storage");
    let (me, them) = (address(own), address(peer));
    let conv = storage.join("conversations").join(hex(&them));
    std::fs::create_dir_all(&conv).expect("the conversation dir");
    std::fs::write(
        storage.join("identity"),
        own.private_key_bytes().expect("a key"),
    )
    .expect("the identity");
    for message in [
        message(own, them, 1_000.0, SENT),
        message(peer, me, 2_000.0, RECEIVED),
    ] {
        std::fs::write(
            conv.join(hex(&message.message_id)),
            container(&message.pack()),
        )
        .expect("a message");
    }
    let mut directory = Vec::new();
    msgpack::map(&mut directory, 1);
    msgpack::string(&mut directory, "entry_list");
    msgpack::array(&mut directory, 1);
    msgpack::array(&mut directory, 3);
    msgpack::bin(&mut directory, &them);
    msgpack::string(&mut directory, NAME);
    msgpack::int(&mut directory, 2);
    std::fs::write(storage.join("directory"), directory).expect("the directory");
    std::fs::write(root.join("ignored"), format!("{}\n", hex(&BLOCKED))).expect("ignored");
    root.to_path_buf()
}
