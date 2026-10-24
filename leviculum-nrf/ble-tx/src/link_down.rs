//! The board's `BLE_LINK_DOWN` line: one per link end, both roles,
//! with the reason the SoftDevice gave for it (912).
//!
//! Before this line a board said nothing when a peripheral link ended
//! except the `BLE_CONN_PARAMS … when=close` reading, and the central
//! role's `BLE_CENTRAL_DOWN` covers only sessions that reached the
//! registry and carries no reason. The reason is what tells a phone
//! that switched Bluetooth off (`remote-user`, every board within the
//! same few milliseconds) from one that walked out of range
//! (`supervision-timeout`, one board at a time, one timeout after the
//! last packet), and a link we closed ourselves (`local-host`) from
//! both.
//!
//! The mapping is a pure function of the HCI status byte the
//! `BLE_GAP_EVT_DISCONNECTED` event carries, so it lives here and is
//! tested on the host; the firmware reads the byte off the connection
//! and renders [`LinkDownLine`].

use core::fmt;

use crate::conn_params::{ConnParams, LinkRole};

/// HCI status 0x08, Connection Timeout: the supervision timeout ran out
/// with nothing heard from the peer.
pub const HCI_CONNECTION_TIMEOUT: u8 = 0x08;
/// HCI status 0x13, Remote User Terminated Connection: the peer closed
/// the link on purpose.
pub const HCI_REMOTE_USER_TERMINATED: u8 = 0x13;
/// HCI status 0x16, Connection Terminated by Local Host: we closed it.
/// The SoftDevice reports this for every local disconnect, whatever
/// reason code the disconnect sent the peer.
pub const HCI_LOCAL_HOST_TERMINATED: u8 = 0x16;
/// HCI status 0x3D, Connection Terminated due to MIC Failure.
pub const HCI_MIC_FAILURE: u8 = 0x3D;
/// HCI status 0x3E, Connection Failed to be Established: the connect
/// request went out and the first connection events never answered.
pub const HCI_CONN_FAILED_TO_BE_ESTABLISHED: u8 = 0x3E;

/// The `reason=` token of a link end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    /// 0x08.
    SupervisionTimeout,
    /// 0x13.
    RemoteUser,
    /// 0x16.
    LocalHost,
    /// 0x3E.
    ConnectionFailedToEstablish,
    /// 0x3D.
    MicFailure,
    /// Any other status, printed as `other-0x<hh>`.
    Other(u8),
    /// No disconnect event within [`disconnect_settle_ms`]: the link
    /// was let go without the SoftDevice having said why.
    Unknown,
}

impl DisconnectReason {
    /// Map the HCI status of a `BLE_GAP_EVT_DISCONNECTED`, or its
    /// absence, to the token.
    #[must_use]
    pub const fn from_hci(status: Option<u8>) -> Self {
        match status {
            Some(HCI_CONNECTION_TIMEOUT) => Self::SupervisionTimeout,
            Some(HCI_REMOTE_USER_TERMINATED) => Self::RemoteUser,
            Some(HCI_LOCAL_HOST_TERMINATED) => Self::LocalHost,
            Some(HCI_CONN_FAILED_TO_BE_ESTABLISHED) => Self::ConnectionFailedToEstablish,
            Some(HCI_MIC_FAILURE) => Self::MicFailure,
            Some(other) => Self::Other(other),
            None => Self::Unknown,
        }
    }
}

impl fmt::Display for DisconnectReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SupervisionTimeout => f.write_str("supervision-timeout"),
            Self::RemoteUser => f.write_str("remote-user"),
            Self::LocalHost => f.write_str("local-host"),
            Self::ConnectionFailedToEstablish => f.write_str("connection-failed-to-establish"),
            Self::MicFailure => f.write_str("mic-failure"),
            Self::Other(code) => write!(f, "other-0x{code:02x}"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// Slack on top of the supervision timeout before a teardown stops
/// waiting for the disconnect event.
pub const DISCONNECT_SETTLE_MARGIN_MS: u64 = 1_000;

/// How long a teardown waits for the SoftDevice's disconnect event
/// after closing a link itself.
///
/// A local disconnect sends `LL_TERMINATE_IND` and completes when the
/// peer acknowledges it, a connection interval or two later; a peer
/// that never acknowledges it ends the procedure at the supervision
/// timeout. One timeout plus [`DISCONNECT_SETTLE_MARGIN_MS`] therefore
/// covers every case the controller can produce, and the wait returns
/// as soon as the event lands, so a healthy link pays a few intervals.
#[must_use]
pub const fn disconnect_settle_ms(params: ConnParams) -> u64 {
    params.timeout_ms() as u64 + DISCONNECT_SETTLE_MARGIN_MS
}

/// The largest supervision timeout the Bluetooth spec allows, 3200
/// units of 10 ms.
pub const MAX_SUPERVISION_TIMEOUT_MS: u64 = 32_000;

/// How long a teardown keeps the `Connection` after its first
/// `disconnect()` before it gives the handle up without dropping it
/// (#472).
///
/// Dropping a `Connection` while the SoftDevice is still disconnecting
/// is what panicked the boards (upstream nrf-softdevice unwraps the
/// drop's own `disconnect()`). The pinned fork swallows that error;
/// this bound is the second line of defence: the teardown holds the
/// handle until the disconnect event has landed, and when it never
/// lands it leaks the handle instead of dropping it, a lost
/// connection slot rather than a reset.
///
/// The longest supervision timeout plus [`DISCONNECT_SETTLE_MARGIN_MS`],
/// so it covers every link whatever it negotiated; and never less than
/// [`disconnect_settle_ms`] of the link at hand, so the `BLE_LINK_DOWN`
/// line, written at the settle bound, always comes first, even for a
/// parameter set read off a confused SoftDevice outside the legal range.
#[must_use]
pub const fn forget_deadline_ms(params: ConnParams) -> u64 {
    let floor = MAX_SUPERVISION_TIMEOUT_MS + DISCONNECT_SETTLE_MARGIN_MS;
    let settle = disconnect_settle_ms(params);
    if settle > floor {
        settle
    } else {
        floor
    }
}

/// The `BLE_LINK_DOWN` line's body, byte-exact; the log layer appends
/// the `t=` stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkDownLine {
    pub role: LinkRole,
    /// The connection handle the link's `BLE_CONN_PARAMS` lines carry.
    pub conn: u16,
    /// The identity the handshake read, if it got that far: the
    /// peripheral's 16-byte write, the central's Identity read.
    pub peer: Option<[u8; 16]>,
    pub reason: DisconnectReason,
}

impl fmt::Display for LinkDownLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BLE_LINK_DOWN role={} conn={} peer=",
            self.role.as_str(),
            self.conn
        )?;
        match self.peer {
            Some(id) => write!(f, "{:02x}{:02x}{:02x}{:02x}", id[0], id[1], id[2], id[3])?,
            None => f.write_str("none")?,
        }
        write!(f, " reason={}", self.reason)
    }
}

/// The `BLE_LINK_FORGOTTEN` line's body, byte-exact; the log layer
/// appends the `t=` stamp.
///
/// Written when a teardown gave a `Connection` up without dropping it,
/// because no disconnect event arrived within [`forget_deadline_ms`]
/// (#472). The connection slot it held is leaked for the rest of the
/// boot; this line is the only way a capture can see that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkForgottenLine {
    pub role: LinkRole,
    /// The handle the link's `BLE_LINK_DOWN` line carried.
    pub conn: u16,
    /// Milliseconds from the teardown's first `disconnect()` to the
    /// give-up.
    pub waited_ms: u64,
}

impl fmt::Display for LinkForgottenLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "BLE_LINK_FORGOTTEN role={} conn={} waited_ms={}",
            self.role.as_str(),
            self.conn,
            self.waited_ms
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn named_hci_codes_map_to_their_tokens() {
        let table = [
            (0x08, "supervision-timeout"),
            (0x13, "remote-user"),
            (0x16, "local-host"),
            (0x3E, "connection-failed-to-establish"),
            (0x3D, "mic-failure"),
        ];
        for (code, token) in table {
            assert_eq!(
                DisconnectReason::from_hci(Some(code)).to_string(),
                token,
                "code {code:#04x}"
            );
        }
    }

    /// The remote's other two termination codes (low resources, power
    /// off) are not folded into `remote-user`: the byte is what a
    /// capture needs, and `other-` keeps it.
    #[test]
    fn unnamed_codes_keep_their_byte_in_two_lower_case_digits() {
        assert_eq!(
            DisconnectReason::from_hci(Some(0x15)).to_string(),
            "other-0x15"
        );
        assert_eq!(
            DisconnectReason::from_hci(Some(0x3b)).to_string(),
            "other-0x3b"
        );
        assert_eq!(
            DisconnectReason::from_hci(Some(0x00)).to_string(),
            "other-0x00"
        );
        assert_eq!(
            DisconnectReason::from_hci(Some(0xff)).to_string(),
            "other-0xff"
        );
    }

    #[test]
    fn no_event_is_unknown() {
        assert_eq!(DisconnectReason::from_hci(None).to_string(), "unknown");
    }

    #[test]
    fn peripheral_line_with_identity() {
        let mut id = [0u8; 16];
        id[..4].copy_from_slice(&[0xc4, 0xa8, 0x08, 0x3f]);
        let line = LinkDownLine {
            role: LinkRole::Peripheral,
            conn: 3,
            peer: Some(id),
            reason: DisconnectReason::from_hci(Some(0x13)),
        };
        assert_eq!(
            line.to_string(),
            "BLE_LINK_DOWN role=peripheral conn=3 peer=c4a8083f reason=remote-user"
        );
    }

    #[test]
    fn central_line_before_the_identity_read() {
        let line = LinkDownLine {
            role: LinkRole::Central,
            conn: 0,
            peer: None,
            reason: DisconnectReason::from_hci(Some(0x3E)),
        };
        assert_eq!(
            line.to_string(),
            "BLE_LINK_DOWN role=central conn=0 peer=none reason=connection-failed-to-establish"
        );
    }

    /// A phone's 5 s timeout waits 6 s at most; the protocol maximum,
    /// 32 s, waits 33 s.
    #[test]
    fn settle_bound_is_one_supervision_timeout_plus_margin() {
        let phone = ConnParams {
            interval_units: 24,
            latency: 0,
            timeout_units: 500,
        };
        assert_eq!(disconnect_settle_ms(phone), 6_000);
        let max = ConnParams {
            interval_units: 3200,
            latency: 0,
            timeout_units: 3200,
        };
        assert_eq!(disconnect_settle_ms(max), 33_000);
    }

    #[test]
    fn forgotten_line() {
        let line = LinkForgottenLine {
            role: LinkRole::Central,
            conn: 1,
            waited_ms: 33_010,
        };
        assert_eq!(
            line.to_string(),
            "BLE_LINK_FORGOTTEN role=central conn=1 waited_ms=33010"
        );
    }

    /// The handle outlives the `BLE_LINK_DOWN` line on every link: the
    /// two fixtures above, every legal timeout, and the largest value
    /// the field can hold.
    #[test]
    fn forget_deadline_is_never_before_the_settle_bound() {
        let fixtures = [
            ConnParams {
                interval_units: 24,
                latency: 0,
                timeout_units: 500,
            },
            ConnParams {
                interval_units: 3200,
                latency: 0,
                timeout_units: 3200,
            },
        ];
        let sweep = (10..=3200u16)
            .chain([u16::MAX])
            .map(|timeout_units| ConnParams {
                interval_units: 6,
                latency: 0,
                timeout_units,
            });
        for params in fixtures.into_iter().chain(sweep) {
            assert!(
                forget_deadline_ms(params) >= disconnect_settle_ms(params),
                "{params:?}: forget {} < settle {}",
                forget_deadline_ms(params),
                disconnect_settle_ms(params)
            );
        }
    }

    /// One maximum supervision timeout plus the margin, whatever the
    /// link negotiated: 33 s for the phone's 5 s link and for the 32 s
    /// one.
    #[test]
    fn forget_deadline_is_the_longest_timeout_plus_margin() {
        for timeout_units in [10, 500, 3200] {
            let params = ConnParams {
                interval_units: 24,
                latency: 0,
                timeout_units,
            };
            assert_eq!(forget_deadline_ms(params), 33_000, "{timeout_units}");
        }
    }
}
