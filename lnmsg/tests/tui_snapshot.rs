//! The messenger (`lnmsg`, and its `lnmsg tui` alias) through the real
//! binary, scripted, against a real daemon: open the conversation, type two lines (`Alt-Enter` between them), send,
//! watch the delivery mark move from `○` to `✓`, read the two-line reply. The frames the script asks for are pinned in
//! `tests/fixtures/tui_session.frames`.
//!
//! The keymap's sessions (`docs/src/concepts/lnmsg-keys-and-layout.md`,
//! section 7, slice 1) need no correspondent: one driven only by vi
//! spellings and one only by Emacs spellings reach the same frames, the
//! enhanced and the plain keyboard reach the same two-line body, `Ctrl-c`
//! outside compose says what quits and the session goes on, and
//! `send_on_enter` off swaps `Enter` and `Alt-Enter`. Pinned in
//! `tests/fixtures/tui_keys.frames`.
//!
//! The chat layout's session (section 7, slice 2) runs one exchange at 80 x
//! 24 and at 120 x 40, the two mockups' sizes: a message from the peer on
//! one day, ours on it and on the next, the peer's two replies arriving while
//! the reader is scrolled up (the `new` rule above them, the view not moved),
//! back to the newest, a reply, and the detail view of our first message with
//! its proof. Pinned in `tests/fixtures/tui_chat.frames`.
//!
//! The mouse's sessions (section 7, slice 3) click a conversation, the
//! compose box, `send` on its border, a message, its mark and the footer's
//! `? help`; turn the wheel over the conversation while the list has the
//! focus; and drag across two messages, whose release copies them by OSC 52.
//! Pinned in `tests/fixtures/tui_mouse.frames`.
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
use lnmsg::fanout::Fanout;
use lnmsg::outbox::{SendRequest, Via};
use lnmsg::send::{run_send, SendOptions};

mod support;

use support::daemon;

const LNMSG: &str = env!("CARGO_BIN_EXE_lnmsg");
const EXPECTED: &str = include_str!("fixtures/tui_session.frames");
const EXPECTED_LATE: &str = include_str!("fixtures/tui_late.frames");
const EXPECTED_KEYS: &str = include_str!("fixtures/tui_keys.frames");
const EXPECTED_CHAT: &str = include_str!("fixtures/tui_chat.frames");
const EXPECTED_MOUSE: &str = include_str!("fixtures/tui_mouse.frames");

const SCRIPT: &str = "\
until ready 20
frame attached
# conversations -> messages -> compose
key enter
key i
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
key i
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
/// so the row reads `direct 2 s` however long the host took. And a script
/// that quits on the proof still leaves the direct leg's `sending` and
/// `proof` lines in `tui.log`: before leviculum 1014 the runner was cut off
/// before it read them (the late-recipient arm of periculum 1013).
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
    // The bare form, plain `lnmsg` with the messenger's options; the session
    // above runs the `tui` alias, so both spellings reach a daemon.
    let args: Vec<String> = vec![
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

    let to = format!("to={}", &their_hex[..8]);
    let steps: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("SEND leg=direct") && line.contains(&to))
        .filter_map(|line| line.split(" state=").nth(1))
        .filter_map(|rest| rest.split(' ').next())
        .collect();
    assert_eq!(
        steps,
        ["path_wait", "sending", "proof"],
        "the direct leg's steps in tui.log:\n{log}"
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

/// The vi reader's session: `j`/`k`/`G`/`g`, `i` to write, arrows to edit,
/// `Esc` back out, `:` for the palette.
const VI_SCRIPT: &str = "\
until ready 20
frame attached
key j
frame second
key k
key G
key g
frame first
key enter
frame messages
key i
type helo
key left
type l
frame compose
key esc
key esc
frame list
key :
type help
key enter
frame help
key esc
frame closed
key q
";

/// The same session in Emacs spellings: `Ctrl-n`/`Ctrl-p`, `Alt->`/`Alt-<`,
/// `Ctrl-b` to edit, `Ctrl-g` back out, `Alt-x` for the palette. Compose is
/// `Tab`, since Emacs has no key that means "start writing".
const EMACS_SCRIPT: &str = "\
until ready 20
frame attached
key ctrl-n
frame second
key ctrl-p
key alt->
key alt-<
frame first
key enter
frame messages
key tab
type helo
key ctrl-b
type l
frame compose
key ctrl-g
key ctrl-g
frame list
key alt-x
type help
key enter
frame help
key ctrl-g
frame closed
key ctrl-q
";

/// The enhanced world: `Shift-Enter` breaks the line, `Ctrl-Enter` sends,
/// and `Ctrl-c` outside compose says what quits without quitting.
const ENHANCED_SCRIPT: &str = "\
until ready 20
keyboard enhanced
key enter
key i
type one
key shift-enter
type two
frame body
key ctrl-enter
frame sent
key esc
key esc
key ctrl-c
frame ctrl-c
key j
frame still here
key q
";

/// The plain world: `Alt-Enter` breaks the line.
const PLAIN_ALT_SCRIPT: &str = "\
until ready 20
key enter
key i
type one
key alt-enter
type two
frame body
key esc
key esc
key q
";

/// The plain world: `Ctrl-j` breaks the line too.
const PLAIN_CTRL_J_SCRIPT: &str = "\
until ready 20
key enter
key i
type one
key ctrl-j
type two
frame body
key esc
key esc
key q
";

/// `send_on_enter` off from the palette: `Enter` breaks the line,
/// `Alt-Enter` sends, and the hint says so.
const ENTER_OFF_SCRIPT: &str = "\
until ready 20
key :
type send on enter
key enter
frame switched
key enter
key i
type one
key enter
type two
frame body
key alt-enter
frame sent
key esc
key esc
key q
";

/// Run one script against `daemon` with two correspondents that never
/// answer, and hand back its frames.
async fn run_keys_script(daemon: &support::Daemon, name: &str, script: &str) -> String {
    run_script_at(daemon, name, script, "80x16").await.0
}

/// The same at `size`, with `tui.log` beside the frames.
async fn run_script_at(
    daemon: &support::Daemon,
    name: &str,
    script: &str,
    size: &str,
) -> (String, String) {
    let home = tempfile::tempdir().expect("tui home");
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&fixed_identity(0x11)).expect("storable"),
    )
    .expect("write the identity");
    let peer = |seed: u8| -> String {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        address.iter().map(|b| format!("{b:02x}")).collect()
    };
    let script_path = home.path().join(format!("{name}.script"));
    let frames = home.path().join(format!("{name}.frames"));
    std::fs::write(&script_path, script).expect("write the script");
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        peer(0x22),
        "--to".into(),
        peer(0x33),
        "--script".into(),
        script_path.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        size.into(),
    ];
    let child_home = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
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
    })
    .await
    .expect("the lnmsg child thread");
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    let log = std::fs::read_to_string(home.path().join("tui.log")).unwrap_or_default();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{name}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The script runner says once which world it starts in.
    assert!(
        log.contains("KEYS enhancement=no newline=Alt-Enter send=Enter"),
        "{name}: tui.log:\n{log}"
    );
    (got, log)
}

/// A frame without the compose pane's border, which carries the hint.
fn without_hint(frame: &str) -> Vec<&str> {
    frame
        .lines()
        .filter(|line| !line.contains(" send · "))
        .collect()
}

/// The frame labelled `label` in a frames file.
fn frame<'a>(frames: &'a str, label: &str) -> &'a str {
    let marker = format!(": {label} ---\n");
    let start = frames
        .find(&marker)
        .map(|at| at + marker.len())
        .unwrap_or_else(|| panic!("no frame {label:?} in:\n{frames}"));
    let end = frames[start..]
        .find("--- frame ")
        .map_or(frames.len(), |at| start + at);
    &frames[start..end]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_keymap_sessions_reach_the_same_screens_in_every_spelling() {
    let daemon = daemon("tui-keys", |b| b).await;
    let mut all = String::new();
    let mut sessions = Vec::new();
    for (name, script) in [
        ("vi", VI_SCRIPT),
        ("emacs", EMACS_SCRIPT),
        ("enhanced", ENHANCED_SCRIPT),
        ("plain-alt-enter", PLAIN_ALT_SCRIPT),
        ("plain-ctrl-j", PLAIN_CTRL_J_SCRIPT),
        ("enter-off", ENTER_OFF_SCRIPT),
    ] {
        let frames = run_keys_script(&daemon, name, script).await;
        all.push_str(&format!("=== {name} ===\n{frames}"));
        sessions.push(frames);
    }
    let [vi, emacs, enhanced, plain_alt, plain_ctrl_j, enter_off] = sessions
        .try_into()
        .unwrap_or_else(|_| panic!("six sessions"));

    // vi and Emacs reach the same screens, frame for frame.
    assert_eq!(vi, emacs, "the vi and the Emacs session differ");

    // The two-line body is the same in both worlds; only the hint differs,
    // and it names the world's own chord.
    let body = frame(&enhanced, "body");
    assert!(body.contains("Enter send · Shift-Enter newline"), "{body}");
    let plain = frame(&plain_alt, "body");
    assert!(plain.contains("Enter send · Alt-Enter newline"), "{plain}");
    assert_eq!(without_hint(body), without_hint(plain));
    assert_eq!(plain, frame(&plain_ctrl_j, "body"));
    assert!(frame(&enhanced, "sent").contains("you: one"), "{enhanced}");

    // Ctrl-c outside compose says what quits, and the session goes on.
    assert!(
        frame(&enhanced, "ctrl-c").contains("Ctrl-q quits"),
        "{enhanced}"
    );
    assert!(
        frame(&enhanced, "still here").contains("lnmsg"),
        "{enhanced}"
    );

    // send_on_enter off: Enter broke the line, Alt-Enter sent.
    let off = frame(&enter_off, "body");
    assert!(off.contains("Alt-Enter send · Enter newline"), "{off}");
    assert!(off.contains("│one"), "{off}");
    assert!(
        frame(&enter_off, "sent").contains("you: one"),
        "{enter_off}"
    );

    if all != EXPECTED_KEYS {
        let kept = std::env::temp_dir().join("lnmsg-tui-keys.frames");
        let _ = std::fs::write(&kept, &all);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             lnmsg/tests/fixtures/tui_keys.frames\n{all}",
            kept.display()
        );
    }
}

/// The chat session. Every time on screen comes from a `clock` step, and the
/// test sends each of the peer's messages only once the frame before it is
/// drawn, so each arrives under its own clock.
const CHAT_SCRIPT: &str = "\
until ready 20
frame attached
clock 2026-10-24 13:58
until received 1 30
clock 2026-10-24 14:02
key enter
key i
type yes, from ten. Shall I bring the T114 and the long antenna?
key enter
until delivery delivered 30
clock 2026-10-26 09:12
type on my way
key enter
until delivery delivered 30
key esc
key k
key k
tick 12
frame reading
clock 2026-10-26 14:04
until received 2 30
tick 12
frame first reply
clock 2026-10-26 14:05
until received 3 30
tick 12
frame new while reading
key G
frame newest
clock 2026-10-26 14:06
key i
type the tripod fits
key enter
until delivery delivered 30
tick 12
frame replied
key esc
key g
key j
key enter
frame detail
key esc
key q
";

/// A frame's lines with the message ids, which hash the LXMF timestamp and so
/// differ on every run, written as `x`s of the same width.
fn without_ids(frames: &str) -> String {
    let mut out = String::new();
    for line in frames.lines() {
        let masked = match (line.find("id     "), line.find("router as ")) {
            (Some(at), _) => {
                let start = at + "id     ".len();
                format!(
                    "{}{}{}",
                    &line[..start],
                    "x".repeat(64),
                    &line[start + 64..]
                )
            }
            (None, Some(at)) => {
                let start = at + "router as ".len();
                format!("{}{}{}", &line[..start], "x".repeat(8), &line[start + 8..])
            }
            (None, None) => line.to_string(),
        };
        out.push_str(&masked);
        out.push('\n');
    }
    out
}

/// One chat session at `size` against its own daemon, the peer answering on
/// the script's frames.
async fn chat_session(size: &str) -> String {
    let daemon = daemon(&format!("tui-chat-{size}"), |b| b).await;
    let home = tempfile::tempdir().expect("tui home");
    let ours = fixed_identity(0x11);
    std::fs::write(
        home.path().join("identity"),
        encode_identity(&ours).expect("storable"),
    )
    .expect("write the identity");
    let our_address = delivery_address(&ours).expect("our address");
    let theirs = fixed_identity(0x22);
    let hex = |seed: u8| -> String {
        let address = delivery_address(&fixed_identity(seed)).expect("an address");
        address.iter().map(|b| format!("{b:02x}")).collect()
    };
    let script = home.path().join("chat.script");
    let frames = home.path().join("chat.frames");
    std::fs::write(&script, CHAT_SCRIPT).expect("write the script");
    // A second correspondent that never answers, so the list has two rows.
    let args: Vec<String> = vec![
        "--instance".into(),
        daemon.instance.clone(),
        "--config".into(),
        daemon.config_dir.path().display().to_string(),
        "--to".into(),
        hex(0x22),
        "--to".into(),
        hex(0x33),
        "--script".into(),
        script.display().to_string(),
        "--frames".into(),
        frames.display().to_string(),
        "--size".into(),
        size.into(),
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
    let options = SendOptions::new(daemon.instance.clone(), Duration::from_secs(20));
    // The engine says `Ready` once, and each send waits for it: the sends go
    // through taps of one fan-out, as the TUI's own runners do.
    let mut fanout = Fanout::default();
    for (after, body) in [
        (None, "are you at the field site tomorrow?"),
        (Some("reading"), "ok, bring the T114."),
        (
            Some("first reply"),
            "and the small tripod\nif it fits in the bag",
        ),
    ] {
        if let Some(label) = after {
            wait_for_frame(&frames, label, Duration::from_secs(60)).await;
        }
        let mut tap = fanout.tap();
        let clock = Tokio::new();
        let send = run_send(
            &mut tap,
            SendRequest {
                destination: our_address,
                title: Vec::new(),
                body: body.as_bytes().to_vec(),
                via: Via::Direct,
            },
            &options,
            &clock,
        );
        tokio::pin!(send);
        let sent = loop {
            tokio::select! {
                sent = &mut send => break sent,
                _ = tokio::time::sleep(Duration::from_millis(20)) => {
                    let _ = fanout.pump(&mut them.outbox);
                }
            }
        };
        assert!(sent.is_ok(), "{body:?} did not go out: {sent:?}");
    }

    let output = child.await.expect("the lnmsg child thread");
    them.stop().await;
    let got = std::fs::read_to_string(&frames).expect("read the frames");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{size}: stderr: {}\nframes:\n{got}",
        String::from_utf8_lossy(&output.stderr)
    );
    got
}

#[tokio::test(flavor = "multi_thread")]
async fn the_chat_layout_draws_the_mockups_at_both_sizes() {
    let mut all = String::new();
    for size in ["80x24", "120x40"] {
        let frames = chat_session(size).await;
        // Read while scrolled up: the rule above the replies, and the block
        // being read still selected and on screen. (The whole exchange fits
        // both panes, so it is drawn from the bottom and grows upward.)
        let reading = frame(&frames, "reading");
        let first = frame(&frames, "first reply");
        let new = frame(&frames, "new while reading");
        assert!(first.contains(" 1 new "), "{size}: {first}");
        assert!(new.contains(" 2 new "), "{size}: {new}");
        let selected = |f: &str| -> Vec<String> {
            f.lines()
                .filter_map(|row| row.split_once("││").map(|(_, pane)| pane))
                .filter(|pane| pane.contains('▌'))
                .map(str::to_string)
                .collect()
        };
        assert!(!selected(reading).is_empty(), "{size}: {reading}");
        assert_eq!(selected(reading), selected(new), "{size}: the cursor moved");
        assert!(frame(&frames, "newest").contains("if it fits in the bag"));
        let detail = frame(&frames, "detail");
        assert!(
            detail.contains("delivered: proof received"),
            "{size}: {detail}"
        );
        assert!(detail.contains("from you to <"), "{size}: {detail}");
        all.push_str(&format!("=== {size} ===\n{}", without_ids(&frames)));
    }
    if all != EXPECTED_CHAT {
        let kept = std::env::temp_dir().join("lnmsg-tui-chat.frames");
        let _ = std::fs::write(&kept, &all);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             lnmsg/tests/fixtures/tui_chat.frames\n{all}",
            kept.display()
        );
    }
}

/// The mouse (section 7, slice 3), by clicks alone: the second
/// conversation's row, the compose box, `send` on its border (the send
/// goes out: its leg is in `tui.log`), the message, its mark (the detail
/// view), outside the view (closed), and the footer's `? help`. Cells count
/// from 1, as the frames do.
const CLICK_SCRIPT: &str = "\
until ready 20
frame attached
click 5 5
frame second conversation
click 40 22
type hello by mouse
frame typed
click 55 21
frame sent by click
click 60 20
frame message selected
click 78 20
frame detail
click 2 2
frame closed
click 62 24
frame help
key esc
key q
";

/// The wheel over the conversation while the list has the focus: the
/// conversation scrolls six rows for two notches, the focus and the list
/// stay, and two notches back are the newest again.
const WHEEL_SCRIPT: &str = "\
until ready 20
key i
type message 1
key enter
type message 2
key enter
type message 3
key enter
type message 4
key enter
type message 5
key enter
type message 6
key enter
type message 7
key enter
type message 8
key enter
type message 9
key enter
type message 10
key enter
key esc
key esc
frame list focused
wheel up 50 10 2
frame wheeled up
wheel down 50 10 2
frame wheeled down
key q
";

/// A drag across two of our messages copies their text by OSC 52, without
/// the times and the marks.
const DRAG_SCRIPT: &str = "\
until ready 20
key i
type first line
key enter
type second line
key enter
key esc
frame two sent
drag 60 18 69 20
frame dragged
tick 12
frame selected
key q
";

#[tokio::test(flavor = "multi_thread")]
async fn the_mouse_clicks_wheels_and_drags_through_the_screen() {
    let daemon = daemon("tui-mouse", |b| b).await;
    let mut all = String::new();
    let mut runs = Vec::new();
    for (name, script) in [
        ("click", CLICK_SCRIPT),
        ("wheel", WHEEL_SCRIPT),
        ("drag", DRAG_SCRIPT),
    ] {
        let (frames, log) = run_script_at(&daemon, name, script, "80x24").await;
        all.push_str(&format!("=== {name} ===\n{frames}"));
        runs.push((frames, log));
    }
    let [(click, click_log), (wheel, _), (drag, _)] =
        runs.try_into().unwrap_or_else(|_| panic!("three sessions"));

    // The click on the second row opened it; the one on `send` sent, the
    // runner's first leg is in the log.
    assert!(
        frame(&click, "second conversation").contains("│▌<5ec0ea2e…>"),
        "{click}"
    );
    assert!(
        frame(&click, "sent by click").contains("hello by mouse  12:00 ○"),
        "{click}"
    );
    assert!(
        click_log.contains("SEND leg=direct to=5ec0ea2e"),
        "tui.log:\n{click_log}"
    );
    assert!(frame(&click, "message selected").contains("▌hello by mouse"));
    assert!(frame(&click, "detail").contains("from you to <5ec0ea2e…>"));
    assert!(!frame(&click, "closed").contains("from you to"));
    assert!(frame(&click, "help").contains("lnmsg help"), "{click}");

    // The wheel moved the conversation and nothing else.
    let up = frame(&wheel, "wheeled up");
    assert_ne!(up, frame(&wheel, "list focused"));
    assert!(up.contains("● lnmsg"), "the list kept the focus: {up}");
    assert!(!up.contains("message 10  12:00"), "{up}");
    assert_eq!(frame(&wheel, "wheeled down"), frame(&wheel, "list focused"));

    // The drag's release copied the two lines and only them.
    let copied = format!(
        "--- osc52 {} ---",
        leviculum_ui::clipboard::osc52("first line\nsecond line").replace('\x1b', "\\e")
    );
    assert!(drag.contains(&copied), "{copied}\n{drag}");
    assert!(
        frame(&drag, "dragged").contains("copied 22 chars"),
        "{drag}"
    );
    if all != EXPECTED_MOUSE {
        let kept = std::env::temp_dir().join("lnmsg-tui-mouse.frames");
        let _ = std::fs::write(&kept, &all);
        panic!(
            "the frames moved; this run's are in {}, the pinned ones in \
             lnmsg/tests/fixtures/tui_mouse.frames\n{all}",
            kept.display()
        );
    }
}
