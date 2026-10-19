//! Boot-time module init for the GNSS receivers this firmware drives.
//!
//! Three receivers, three proprietary command languages, one shape. The
//! WisMesh Pocket V2 carries a u-blox ZOE-M8Q and speaks UBX
//! ([`ubx`], Codeberg #324); the Heltec Mesh Node T114 carries a
//! Quectel L76K and speaks the CASIC `$PCAS` sentences ([`l76k`],
//! Codeberg #69); the SenseCAP T1000-E carries an Airoha AG3335 and
//! speaks the `$PAIR` sentences ([`airoha`], Codeberg #456). What they
//! have in common is everything except the bytes:
//!
//! - the sequence is **one-shot per boot** and silent forever after,
//! - **nothing is transmitted before the presence machine has locked a
//!   baud** (a command at the wrong baud is garbage into the module),
//! - each step after the first additionally waits for a settle period
//!   **and** a *fresh* clean sentence, so a command is never fired into
//!   a module that is still reconfiguring,
//! - an outcome — ok, nak or timeout — is **reported, never retried and
//!   never blocking**, mirroring the Meshtastic reference which warns
//!   and continues (`meshtastic/src/gps/GPS.cpp`).
//!
//! That common shape is [`ModuleInit`], and it is what lets the
//! firmware's single GNSS driver task serve every board: it feeds RX
//! bytes, lock state, the clean-sentence counter and time into whichever
//! sequencer its board selected, and acts on the [`Output`]s without
//! knowing which module language it is speaking.
//!
//! All sequencers are pure and host-tested, like
//! `leviculum-gnss-presence` beside them. Frame bytes are computed at
//! compile time from the payload, never hardcoded, and the tests assert
//! the result against independently computed fixtures so a checksum bug
//! cannot self-confirm.

#![cfg_attr(not(test), no_std)]

pub mod airoha;
pub mod l76k;
pub mod ubx;

pub use airoha::AirohaInit;
pub use l76k::L76kInit;
pub use ubx::UbxInit;

/// Largest frame any sequencer in this crate emits. Drivers size their
/// TX staging buffer with this (UBX frames are flash consts and nRF
/// EasyDMA reads RAM only, so the driver has to stage them).
///
/// The bound is the L76K sentence-selection sentence at 38 bytes; the
/// longest UBX frame is 21. Each module asserts its own frames against
/// this at compile time.
pub const MAX_FRAME: usize = 38;

/// How an acknowledged step concluded. `as_str` is the stable
/// `[GNSS_INIT] step=<...> ack=<...>` log token.
///
/// `Nak` is UBX-only: the L76K's `$PCAS` commands are not acknowledged
/// at all, so its one acknowledged step is a query whose answer either
/// arrives ([`Ack`](Self::Ack)) or does not ([`Timeout`](Self::Timeout)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AckOutcome {
    Ack,
    Nak,
    Timeout,
}

impl AckOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            AckOutcome::Ack => "ok",
            AckOutcome::Nak => "nak",
            AckOutcome::Timeout => "timeout",
        }
    }
}

/// One sequencer output, handed to the driver's `emit` callback.
///
/// `step` is the log token, not a typed enum: it is the one field every
/// module language must agree on (the driver prints it and periculum
/// greps it), and the step *sets* are deliberately disjoint. Each
/// module's `Step::as_str` is the sole producer of these strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    /// Write this frame to the module, then log
    /// `[GNSS_INIT] step=<step> sent`.
    Send {
        step: &'static str,
        frame: &'static [u8],
    },
    /// An acknowledged step concluded; log
    /// `[GNSS_INIT] step=<step> ack=<outcome>`. Purely informational —
    /// the sequence continues either way.
    AckResult {
        step: &'static str,
        outcome: AckOutcome,
    },
}

/// The surface the firmware's GNSS driver task drives, implemented once
/// per module language.
///
/// Both methods take the driver's view of the line and may emit any
/// number of outputs (in practice at most one [`Output::Send`] and one
/// [`Output::AckResult`] per call). An implementation must be silent
/// before the first lock and silent forever after its last step.
pub trait ModuleInit {
    /// Feed a chunk of RX bytes — the same chunk the presence machine
    /// sees. Only consulted while a step waits for an answer.
    fn on_bytes(&mut self, bytes: &[u8], now_ms: u64, emit: &mut dyn FnMut(Output));

    /// Drive the sequence. `locked` and `sentences` come from the
    /// presence machine (`locked()` / `sentences_seen()`), `now_ms` from
    /// the same monotonic clock it uses.
    fn poll(&mut self, locked: bool, sentences: u32, now_ms: u64, emit: &mut dyn FnMut(Output));
}

/// Build an NMEA sentence at compile time: `$`, body, `*`, the two
/// uppercase hex digits of the XOR checksum over the body, CRLF. Shared
/// by the two NMEA-speaking modules ([`l76k`], [`airoha`]).
///
/// `N` must be `body.len() + 6`; the assert makes a mismatch a compile
/// error because the function is only evaluated in const context (the
/// `FRAME` consts of each module).
pub(crate) const fn nmea_frame<const N: usize>(body: &[u8]) -> [u8; N] {
    assert!(N == body.len() + 6, "N must be body length + 6");
    let mut f = [0u8; N];
    f[0] = b'$';
    let mut ck: u8 = 0;
    let mut i = 0;
    while i < body.len() {
        f[1 + i] = body[i];
        ck ^= body[i];
        i += 1;
    }
    let end = 1 + body.len();
    f[end] = b'*';
    f[end + 1] = hex_upper(ck >> 4);
    f[end + 2] = hex_upper(ck & 0x0F);
    f[end + 3] = b'\r';
    f[end + 4] = b'\n';
    f
}

/// One nibble as an uppercase ASCII hex digit (NMEA checksums are
/// uppercase; the reference's literals are).
const fn hex_upper(nibble: u8) -> u8 {
    if nibble < 10 {
        b'0' + nibble
    } else {
        b'A' + (nibble - 10)
    }
}

/// Rolling literal matcher for a probe answer in the RX byte stream.
///
/// The answer arrives inside a normal NMEA burst, so it has to be found
/// between other sentences and possibly split across read chunks. A
/// byte that breaks the match may itself be the start of a new one
/// (`$` is the only such byte for the patterns used here, since each
/// begins with `$` and contains no other), so the reset is to "matched
/// one byte" in that case.
pub(crate) struct LiteralScanner {
    pattern: &'static [u8],
    matched: usize,
}

impl LiteralScanner {
    pub(crate) const fn new(pattern: &'static [u8]) -> Self {
        Self {
            pattern,
            matched: 0,
        }
    }

    /// Feed one byte; `true` on the byte that completes the pattern.
    pub(crate) fn push(&mut self, byte: u8) -> bool {
        if byte == self.pattern[self.matched] {
            self.matched += 1;
            if self.matched == self.pattern.len() {
                self.matched = 0;
                return true;
            }
            return false;
        }
        // Mismatch: restart, but do not swallow a byte that begins the
        // pattern itself.
        self.matched = usize::from(byte == self.pattern[0]);
        false
    }
}
