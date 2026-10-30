//! The media profile's log surface the interfaces share (999, step 4 of
//! `docs/src/concepts/firmware-layers.md`).
//!
//! The `[MEDIA]` lines both carrier interfaces write — the drop-run pair
//! a down carrier produces and the held-down line of a profile that kept
//! a carrier off at boot. They moved up from the bind crate's `media`
//! module with the BLE `Interface` implementation: the LoRa and BLE
//! interfaces log the SAME run shape (`leviculum_media_state::DropRun`),
//! so the lines live once, in the stratum both interfaces can name. The
//! media profile itself — what is configured, what this boot runs — is
//! still the bind crate's `media` module; when it moves up it joins this
//! module.

/// Report a run of packets a down carrier threw away.
///
/// Called by the interfaces on the first drop of a run and at every
/// decade after it, never per packet: the reasoning, and the host tests,
/// are in [`leviculum_media_state::DropRun`]. `packets=` is the run's
/// count at the moment of the line, so the last such line is a lower
/// bound on the run within a factor of ten, and
/// [`log_tx_resumed`] states the exact total when the carrier comes back.
///
/// ```text
/// [MEDIA] MEDIA_TX_DROP iface=ble packets=10 bytes=450 reason=carrier-off
/// ```
pub fn log_tx_drop(iface: &str, run: leviculum_media_state::Swallowed) {
    crate::log::log_fmt(
        "[MEDIA] ",
        format_args!(
            "MEDIA_TX_DROP iface={} packets={} bytes={} reason=carrier-off",
            iface, run.packets, run.bytes
        ),
    );
}

/// Close a drop run: the carrier took a packet again, and this is the
/// only line carrying the run's untruncated totals.
///
/// ```text
/// [MEDIA] MEDIA_TX_RESUMED iface=ble packets=37 bytes=1665
/// ```
pub fn log_tx_resumed(iface: &str, run: leviculum_media_state::Swallowed) {
    crate::log::log_fmt(
        "[MEDIA] ",
        format_args!(
            "MEDIA_TX_RESUMED iface={} packets={} bytes={}",
            iface, run.packets, run.bytes
        ),
    );
}

/// Say that a carrier was held down at boot because the profile said so.
///
/// Emitted next to the carrier's own init log so a reader who greps for
/// `[LORA]` or `[BLE ]` and finds nothing has the reason on the line
/// where the bring-up would have been, not only in the `[MEDIA]` banner.
pub fn log_carrier_held_down(carrier: &str) {
    crate::log::log_fmt_critical(
        "[MEDIA] ",
        format_args!("carrier={} state=down reason=profile", carrier),
    );
}
