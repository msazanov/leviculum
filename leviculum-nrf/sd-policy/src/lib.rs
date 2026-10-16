//! Pure policy decisions for peripherals the S140 SoftDevice owns.
//!
//! Post-`Softdevice::enable`, RNG/POWER/RADIO/… are PREGION-protected:
//! any direct register access traps as `NRF_FAULT_ID_APP_MEMACC` and
//! panics the whole node (Codeberg #249/#250). The firmware therefore
//! has to decide, per failed syscall, whether the legacy direct-register
//! path is legal or whether it must wait for the SoftDevice. That
//! decision is pure (error code + SD-enabled flag → action), so it lives
//! here where the host can unit-test the full table, next to the
//! `leviculum-screen` painter that follows the same pattern.

#![cfg_attr(not(test), no_std)]

/// `NRF_ERROR_INVALID_STATE`: the SoftDevice is not enabled, the syscall
/// cannot serve the request at all.
pub const NRF_ERROR_INVALID_STATE: u32 = 8;

/// `NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES`: the SoftDevice is running but
/// its CSPRNG pool currently holds fewer bytes than requested. The pool
/// refills at roughly 120 µs/byte; this is a transient condition, not a
/// failure.
pub const NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES: u32 = 8199;

/// What the caller must do after `sd_rand_application_vector_get`
/// returned a non-zero error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RandAction {
    /// Direct RNG register access. Legal ONLY while the SoftDevice is
    /// disabled (no PREGION protection yet).
    Direct,
    /// Poll `sd_rand_application_bytes_available_get` and consume bytes
    /// as the pool refills. Never touch registers.
    Wait,
    /// Same as [`RandAction::Wait`], but the error code was not one the
    /// table anticipated — emit a debug log so the stall is
    /// diagnosable. Still never touch registers.
    WaitAndLog,
}

/// Decide how to make progress after an RNG syscall error.
///
/// The invariant this function exists to enforce: **while the SoftDevice
/// is enabled, no input maps to [`RandAction::Direct`]**. A MEMACC fault
/// reboots the node and (via GPREGRET-adjacent postmortem machinery)
/// records a bogus panic; waiting merely stalls one task.
pub fn rand_error_action(error_code: u32, sd_enabled: bool) -> RandAction {
    if !sd_enabled {
        // No PREGION protection: the direct register path is legal and
        // is the only one that can make progress (the syscall keeps
        // failing while the SoftDevice is off).
        return RandAction::Direct;
    }
    match error_code {
        NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES => RandAction::Wait,
        _ => RandAction::WaitAndLog,
    }
}

/// Polls of an empty pool after which [`wait_for_pool`] gives up.
///
/// About 10^6 iterations, each at least one syscall round trip: a
/// deliberately generous multiple of the ~120 µs/byte refill interval, so
/// running out can only mean "the pool is not refilling at all", never
/// "the pool is merely slow".
pub const RAND_STALL_SPINS: u32 = 1_000_000;

/// The pool stayed dry for a whole spin budget: the SoftDevice's CSPRNG
/// is not refilling. Its `Display` is the panic message the firmware
/// writes into the post-mortem record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStalled {
    /// Polls that returned no byte before giving up.
    pub spins: u32,
}

impl core::fmt::Display for PoolStalled {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "rng pool stalled: SoftDevice gave no bytes for {} spins",
            self.spins
        )
    }
}

/// The [`RandAction::Wait`] loop: call `poll` until it yields at least
/// one byte, and return that count, or give up after `budget` polls that
/// yielded none.
///
/// `poll` reads the available-byte count and consumes what is there; it
/// returns `None` (or `Some(0)`) when the pool was empty or a race for the
/// counted bytes was lost. Both count against the budget: a pool that
/// never lets a byte through is stalled whatever the reason.
///
/// The loop is bounded because the caller cannot yield: it is reached
/// through the sync `RngCore` trait on the single thread-mode executor,
/// so an unbounded spin parks every task, including the one that drains
/// the log to the host (review finding L-0030).
pub fn wait_for_pool(
    budget: u32,
    mut poll: impl FnMut() -> Option<usize>,
) -> Result<usize, PoolStalled> {
    let mut spins: u32 = 0;
    loop {
        if let Some(taken) = poll().filter(|&n| n > 0) {
            return Ok(taken);
        }
        spins = spins.saturating_add(1);
        if spins >= budget {
            return Err(PoolStalled { spins });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four-row table from Codeberg #250.
    #[test]
    fn invalid_state_sd_off_goes_direct() {
        assert_eq!(
            rand_error_action(NRF_ERROR_INVALID_STATE, false),
            RandAction::Direct
        );
    }

    #[test]
    fn not_enough_values_sd_on_waits() {
        assert_eq!(
            rand_error_action(NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES, true),
            RandAction::Wait
        );
    }

    #[test]
    fn not_enough_values_sd_off_goes_direct() {
        assert_eq!(
            rand_error_action(NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES, false),
            RandAction::Direct
        );
    }

    #[test]
    fn unknown_error_sd_on_waits_and_logs() {
        assert_eq!(rand_error_action(0xDEAD, true), RandAction::WaitAndLog);
    }

    /// Beyond the table: any error with the SoftDevice off resolves to
    /// the direct path (it is legal there, and the syscall cannot make
    /// progress), including codes the table never anticipated.
    #[test]
    fn unknown_error_sd_off_goes_direct() {
        assert_eq!(rand_error_action(0xDEAD, false), RandAction::Direct);
    }

    /// Contradictory input (INVALID_STATE while the SD claims to be
    /// enabled) must still never touch registers.
    #[test]
    fn invalid_state_sd_on_never_direct() {
        assert_eq!(
            rand_error_action(NRF_ERROR_INVALID_STATE, true),
            RandAction::WaitAndLog
        );
    }

    /// The enforced invariant, exhaustively: sd_enabled == true never
    /// yields Direct, for any error code shape.
    #[test]
    fn sd_on_never_direct_for_any_code() {
        for code in [
            0u32,
            1,
            NRF_ERROR_INVALID_STATE,
            NRF_ERROR_SOC_RAND_NOT_ENOUGH_VALUES,
            8192,
            u32::MAX,
        ] {
            assert_ne!(rand_error_action(code, true), RandAction::Direct);
        }
    }
    /// L-0030: a pool that never refills ends the wait, after exactly the
    /// budget, instead of spinning forever.
    #[test]
    fn a_pool_that_never_refills_stalls_after_the_budget() {
        let mut polls = 0u32;
        let got = wait_for_pool(1000, || {
            polls += 1;
            None
        });
        assert_eq!(got, Err(PoolStalled { spins: 1000 }));
        assert_eq!(polls, 1000);
    }

    #[test]
    fn a_pool_that_refills_late_returns_what_it_gave() {
        let mut polls = 0u32;
        let got = wait_for_pool(1000, || {
            polls += 1;
            (polls == 999).then_some(7)
        });
        assert_eq!(got, Ok(7));
    }

    /// A lost race (`Some(0)`) is a poll without a byte and counts.
    #[test]
    fn lost_races_count_against_the_budget() {
        assert_eq!(wait_for_pool(3, || Some(0)), Err(PoolStalled { spins: 3 }));
    }

    /// The panic message the next boot replays as `[PANIC_PMRT]`.
    #[test]
    fn the_stall_names_itself_in_the_panic_message() {
        let msg = format!(
            "{}",
            PoolStalled {
                spins: RAND_STALL_SPINS
            }
        );
        assert_eq!(
            msg,
            "rng pool stalled: SoftDevice gave no bytes for 1000000 spins"
        );
    }
}
