//! The import and the export (`loquax::import`, `loquax::export`), on
//! fixtures written by the tests from the formats
//! (`docs/drafts/loquax-import-2026-10.md`): a NomadNet tree, raw identity
//! files, a Base32 key. No messenger's real data directory is read: every
//! path here is under a temporary directory, and the scan is pointed at a
//! fake home with the system location off.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use leviculum_lxmf::message::{DeliveryMethod, Message as Lxm};
use leviculum_lxmf::msgpack;
use leviculum_std::Identity;
use leviculum_ui::delivery::DeliveryState;
use loquax::engine::delivery_address;
use loquax::import::{self, key, Choices, Source};
use loquax::store::{Store, STORE_FILE};
use loquax::ui::history::StoreWrite;
use loquax::ui::{LedgerEntry, Message, Signature};

fn identity() -> Identity {
    leviculum_std::generate_identity()
}

fn raw(identity: &Identity) -> [u8; 64] {
    identity.private_key_bytes().expect("key")
}

fn address(identity: &Identity) -> [u8; 16] {
    delivery_address(identity).expect("address")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const ALL: Choices = Choices {
    contacts: true,
    history: true,
    identity: false,
};

/// LXMF's `packed_container()`, as NomadNet writes each message.
fn container(state: i64, packed: &[u8], method: i64) -> Vec<u8> {
    let mut out = Vec::new();
    msgpack::map(&mut out, 5);
    msgpack::string(&mut out, "state");
    msgpack::int(&mut out, state);
    msgpack::string(&mut out, "lxmf_bytes");
    msgpack::bin(&mut out, packed);
    msgpack::string(&mut out, "transport_encrypted");
    msgpack::bool(&mut out, false);
    msgpack::string(&mut out, "transport_encryption");
    msgpack::nil(&mut out);
    msgpack::string(&mut out, "method");
    msgpack::int(&mut out, method);
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
    .expect("message")
}

/// A NomadNet tree for `own`, talking to `peer`.
struct NomadNet {
    root: PathBuf,
    own: Identity,
    peer: Identity,
    /// The ids, in time order: sent, received intact, received stripped.
    ids: Vec<[u8; 32]>,
}

fn nomadnet(base: &Path) -> NomadNet {
    let root = base.join(".nomadnetwork");
    let storage = root.join("storage");
    let own = identity();
    let peer = identity();
    let (me, them) = (address(&own), address(&peer));
    let conv = storage.join("conversations").join(hex(&them));
    std::fs::create_dir_all(&conv).expect("mkdir");
    std::fs::write(storage.join("identity"), raw(&own)).expect("identity");

    let sent = message(&own, them, 1_000.0, "hello from nomadnet");
    std::fs::write(
        conv.join(hex(&sent.message_id)),
        container(0x08, &sent.pack(), 2),
    )
    .expect("sent");
    let received = message(&peer, me, 2_000.0, "an answer");
    std::fs::write(
        conv.join(hex(&received.message_id)),
        container(0x08, &received.pack(), 2),
    )
    .expect("received");
    // A message with a file attachment, as NomadNet keeps it: the field
    // moved out to `attachments/<id>/`, the bytes rewritten without it, the
    // original id kept as the name, and its own verdict in `.index`.
    let mut file = Vec::new();
    msgpack::array(&mut file, 1);
    msgpack::array(&mut file, 2);
    msgpack::string(&mut file, "notes.txt");
    msgpack::bin(&mut file, b"attached");
    let mut attached = Lxm::create(
        me,
        them,
        &peer,
        3_000.0,
        Vec::new(),
        b"see the file".to_vec(),
        vec![(5, file)],
        DeliveryMethod::Direct,
    )
    .expect("attached");
    let id = attached.message_id;
    attached.fields.clear();
    std::fs::write(conv.join(hex(&id)), container(0x08, &attached.pack(), 2)).expect("stripped");
    let att = storage.join("attachments").join(hex(&id));
    std::fs::create_dir_all(&att).expect("attachments");
    std::fs::write(att.join("file_0"), b"attached").expect("file");
    let mut manifest = Vec::new();
    msgpack::map(&mut manifest, 1);
    msgpack::string(&mut manifest, "files");
    msgpack::array(&mut manifest, 1);
    msgpack::map(&mut manifest, 1);
    msgpack::string(&mut manifest, "name");
    msgpack::string(&mut manifest, "notes.txt");
    std::fs::write(att.join("manifest"), manifest).expect("manifest");
    let mut index = Vec::new();
    msgpack::map(&mut index, 1);
    msgpack::string(&mut index, &hex(&id));
    msgpack::map(&mut index, 2);
    msgpack::string(&mut index, "signature_validated");
    msgpack::bool(&mut index, true);
    msgpack::string(&mut index, "sort_timestamp");
    msgpack::f64(&mut index, 3_005.5);
    std::fs::write(conv.join(".index"), index).expect("index");
    // A file that is not a message, and a temporary one NomadNet left.
    std::fs::write(conv.join("ab".repeat(32)), b"garbage").expect("garbage");
    std::fs::write(conv.join(format!("{}.tmp.1.x", "cd".repeat(32))), b"tmp").expect("tmp");

    // The directory: the peer named, a node entry, a short old entry.
    let blocked = [0xbb; 16];
    let other = [0xcc; 16];
    let mut directory = Vec::new();
    msgpack::map(&mut directory, 2);
    msgpack::string(&mut directory, "entry_list");
    msgpack::array(&mut directory, 3);
    msgpack::array(&mut directory, 8);
    msgpack::bin(&mut directory, &them);
    msgpack::string(&mut directory, "Bob");
    msgpack::int(&mut directory, 0xff);
    msgpack::bool(&mut directory, false);
    msgpack::nil(&mut directory);
    msgpack::bool(&mut directory, false);
    msgpack::nil(&mut directory);
    msgpack::string(&mut directory, "");
    msgpack::array(&mut directory, 4);
    msgpack::bin(&mut directory, &[0xdd; 16]);
    msgpack::string(&mut directory, "a node");
    msgpack::int(&mut directory, 2);
    msgpack::bool(&mut directory, true);
    msgpack::array(&mut directory, 3);
    msgpack::bin(&mut directory, &other);
    msgpack::string(&mut directory, "Carol");
    msgpack::int(&mut directory, 2);
    msgpack::string(&mut directory, "announce_stream");
    msgpack::array(&mut directory, 0);
    std::fs::write(storage.join("directory"), directory).expect("directory");
    std::fs::write(
        root.join("ignored"),
        format!("{}\nnot hex\n", hex(&blocked)),
    )
    .expect("ignored");
    let mut settings = Vec::new();
    msgpack::map(&mut settings, 2);
    msgpack::string(&mut settings, "display_name");
    msgpack::string(&mut settings, "Nomad Me");
    msgpack::string(&mut settings, "propagation_node");
    msgpack::bin(&mut settings, &[0xee; 16]);
    std::fs::write(storage.join("peersettings"), settings).expect("peersettings");
    NomadNet {
        root,
        own,
        peer,
        ids: vec![sent.message_id, received.message_id, id],
    }
}

fn found(tree: &NomadNet) -> import::Found {
    import::sniff(&tree.root).expect("a NomadNet tree")
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => out.extend(files_under(&path)),
                false => out.push(path),
            }
        }
    }
    out
}

fn conversation(home: &Path, me: [u8; 16], peer: [u8; 16]) -> Vec<Message> {
    let store = Store::open_in(home, me).expect("store");
    store
        .restore()
        .expect("restore")
        .into_iter()
        .find(|c| c.peer == peer)
        .map(|c| c.messages)
        .unwrap_or_default()
}

#[test]
fn a_dry_run_counts_and_writes_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let report = import::import(&found(&tree), &home, ALL, true).expect("dry run");
    assert!(!home.exists(), "a dry run made {:?}", files_under(&home));
    assert_eq!(report.conversations.len(), 1);
    let c = &report.conversations[0];
    assert_eq!((c.new, c.held, c.unreadable), (3, 0, 1));
    assert_eq!(report.contacts_new, 3, "Bob, Carol, the blocked peer");
    assert_eq!(report.attachments_skipped, 1);
    assert_eq!(report.propagation_node, Some([0xee; 16]));
    assert!(report
        .lines(true)
        .iter()
        .any(|l| l.contains("3 messages to write")));
}

#[test]
fn contacts_names_blocks_and_the_propagation_node_come_over_and_ours_win() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    std::fs::create_dir_all(&home).expect("home");
    let them = address(&tree.peer);
    std::fs::write(
        home.join("contacts"),
        format!("# mine\nname {} Robert\n", hex(&them)),
    )
    .expect("contacts");
    let choices = Choices {
        history: false,
        ..ALL
    };
    let report = import::import(&found(&tree), &home, choices, false).expect("import");
    assert_eq!((report.contacts_new, report.contacts_held), (2, 1));
    let contacts = loquax::names::load_contacts(&home).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&them).map(String::as_str),
        Some("Robert")
    );
    assert_eq!(
        contacts.petnames.get(&[0xcc; 16]).map(String::as_str),
        Some("Carol")
    );
    assert!(!contacts.petnames.contains_key(&[0xdd; 16]), "a node entry");
    assert!(contacts.blocked.contains(&[0xbb; 16]));
    let text = std::fs::read_to_string(home.join("contacts")).expect("text");
    assert!(
        text.starts_with("# mine\n"),
        "the reader's lines stay: {text}"
    );
    let config = loquax::config::load(&home).expect("config");
    assert_eq!(config.propagation_node, Some([0xee; 16]));
    assert!(!home.join(STORE_FILE).exists(), "no history was asked for");
    assert!(!home.join("identity").exists(), "nor the identity");
}

#[test]
fn history_verifies_what_it_can_and_keeps_the_source_verdict_as_the_sources() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let report = import::import(&found(&tree), &home, ALL, false).expect("import");
    let (me, them) = (address(&tree.own), address(&tree.peer));
    assert_eq!(report.history_address, Some(me));
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(bodies, ["hello from nomadnet", "an answer", "see the file"]);
    let ids: Vec<[u8; 32]> = messages.iter().filter_map(|m| m.id).collect();
    assert_eq!(ids, tree.ids);

    // Ours, signed with the tree's identity: checked, and it holds.
    assert!(messages[0].outbound);
    assert_eq!(messages[0].signature, Signature::Verified);
    assert_eq!(messages[0].delivery, Some(DeliveryState::Delivered));
    // Intact from a sender whose key loquax does not have: unverified, its
    // bytes kept for the check when the key comes.
    assert_eq!(messages[1].signature, Signature::Unverified);
    let store = Store::open_in(&home, me).expect("store");
    let held = store.unverified().expect("held");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, tree.ids[1]);
    assert_eq!(held[0].1, them);
    let kept = Lxm::unpack(&held[0].2, None, Some(&tree.peer), DeliveryMethod::Direct)
        .expect("kept bytes");
    assert_eq!(
        kept.verification,
        leviculum_lxmf::Verification::Valid,
        "the kept bytes are the signed ones"
    );
    // Stripped of its attachment: not ours to call invalid; NomadNet's word
    // is shown as NomadNet's.
    assert_eq!(messages[2].signature, Signature::Unverified);
    let ledger: Vec<&str> = messages[2].ledger.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(
        ledger,
        [
            "imported from NomadNet",
            "NomadNet said: signature verified"
        ]
    );
    assert_eq!(messages[2].at.map(|at| at.unix_secs), Some(3_005));
    let log = std::fs::read_to_string(home.join(import::LOG_FILE)).expect("log");
    assert!(log.contains(&format!("conv={} new=3 held=0 unreadable=1", hex(&them))));
    assert!(
        !log.contains("hello from nomadnet"),
        "no content in the log"
    );
    assert!(!log.contains(&hex(&raw(&tree.own))), "no key in the log");
}

#[test]
fn a_second_run_is_idempotent() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    import::import(&found(&tree), &home, ALL, false).expect("first");
    let again = import::import(&found(&tree), &home, ALL, false).expect("second");
    let c = &again.conversations[0];
    assert_eq!((c.new, c.held), (0, 3));
    assert_eq!(again.contacts_new, 0);
    let me = address(&tree.own);
    let store = Store::open_in(&home, me).expect("store");
    assert_eq!(store.message_count().expect("count"), 3);
    let dry = import::import(&found(&tree), &home, ALL, true).expect("dry");
    assert_eq!(
        (dry.conversations[0].new, dry.conversations[0].held),
        (0, 3)
    );
}

fn own_message(key: u64, body: &str, id: [u8; 32]) -> Message {
    Message {
        outbound: true,
        body: body.to_string(),
        id: Some(id),
        delivery: Some(DeliveryState::Delivered),
        signature: Signature::Verified,
        leg: None,
        unproven: false,
        at: None,
        size: body.len(),
        ledger: vec![LedgerEntry {
            at: None,
            what: "queued".to_string(),
        }],
        last_leg: None,
        activity: key,
        fields: Vec::new(),
    }
}

/// Imported history is older than what loquax wrote, so it sits below it,
/// and a later import of still older messages goes first without moving
/// loquax's own rows.
#[test]
fn imported_history_sorts_below_loquaxs_own_and_a_later_import_stays_in_time_order() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let (me, them) = (address(&tree.own), address(&tree.peer));
    std::fs::create_dir_all(&home).expect("home");
    let mut store = Store::open_in(&home, me).expect("store");
    let key = store.last_key().expect("key") + 1;
    store
        .write(&[StoreWrite::Message {
            peer: them,
            message: own_message(key, "written in loquax", [0x42; 32]),
        }])
        .expect("own");
    drop(store);
    import::import(&found(&tree), &home, ALL, false).expect("import");
    // An older message turns up in a second import.
    let older = message(&tree.own, them, 500.0, "the oldest");
    let conv = tree.root.join("storage/conversations").join(hex(&them));
    std::fs::write(
        conv.join(hex(&older.message_id)),
        container(0x08, &older.pack(), 2),
    )
    .expect("older");
    let second = import::import(&found(&tree), &home, ALL, false).expect("second");
    assert_eq!(
        (second.conversations[0].new, second.conversations[0].held),
        (1, 3)
    );
    let messages = conversation(&home, me, them);
    let bodies: Vec<&str> = messages.iter().map(|m| m.body.as_str()).collect();
    assert_eq!(
        bodies,
        [
            "the oldest",
            "hello from nomadnet",
            "an answer",
            "see the file",
            "written in loquax"
        ]
    );
    assert_eq!(messages[4].activity, key, "loquax's own row keeps its key");
    let store = Store::open_in(&home, me).expect("store");
    assert_eq!(store.last_key().expect("key"), key);
    // The text index follows the rewritten block.
    let mut loaded = std::collections::BTreeMap::new();
    loaded.insert(them, u64::MAX);
    let hits = store.search("oldest", &loaded).expect("search");
    assert_eq!(hits.len(), 1);
}

#[test]
fn the_scan_looks_only_at_the_documented_places() {
    let dir = tempfile::tempdir().expect("dir");
    let user = dir.path();
    let tree = nomadnet(user);
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, bytes).expect("write");
    };
    let sideband = identity();
    write(
        &user.join(".config/sideband/app_storage/primary_identity"),
        &raw(&sideband),
    );
    let meshchatx = identity();
    write(
        &user.join(".reticulum-meshchatx/identities/aaaa/identity"),
        &raw(&meshchatx),
    );
    write(
        &user.join(".local/share/org.ratspeak.desktop/.ratspeak/identities/bbbb/identity.enc"),
        b"{\"version\": 1}",
    );
    let exported = identity();
    write(&user.join("Downloads/mine.rnsidentity"), &raw(&exported));
    // Not looked at: another name in Downloads, a deeper directory.
    write(&user.join("Downloads/other.bin"), &raw(&identity()));
    write(
        &user.join(".reticulum-meshchatx/identities/aaaa/deeper/identity"),
        &raw(&identity()),
    );
    let found = import::scan(user, false);
    let apps: Vec<&str> = found.iter().map(|f| f.app).collect();
    assert_eq!(
        apps,
        ["NomadNet", "Sideband", "MeshChatX", "Ratspeak", "file"]
    );
    assert_eq!(found[0].address, Some(address(&tree.own)));
    assert_eq!((found[0].messages, found[0].contacts), (4, 3));
    assert_eq!(found[1].address, Some(address(&sideband)));
    assert_eq!(found[2].address, Some(address(&meshchatx)));
    assert!(matches!(found[3].source, Source::Protected));
    assert!(!found[3].importable());
    assert!(found[3].line().contains("protected by a passcode"));
    assert_eq!(found[4].address, Some(address(&exported)));
}

#[test]
fn every_adoption_path_takes_the_address_and_sets_the_old_one_aside() {
    let dir = tempfile::tempdir().expect("dir");
    let identity_only = Choices {
        contacts: false,
        history: false,
        identity: true,
    };
    // A raw file, a Base32 file, a pasted key, a NomadNet tree.
    let raw_id = identity();
    let raw_path = dir.path().join("x.rnsidentity");
    std::fs::write(&raw_path, raw(&raw_id)).expect("raw");
    let b32_id = identity();
    let b32_path = dir.path().join("key.txt");
    std::fs::write(&b32_path, key::to_base32(&raw(&b32_id))).expect("b32");
    let pasted_id = identity();
    let tree = nomadnet(dir.path());
    let sources = [
        (import::sniff(&raw_path).expect("raw"), address(&raw_id)),
        (import::sniff(&b32_path).expect("b32"), address(&b32_id)),
        (
            import::pasted("pasted", Path::new("(stdin)"), raw(&pasted_id)),
            address(&pasted_id),
        ),
        (found(&tree), address(&tree.own)),
    ];
    for (n, (source, want)) in sources.iter().enumerate() {
        let home = dir.path().join(format!("home{n}"));
        let before =
            loquax::identity::load_or_create(&home.join("identity")).expect("loquax's own");
        let old = address(&before);
        let report = import::import(source, &home, identity_only, false).expect("adopt");
        assert_eq!(import::current_address(&home), Some(*want), "source {n}");
        let aside = home.join(format!("identity.{}.bak", hex(&old)));
        assert!(aside.is_file(), "source {n}: the old identity is kept");
        assert!(report.lines(false).iter().any(|l| l.contains("set aside")));
    }
}

/// The old identity's history stays in the store under its address, and
/// moving the file back brings both back.
#[test]
fn the_set_aside_identity_keeps_its_history() {
    let dir = tempfile::tempdir().expect("dir");
    let home = dir.path().join("loquax");
    let mine = loquax::identity::load_or_create(&home.join("identity")).expect("own");
    let old = address(&mine);
    let mut store = Store::open_in(&home, old).expect("store");
    let key = store.last_key().expect("key") + 1;
    store
        .write(&[StoreWrite::Message {
            peer: [0x55; 16],
            message: own_message(key, "before the switch", [0x43; 32]),
        }])
        .expect("write");
    drop(store);
    let new = identity();
    let source = import::pasted("pasted", Path::new("(stdin)"), raw(&new));
    let identity_only = Choices {
        contacts: false,
        history: false,
        identity: true,
    };
    import::import(&source, &home, identity_only, false).expect("adopt");
    assert!(conversation(&home, address(&new), [0x55; 16]).is_empty());
    std::fs::rename(
        home.join(format!("identity.{}.bak", hex(&old))),
        home.join("identity"),
    )
    .expect("switch back");
    assert_eq!(import::current_address(&home), Some(old));
    let back = conversation(&home, old, [0x55; 16]);
    assert_eq!(back[0].body, "before the switch");
}

#[test]
fn the_questions_take_defaults_and_adopt_only_on_yes() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let sources = [found(&tree)];

    let home = dir.path().join("defaults");
    let mut out = Vec::new();
    let reports =
        import::ask_and_import(&sources, &home, &mut &b"\n\ny\n"[..], &mut out).expect("asked");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(text.contains("Do not run NomadNet and Loquax"), "{text}");
    assert_eq!(reports.len(), 1);
    assert!(reports[0].as_ref().expect("ok").adopted.is_none());
    assert_eq!(import::current_address(&home), None, "y is not yes");
    assert_eq!(
        Store::open_in(&home, address(&tree.own))
            .expect("store")
            .message_count()
            .expect("count"),
        3,
        "history is yes by default"
    );

    let home = dir.path().join("yes");
    let mut out = Vec::new();
    import::ask_and_import(&sources, &home, &mut &b"n\nn\nyes\n"[..], &mut out).expect("asked");
    assert_eq!(import::current_address(&home), Some(address(&tree.own)));
    assert!(!home.join(STORE_FILE).exists(), "history was declined");
    assert!(!home.join("contacts").exists(), "contacts were declined");

    let home = dir.path().join("eof");
    let mut out = Vec::new();
    import::ask_and_import(&sources, &home, &mut &b""[..], &mut out).expect("asked");
    assert_eq!(import::current_address(&home), None);
}

/// Export, import into an empty loquax, and the same messages come back in
/// the same verification state: ours verified by the signature the export
/// made, a held message held again with its bytes, and a message loquax
/// verified on arrival, whose bytes the store does not keep, with
/// loquax's verdict shown as the export's word.
#[test]
fn the_export_round_trips_through_the_import() {
    let dir = tempfile::tempdir().expect("dir");
    let a = dir.path().join("a");
    let own = loquax::identity::load_or_create(&a.join("identity")).expect("own");
    let me = address(&own);
    let peer = identity();
    let them = address(&peer);
    std::fs::write(
        a.join("contacts"),
        format!("name {} Bob\nblock {}\n", hex(&them), hex(&[0xbb; 16])),
    )
    .expect("contacts");
    let mut store = Store::open_in(&a, me).expect("store");
    let base = store.last_key().expect("key");
    let held = message(&peer, me, 4_000.0, "held for its key");
    let received = |key: u64, body: &str, id: [u8; 32], signature| Message {
        outbound: false,
        delivery: None,
        signature,
        ledger: Vec::new(),
        ..own_message(key, body, id)
    };
    store
        .write(&[
            StoreWrite::Message {
                peer: them,
                message: own_message(base + 1, "sent from loquax", [0x11; 32]),
            },
            StoreWrite::Message {
                peer: them,
                message: received(
                    base + 2,
                    "verified on arrival",
                    [0x22; 32],
                    Signature::Verified,
                ),
            },
            StoreWrite::Message {
                peer: them,
                message: received(
                    base + 3,
                    "held for its key",
                    held.message_id,
                    Signature::Unverified,
                ),
            },
        ])
        .expect("write");
    store
        .keep_unverified(&held.message_id, &them, &held.pack())
        .expect("keep");
    drop(store);

    let out = dir.path().join("export");
    let done = loquax::export::export(&a, &out).expect("export");
    assert_eq!(
        (done.messages, done.conversations, done.contacts),
        (3, 1, 1)
    );
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(out.join("storage/identity"))
            .expect("identity")
            .permissions()
            .mode()
    };
    assert_eq!(mode & 0o777, 0o600);
    assert!(
        loquax::export::export(&a, &out).is_err(),
        "an export never writes over one"
    );

    let b = dir.path().join("b");
    let source = import::sniff(&out).expect("the export reads as a tree");
    assert_eq!(source.app, "Loquax");
    assert_eq!(source.address, Some(me));
    let choices = Choices {
        identity: true,
        ..ALL
    };
    import::import(&source, &b, choices, false).expect("import");
    assert_eq!(import::current_address(&b), Some(me));
    let back = conversation(&b, me, them);
    let summary: Vec<(bool, &str, Option<[u8; 32]>, Signature)> = back
        .iter()
        .map(|m| (m.outbound, m.body.as_str(), m.id, m.signature))
        .collect();
    assert_eq!(
        summary,
        [
            (
                true,
                "sent from loquax",
                Some([0x11; 32]),
                Signature::Verified
            ),
            (
                false,
                "verified on arrival",
                Some([0x22; 32]),
                Signature::Unverified
            ),
            (
                false,
                "held for its key",
                Some(held.message_id),
                Signature::Unverified
            ),
        ]
    );
    let said: Vec<&str> = back[1].ledger.iter().map(|e| e.what.as_str()).collect();
    assert_eq!(
        said,
        ["imported from Loquax", "Loquax said: signature verified"]
    );
    let store = Store::open_in(&b, me).expect("store");
    let kept = store.unverified().expect("held");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].2, held.pack(), "the held bytes travel whole");
    let contacts = loquax::names::load_contacts(&b).expect("contacts");
    assert_eq!(
        contacts.petnames.get(&them).map(String::as_str),
        Some("Bob")
    );
    assert!(contacts.blocked.contains(&[0xbb; 16]));
    // A second round adds nothing.
    let again = import::import(&source, &b, ALL, false).expect("again");
    assert_eq!(again.conversations[0].new, 0);
}

fn loquax(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loquax"));
    command
        .env("LNMSG_HOME", home)
        .env("HOME", home.join("user"))
        .env_remove("LEVICULUM_EVENT_LOG");
    command
}

/// Run `command` with `input` as its answers on stdin, to its end.
fn answered(mut command: Command, input: &[u8]) -> std::process::Output {
    use std::io::Write;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = leviculum_std::process::spawn_supervised(command).expect("spawn loquax");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("answers");
    child.wait_with_output().expect("run")
}

#[test]
fn the_command_line_dry_run_lists_and_the_import_asks() {
    let dir = tempfile::tempdir().expect("dir");
    let tree = nomadnet(dir.path());
    let home = dir.path().join("loquax");
    let output = loquax(&home)
        .args(["import", "--dry-run"])
        .arg(&tree.root)
        .output()
        .expect("run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("new 3  held 0  unreadable 1"), "{stdout}");
    assert!(stdout.contains("identity: offered"), "{stdout}");
    assert!(!home.exists(), "a dry run writes nothing");

    let mut command = loquax(&home);
    command.arg("import").arg(&tree.root);
    let output = answered(command, b"\n\nyes\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Loquax now answers on"), "{stdout}");
    let printed = loquax(&home).arg("address").output().expect("address");
    assert_eq!(
        String::from_utf8_lossy(&printed.stdout).trim(),
        hex(&address(&tree.own))
    );

    let key = identity();
    let mut command = loquax(&home);
    command.args(["import", "--key"]);
    let typed = format!("{}\nyes\n", key::to_base32(&raw(&key)));
    let output = answered(command, typed.as_bytes());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(
        !stdout.contains(&key::to_base32(&raw(&key))),
        "the key is not echoed"
    );
    assert_eq!(import::current_address(&home), Some(address(&key)));
}
