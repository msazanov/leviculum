//! The message store's policy half (Codeberg #384, #469 step 1): the
//! requests other modules make of the record log, the counters and lines
//! it reports, and the body of the one task allowed to hold an append.
//!
//! The log is [`leviculum_record_log`] over the [`Flash`] seam. Two
//! producers write records: the propagation role (message and peer records
//! through the [`PnOp`] channel below) and the `--store-storm` bench
//! instrument (synthetic records, so the erase storm can be measured under
//! BLE and LoRa load). Everything reaches the log by channel; the sender
//! never waits for the flash.
//!
//! # Where the cut is
//!
//! This module holds what is the same on every chip: the channels and
//! their requests ([`request_storm`], [`request_board_list`],
//! [`pn_execute`]), the counters, the mount, the propagation and storm
//! operations, and [`run`], the task body, generic over the flash and
//! over the [`Region`] the operator lists are scanned in. The bind crate
//! keeps what touches its part: where the region lies, the flash device
//! with its geometry and its retry loop around a shared handle (which
//! reports each refused operation through [`note_op_fail`]), and the
//! embassy task that calls [`run`] (on the nRF52840
//! `leviculum-nrf/src/record_store.rs`).
//!
//! # Why the log lives in a task and is reached by channel
//!
//! A backend's write and erase may not be cancelled: `nrf_softdevice::Flash`
//! arms a `DropBomb` and **panics** if one is dropped in flight. A caller
//! that could cancel an append, a `select!` with a timeout or a connection
//! task that goes away, is therefore a caller that could panic the board.
//! Nothing that can be cancelled may hold an append, so the log is owned by
//! a task that does nothing else, and [`run`]'s one `select` has only
//! cancel-safe arms.
//!
//! # What is logged
//!
//! One line at mount, ungated so it reaches a host that attaches late:
//!
//! ```text
//! STORE mount state=<ours|formatted> pages=<n> live=<n> free_bytes=<n> t=<ms>
//! ```
//!
//! then, at runtime, one line per failed flash operation and a summary
//! whenever a counter moved since the last one:
//!
//! ```text
//! STORE op_fail op=<erase|write> attempt=<n> t=<ms>
//! STORE stats appends=<n> fails=<n> sealed_pages=<n> t=<ms>
//! ```
//!
//! `fails` counts flash operations the backend refused (the `op_fail`
//! lines); `sealed_pages` counts pages given up because an append failed part
//! way through and the rest of that page can no longer be programmed. The
//! `t=<ms>` stamp is appended by [`crate::log`] to every line, so neither
//! format string carries one.

extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use leviculum_pn_store::Region;
use leviculum_record_log::board::{mount_or_format, sealed_a_page, storm_admits, storm_record};
use leviculum_record_log::{Error as LogError, RecordLog, KEY_LEN, MAX_BODY};

use crate::seams::Flash;

/// Tag every record the bench instrument writes carries.
///
/// The record log reads no tag — it is one opaque byte (see that crate's
/// module docs) — so this is a convention between the instrument that writes
/// synthetic records and whatever purges them later. It exists so a measuring
/// run's records are distinguishable from a real message's without reading a
/// body: a later batch can sweep the region for `tag == TAG_BENCH` and purge
/// exactly those.
pub const TAG_BENCH: u8 = 0xB0;

/// The propagation role's tag map (`leviculum-pn-store`) must keep bench
/// records invisible: above the message clamp and distinct from the peer
/// tag, or a storm would masquerade as messages or peers.
const _: () = assert!(
    TAG_BENCH > leviculum_pn_store::MESSAGE_TAG_MAX
        && TAG_BENCH != leviculum_pn_store::TAG_PEER
        && TAG_BENCH != leviculum_pn_store::TAG_PRICED
        && TAG_BENCH != leviculum_pn_store::TAG_RECALL
        && TAG_BENCH != leviculum_pn_store::TAG_STATIC
        && TAG_BENCH != leviculum_pn_store::TAG_AUTH
        && TAG_BENCH != leviculum_pn_store::TAG_CONTROL,
    "TAG_BENCH collides with the propagation store's tag map"
);

/// The largest synthetic record the bench instrument will write, and the bound
/// the control envelope refuses above
/// ([`leviculum_core::envelope::STORE_STORM_MAX_BYTES`]).
///
/// Tied to the envelope's number here rather than restated: the envelope is
/// what refuses a host's value, and this assertion is what stops that bound
/// from outgrowing the page a record has to fit in.
const _: () = assert!(
    leviculum_core::envelope::STORE_STORM_MAX_BYTES as usize <= MAX_BODY,
    "the control envelope would accept a storm record larger than a page can hold"
);

/// What the bench instrument may ask the store task to do. The message
/// producer — the propagation engine — has its own channel (`PN_OPS`)
/// because its ops carry heap bodies and demand a per-op reply; neither
/// is another owner of the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Append `records` synthetic records of `size` body bytes each, tagged
    /// [`TAG_BENCH`] (#384 bench instrument, `--store-storm`). Bounded and
    /// refused while one is running — see [`request_storm`].
    Storm { records: u16, size: u16 },
}

/// A pending operator-list write (audit 2026-10-12 D4, D18, `lnflash
/// --static-peer` and kin) — see [`request_board_list`]. Its own channel
/// rather than a [`Request`] variant: the list is 258 B, and every storm
/// request would carry that width.
static BOARD_LIST_REQUESTS: Channel<
    CriticalSectionRawMutex,
    leviculum_core::envelope::PnListWire,
    1,
> = Channel::new();

/// The envelope's bound and the store's are one number.
const _: () = assert!(
    leviculum_core::envelope::PN_LIST_MAX_HASHES == leviculum_pn_store::BOARD_LIST_MAX,
    "the operator-list frame and the store disagree on a list's cap"
);

/// The store's list for an envelope `list` byte.
fn board_list_of(list: u8) -> Option<leviculum_pn_store::BoardList> {
    use leviculum_core::envelope::{
        PN_LIST_AUTH_ALLOWED, PN_LIST_CONTROL_ALLOWED, PN_LIST_STATIC_PEERS,
    };
    use leviculum_pn_store::BoardList;
    match list {
        PN_LIST_STATIC_PEERS => Some(BoardList::StaticPeers),
        PN_LIST_AUTH_ALLOWED => Some(BoardList::AuthAllowed),
        PN_LIST_CONTROL_ALLOWED => Some(BoardList::ControlAllowed),
        _ => None,
    }
}

/// Whether the last [`Request::BoardList`] landed: every op of the write
/// performed. Signalled by the store task, awaited by the serial task's
/// [`board_list_landed`].
static BOARD_LIST_DONE: Signal<CriticalSectionRawMutex, bool> = Signal::new();

/// Pending requests. Depth 1: a second storm while one runs is refused by
/// [`request_storm`] rather than queued, because a measurement that silently
/// ran twice is worse than one that was told no.
static REQUESTS: Channel<CriticalSectionRawMutex, Request, 1> = Channel::new();

/// One write the propagation engine owes the log (Codeberg #384 part 3):
/// the flush half of `leviculum-pn-store`'s queued ops. Bodies ride the
/// heap because a message body is up to a page.
pub enum PnOp {
    Append {
        key: [u8; KEY_LEN],
        time: u32,
        tag: u8,
        body: alloc::vec::Vec<u8>,
    },
    /// Purge the record at this **region-relative** offset iff its key
    /// still matches — the page may have been reclaimed since the caller
    /// scanned it, and purging whatever now sits there would corrupt a
    /// stranger record's commit word.
    Purge { offset: u32, key: [u8; KEY_LEN] },
}

/// What a completed [`PnOp`] reports back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PnDone {
    /// The log's `free_bytes` after the op: bytes appendable before
    /// reclaim has to erase a page that still holds records. Reported,
    /// and consulted by nobody since 2026-09-13 — the engine's
    /// `PropagationStore::free_space` answers the trait's question
    /// (bytes a body could still use across the region) from its own
    /// scan, because this number is the active page's room after the
    /// first lap and reads 0 on a full page beside an almost empty
    /// store (`leviculum-nrf/pn-store/src/lib.rs`, `free_space`).
    pub free_bytes: u32,
}

/// Depth 1 and one caller: the propagation engine flushes one op at a
/// time and awaits [`PN_DONE`] before the next, which is what keeps the
/// reply unambiguous without a ticket.
static PN_OPS: Channel<CriticalSectionRawMutex, PnOp, 1> = Channel::new();
static PN_DONE: Signal<CriticalSectionRawMutex, Result<PnDone, ()>> = Signal::new();

/// Execute one propagation-store write on the log task.
///
/// `Err(())` means the op did not land: the store is not mounted, or the
/// flash refused it past the retry budget. The engine's contract with
/// `leviculum-pn-store` is exactly this bool — on an append failure it
/// un-remembers the id from the role's duplicate cache and sends no
/// proof; on a purge failure it drops the mask and the record reappears.
///
/// Must not be called concurrently with itself (single engine, main
/// loop); the await is safe to hold across the flash's own retries
/// because the caller runs in an arm of the main loop, never inside a
/// `select` that could drop it — and even a drop would only lose the
/// *reply*: the op itself completes on this task, which owns the log.
pub async fn pn_execute(op: PnOp) -> Result<PnDone, ()> {
    if !MOUNTED.load(Ordering::Relaxed) {
        return Err(());
    }
    PN_DONE.reset();
    PN_OPS.send(op).await;
    PN_DONE.wait().await
}

/// Set once the log is mounted. Until then there is nothing to append to and
/// [`request_storm`] refuses.
static MOUNTED: AtomicBool = AtomicBool::new(false);
/// Set while a storm is being executed.
static STORM_RUNNING: AtomicBool = AtomicBool::new(false);

/// Records successfully appended since boot.
static APPENDS: AtomicU32 = AtomicU32::new(0);
/// Flash operations the backend refused, retried or not.
static OP_FAILS: AtomicU32 = AtomicU32::new(0);
/// Pages given up because an append failed after its first program run.
static SEALED_PAGES: AtomicU32 = AtomicU32::new(0);

/// How often the task prints [`stats`](self#what-is-logged), if anything moved.
const STATS_PERIOD_S: u64 = 300;

/// Ask the store task to append `records` synthetic records of `size` bytes.
///
/// Never blocks and never touches flash on the caller's stack. `false` means
/// the request was refused and nothing will happen: the log is not mounted, a
/// storm is already running, or the slot is taken. The caller answers the host
/// with a refusal — an instrument that silently dropped a request would make
/// the next measurement a lie.
///
/// The bounds on `records` and `size` belong to
/// [`leviculum_core::envelope::classify_control_frame`], which refuses out-of-
/// range values by name on every binary; this function re-checks `size`
/// against what a page can hold so that a caller which is not the envelope
/// cannot get past it.
pub fn request_storm(records: u16, size: u16) -> bool {
    if !storm_admits(
        MOUNTED.load(Ordering::Relaxed),
        STORM_RUNNING.load(Ordering::Relaxed),
        records,
        size,
    ) {
        return false;
    }
    REQUESTS.try_send(Request::Storm { records, size }).is_ok()
}

/// Ask the store task to replace one operator list.
///
/// The list is a record of the message log, so the log's one writer writes
/// it, and the same task keeps it from being reclaimed (the refresh after
/// every page change in [`run`]); the engine only reads the lists at
/// boot. `false` means nothing will happen: the log is not mounted, a storm
/// is running, or the slot is taken. The caller then refuses with BUSY.
pub fn request_board_list(wire: leviculum_core::envelope::PnListWire) -> bool {
    if !MOUNTED.load(Ordering::Relaxed) || STORM_RUNNING.load(Ordering::Relaxed) {
        return false;
    }
    BOARD_LIST_DONE.reset();
    BOARD_LIST_REQUESTS.try_send(wire).is_ok()
}

/// Wait for the outcome of the request [`request_board_list`] accepted.
/// `None` when the store did not answer within `within` — the write may
/// still land, so the caller must not claim it did.
pub async fn board_list_landed(within: embassy_time::Duration) -> Option<bool> {
    embassy_time::with_timeout(within, BOARD_LIST_DONE.wait())
        .await
        .ok()
}

/// Whether a storm is running right now.
pub fn storm_running() -> bool {
    STORM_RUNNING.load(Ordering::Relaxed)
}

/// Whether the log is mounted.
pub fn mounted() -> bool {
    MOUNTED.load(Ordering::Relaxed)
}

/// Count one flash operation the backend refused and say so: the
/// `op_fail` line. For the bind crate's flash device, which retries a
/// refused operation (`leviculum_record_log::board::retry_after` says how
/// often and how long apart) and knows its kind.
pub fn note_op_fail(op: &str, attempt: u8) {
    OP_FAILS.fetch_add(1, Ordering::Relaxed);
    crate::log::log_fmt(
        "STORE ",
        format_args!("op_fail op={} attempt={}", op, attempt),
    );
}

/// Mount the store and serve requests for the lifetime of the board: the
/// body of the bind crate's store task.
///
/// Mounts first, and formats only a region no log of ours wrote: `mount`
/// answers `None` for an unformatted region *and* for somebody else's data,
/// and either way the one erase that follows is this board's decision about
/// its own flash. Nothing above this task can ask for a format. `base` and
/// `len` are the region on `device`; `lists` maps the same region for the
/// operator lists' scans.
pub async fn run<F: Flash, R: Region>(device: F, base: u32, len: u32, lists: R) {
    let mut log = match mount(device, base, len).await {
        Some(log) => log,
        // No store this boot. The task ends rather than spinning, `MOUNTED`
        // stays false, and every request is refused with a reason instead of
        // being dropped into a channel nothing reads.
        None => return,
    };
    MOUNTED.store(true, Ordering::Relaxed);

    let mut ticker = embassy_time::Ticker::every(embassy_time::Duration::from_secs(STATS_PERIOD_S));
    let mut reported = (0u32, 0u32, 0u32);
    // The active page the operator lists were last checked against; a copy
    // only ages when this moves. `u32::MAX` checks once at mount.
    let mut lists_checked_at = u32::MAX;
    loop {
        use embassy_futures::select::{select4, Either4};
        // The only place this task selects, and therefore the only place a
        // future of it is ever dropped: all arms are cheap and cancel-safe,
        // and an append is never inside one. See the module docs on the
        // `DropBomb`.
        match select4(
            REQUESTS.receive(),
            PN_OPS.receive(),
            ticker.next(),
            BOARD_LIST_REQUESTS.receive(),
        )
        .await
        {
            Either4::First(Request::Storm { records, size }) => {
                STORM_RUNNING.store(true, Ordering::Relaxed);
                storm(&mut log, records, size).await;
                STORM_RUNNING.store(false, Ordering::Relaxed);
            }
            Either4::Second(op) => {
                let outcome = pn_perform(&mut log, base, op).await;
                PN_DONE.signal(outcome);
            }
            Either4::Third(()) => {}
            Either4::Fourth(wire) => {
                let landed = write_board_list(&mut log, base, &lists, wire).await;
                BOARD_LIST_DONE.signal(landed);
            }
        }
        if log.sequence() != lists_checked_at {
            lists_checked_at = log.sequence();
            refresh_board_lists(&mut log, base, &lists).await;
        }
        let now = (
            APPENDS.load(Ordering::Relaxed),
            OP_FAILS.load(Ordering::Relaxed),
            SEALED_PAGES.load(Ordering::Relaxed),
        );
        // Silent while nothing happens: a board that never appends has
        // nothing to summarise, and a debug capture of a long run should not
        // have to scroll past it.
        if now != reported {
            crate::log::log_fmt(
                "STORE ",
                format_args!(
                    "stats appends={} fails={} sealed_pages={}",
                    now.0, now.1, now.2
                ),
            );
            reported = now;
        }
    }
}

/// Perform one propagation-store write (Codeberg #384 part 3).
///
/// The append counts into the same [`APPENDS`] the storms count into; a
/// failure counts a sealed page exactly as a storm's does. The purge
/// re-validates the record at its offset before touching its commit word
/// — a reclaimed page answers "no record" or a different key, and the op
/// then reports success with nothing to do, because a record that is
/// already gone is what the purge wanted.
async fn pn_perform<F: Flash>(log: &mut RecordLog<F>, base: u32, op: PnOp) -> Result<PnDone, ()> {
    match op {
        PnOp::Append {
            key,
            time,
            tag,
            body,
        } => {
            let room_before = log.sector_room();
            match log.append(&key, time, tag, &body).await {
                Ok(_) => {
                    APPENDS.fetch_add(1, Ordering::Relaxed);
                    Ok(PnDone {
                        free_bytes: log.free_bytes(),
                    })
                }
                Err(err) => {
                    if sealed_a_page(room_before, log.sector_room()) {
                        SEALED_PAGES.fetch_add(1, Ordering::Relaxed);
                    }
                    crate::log::log_fmt(
                        "STORE ",
                        format_args!("pn_append_failed reason={}", reason(&err)),
                    );
                    Err(())
                }
            }
        }
        PnOp::Purge { offset, key } => {
            let at = base.saturating_add(offset);
            match log.record_at(at).await {
                Ok(Some(record)) if record.key == key => match log.purge(&record).await {
                    Ok(()) => Ok(PnDone {
                        free_bytes: log.free_bytes(),
                    }),
                    Err(err) => {
                        crate::log::log_fmt(
                            "STORE ",
                            format_args!("pn_purge_failed reason={}", reason(&err)),
                        );
                        Err(())
                    }
                },
                Ok(_) => Ok(PnDone {
                    free_bytes: log.free_bytes(),
                }),
                Err(err) => {
                    crate::log::log_fmt(
                        "STORE ",
                        format_args!("pn_purge_failed reason={}", reason(&err)),
                    );
                    Err(())
                }
            }
        }
    }
}

/// Perform a run of [`leviculum_pn_store::FlushOp`]s in order; whether
/// every one landed.
async fn perform_all<F: Flash>(
    log: &mut RecordLog<F>,
    base: u32,
    ops: alloc::vec::Vec<leviculum_pn_store::FlushOp>,
) -> bool {
    use leviculum_pn_store::FlushOp;
    let mut landed = true;
    for op in ops {
        let op = match op {
            FlushOp::Append {
                key,
                time,
                tag,
                body,
            } => PnOp::Append {
                key,
                time,
                tag,
                body,
            },
            FlushOp::Purge { offset, key } | FlushOp::ConfirmedPurge { offset, key } => {
                PnOp::Purge { offset, key }
            }
        };
        landed &= pn_perform(log, base, op).await.is_ok();
    }
    landed
}

/// Replace one operator list (audit 2026-10-12 D4, D18): the new record,
/// then the purge of the old. One line says what landed; the boot line
/// `PN_LISTS` of the next reset says what the role took.
async fn write_board_list<F: Flash, R: Region>(
    log: &mut RecordLog<F>,
    base: u32,
    region: &R,
    wire: leviculum_core::envelope::PnListWire,
) -> bool {
    let Some(list) = board_list_of(wire.list()) else {
        return false;
    };
    let Some(ops) = leviculum_pn_store::board_list_write_ops(region, list, wire.hashes()) else {
        return false;
    };
    let landed = perform_all(log, base, ops).await;
    crate::log::log_fmt_critical(
        "STORE ",
        format_args!(
            "pn_list list={} count={} landed={} effective=next-boot",
            wire.list(),
            wire.hashes().len(),
            landed as u8
        ),
    );
    landed
}

/// Rewrite any operator list whose copy aged into the older half of the
/// region ([`leviculum_pn_store::board_list_refresh_ops`]). One scan per
/// list, run only when the active page moved; nothing is written while the
/// lists are fresh.
async fn refresh_board_lists<F: Flash, R: Region>(log: &mut RecordLog<F>, base: u32, region: &R) {
    let ops = leviculum_pn_store::board_list_refresh_ops(region);
    if ops.is_empty() {
        return;
    }
    let landed = perform_all(log, base, ops).await;
    crate::log::log_fmt(
        "STORE ",
        format_args!("pn_list_refresh landed={}", landed as u8),
    );
}

/// Mount the region, formatting it once if it is not ours, and say which of
/// the two happened.
///
/// `None` means the store is unavailable for this boot and is the one outcome
/// that leaves [`MOUNTED`] false — every control request is then refused
/// rather than silently dropped. What counts as ours, and which failure
/// leaves no store, is [`mount_or_format`].
async fn mount<F: Flash>(device: F, base: u32, len: u32) -> Option<RecordLog<F>> {
    let mounted = match mount_or_format(device, base, len).await {
        Ok(mounted) => mounted,
        Err(err) => {
            log_mount_failure(&err);
            return None;
        }
    };
    crate::log::log_fmt_critical(
        "STORE ",
        format_args!(
            "mount state={} pages={} live={} free_bytes={}",
            mounted.state.as_str(),
            mounted.log.sectors(),
            mounted.live,
            mounted.log.free_bytes()
        ),
    );
    Some(mounted.log)
}

fn log_mount_failure<E>(err: &LogError<E>) {
    crate::log::log_fmt_critical(
        "STORE ",
        format_args!("mount failed reason={}", reason(err)),
    );
}

/// Why a store operation failed, as one stable token per cause.
fn reason<E>(err: &LogError<E>) -> &'static str {
    match err {
        LogError::Flash(_) => "flash",
        LogError::BodyTooLarge => "body-too-large",
        LogError::BadRegion => "bad-region",
        LogError::OutOfBounds => "out-of-bounds",
        LogError::UnsupportedGeometry => "geometry",
    }
}

/// Append `records` synthetic records of `size` bytes, tagged [`TAG_BENCH`].
///
/// The instrument for the erase-storm measurement: N records of a known size
/// is N × `stride` bytes of programming and one erase per page it fills, under
/// whatever BLE and LoRa load the board is carrying at the time. Nothing is
/// persisted as configuration and nothing is repeated — a storm happens once,
/// when asked.
async fn storm<F: Flash>(log: &mut RecordLog<F>, records: u16, size: u16) {
    let start = embassy_time::Instant::now();
    let mut body = [0xA5u8; leviculum_core::envelope::STORE_STORM_MAX_BYTES as usize];
    let size = core::cmp::min(size as usize, body.len());
    let mut appended = 0u16;
    let mut failed = 0u16;

    for n in 0..records {
        // The key and the index bytes are `storm_record`'s; the log itself
        // reads neither.
        let (key, size) = storm_record(n, size as u16, &mut body);

        let room_before = log.sector_room();
        // Uptime, not wall time: the timestamp is opaque to the log, a bench
        // record is only ever read back against the same boot, and a board
        // with no GNSS fix has no calendar to offer.
        let time = embassy_time::Instant::now().as_millis() as u32;
        match log.append(&key, time, TAG_BENCH, &body[..size]).await {
            Ok(_) => {
                APPENDS.fetch_add(1, Ordering::Relaxed);
                appended += 1;
            }
            Err(err) => {
                failed += 1;
                if sealed_a_page(room_before, log.sector_room()) {
                    SEALED_PAGES.fetch_add(1, Ordering::Relaxed);
                }
                crate::log::log_fmt(
                    "STORE ",
                    format_args!("append_failed record={} reason={}", n, reason(&err)),
                );
            }
        }
    }

    crate::log::log_fmt(
        "STORE ",
        format_args!(
            "storm records={} size={} appended={} failed={} seq={} ms={}",
            records,
            size,
            appended,
            failed,
            log.sequence(),
            start.elapsed().as_millis()
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fakes::{ready, FakeFlash};

    const PAGES: usize = 4;

    #[test]
    fn a_blank_region_mounts_and_takes_appends_and_a_storm() {
        let len = (PAGES * FakeFlash::ERASE_SIZE) as u32;
        let mut log = ready(mount(FakeFlash::new(PAGES), 0, len)).expect("a blank region formats");
        let before = APPENDS.load(Ordering::Relaxed);
        let done = ready(pn_perform(
            &mut log,
            0,
            PnOp::Append {
                key: [7; KEY_LEN],
                time: 1,
                tag: 0,
                body: alloc::vec![0x42; 100],
            },
        ));
        assert!(done.is_ok());
        ready(storm(&mut log, 3, 64));
        assert_eq!(APPENDS.load(Ordering::Relaxed), before + 4);
    }

    #[test]
    fn every_store_failure_has_its_own_token() {
        let tokens = [
            reason::<()>(&LogError::Flash(())),
            reason::<()>(&LogError::BodyTooLarge),
            reason::<()>(&LogError::BadRegion),
            reason::<()>(&LogError::OutOfBounds),
            reason::<()>(&LogError::UnsupportedGeometry),
        ];
        for (i, a) in tokens.iter().enumerate() {
            assert!(tokens[i + 1..].iter().all(|b| a != b), "{a} twice");
        }
    }
}
