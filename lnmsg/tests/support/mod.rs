//! The in-process daemon the CLI tests attach lnmsg to: a user's `lnsd`,
//! sharing its instance under a name unique to the test, with the config
//! directory lnmsg is pointed at with `--config`. Shared by `mailbox_cli.rs`
//! and `tui_snapshot.rs`.

// Each test crate includes this module and uses its own part of it.
#![allow(dead_code)]

use leviculum_std::driver::ReticulumNodeBuilder;
use leviculum_std::ReticulumNode;

pub struct Daemon {
    pub node: ReticulumNode,
    pub instance: String,
    /// The config directory lnmsg is pointed at with `--config`: the authkey
    /// is read from `storage/transport_identity` under it.
    pub config_dir: tempfile::TempDir,
}

pub fn unique(tag: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "lnmsg-mbx-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

pub async fn daemon(
    tag: &str,
    tcp: impl FnOnce(ReticulumNodeBuilder) -> ReticulumNodeBuilder,
) -> Daemon {
    let instance = unique(tag);
    let config_dir = tempfile::tempdir().expect("daemon config dir");
    let builder = ReticulumNodeBuilder::new()
        // A user's daemon: no transport, as `lnsd --writeconfig` writes it.
        .enable_transport(false)
        .share_instance(true)
        .instance_name(instance.clone())
        .storage_path(config_dir.path().join("storage"));
    let mut node = tcp(builder).build_sync().expect("build daemon");
    node.start().await.expect("start daemon");
    Daemon {
        node,
        instance,
        config_dir,
    }
}
