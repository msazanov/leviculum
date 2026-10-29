//! How a client talks to its home node: a link to `loquax.home`, identified
//! with the device's own identity, one request, the answer, the link closed.
//! Over the driver's own link API (`leviculum_std::ReticulumNode`), next to
//! the engine rather than inside it: the engine runs under the core lock and
//! may not wait, and a pull is nothing but waiting.

use std::time::Duration;

use leviculum_core::{DestinationHash, Identity, LinkId};
use leviculum_std::ReticulumNode;

use super::wire::{self, Entry, Status};

/// How often the path to the home destination is asked for again while
/// none has come: the reference's wait between a path request and the next
/// attempt (`leviculum_lxmf::router::PATH_REQUEST_WAIT_MS`).
const PATH_RETRY: Duration = Duration::from_millis(leviculum_lxmf::router::PATH_REQUEST_WAIT_MS);
/// How often a waiting path is looked for.
const PATH_POLL: Duration = Duration::from_millis(100);

/// Why talking to the home node failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeError {
    /// No path to the home destination came within the budget.
    NoPath,
    /// The link did not come up.
    NoLink(String),
    /// The home node answered with a refusal.
    Refused(Status),
    /// The request or its answer did not make it.
    Transfer(String),
    /// The answer did not decode.
    Malformed,
    /// The budget ran out.
    Timeout,
}

impl core::fmt::Display for HomeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoPath => write!(f, "no path to the home node"),
            Self::NoLink(detail) => write!(f, "the home node did not answer the link: {detail}"),
            Self::Refused(status) => write!(f, "{status}"),
            Self::Transfer(detail) => write!(f, "the request to the home node failed: {detail}"),
            Self::Malformed => write!(f, "the home node's answer did not decode"),
            Self::Timeout => write!(f, "the home node did not answer in time"),
        }
    }
}

impl std::error::Error for HomeError {}

/// What one pull brought: every entry after the cursor, oldest first, and
/// the oldest sequence the home node still holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Pulled {
    pub entries: Vec<Entry>,
    pub first: u64,
}

/// Redeem an admission token: link, identify as `device`, `/join`.
pub async fn join(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    token: &[u8; 16],
    name: &str,
    budget: Duration,
) -> Result<(), HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let answer = request(
        node,
        &link,
        super::JOIN_PATH,
        &wire::join_request(token, name),
        deadline,
    )
    .await;
    let _ = node.close_link(&link).await;
    match wire::read_status(&answer?).map_err(|_| HomeError::Malformed)? {
        Status::Ok => Ok(()),
        status => Err(HomeError::Refused(status)),
    }
}

/// Pull everything after `cursor`, page by page over one link.
pub async fn pull(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    cursor: u64,
    budget: Duration,
) -> Result<Pulled, HomeError> {
    let deadline = tokio::time::Instant::now() + budget;
    let link = open(node, public_key, device, deadline).await?;
    let pulled = pages(node, &link, cursor, deadline).await;
    let _ = node.close_link(&link).await;
    pulled
}

async fn pages(
    node: &ReticulumNode,
    link: &LinkId,
    mut cursor: u64,
    deadline: tokio::time::Instant,
) -> Result<Pulled, HomeError> {
    let mut pulled = Pulled::default();
    loop {
        let answer = request(
            node,
            link,
            super::SYNC_PATH,
            &wire::sync_request(cursor),
            deadline,
        )
        .await?;
        let page = wire::read_sync_answer(&answer)
            .map_err(|_| HomeError::Malformed)?
            .map_err(HomeError::Refused)?;
        pulled.first = page.first;
        let last = page.entries.last().map(|entry| entry.seq);
        pulled.entries.extend(page.entries);
        match (page.more, last) {
            (true, Some(last)) if last > cursor => cursor = last,
            _ => return Ok(pulled),
        }
    }
}

/// A link to the home destination of `public_key`, identified as `device`.
async fn open(
    node: &ReticulumNode,
    public_key: &[u8; 64],
    device: &Identity,
    deadline: tokio::time::Instant,
) -> Result<LinkId, HomeError> {
    let destination =
        DestinationHash::new(super::destination_hash(public_key).map_err(HomeError::NoLink)?);
    let mut asked: Option<tokio::time::Instant> = None;
    while !node.has_path(&destination) {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(HomeError::NoPath);
        }
        if asked.is_none_or(|at| now.duration_since(at) >= PATH_RETRY) {
            asked = Some(now);
            node.request_path(&destination)
                .await
                .map_err(|e| HomeError::NoLink(e.to_string()))?;
        }
        tokio::time::sleep(PATH_POLL).await;
    }
    let mut signing_key = [0u8; 32];
    signing_key.copy_from_slice(&public_key[32..]);
    let (handle, established) = node
        .connect_awaited(&destination, &signing_key)
        .await
        .map_err(|e| HomeError::NoLink(e.to_string()))?;
    let link = *handle.link_id();
    match tokio::time::timeout_at(deadline, established).await {
        Err(_) => {
            let _ = node.close_link(&link).await;
            return Err(HomeError::Timeout);
        }
        Ok(Err(error)) => return Err(HomeError::NoLink(error.to_string())),
        Ok(Ok(_)) => {}
    }
    if let Err(error) = node.identify_link(&link, device).await {
        let _ = node.close_link(&link).await;
        return Err(HomeError::NoLink(error.to_string()));
    }
    Ok(link)
}

/// One request on `link` and its answer's bytes.
async fn request(
    node: &ReticulumNode,
    link: &LinkId,
    path: &str,
    data: &[u8],
    deadline: tokio::time::Instant,
) -> Result<Vec<u8>, HomeError> {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    let timeout_ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
    let (_, response) = node
        .send_request_awaited(link, path, Some(data), Some(timeout_ms))
        .await
        .map_err(|e| HomeError::Transfer(e.to_string()))?;
    match tokio::time::timeout_at(deadline, response).await {
        Err(_) => Err(HomeError::Timeout),
        Ok(Err(error)) => Err(HomeError::Transfer(error.to_string())),
        Ok(Ok(info)) => Ok(info.response_data),
    }
}

/// The file under the loquax home that makes this device a client: the home
/// identity's public key in hex, written by `loquax home join`.
pub const CLIENT_FILE: &str = "home-client";
/// The device's own RNS identity, which the home node admitted.
pub const DEVICE_IDENTITY_FILE: &str = "device-identity";
/// The last archive sequence this client took in.
pub const CURSOR_FILE: &str = "home-cursor";

/// The home identity's public key, when this device is a client.
pub fn load_client(home: &std::path::Path) -> Result<Option<[u8; 64]>, String> {
    let path = home.join(CLIENT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let text = text.trim();
    let mut key = [0u8; 64];
    let parsed = text.len() == 128
        && key.iter_mut().enumerate().all(|(i, byte)| {
            u8::from_str_radix(&text[2 * i..2 * i + 2], 16)
                .map(|value| *byte = value)
                .is_ok()
        });
    match parsed {
        true => Ok(Some(key)),
        false => Err(format!(
            "{}: not a home identity's public key (128 hex characters)",
            path.display()
        )),
    }
}

/// Make this device a client of the home identity whose key this is.
pub fn save_client(home: &std::path::Path, public_key: &[u8; 64]) -> Result<(), String> {
    let text: String = public_key.iter().map(|b| format!("{b:02x}")).collect();
    write_atomically(&home.join(CLIENT_FILE), &format!("{text}\n"))
}

/// The cursor this client pulls from: 0 before the first pull.
pub fn load_cursor(home: &std::path::Path) -> u64 {
    std::fs::read_to_string(home.join(CURSOR_FILE))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn save_cursor(home: &std::path::Path, cursor: u64) -> Result<(), String> {
    write_atomically(&home.join(CURSOR_FILE), &format!("{cursor}\n"))
}

fn write_atomically(path: &std::path::Path, text: &str) -> Result<(), String> {
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, text)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// One pull into the engine: everything after the saved cursor, handed to
/// `outbox` as [`crate::outbox::Command::Ingest`] in sequence order, then the
/// cursor moved past it. How many entries came.
///
/// The cursor moves only after every entry was handed on, so a pull cut
/// short is pulled again; the screen drops a message it already holds.
pub async fn pull_into(
    outbox: &mut impl crate::outbox::Outbox,
    node: &ReticulumNode,
    home: &std::path::Path,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
) -> Result<usize, HomeError> {
    let cursor = load_cursor(home);
    let pulled = pull(node, public_key, device, cursor, budget).await?;
    if pulled.first > cursor.saturating_add(1) && cursor > 0 {
        tracing::warn!(
            "HOME pull: the home node no longer holds entries {}..{} (its archive cap)",
            cursor + 1,
            pulled.first - 1
        );
    }
    let mut last = cursor;
    for entry in &pulled.entries {
        outbox
            .submit(crate::outbox::Command::Ingest {
                packed: entry.packed.clone(),
            })
            .map_err(|gone| HomeError::Transfer(gone.to_string()))?;
        last = last.max(entry.seq);
    }
    if last != cursor {
        save_cursor(home, last).map_err(HomeError::Transfer)?;
    }
    tracing::info!(
        "HOME pull cursor={cursor} entries={} now={last}",
        pulled.entries.len()
    );
    Ok(pulled.entries.len())
}

/// How long a client's fetch waits for the engine to report the messages it
/// was handed, after the pull: the engine takes commands every 200 ms.
const SETTLE: Duration = Duration::from_secs(5);

/// `loquax fetch` on a client: pull from the home node, then print what is
/// new as a fetch from a propagation node prints it. How many were new.
#[allow(clippy::too_many_arguments)]
pub async fn fetch<E: crate::effects::Effects>(
    outbox: &mut impl crate::outbox::Outbox,
    node: &ReticulumNode,
    home: &std::path::Path,
    public_key: &[u8; 64],
    device: &Identity,
    budget: Duration,
    effects: &mut E,
    seen: &mut crate::seen::SeenStore,
) -> Result<usize, crate::fetch::FetchError> {
    use crate::fetch::{take_received, FetchError};
    use crate::outbox::OutboxEvent;
    let pulled = pull_into(outbox, node, home, public_key, device, budget)
        .await
        .map_err(|error| FetchError::SyncFailed(error.to_string()))?;
    let deadline = tokio::time::Instant::now() + SETTLE;
    let (mut taken, mut new) = (0, 0);
    while taken < pulled && tokio::time::Instant::now() < deadline {
        match outbox.try_next_event()? {
            Some(OutboxEvent::Received {
                message_id,
                source,
                title,
                body,
                verified,
                ..
            }) => {
                taken += 1;
                if take_received(seen, effects, &message_id, &source, &title, &body, verified)? {
                    new += 1;
                }
            }
            Some(_) => {}
            None => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    Ok(new)
}
