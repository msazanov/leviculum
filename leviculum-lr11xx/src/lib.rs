//! Async `no_std` driver for Semtech LR11xx (LR1110, LR1120, LR1121) LoRa
//! transceivers, generic over embedded-hal-async.
//!
//! * [`cmd`]: opcodes and pure argument encoders, pinned to SWDR001 v3.0.0.
//! * [`board`]: per-board facts (TCXO, RF switch, regulator, LF clock, PA
//!   table, RSSI calibration) as a [`Board`] value.
//! * [`driver`]: the async driver, [`Lr11xx`].
//!
//! Normative references: Semtech SWDR001 v3.0.0 (LR11xx driver), SWSD003
//! v2.4.0 (LR11xx SDK and shield files), the LR1110 datasheet
//! (DS.LR1110.W.APP) and user manual (UM.LR1110.W.APP). See the README for
//! the initialisation sequence, the board description and limitations.

#![no_std]

pub mod board;
pub mod cmd;
pub mod driver;

pub use board::{
    Board, Chip, LfClk, PaTable, RegMode, RssiCalibration, RssiCalibrationSet, TcxoVoltage,
};
pub use driver::{
    ChannelSample, ChipStatus, CrcErrFrame, Error, LogFn, Lr11xx, RxStatus, TxPowerProgram, Version,
};
