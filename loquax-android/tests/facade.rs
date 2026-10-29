//! The facade the app's Java calls, on the host and without JNI: the bytes
//! in and out of `start`, `command`, `query` and `nextEvent`, and a session
//! against a real home node (`loquax::home::daemon`, in process) on a Rust
//! transport node with a TCP server, the leaf dialling in as the phone does.

use std::path::Path;
use std::time::Duration;

use leviculum_std::driver::ReticulumNodeBuilder;
use loquax_android::schema::{self, Event, Request, Start, Started};
use loquax_android::session::Session;

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

/// The home node of `home`: its identity, a token issued for "phone" in its
/// store, and the code that carries both.
fn admit(home: &Path) -> (leviculum_core::Identity, String) {
    let identity = loquax::identity::load_or_create(&home.join("identity")).expect("identity");
    let address = loquax::engine::delivery_address(&identity).expect("address");
    let mut store = loquax::store::Store::open_in(home, address).expect("the home store");
    let token = [0x42; 16];
    store
        .issue_token(&token, "phone", leviculum_std::local_time::unix_now_secs())
        .expect("a token");
    let code = loquax::home::code::HomeCode {
        public_key: identity.public_key_bytes(),
        token,
    }
    .encode();
    (identity, code)
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
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let instance = format!(
        "loquax-android-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let storage = tempfile::tempdir().expect("transport storage");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port");
    let port = listener.local_addr().expect("its address").port();
    let mut transport = ReticulumNodeBuilder::new()
        .enable_transport(true)
        .share_instance(true)
        .instance_name(instance.clone())
        .add_tcp_server_listener(listener)
        .storage_path(storage.path().to_path_buf())
        .build_sync()
        .expect("the transport node");
    transport.start().await.expect("start it");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let home = tempfile::tempdir().expect("home node dir");
    let (identity, code) = admit(home.path());
    let daemon_storage = home.path().join("storage");
    std::fs::create_dir_all(&daemon_storage).expect("daemon storage");
    let daemon = tokio::spawn(loquax::home::daemon::run(
        loquax::home::daemon::DaemonOptions {
            home: home.path().to_path_buf(),
            instance: instance.clone(),
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
        },
    ));

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
        assert_eq!(list.conversations[0].last, "hallo vom Telefon");
        assert!(list.conversations[0].ours);

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
        assert_eq!(messages.messages.len(), 1, "{messages:?}");
        assert_eq!(messages.messages[0].body, "hallo vom Telefon");
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

    daemon.abort();
    transport.stop().await.expect("stop the transport node");
}
