//! `lnmsg tui` through the real binary, scripted, against a real daemon:
//! open the conversation, type two lines (`Alt-Enter` between them), send,
//! watch the delivery mark move from `○` to `✓`, read the two-line reply. The frames the script asks for are pinned in
//! `tests/fixtures/tui_session.frames`.
//!
//! The daemon is the in-process one `mailbox_cli.rs` uses. The correspondent
//! is a second lnmsg engine attached to it in this process
//! (`lnmsg::engine::attach`), a full LXMF router, so the proof that turns the
//! mark green is a real one. Both identities are fixed, which is what makes
//! the addresses on the screen, and so the frames, the same on every run.
//!
//! The order matters once: the correspondent attaches after the TUI is ready,
//! so its one announce reaches an engine that is already listening.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use leviculum_core::identity_store::encode_identity;
use leviculum_std::Identity;
use lnmsg::clock::Tokio;
use lnmsg::effects::Terminal;
use lnmsg::engine::{attach, delivery_address, AttachConfig};
use lnmsg::outbox::{SendRequest, Via};
use lnmsg::send::{run_send, SendOptions};

mod support;

use support::daemon;

const LNMSG: &str = env!("CARGO_BIN_EXE_lnmsg");
const EXPECTED: &str = include_str!("fixtures/tui_session.frames");
const EXPECTED_LATE: &str = include_str!("fixtures/tui_late.frames");

const SCRIPT: &str = "\
until ready 20
frame attached
# conversations -> messages -> compose
key enter
key enter
type hello from the tui
newline
type on two lines
frame typed
key enter
frame queued
until delivery delivered 30
frame delivered
until received 1 30
frame reply
key esc
key q
";

/// A send to a correspondent that is not there yet: the row carries the
/// direct leg and its seconds, then the correspondent attaches and the proof
/// lands, and the script leaves in the pump that showed it.
const LATE_SCRIPT: &str = "\
until ready 20
frame attached
key enter
key enter
type are you there
key enter
until leg direct 5
sleep 2500
frame waiting
until delivery delivered 30
frame delivered
key esc
key q
";

fn fixed_identity(seed: u8) -> Identity {
    Identity::from_private_key_bytes(&[seed; 64]).expect("a 64-byte key")
}

/// Wait for the frames file to hold a frame with this label.
async fn wait_for_frame(frames: &Path, label: &str, within: Duration) {
    let deadline = Instant::now() + within;
    let marker = format!(": {label} ---");
    loop {
        let text = std::fs::read_to_string(frames).unwrap_or_default();
        if text.contains(&marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the TUI never drew {label:?}; frames so far:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scripted_session_sends_sees_the_proof_and_reads_the_reply() {
    let daemon = daemon("tui", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let their_address = delivery_address(&theirs).expect("their address");
    let their_hex: String = their_address.iter().map(|b| format!("{b:02x}")).collect();

    let script = home.path().join("session.script");
    let frames = home.path().join("session.frames");
    std::fs::write(&script, SCRIPT).expect("write the script");

    let args: Vec<String> = vec![
        "tui".into(),
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex,
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn lnmsg");
        child.wait_with_output().expect("wait for lnmsg")
    });

    wait_for_frame(&frames, "attached", Duration::from_secs(20)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let mut them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");

    wait_for_frame(&frames, "delivered", Duration::from_secs(40)).await;
    let reply = run_send(
        &mut them.outbox,
        SendRequest {
            destination: our_address,
            title: Vec::new(),
            body: b"hello back\nalso on two lines".to_vec(),
            via: Via::Direct,
        },
        &SendOptions::new(daemon.instance.clone(), Duration::from_secs(20)),
        &Tokio::new(),
    )
    .await;
    assert!(reply.is_ok(), "the reply did not go out: {reply:?}");

    let output = child.await.expect("the lnmsg child thread");
    them.stop().await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {stderr}\nframes:\n{got}"
    );
    if got != EXPECTED {
        // Kept where a reader can diff it and, when the change is on purpose,
        // copy it over the fixture.
        let kept = std::env::temp_dir().join("lnmsg-tui-session.frames");
        let _ = std::fs::write(&kept, &got);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             lnmsg/tests/fixtures/tui_session.frames\n{got}",
            kept.display()
        );
    }
}

/// The leg label's seconds come from the script's clock: `until leg` applies
/// no tick and `sleep 2500` applies ten, one per 250 ms of its own length,
/// so the row reads `direct 2 s` however long the host took.
#[tokio::test(flavor = "multi_thread")]
async fn a_late_recipient_shows_the_direct_leg_and_the_log_keeps_its_steps() {
    let daemon = daemon("tui-late", |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let theirs = fixed_identity(0x22);
    let their_address = delivery_address(&theirs).expect("their address");
    let their_hex: String = their_address.iter().map(|b| format!("{b:02x}")).collect();

    let script = home.path().join("late.script");
    let frames = home.path().join("late.frames");
    std::fs::write(&script, LATE_SCRIPT).expect("write the script");
    let args: Vec<String> = vec![
        "tui".into(),
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        their_hex.clone(),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        "80x16".into(),
    ];
    let child_home = home.path().to_path_buf();
    let child = tokio::task::spawn_blocking(move || {
        let mut command = Command::new(LNMSG);
        command
            .args(&args)
            .env("LNMSG_HOME", &child_home)
            .env("LNMSG_DISPLAY_NAME", "tui")
            .env_remove("LEVICULUM_EVENT_LOG")
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = leviculum_std::process::spawn_supervised(command).expect("spawn lnmsg");
        child.wait_with_output().expect("wait for lnmsg")
    });

    // Only once the label is on screen does the correspondent appear.
    wait_for_frame(&frames, "waiting", Duration::from_secs(30)).await;
    let storage = tempfile::tempdir().expect("their storage");
    let them = attach(AttachConfig {
        instance: daemon.instance.clone(),
        storage_dir: storage.path().to_path_buf(),
        identity: theirs,
        display_name: b"them".to_vec(),
        notices: Terminal::new(storage.path().to_path_buf()),
    })
    .await
    .expect("the correspondent attaches");

    let output = child.await.expect("the lnmsg child thread");
    them.stop().await;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {stderr}\nframes:\n{got}\ntui.log:\n{log}"
    );

    if got != EXPECTED_LATE {
        let kept = std::env::temp_dir().join("lnmsg-tui-late.frames");
        let _ = std::fs::write(&kept, &got);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             lnmsg/tests/fixtures/tui_late.frames\n{got}",
            kept.display()
        );
    }
}
