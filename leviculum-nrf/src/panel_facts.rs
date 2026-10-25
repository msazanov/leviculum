//! What the status page shows that only this crate's backends know (#469
//! 980): the bind crate's fill of `leviculum_firmware::display::StatusFacts`,
//! handed to the page composition by both panels' tasks (`display.rs`,
//! `st7789.rs`) and called by it once per lit frame tick.

use core::sync::atomic::Ordering;

use leviculum_firmware::display::StatusFacts;

/// The LoRa frame counters, the BLE links and the propagation store fill,
/// now.
pub fn status_facts() -> StatusFacts {
    StatusFacts {
        rx: crate::lora::LORA_RX_COUNT.load(Ordering::Relaxed),
        tx: crate::lora::LORA_TX_COUNT.load(Ordering::Relaxed),
        // Same source as the `links=` field of the `BLE_COUNTERS` log
        // line, so the screen and the capture can never disagree. Not
        // behind a cfg: `ble` is an ungated module and every BSP feature
        // pulls `softdevice`, so no configuration of this crate builds
        // without BLE; the painter's `None` arm exists for a board that
        // one day has none. `MAX_LINKS` is 4, so the saturation never
        // bites.
        ble_peers: Some(crate::ble::HVN_DRAIN.claimed().min(u8::MAX as usize) as u8),
        pn_fill: crate::pn::fill_indicator(),
    }
}
