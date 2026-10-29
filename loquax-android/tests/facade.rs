//! The facade the app's Java calls, on the host and without JNI: the bytes
//! in and out of `start`, `command`, `query` and `nextEvent`, and a session
//! against a real home node (`loquax::home::daemon`, in process) on a Rust
//! transport node with a TCP server, the leaf dialling in as the phone does.

use std::path::Path;
use std::time::Duration;

use leviculum_std::driver::{ReticulumNode, ReticulumNodeBuilder};
use loquax_android::schema::{self, Event, Request, Start, Started};
use loquax_android::session::Session;

/// The exported facade keeps one session per process, and the tests of a
/// binary share the process: those that go through it take turns. Tokio's
/// lock, which the async test holds across its awaits.
static FACADE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn started(bytes: &[u8]) -> Started {
    rmp_serde::from_slice(bytes).expect("a Started")
}

fn start_bytes(start: &Start) -> Vec<u8> {
    schema::encode(start)
}

/// The ways a first start goes wrong, each with what the join screen asks
/// for and words for it. Through the exported facade, which keeps no
/// session after a failed start.
#[test]
fn a_first_start_says_what_it_needs() {
    let _facade = FACADE.blocking_lock();
    let dir = tempfile::tempdir().expect("app dir");
    let base = Start {
        dir: dir.path().display().to_string(),
        ..Start::default()
    };

    let answer = started(&loquax_android::start(&start_bytes(&base)));
    assert!(!answer.ok);
    assert_eq!(answer.need.as_deref(), Some("code"));
    assert!(answer.error.unwrap().contains("not joined"));

    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some("lqxh1nonsense".to_string()),
        ..base.clone()
    })));
    assert_eq!(answer.need.as_deref(), Some("code"));
    assert!(answer.error.unwrap().starts_with("That is not a home code"));

    let code = code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng));
    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code.clone()),
        ..base.clone()
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("host:port"));

    let answer = started(&loquax_android::start(&start_bytes(&Start {
        code: Some(code),
        server: Some("homenode.lan".to_string()),
        ..base
    })));
    assert_eq!(answer.need.as_deref(), Some("server"));
    assert!(answer.error.unwrap().contains("has no port"));

    let answer = started(&loquax_android::start(b"\xc1"));
    assert!(!answer.ok);
    assert!(answer.error.unwrap().contains("did not decode"));

    // Nothing runs, and the calls say so instead of failing.
    let done: schema::Done =
        rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Pull)))
            .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    let done: schema::Done = rmp_serde::from_slice(&loquax_android::query(&schema::encode(
        &schema::Query::Conversations,
    )))
    .expect("a Done");
    assert_eq!(done.error.as_deref(), Some("not started"));
    let event: Event =
        rmp_serde::from_slice(&loquax_android::next_event(Duration::ZERO).expect("an event"))
            .expect("an Event");
    assert!(matches!(event, Event::Stopped { .. }), "{event:?}");
    // No join file, no server file: nothing was written for a failed start.
    assert!(!dir.path().join(loquax::home::client::CLIENT_FILE).exists());
    assert!(!dir
        .path()
        .join(loquax_android::session::SERVER_FILE)
        .exists());
}

/// An admission code for a fresh token of `identity`'s home, not issued
/// anywhere: enough for the checks before any network.
fn code_for(identity: &leviculum_core::Identity) -> String {
    loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token: [7; 16],
    }
    .encode()
}

/// A token issued for "phone" in the store of the home node of `home`, and
/// the code that carries it with the home identity's public key.
fn admit(home: &Path) -> String {
    let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home, address).expect("the home store");
    let token = [0x42; 16];
    store
        .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
        .expect("a token");
    loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token,
    }
    .encode()
}

/// Wait for a query's answer to satisfy `ok`, reading events meanwhile.
fn wait_for<T: serde::de::DeserializeOwned + std::fmt::Debug>(
    session: &Session,
    query: schema::Query,
    secs: u64,
    ok: impl Fn(&T) -> bool,
) -> T {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        let answer: T =
            rmp_serde::from_slice(&session.query(query.clone()).expect("a query")).expect("decode");
        if ok(&answer) {
            return answer;
        }
        assert!(std::time::Instant::now() < deadline, "never: {answer:?}");
        let _ = session.next_event(Duration::from_millis(200));
    }
}

/// The phone end to end: the first start joins with the code over TCP and
/// keeps the client and the server; a send goes through the home node,
/// which names the bubble; a pull runs; the conversation list shows it. The
/// second start needs neither code nor server and shows the conversation
/// from the store. A second code on a joined phone is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_joins_sends_pulls_and_comes_back() {
    let instance = instance();
    // The transport's Reticulum config directory, its storage under
    // `storage/`, as lnsd keeps it: the home node takes its RPC key there.
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let first = Start {
        dir: dir.clone(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code.clone()),
        name: Some("Pixel".to_string()),
        offset: 3600,
    };
    let peer = loquax::address::to_hex(&[0x5a; 16]);
    let (address, peer) = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        let address = loquax::address::to_hex(&session.address());
        session
            .request(Request::Send {
                peer: peer.clone(),
                body: "hallo vom Telefon".to_string(),
            })
            .expect("send");
        // The home node took it: the bubble carries its id.
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages.last().is_some_and(|b| !b.id.is_empty()),
        );
        let bubble = messages.messages.last().expect("the bubble");
        assert!(bubble.ours);
        assert_eq!(bubble.body, "hallo vom Telefon");
        assert_ne!(bubble.state, "failed", "{bubble:?}");
        // A reply to it (1137): sent with the target's id, which the row
        // reads back out of its own fields, and the target's text quoted.
        let target = bubble.id.clone();
        session
            .request(Request::Reply {
                peer: peer.clone(),
                target: target.clone(),
                body: "und die Antwort".to_string(),
            })
            .expect("reply");
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages.len() == 2 && !m.messages[1].id.is_empty(),
        );
        let reply = &messages.messages[1];
        assert_eq!(reply.body, "und die Antwort");
        assert_eq!(reply.quote_to.as_deref(), Some(target.as_str()));
        assert_eq!(reply.quote.as_deref(), Some("hallo vom Telefon"));
        assert_eq!(reply.quote_ours, Some(true));
        assert!(messages.home_client);
        let info: schema::Info = rmp_serde::from_slice(
            &session
                .query(schema::Query::Info {
                    peer: peer.clone(),
                    key: reply.key,
                })
                .expect("info"),
        )
        .expect("decode");
        assert!(info.rows.iter().any(|r| r.contains(&reply.id)), "{info:?}");
        // The reaction sent and the emoji picked are the recents, newest
        // first, kept in the app's directory.
        session
            .request(Request::React {
                peer: peer.clone(),
                target: target.clone(),
                content: "\u{1F44D}".to_string(),
            })
            .expect("react");
        session
            .request(Request::Used {
                emoji: "\u{1F600}".to_string(),
            })
            .expect("used");
        let recents: schema::Recents =
            rmp_serde::from_slice(&session.query(schema::Query::Recents).expect("recents"))
                .expect("decode");
        assert_eq!(recents.emoji, ["\u{1F600}", "\u{1F44D}"]);
        let messages: schema::Messages = wait_for(
            &session,
            schema::Query::Messages { peer: peer.clone() },
            60,
            |m: &schema::Messages| m.messages[0].chips.len() == 1,
        );
        let chip = &messages.messages[0].chips[0];
        assert_eq!((chip.content.as_str(), chip.ours), ("\u{1F44D}", true));
        // A peer without a name: the avatar shows its address's first byte,
        // in the app's colour since it sent no icon appearance (1129).
        assert_eq!(messages.initials, peer[..2].to_uppercase());
        assert_eq!(
            (messages.icon_fg.as_deref(), messages.icon_bg.as_deref()),
            (None, None)
        );

        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("the pull failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "no pull ended"),
            }
        }
        let list: schema::Conversations = wait_for(
            &session,
            schema::Query::Conversations,
            5,
            |c: &schema::Conversations| !c.conversations.is_empty(),
        );
        assert_eq!(list.conversations[0].peer, peer);
        assert_eq!(list.conversations[0].last, "you reacted \u{1F44D}");
        assert!(list.conversations[0].ours);
        assert_eq!(list.conversations[0].initials, peer[..2].to_uppercase());
        // The row's mark (1130): ours is last, so a delivery word, a text.
        assert_eq!(list.conversations[0].kind, "reaction");
        assert!(!list.conversations[0].state.is_empty());
        assert_eq!(list.initials.chars().count(), 2);
        assert_eq!(list.address.len(), 32);

        // Negative: an address that is none, and a message id that is none.
        assert!(session
            .request(Request::Open {
                peer: "zz".to_string()
            })
            .unwrap_err()
            .contains("not an address"));
        assert!(session
            .request(Request::React {
                peer: peer.clone(),
                target: "00".to_string(),
                content: "ok".to_string()
            })
            .unwrap_err()
            .contains("not a message id"));
        session.stop();
        (address, peer)
    })
    .await
    .expect("the first session");

    // The second start: no code, no server, the conversation from the store.
    let again = Start {
        dir: dir.clone(),
        ..Start::default()
    };
    let other = code_for(&leviculum_core::Identity::generate(&mut rand_core::OsRng));
    tokio::task::spawn_blocking(move || {
        let session = Session::start(&again).unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(loquax::address::to_hex(&session.address()), address);
        assert_eq!(session.server(), format!("127.0.0.1:{port}"));
        let messages: schema::Messages = rmp_serde::from_slice(
            &session
                .query(schema::Query::Messages { peer: peer.clone() })
                .expect("query"),
        )
        .expect("decode");
        assert_eq!(messages.messages.len(), 2, "{messages:?}");
        assert_eq!(messages.messages[0].body, "hallo vom Telefon");
        // The reply's field came back from the store with the message.
        assert_eq!(
            messages.messages[1].quote_to.as_deref(),
            Some(messages.messages[0].id.as_str())
        );
        session.stop();
        drop(session);

        let refused = Session::start(&Start {
            dir,
            code: Some(other),
            ..Start::default()
        })
        .err()
        .expect("another home's code");
        assert!(refused.text.contains("another home node"), "{refused:?}");
    })
    .await
    .expect("the second session");

    node.kill().await;
}

/// The wake on the phone (1123): a joined session, its open pull done,
/// then a loquax messenger on the home node's instance sends to the home
/// address. The session pulls on the home node's wake, with no `pull`
/// asked, says `changed`, and the conversation list shows the message.
/// Then the sender renames itself (1136): the status line hears nothing,
/// neither a wake's pull nor a count, and opening the conversation pulls
/// quietly and brings the new name.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_message_shows_on_the_phone_without_a_pull() {
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let first = Start {
        dir: phone.path().display().to_string(),
        server: Some(format!("127.0.0.1:{port}")),
        code: Some(code),
        name: Some("Pixel".to_string()),
        offset: 0,
    };
    let session = tokio::task::spawn_blocking(move || {
        let session = Session::start(&first).unwrap_or_else(|error| panic!("{error:?}"));
        // The pull on open is quiet; a Refresh says its end, and the
        // watching link's first word settles.
        session.request(Request::Pull).expect("pull");
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            match session.next_event(Duration::from_millis(500)) {
                Some(Event::Sync { state, .. }) if state == "done" => break,
                Some(Event::Sync { state, detail, .. }) if state == "failed" => {
                    panic!("the pull on open failed: {detail}")
                }
                _ => assert!(std::time::Instant::now() < deadline, "no pull ended"),
            }
        }
        std::thread::sleep(Duration::from_secs(3));
        while session.next_event(Duration::ZERO).is_some() {}
        session
    })
    .await
    .expect("the session");
    let address = session.address();

    let sender = tempfile::tempdir().expect("sender dir");
    let storage = sender.path().join("storage");
    std::fs::create_dir_all(&storage).expect("sender storage");
    let sender_identity = leviculum_core::Identity::generate(&mut rand_core::OsRng);
    let sender_address = loquax::address::to_hex(
        &loquax::engine::delivery_address(&sender_identity).expect("address"),
    );
    let mut attached = loquax::engine::attach(loquax::engine::AttachConfig {
        instance: instance.clone(),
        config_dir: Some(rns.path().to_path_buf()),
        storage_dir: storage.clone(),
        identity: sender_identity.clone(),
        display_name: b"sender".to_vec(),
        notices: loquax::effects::Terminal::new(sender.path().to_path_buf()),
    })
    .await
    .expect("the sender attaches");
    let body = "ohne Ziehen";
    loquax::send::run_send(
        &mut attached.outbox,
        loquax::outbox::SendRequest {
            destination: address,
            title: Vec::new(),
            body: body.as_bytes().to_vec(),
            fields: Vec::new(),
            via: loquax::outbox::Via::Direct,
        },
        &loquax::send::SendOptions::new(instance.clone(), Duration::from_secs(40)),
        &loquax::clock::Tokio::new(),
    )
    .await
    .expect("the sender hands it on");
    let sent_at = std::time::Instant::now();

    let session = tokio::task::spawn_blocking(move || {
        let mut changed = false;
        // The wake's pull is quiet but for its result: one new message.
        let mut said_one = false;
        let mut look = |event: Option<Event>, changed: &mut bool| match event {
            Some(Event::Changed { .. }) => *changed = true,
            Some(Event::Sync { state, detail, .. }) => {
                assert_eq!((state.as_str(), detail.as_str()), ("done", "1"));
                said_one = true;
            }
            _ => {}
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            let list: schema::Conversations = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Conversations)
                    .expect("a query"),
            )
            .expect("decode");
            if changed && list.conversations.iter().any(|c| c.last == body) {
                eprintln!(
                    "WAKE phone shows it {} ms after the send returned",
                    sent_at.elapsed().as_millis()
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the message never showed without a pull: {list:?}"
            );
            look(session.next_event(Duration::from_millis(100)), &mut changed);
        }
        // The pull's end comes in the pump that showed the message, or the
        // next; nothing of this pull is left for the rename below.
        let settled = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < settled {
            look(session.next_event(Duration::from_millis(100)), &mut changed);
        }
        assert!(said_one, "the wake's pull said its one new message");
        session
    })
    .await
    .expect("the wait for the wake");
    attached.stop().await;

    // The sender comes back under another name and announces it.
    let renamed = loquax::engine::attach(loquax::engine::AttachConfig {
        instance: instance.clone(),
        config_dir: Some(rns.path().to_path_buf()),
        storage_dir: storage,
        identity: sender_identity,
        display_name: b"renamed".to_vec(),
        notices: loquax::effects::Terminal::new(sender.path().to_path_buf()),
    })
    .await
    .expect("the sender attaches again");
    loquax::outbox::Outbox::submit(&renamed.outbox, loquax::outbox::Command::Announce)
        .expect("announce");
    tokio::task::spawn_blocking(move || {
        // Whatever pulls run meanwhile: a name says nothing on the line.
        let silent = |event: Option<Event>| {
            assert!(
                !matches!(event, Some(Event::Sync { .. } | Event::Notice { .. })),
                "a name made the status line speak: {event:?}"
            );
        };
        let quiet_until = std::time::Instant::now() + Duration::from_secs(4);
        while std::time::Instant::now() < quiet_until {
            silent(session.next_event(Duration::from_millis(100)));
        }
        // Opening the conversation pulls, quietly, and the name comes. It
        // is opened again every two seconds, in case its pull found one
        // running that began before the name was archived.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut opened = std::time::Instant::now() - Duration::from_secs(2);
        loop {
            if opened.elapsed() >= Duration::from_secs(2) {
                session
                    .request(Request::Open {
                        peer: sender_address.clone(),
                    })
                    .expect("open");
                opened = std::time::Instant::now();
            }
            let messages: schema::Messages = rmp_serde::from_slice(
                &session
                    .query(schema::Query::Messages {
                        peer: sender_address.clone(),
                    })
                    .expect("a query"),
            )
            .expect("decode");
            // The label is the name with the screen's short-hash mark.
            if messages.name.starts_with("renamed") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the new name never came: {:?}",
                messages.name
            );
            silent(session.next_event(Duration::from_millis(100)));
        }
        session.stop();
    })
    .await
    .expect("the rename");
    renamed.stop().await;
    node.kill().await;
}

/// A shared-instance name of this test process's own.
fn instance() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "loquax-android-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// A home node: a Rust transport node with a TCP server on `listener`, its
/// config directory `rns`, and `loquax::home::daemon` of `home` on its
/// shared instance, both in this process.
struct HomeNode {
    transport: ReticulumNode,
    daemon: tokio::task::JoinHandle<Result<(), String>>,
}

impl HomeNode {
    async fn start(
        listener: std::net::TcpListener,
        instance: &str,
        rns: &Path,
        home: &Path,
    ) -> Self {
        let mut transport = ReticulumNodeBuilder::new()
            .enable_transport(true)
            .share_instance(true)
            .instance_name(instance.to_string())
            .add_tcp_server_listener(listener)
            .storage_path(rns.join("storage"))
            .build_sync()
            .expect("the transport node");
        transport.start().await.expect("start it");
        tokio::time::sleep(Duration::from_millis(500)).await;

        let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
        let daemon_storage = home.join("storage");
        std::fs::create_dir_all(&daemon_storage).expect("daemon storage");
        let daemon = tokio::spawn(loquax::home::daemon::run(
            loquax::home::daemon::DaemonOptions {
                home: home.to_path_buf(),
                instance: instance.to_string(),
                config_dir: rns.to_path_buf(),
                storage_dir: daemon_storage,
                identity,
                display_name: b"home".to_vec(),
                archive_cap_bytes: 1 << 20,
                token_ttl_secs: 3600,
                policy: loquax::home::policy(&loquax::config::Config::default()),
                preferred_pn: None,
                pn_source: "none",
                sync_bound_ms: None,
                fetch_budget: Duration::from_secs(10),
                mailbox: Box::new(|| {
                    Box::pin(async { loquax::mailbox_state::MailboxRun { input: None } })
                }),
                wake: true,
            },
        ));
        Self { transport, daemon }
    }

    /// Kill both, as a power cut does to the box they run on: the daemon
    /// gone with its lock, the TCP server closed.
    async fn kill(mut self) {
        self.daemon.abort();
        let _ = self.daemon.await;
        self.transport
            .stop()
            .await
            .expect("stop the transport node");
    }
}

/// The next event through the facade, up to `secs`, that `want` picks;
/// the others pass. A `stopped` that `want` does not pick fails the test.
fn facade_event(secs: u64, what: &str, want: impl Fn(&Event) -> bool) -> Event {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "no {what} in {secs} s"
        );
        let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) else {
            continue;
        };
        let event: Event = rmp_serde::from_slice(&bytes).expect("an Event");
        if want(&event) {
            return event;
        }
        assert!(
            !matches!(event, Event::Stopped { .. }),
            "stopped while waiting for {what}: {event:?}"
        );
    }
}

fn link_event(up: bool, secs: u64) {
    facade_event(
        secs,
        &format!("link up={up}"),
        |event| matches!(event, Event::Link { up: now, .. } if *now == up),
    );
}

/// Send `body` to `peer` through the facade and wait until the home node
/// took it: its bubble carries an id.
fn send_through_facade(peer: &str, body: &str, secs: u64) {
    let done: schema::Done =
        rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Send {
            peer: peer.to_string(),
            body: body.to_string(),
        })))
        .expect("a Done");
    assert!(done.ok, "{done:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    let mut notices = Vec::new();
    loop {
        let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Messages {
                peer: peer.to_string(),
            }),
        ))
        .expect("Messages");
        let bubble = messages.messages.iter().find(|m| m.body == body);
        if let Some(bubble) = bubble.filter(|b| !b.id.is_empty()) {
            assert_ne!(bubble.state, "failed", "{bubble:?}");
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the home node never took {body:?}: {messages:?}, notices {notices:?}"
        );
        if let Some(bytes) = loquax_android::next_event(Duration::from_millis(200)) {
            if let Ok(Event::Notice { text, .. }) = rmp_serde::from_slice(&bytes) {
                notices.push(text);
            }
        }
    }
}

/// The phone's session over a home node that goes away and comes back,
/// and over an engine that dies, through the exported facade as Java
/// calls it. The first start takes the code with the address behind it
/// and no server. The link event says when the TCP interface is up; the
/// home node killed, it says down, and the leaf's TCP client redials the
/// home node brought back on the same port, so a send goes through again
/// in the same session. The engine dead, the library says `stopped` with
/// why and drops the session; the service's next start, with neither code
/// nor server, builds a fresh one, and a send goes through that.
#[tokio::test(flavor = "multi_thread")]
async fn a_phone_session_rides_out_a_dead_home_node_and_a_dead_engine() {
    let _facade = FACADE.lock().await;
    let instance = instance();
    let rns = tempfile::tempdir().expect("transport config dir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let home = tempfile::tempdir().expect("home node dir");
    let code = admit(home.path());
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;

    let phone = tempfile::tempdir().expect("app dir");
    let dir = phone.path().display().to_string();
    let server = format!("127.0.0.1:{port}");
    let first = Start {
        dir: dir.clone(),
        code: Some(format!("{code}@{server}")),
        name: Some("Pixel".to_string()),
        ..Start::default()
    };
    let peer = loquax::address::to_hex(&[0x6b; 16]);
    let (to, expected) = (peer.clone(), server.clone());
    tokio::task::spawn_blocking(move || {
        let answer = started(&loquax_android::start(&schema::encode(&first)));
        assert!(answer.ok, "{answer:?}");
        assert_eq!(answer.server.as_deref(), Some(expected.as_str()));
        link_event(true, 30);
        send_through_facade(&to, "vor dem Ausfall", 60);
    })
    .await
    .expect("the first session");

    node.kill().await;
    tokio::task::spawn_blocking(|| link_event(false, 30))
        .await
        .expect("the link goes down");

    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("the same port again");
    let node = HomeNode::start(listener, &instance, rns.path(), home.path()).await;
    let to = peer.clone();
    tokio::task::spawn_blocking(move || {
        // The TCP client redials every 5 s, doubling after the third try.
        link_event(true, 90);
        send_through_facade(&to, "nach dem Ausfall", 60);
    })
    .await
    .expect("the session after the home node came back");

    let to = peer.clone();
    tokio::task::spawn_blocking(move || {
        assert!(loquax_android::simulate_engine_death());
        let stopped = facade_event(10, "stopped", |e| matches!(e, Event::Stopped { .. }));
        match stopped {
            Event::Stopped { detail, .. } => {
                assert!(detail.contains("stopped answering"), "{detail}")
            }
            other => panic!("{other:?}"),
        }
        // The dead session is gone from the library, not handed out again.
        let done: schema::Done =
            rmp_serde::from_slice(&loquax_android::command(&schema::encode(&Request::Pull)))
                .expect("a Done");
        assert_eq!(done.error.as_deref(), Some("not started"));

        // The service's retry: neither code nor server, a fresh session.
        let again = Start {
            dir,
            ..Start::default()
        };
        let answer = started(&loquax_android::start(&schema::encode(&again)));
        assert!(answer.ok, "{answer:?}");
        assert_eq!(answer.server.as_deref(), Some(server.as_str()));
        link_event(true, 30);
        send_through_facade(&to, "nach dem Neustart", 60);
        let messages: schema::Messages = rmp_serde::from_slice(&loquax_android::query(
            &schema::encode(&schema::Query::Messages { peer: to.clone() }),
        ))
        .expect("Messages");
        let bodies: Vec<&str> = messages.messages.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(
            bodies,
            ["vor dem Ausfall", "nach dem Ausfall", "nach dem Neustart"]
        );
        loquax_android::stop();
    })
    .await
    .expect("the session after the engine died");

    node.kill().await;
}
