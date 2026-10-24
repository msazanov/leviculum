//! The names the board's interfaces report (Codeberg #235, #346, #469).
//!
//! The counters behind them are `leviculum_firmware::iface_bytes`; the
//! table stays here because its LoRa entry names the radio chip, which is
//! the bind crate's to say (`just firmware-no-chip-cfg`).

use leviculum_firmware::iface_bytes::IFACE_COUNT;

/// The name each interface reports, indexed by interface id.
///
/// The one table: the `Interface::name()` impls read it, the `[TRANSPORT]`
/// lines print it, and the `/status` bundle answers with it. A remote
/// `rnstatus -R` and a debug capture therefore name the same carrier the same
/// way, which is the whole point of feeding both from one set of counters.
pub const NAMES: [&str; IFACE_COUNT] = ["serial_usb", "lora_sx1262", "ble"];
