//! Pure-Rust streaming Synopsys Liberty (`.lib`) reader for Low-Power
//! Verification (Stages 3–4 gate/PG netlist checking).

pub mod cell;
pub mod lexer;
pub mod parser;

use std::path::Path;

pub use cell::{
    BusType, IsoClampKind, LevelShifterType, LibertyCell, LibertyDb, PgPinDef, PgType, PinDir,
    RetentionPinKind, SignalPinDef,
};
pub use lexer::LibertyError;

impl LibertyDb {
    /// Parses a single `.lib` file into a new [`LibertyDb`].
    pub fn from_file(path: &Path) -> Result<Self, LibertyError> {
        let mut db = Self::default();
        parser::parse_file_into(path, &mut db)?;
        Ok(db)
    }

    /// Parses Liberty `.lib` text into a new [`LibertyDb`].
    pub fn from_str(src: &str, file: &str) -> Result<Self, LibertyError> {
        let mut db = Self::default();
        parser::parse_str_into(src, file, &mut db)?;
        Ok(db)
    }

    /// Parses an additional `.lib` file and merges its cells/voltage maps into `self`.
    pub fn merge_file(&mut self, path: &Path) -> Result<(), LibertyError> {
        parser::parse_file_into(path, self)
    }

    /// Parses additional Liberty `.lib` text and merges it into `self`.
    pub fn merge_str(&mut self, src: &str, file: &str) -> Result<(), LibertyError> {
        parser::parse_str_into(src, file, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_LIB: &str = r#"
library (sample_lp) {
    delay_model : table_lookup;
    voltage_map (VDD, 0.90);
    voltage_map (VDDH, 1.10);
    voltage_map (VDD_BK, 0.90);
    voltage_map (VSS, 0.00);

    type (bus4) {
        base_type : array;
        data_type : bit;
        bit_width : 4;
        bit_from : 3;
        bit_to : 0;
        downto : true;
    }

    cell (ISO_LO_X1) {
        is_isolation_cell : true;
        isolation_enable_condition : "ISO";
        pg_pin (VDD_BK) { pg_type : backup_power; voltage_name : VDD_BK; }
        pg_pin (VSS)    { pg_type : primary_ground; voltage_name : VSS; }
        pin (A)   { direction : input; isolation_cell_data_pin : true; related_power_pin : "VDD_BK"; }
        pin (ISO) { direction : input; isolation_cell_enable_pin : true; related_power_pin : "VDD_BK"; }
        pin (Y) {
            direction : output;
            function : "A & !ISO";
            related_power_pin : "VDD_BK";
            timing () {
                related_pin : "A";
                cell_rise (delay_7x7) {
                    values ( \
                        "0.01, 0.02", \
                        "0.03, 0.04"
                    );
                }
            }
        }
    }

    cell (ISO_HI_X1) {
        is_isolation_cell : true;
        isolation_enable_condition : "ISO";
        pin (A)   { direction : input; isolation_cell_data_pin : true; }
        pin (ISO) { direction : input; isolation_cell_enable_pin : true; }
        pin (Y)   { direction : output; function : "A | ISO"; }
    }

    cell (LS_LH_X1) {
        is_level_shifter : true;
        level_shifter_type : LH;
        input_voltage_range (0.65, 1.00);
        output_voltage_range (1.00, 1.25);
        pg_pin (VDDL) { pg_type : primary_power; voltage_name : VDD; }
        pg_pin (VDDH) { pg_type : backup_power; voltage_name : VDDH; }
        pin (A) { direction : input; level_shifter_data_pin : true; related_power_pin : "VDDL"; }
        pin (Y) { direction : output; function : "A"; related_power_pin : "VDDH"; }
    }

    cell (RET_DFF_X1) {
        retention_cell : "RETAIN";
        ff (IQ, IQN) { clocked_on : "CK"; next_state : "D"; }
        pin (SAVE)    { direction : input; retention_pin (save, "high"); }
        pin (RESTORE) { direction : input; retention_pin (restore, "high"); }
    }

    cell (SRAM_MACRO) {
        is_macro_cell : true;
        bus (DOUT) { bus_type : bus4; direction : output; related_power_pin : "VDD_BK"; }
    }
}
"#;

    #[test]
    fn parses_lp_cells_and_skips_nldm_tables() {
        let db = LibertyDb::from_str(SAMPLE_LIB, "sample.lib").expect("parse");
        assert_eq!(db.cell_count(), 5);
        assert_eq!(db.voltage_map.get("VDDH"), Some(&1.10));

        let iso_lo = db.cell("ISO_LO_X1").unwrap();
        assert!(iso_lo.is_isolation_cell);
        assert_eq!(iso_lo.iso_enable_active_high(), Some(true));
        assert_eq!(iso_lo.iso_clamp_kind(), Some(IsoClampKind::Zero));
        assert_eq!(iso_lo.pg_pin("VDD_BK").unwrap().nominal_voltage, Some(0.90));

        let iso_hi = db.cell("ISO_HI_X1").unwrap();
        assert_eq!(iso_hi.iso_clamp_kind(), Some(IsoClampKind::One));

        let ls = db.cell("LS_LH_X1").unwrap();
        assert!(ls.is_level_shifter);
        assert_eq!(ls.level_shifter_type, Some(LevelShifterType::Lh));
        assert_eq!(ls.input_voltage_range, Some((0.65, 1.00)));
        assert_eq!(ls.output_voltage_range, Some((1.00, 1.25)));

        let ret = db.cell("RET_DFF_X1").unwrap();
        assert!(ret.is_retention());
        assert!(ret.is_sequential());
        assert_eq!(
            ret.pin("SAVE").unwrap().retention_pin,
            Some((RetentionPinKind::Save, Some("high".to_string())))
        );

        let sram = db.cell("SRAM_MACRO").unwrap();
        assert!(sram.is_macro_cell);
        assert_eq!(sram.pin("DOUT").unwrap().width, 4);
    }

    #[test]
    fn malformed_syntax_reports_exact_line_14() {
        let bad = "library (malformed_lp_lib) {\n\
    delay_model : table_lookup;\n\
    nom_process : 1.0;\n\
    nom_temperature : 25.0;\n\
    nom_voltage : 0.90;\n\
    voltage_map (VDD, 0.90);\n\
    voltage_map (VSS, 0.00);\n\
    cell (BUF_OK) {\n\
        pg_pin (VDD) { pg_type : primary_power; voltage_name : VDD; }\n\
        pg_pin (VSS) { pg_type : primary_ground; voltage_name : VSS; }\n\
        pin (A) { direction : input; related_power_pin : \"VDD\"; }\n\
        pin (Y) { direction : output; function : \"A\"; related_power_pin : \"VDD\"; }\n\
    }\n\
    cell (BROKEN_X1 {\n\
        pg_pin (VDD) {\n\
            pg_type : primary_power;\n\
        }\n\
    }\n\
}\n";
        let err = LibertyDb::from_str(bad, "malformed_syntax.lib").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("malformed_syntax.lib:14"), "got: {msg}");
    }
}
