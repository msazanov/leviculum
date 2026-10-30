//! The client runners of a home node's device, one copy for every frontend
//! that is such a client: the terminal and the window (`shell.rs` in the
//! binary) and the Android app (`loquax-android`'s session). Each frontend
//! keeps its own queue, its own tasks and its own way of saying what
//! happened; what a send, a pull, a mark and the store's side of an engine
//! event do is here (leviculum 1116, the rule against duplicated code).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use leviculum_core::Identity;
use leviculum_lxmf::Field;
use leviculum_std::ReticulumNode;

use crate::outbox::{Command, Outbox, OutboxEvent, OutboxGone};
use crate::store::Store;
use crate::ui::UiEvent;

/// How often a send runner reads its tap while it waits for its bubble to
/// be named.
const POLL: Duration = Duration::from_millis(20);

/// What a client's runners need: the home identity's key, the device's own
/// identity, and the node to open the link on.
#[derive(Clone)]
pub struct HomeClient {
    pub public_key: [u8; 64],
    pub device: Identity,
    pub node: Arc<ReticulumNode>,
}

/// One compose send on a client: handed to the home node with `/send`,
/// which delivers it. The bubble is named after the id the home node gave
/// it, and takes its later states from the archive's entries on each pull.
/// `settle` is how long the runner waits for that name to reach the
/// screen: the screen names the oldest running send, so the name has to
/// arrive before this runner's end does.
pub async fn send_home(
    tap: &mut impl Outbox,
    client: &HomeClient,
    to: [u8; 16],
    body: String,
    fields: Vec<Field>,
    budget: Duration,
    settle: Duration,
) -> UiEvent {
    let submission = super::wire::Submission {
        client_id: super::client::client_id(),
        destination: to,
        title: Vec::new(),
        content: body.into_bytes(),
        fields,
    };
    let accepted = super::client::send_retrying(
        &client.node,
        &client.public_key,
        &client.device,
        &submission,
        budget,
    )
    .await;
    let message_id = match accepted {
        Ok(id) => id,
        Err(error) => return UiEvent::SendDone(Err(format!("mailbox: {error}"))),
    };
    tracing::info!(
        "HOME send accepted message={}",
        crate::address::to_hex(&message_id)
    );
    if tap.submit(Command::HomeAccepted { message_id }).is_err() {
        return UiEvent::SendDone(Err(OutboxGone.to_string()));
    }
    let deadline = tokio::time::Instant::now() + settle;
    while tokio::time::Instant::now() < deadline {
        match tap.try_next_event() {
            Ok(Some(OutboxEvent::Queued { message_id: id })) if id == message_id => break,
            Ok(Some(_)) => {}
            Ok(None) => tokio::time::sleep(POLL).await,
            Err(gone) => return UiEvent::SendDone(Err(gone.to_string())),
        }
    }
    UiEvent::SendDone(Ok(()))
}

/// A client's fetch: one pull from its home node, with twice a send's
/// `budget`, as on the command line. It counts the received messages that
/// came ([`super::client::Pull::messages`]), never entries: a pull that
/// brought only names, marks, states or the user's own sent message
/// brought nothing new (1136). The cursor comes back for
/// [`settle_pull`]: the runner sees the fence on its tap in the same pump
/// that hands the screen the pulled messages, and the frontend writes those
/// to the store before it takes the runner's end.
pub async fn pull(
    outbox: &mut impl Outbox,
    client: &HomeClient,
    home: &Path,
    budget: Duration,
) -> (Result<usize, String>, Option<u64>) {
    match super::client::pull_into(
        outbox,
        &client.node,
        home,
        &client.public_key,
        &client.device,
        budget * 2,
    )
    .await
    {
        Ok(pull) => {
            if pull.imported.is_empty() {
                return (Ok(pull.messages()), pull.cursor);
            }
            // Imported history goes to the store before the cursor may pass
            // it: one that cannot be kept is pulled again next time.
            match keep_imported(home, &client.public_key, &pull.imported) {
                Ok(_) => (Ok(pull.messages()), pull.cursor),
                Err(error) => (Err(error), None),
            }
        }
        Err(error) => (Err(error.to_string()), None),
    }
}

/// Write the imported history entries of a pull
/// ([`crate::home::KIND_IMPORTED`]) into the store of the home identity's
/// address under `home`, one conversation at a time, as the import on the
/// home node wrote them: how many rows were new. An entry of another shape
/// is passed by and said in the log. The screen shows them from its next
/// start; the store keeps them in time order below its own rows.
pub fn keep_imported(
    home: &Path,
    public_key: &[u8; 64],
    entries: &[Vec<u8>],
) -> Result<usize, String> {
    let address = super::lxmf_address(public_key)?;
    let mut groups = std::collections::BTreeMap::<_, Vec<_>>::new();
    for bytes in entries {
        let Some((origin, peer, row)) = crate::import::archive::read_entry(bytes) else {
            tracing::warn!("HOME pull: an imported entry of another shape");
            continue;
        };
        groups.entry((origin, peer)).or_default().push(row);
    }
    let mut store = Store::open_in(home, address).map_err(|e| e.to_string())?;
    let mut new = 0;
    for ((origin, peer), rows) in &groups {
        let counts = store
            .import_conversation(*peer, origin, rows)
            .map_err(|e| e.to_string())?;
        tracing::info!(
            "HOME imported source={origin} conv={} new={} held={}",
            crate::address::to_hex(peer),
            counts.new,
            counts.held
        );
        new += counts.new;
    }
    Ok(new)
}

/// A pull ended: what it brought is in the store by now, so the cursor may
/// pass it.
pub fn settle_pull(home: &Path, cursor: Option<u64>) {
    if let Some(cursor) = cursor {
        if let Err(error) = super::client::save_cursor(home, cursor) {
            tracing::warn!("HOME cursor: {error}");
        }
    }
}

/// The marks a fold made, handed to the home node in order: the screen
/// already shows them. A mark the home node did not take is said in the
/// log; the other devices keep the message unread until this device marks
/// it again.
pub async fn hand_marks(client: HomeClient, marks: Vec<super::wire::Mark>, budget: Duration) {
    for mark in marks {
        let outcome = super::client::mark(
            &client.node,
            &client.public_key,
            &client.device,
            &mark,
            budget,
        )
        .await;
        match outcome {
            Ok(()) => {
                tracing::info!("HOME mark {} messages={}", mark.flag.word(), mark.ids.len())
            }
            Err(error) => tracing::warn!(
                "HOME mark {} messages={} not taken: {error}",
                mark.flag.word(),
                mark.ids.len()
            ),
        }
    }
}

/// The petnames a fold set, handed to the home node in order (1139): the
/// screen and the contacts file have them already. One the home node did
/// not take is said in the log; the other devices keep the name they had
/// until this device sets one again.
pub async fn hand_petnames(
    client: HomeClient,
    petnames: Vec<([u8; 16], Option<String>)>,
    budget: Duration,
) {
    for (peer, petname) in petnames {
        let outcome = super::client::petname(
            &client.node,
            &client.public_key,
            &client.device,
            &peer,
            petname.as_deref(),
            budget,
        )
        .await;
        let peer = crate::address::to_hex(&peer);
        match outcome {
            Ok(()) => tracing::info!("HOME petname peer={peer}"),
            Err(error) => tracing::warn!("HOME petname peer={peer} not taken: {error}"),
        }
    }
}

/// The peer states a fold set, handed to the home node in order (1146), as
/// petnames are.
pub async fn hand_peer_states(
    client: HomeClient,
    states: Vec<([u8; 16], super::wire::PeerState)>,
    budget: Duration,
) {
    for (peer, state) in states {
        let outcome = super::client::peer_state(
            &client.node,
            &client.public_key,
            &client.device,
            &peer,
            &state,
            budget,
        )
        .await;
        let peer = crate::address::to_hex(&peer);
        match outcome {
            Ok(()) => tracing::info!("HOME peerstate peer={peer}"),
            Err(error) => tracing::warn!("HOME peerstate peer={peer} not taken: {error}"),
        }
    }
}

/// A received message the store already holds, which the screen drops
/// before it folds: the mailbox copy of one that came directly, perhaps
/// before a restart and below the window the screen loaded. A store that
/// cannot answer lets the message through; a duplicate beats a lost one.
pub fn already_held(store: &Store, event: &OutboxEvent) -> bool {
    let OutboxEvent::Received { message_id, .. } = event else {
        return false;
    };
    match store.holds_received(message_id) {
        Ok(held) => {
            if held {
                tracing::info!("HELD message={}", crate::address::to_hex(message_id));
            }
            held
        }
        Err(error) => {
            tracing::warn!("{error}");
            false
        }
    }
}

/// The store's side of a signature that waits on its sender's key: the
/// message's bytes while it waits, the verdict once it is in. The verdict
/// goes to the store here because the screen holds only a window of each
/// conversation (leviculum 1079).
pub fn keep_signature(store: &mut Store, event: &OutboxEvent) {
    let kept = match event {
        OutboxEvent::Unverified {
            message_id,
            source,
            packed,
        } => store.keep_unverified(message_id, source, packed),
        OutboxEvent::Verified {
            message_id, valid, ..
        } => store.settle_verdict(message_id, *valid),
        _ => return,
    };
    if let Err(error) = kept {
        crate::events::store_failed(&error.to_string());
    }
}

/// A name an announce carried, kept for the next run; never the screen's
/// own address.
pub fn keep_name(
    store: &mut Store,
    names: &crate::names::Names,
    own: Option<[u8; 16]>,
    destination: [u8; 16],
) {
    if own == Some(destination) {
        return;
    }
    let Some(name) = names.announced(&destination) else {
        return;
    };
    if let Err(error) = store.save_name(destination, name) {
        crate::events::store_failed(&error.to_string());
    }
}
