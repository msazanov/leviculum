//! Per-board transceiver facts; see the README's "Board description".

/// `SetDioAsRfSwitch` bits (`src/lr11xx_system_types.h:244-246`): RFSW0..2 on DIO5..7.
pub const RFSW0: u8 = 1 << 0;
pub const RFSW1: u8 = 1 << 1;
pub const RFSW2: u8 = 1 << 2;

/// `GetVersion` type (`src/lr11xx_system_types.h:314-316`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    Lr1110 = 0x01,
    Lr1120 = 0x02,
    Lr1121 = 0x03,
}

/// `SetRegMode` (`src/lr11xx_system_types.h:226-227`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegMode {
    Ldo = 0x00,
    DcDc = 0x01,
}

/// `CfgLfClk` source (`src/lr11xx_system_types.h:216-218`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfClk {
    Rc = 0x00,
    Xtal = 0x01,
    Ext = 0x02,
}

/// `SetTcxoMode` supply (`src/lr11xx_system_types.h:280-287`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcxoVoltage {
    V1_6 = 0x00,
    V1_7 = 0x01,
    V1_8 = 0x02,
    V2_2 = 0x03,
    V2_4 = 0x04,
    V2_7 = 0x05,
    V3_0 = 0x06,
    V3_3 = 0x07,
}

/// The `SetDioAsRfSwitch` pattern set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RfSwitch {
    pub enable: u8,
    pub standby: u8,
    pub rx: u8,
    pub tx: u8,
    pub tx_hp: u8,
    pub tx_hf: u8,
    pub gnss: u8,
    pub wifi: u8,
}

/// A TCXO powered from the part's VTCXO pin; start-up in 32.768 kHz RTC steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tcxo {
    pub voltage: TcxoVoltage,
    pub startup_steps: u32,
}

/// One `SetRssiCalibration` table: gains g4..g13, g13hp1..g13hp7, then the offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RssiCalibration {
    pub gain_tune: [u8; 17],
    pub gain_offset: i16,
}

/// RSSI calibration per frequency range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RssiCalibrationSet {
    pub below_600mhz: RssiCalibration,
    pub from_600mhz_to_2ghz: RssiCalibration,
    pub above_2ghz: RssiCalibration,
}

impl RssiCalibrationSet {
    /// SWSD003 v2.4.0 `smtc_shield_lr11xx_common.c:173-188`.
    pub const fn for_freq(&self, freq_hz: u32) -> &RssiCalibration {
        if freq_hz < 600_000_000 {
            &self.below_600mhz
        } else if freq_hz <= 2_000_000_000 {
            &self.from_600mhz_to_2ghz
        } else {
            &self.above_2ghz
        }
    }
}

/// SWSD003 v2.4.0 `smtc_shield_lr11xx_common.c:57-119`.
pub const SEMTECH_RSSI_CALIBRATION: RssiCalibrationSet = RssiCalibrationSet {
    below_600mhz: RssiCalibration {
        gain_tune: [12, 12, 14, 0, 1, 3, 4, 4, 3, 6, 6, 6, 6, 6, 6, 6, 6],
        gain_offset: 0,
    },
    from_600mhz_to_2ghz: RssiCalibration {
        gain_tune: [2, 2, 2, 3, 3, 4, 5, 4, 4, 6, 5, 5, 6, 6, 6, 7, 6],
        gain_offset: 0,
    },
    above_2ghz: RssiCalibration {
        gain_tune: [6, 7, 6, 4, 3, 4, 14, 12, 14, 12, 12, 12, 12, 8, 8, 9, 9],
        gain_offset: 2030,
    },
};

/// Which PA a row selects (`src/lr11xx_radio_types.h:83-85`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaSel {
    Lp = 0x00,
    Hp = 0x01,
    /// LR1120/LR1121 2.4 GHz PA; this driver's LoRa path is sub-GHz only.
    Hf = 0x02,
}

/// What supplies the PA (`src/lr11xx_radio_types.h:319-320`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaSupply {
    Vreg = 0x00,
    Vbat = 0x01,
}

/// One PA table row: the `SetTxParams` power and the four `SetPaCfg` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// `SetTxParams` power, not the expected output.
    pub power: i8,
    pub pa_sel: PaSel,
    pub supply: PaSupply,
    pub duty_cycle: u8,
    pub hp_sel: u8,
}

impl Row {
    /// The `SetPaCfg` argument block.
    pub const fn pa_cfg(&self) -> [u8; 4] {
        [
            self.pa_sel as u8,
            self.supply as u8,
            self.duty_cycle,
            self.hp_sel,
        ]
    }
}

/// One row per dBm from `min_dbm` upward; validated at construction.
#[derive(Debug, Clone, Copy)]
pub struct PaTable {
    rows: &'static [Row],
    min_dbm: i8,
}

impl PaTable {
    /// Panics (at compile time in a `const`) on an empty table or one past +127 dBm.
    pub const fn new(rows: &'static [Row], min_dbm: i8) -> Self {
        assert!(!rows.is_empty(), "empty PA table");
        assert!(
            min_dbm as i32 + rows.len() as i32 - 1 <= i8::MAX as i32,
            "PA table past +127 dBm"
        );
        Self { rows, min_dbm }
    }

    pub const fn min_dbm(&self) -> i8 {
        self.min_dbm
    }

    pub const fn max_dbm(&self) -> i8 {
        (self.min_dbm as i32 + self.rows.len() as i32 - 1) as i8
    }

    pub const fn rows(&self) -> &'static [Row] {
        self.rows
    }
}

/// Everything the driver takes from the board.
#[derive(Debug)]
pub struct Board {
    pub name: &'static str,
    pub chip: Chip,
    pub tcxo: Option<Tcxo>,
    pub rf_switch: RfSwitch,
    pub reg_mode: RegMode,
    pub lf_clk: LfClk,
    pub wait_32k_ready: bool,
    pub rx_boosted: bool,
    pub rssi_calibration: Option<RssiCalibrationSet>,
    pub pa: PaTable,
}

/// What [`Board::pa_for_dbm`] settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaChoice {
    pub row: Row,
    /// The output the row is for, after the clamp.
    pub expected_dbm: i8,
    pub clamped: bool,
}

impl Board {
    /// The row for a requested output, clamped into the table's range.
    pub fn pa_for_dbm(&self, requested_dbm: i8) -> PaChoice {
        let expected = requested_dbm.clamp(self.pa.min_dbm(), self.pa.max_dbm());
        let idx = (expected as i16 - self.pa.min_dbm() as i16) as usize;
        PaChoice {
            row: self.pa.rows()[idx],
            expected_dbm: expected,
            clamped: expected != requested_dbm,
        }
    }
}

/// Semtech LR1110MB1DIS/LR1110MB1DJS shield (SWSD003 v2.4.0; evidence in the README).
pub const LR1110MB1DXS: Board = Board {
    name: "lr1110mb1dxs",
    chip: Chip::Lr1110,
    tcxo: Some(Tcxo {
        voltage: TcxoVoltage::V3_0,
        startup_steps: 300,
    }),
    rf_switch: RfSwitch {
        enable: RFSW0 | RFSW1 | RFSW2,
        standby: 0,
        rx: RFSW0,
        tx: RFSW0 | RFSW1,
        tx_hp: RFSW1,
        tx_hf: 0,
        gnss: RFSW2,
        wifi: 0,
    },
    reg_mode: RegMode::DcDc,
    lf_clk: LfClk::Xtal,
    wait_32k_ready: true,
    rx_boosted: false,
    rssi_calibration: Some(SEMTECH_RSSI_CALIBRATION),
    pa: PaTable::new(&LR1110MB1DXS_PA_TABLE, -17),
};

/// SWSD003 v2.4.0 `smtc_shield_lr1110mb1dxs_common.c:55-416`, -17 to +22 dBm.
pub const LR1110MB1DXS_PA_TABLE: [Row; 40] = [
    // -17 dBm
    Row {
        power: -15,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -16 dBm
    Row {
        power: -14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -15 dBm
    Row {
        power: -13,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -14 dBm
    Row {
        power: -12,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -13 dBm
    Row {
        power: -11,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -12 dBm
    Row {
        power: -9,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -11 dBm
    Row {
        power: -8,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -10 dBm
    Row {
        power: -7,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -9 dBm
    Row {
        power: -6,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -8 dBm
    Row {
        power: -5,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -7 dBm
    Row {
        power: -4,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -6 dBm
    Row {
        power: -3,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -5 dBm
    Row {
        power: -2,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -4 dBm
    Row {
        power: -1,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -3 dBm
    Row {
        power: 0,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -2 dBm
    Row {
        power: 1,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // -1 dBm
    Row {
        power: 2,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +0 dBm
    Row {
        power: 3,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +1 dBm
    Row {
        power: 3,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x01,
        hp_sel: 0x00,
    },
    // +2 dBm
    Row {
        power: 4,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x01,
        hp_sel: 0x00,
    },
    // +3 dBm
    Row {
        power: 7,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +4 dBm
    Row {
        power: 8,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +5 dBm
    Row {
        power: 9,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +6 dBm
    Row {
        power: 10,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +7 dBm
    Row {
        power: 12,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +8 dBm
    Row {
        power: 13,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +9 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x00,
        hp_sel: 0x00,
    },
    // +10 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x01,
        hp_sel: 0x00,
    },
    // +11 dBm
    Row {
        power: 13,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x02,
        hp_sel: 0x00,
    },
    // +12 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x02,
        hp_sel: 0x00,
    },
    // +13 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x04,
        hp_sel: 0x00,
    },
    // +14 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x05,
        hp_sel: 0x00,
    },
    // +15 dBm
    Row {
        power: 14,
        pa_sel: PaSel::Lp,
        supply: PaSupply::Vreg,
        duty_cycle: 0x07,
        hp_sel: 0x00,
    },
    // +16 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x03,
        hp_sel: 0x03,
    },
    // +17 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x04,
        hp_sel: 0x03,
    },
    // +18 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x02,
        hp_sel: 0x05,
    },
    // +19 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x05,
        hp_sel: 0x04,
    },
    // +20 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x03,
        hp_sel: 0x07,
    },
    // +21 dBm
    Row {
        power: 21,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x04,
        hp_sel: 0x07,
    },
    // +22 dBm
    Row {
        power: 22,
        pa_sel: PaSel::Hp,
        supply: PaSupply::Vbat,
        duty_cycle: 0x04,
        hp_sel: 0x07,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shield_table_has_one_row_per_dbm() {
        assert_eq!(LR1110MB1DXS.pa.min_dbm(), -17);
        assert_eq!(LR1110MB1DXS.pa.max_dbm(), 22);
        assert_eq!(
            LR1110MB1DXS_PA_TABLE.len(),
            (LR1110MB1DXS.pa.max_dbm() - LR1110MB1DXS.pa.min_dbm() + 1) as usize
        );
    }

    #[test]
    #[should_panic(expected = "empty PA table")]
    fn an_empty_pa_table_is_refused() {
        let _ = PaTable::new(&[], 0);
    }

    #[test]
    fn rssi_calibration_ranges_are_semtechs() {
        let set = &SEMTECH_RSSI_CALIBRATION;
        assert_eq!(set.for_freq(599_999_999), &set.below_600mhz);
        assert_eq!(set.for_freq(600_000_000), &set.from_600mhz_to_2ghz);
        assert_eq!(set.for_freq(2_000_000_000), &set.from_600mhz_to_2ghz);
        assert_eq!(set.for_freq(2_000_000_001), &set.above_2ghz);
    }

    #[test]
    fn the_14_and_22_dbm_rows_are_semtechs() {
        // `smtc_shield_lr1110mb1dxs_common.c`, the 14 dBm row: LP, VREG,
        // duty 0x05, hp 0x00, power 14.
        let r = LR1110MB1DXS.pa_for_dbm(14);
        assert!(!r.clamped);
        assert_eq!(r.row.power, 14);
        assert_eq!(r.row.pa_cfg(), [0x00, 0x00, 0x05, 0x00]);
        // The 22 dBm row: HP, VBAT, duty 0x04, hp 0x07, power 22.
        let r = LR1110MB1DXS.pa_for_dbm(22);
        assert!(!r.clamped);
        assert_eq!(r.row.power, 22);
        assert_eq!(r.row.pa_cfg(), [0x01, 0x01, 0x04, 0x07]);
    }

    #[test]
    fn requests_outside_the_table_clamp_and_say_so() {
        let r = LR1110MB1DXS.pa_for_dbm(30);
        assert!(r.clamped);
        assert_eq!(r.expected_dbm, 22);
        let r = LR1110MB1DXS.pa_for_dbm(-40);
        assert!(r.clamped);
        assert_eq!(r.expected_dbm, -17);
        assert_eq!(r.row.power, -15);
        // Negative control: an in-range request is not reported clamped.
        assert!(!LR1110MB1DXS.pa_for_dbm(2).clamped);
    }

    #[test]
    fn the_high_power_rows_run_the_hp_pa_from_vbat() {
        // Every row from 16 dBm up uses the HP PA on VBAT, every row below
        // the LP PA on VREG (the LP PA reaches 15 dBm at its top duty
        // cycle): the two halves of the part's PA architecture (datasheet,
        // "two PAs: +14 dBm at 28 mA, +22 dBm at 120 mA"), and the current
        // limit a driver must respect.
        for dbm in -17..=15 {
            let r = LR1110MB1DXS.pa_for_dbm(dbm).row;
            assert_eq!(
                (r.pa_sel, r.supply),
                (PaSel::Lp, PaSupply::Vreg),
                "{dbm} dBm"
            );
        }
        for dbm in 16..=22 {
            let r = LR1110MB1DXS.pa_for_dbm(dbm).row;
            assert_eq!(
                (r.pa_sel, r.supply),
                (PaSel::Hp, PaSupply::Vbat),
                "{dbm} dBm"
            );
        }
    }
}
