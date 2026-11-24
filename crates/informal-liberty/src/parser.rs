//! Streaming recursive-descent Liberty (`.lib`) parser.
//!
//! Parses `library`, `voltage_map`, `type`, `cell`, `pg_pin`, `pin`, `bus`,
//! `bundle`, `ff`, and `latch` groups while skipping large timing/power LUTs
//! (`timing`, `internal_power`, `leakage_power`, `lu_table_template`, etc.)
//! directly in the lexer without allocating intermediate AST nodes.

use std::collections::HashMap;
use std::path::Path;

use crate::cell::{
    BusType, LevelShifterType, LibertyCell, LibertyDb, PgPinDef, PgType, PinDir, RetentionPinKind,
    SignalPinDef,
};
use crate::lexer::{tok_desc, Lexer, LibertyError, TokKind};

/// Parses a `.lib` file on disk and merges its contents into `db`.
pub fn parse_file_into(path: &Path, db: &mut LibertyDb) -> Result<(), LibertyError> {
    let file_str = path.display().to_string();
    let src = std::fs::read_to_string(path).map_err(|e| LibertyError::Io {
        file: file_str.clone(),
        message: e.to_string(),
    })?;
    parse_str_into(&src, &file_str, db)
}

/// Parses Liberty `.lib` text and merges its libraries/cells into `db`.
pub fn parse_str_into(src: &str, file: &str, db: &mut LibertyDb) -> Result<(), LibertyError> {
    let mut lex = Lexer::new(src, file);
    let mut parsed_any = false;
    loop {
        match &lex.peek()?.kind {
            TokKind::Eof => break,
            TokKind::Semi => {
                lex.next_tok()?;
            }
            TokKind::Atom("library") => {
                parsed_any = true;
                parse_library(&mut lex, db)?;
            }
            _ => {
                let tok = lex.next_tok()?;
                return Err(lex.err(
                    tok.line,
                    format!(
                        "expected top-level `library (...)`, found {}",
                        tok_desc(&tok.kind)
                    ),
                ));
            }
        }
    }
    if !parsed_any {
        return Err(lex.err(1, "empty Liberty file (no `library` group found)"));
    }
    Ok(())
}

fn parse_library(lex: &mut Lexer<'_>, db: &mut LibertyDb) -> Result<(), LibertyError> {
    let _kw = lex.next_tok()?; // `library`
    let args = parse_arg_list(lex)?;
    let lib_name = args.first().cloned().unwrap_or_else(|| "unnamed".to_string());
    let open_line = lex.expect_lbrace()?;
    if !db.libraries.contains(&lib_name) {
        db.libraries.push(lib_name.clone());
    }

    let mut local_voltages: HashMap<String, f64> = HashMap::new();
    let mut local_bus_types: HashMap<String, BusType> = HashMap::new();

    loop {
        match &lex.peek()?.kind {
            TokKind::RBrace => {
                lex.next_tok()?;
                break;
            }
            TokKind::Eof => {
                return Err(lex.err(open_line, "unclosed `library` block before end of file"));
            }
            TokKind::Semi => {
                lex.next_tok()?;
            }
            _ => {
                let name_tok = lex.next_tok()?;
                let name = tok_as_word(lex, name_tok)?;
                match &lex.peek()?.kind {
                    TokKind::Colon => {
                        let _val = parse_simple_attr(lex)?;
                    }
                    TokKind::LParen => {
                        let args = parse_arg_list(lex)?;
                        match &lex.peek()?.kind {
                            TokKind::LBrace => {
                                let grp_line = lex.expect_lbrace()?;
                                match name.as_str() {
                                    "type" => {
                                        let bt = parse_bus_type(
                                            lex,
                                            args.first().cloned().unwrap_or_default(),
                                            grp_line,
                                        )?;
                                        local_bus_types.insert(bt.name.clone(), bt.clone());
                                        db.bus_types.insert(bt.name.clone(), bt);
                                    }
                                    "cell" => {
                                        let cell_name = args.first().cloned().ok_or_else(|| {
                                            lex.err(grp_line, "`cell` group missing cell name")
                                        })?;
                                        let cell = parse_cell(
                                            lex,
                                            cell_name,
                                            &lib_name,
                                            &local_voltages,
                                            &local_bus_types,
                                            db,
                                            grp_line,
                                        )?;
                                        db.cells.insert(cell.name.clone(), cell);
                                    }
                                    _ => {
                                        lex.skip_braced_body(grp_line)?;
                                    }
                                }
                            }
                            TokKind::Semi => {
                                lex.next_tok()?;
                                if name == "voltage_map" && args.len() >= 2 {
                                    if let Ok(v) = args[1].parse::<f64>() {
                                        local_voltages.insert(args[0].clone(), v);
                                        db.voltage_map.insert(args[0].clone(), v);
                                    }
                                }
                            }
                            _ => {
                                // Complex attribute without trailing semicolon (rare but legal in Liberty).
                                if name == "voltage_map" && args.len() >= 2 {
                                    if let Ok(v) = args[1].parse::<f64>() {
                                        local_voltages.insert(args[0].clone(), v);
                                        db.voltage_map.insert(args[0].clone(), v);
                                    }
                                }
                            }
                        }
                    }
                    other => {
                        let desc = tok_desc(other);
                        let line = lex.line();
                        return Err(lex.err(
                            line,
                            format!("expected `:` or `(` after `{name}`, found {desc}"),
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

fn parse_bus_type(
    lex: &mut Lexer<'_>,
    name: String,
    open_line: usize,
) -> Result<BusType, LibertyError> {
    let mut bt = BusType {
        name,
        bit_width: 1,
        bit_from: 0,
        bit_to: 0,
        downto: true,
    };
    loop {
        match &lex.peek()?.kind {
            TokKind::RBrace => {
                lex.next_tok()?;
                break;
            }
            TokKind::Eof => {
                return Err(lex.err(open_line, "unclosed `type` block before end of file"));
            }
            TokKind::Semi => {
                lex.next_tok()?;
            }
            _ => {
                let k_tok = lex.next_tok()?;
                let key = tok_as_word(lex, k_tok)?;
                match &lex.peek()?.kind {
                    TokKind::Colon => {
                        let val = parse_simple_attr(lex)?;
                        match key.as_str() {
                            "bit_width" => {
                                if let Ok(w) = val.parse::<u32>() {
                                    bt.bit_width = w.max(1);
                                }
                            }
                            "bit_from" => {
                                if let Ok(v) = val.parse::<i32>() {
                                    bt.bit_from = v;
                                }
                            }
                            "bit_to" => {
                                if let Ok(v) = val.parse::<i32>() {
                                    bt.bit_to = v;
                                }
                            }
                            "downto" => {
                                bt.downto = parse_bool(&val);
                            }
                            _ => {}
                        }
                    }
                    TokKind::LParen => {
                        skip_complex_or_group(lex)?;
                    }
                    other => {
                        let desc = tok_desc(other);
                        let line = lex.line();
                        return Err(lex.err(
                            line,
                            format!("unexpected token {desc} inside `type` group"),
                        ));
                    }
                }
            }
        }
    }
    Ok(bt)
}

fn parse_cell(
    lex: &mut Lexer<'_>,
    name: String,
    lib_name: &str,
    local_voltages: &HashMap<String, f64>,
    local_bus_types: &HashMap<String, BusType>,
    db: &LibertyDb,
    open_line: usize,
) -> Result<LibertyCell, LibertyError> {
    let mut cell = LibertyCell {
        name,
        library: lib_name.to_string(),
        is_isolation_cell: false,
        isolation_enable_condition: None,
        is_level_shifter: false,
        level_shifter_type: None,
        valid_location: None,
        input_voltage_range: None,
        output_voltage_range: None,
        is_always_on: false,
        retention_cell: None,
        switch_cell_type: None,
        is_macro_cell: false,
        has_ff: false,
        has_latch: false,
        pg_pins: Vec::new(),
        pins: Vec::new(),
    };

    loop {
        match &lex.peek()?.kind {
            TokKind::RBrace => {
                lex.next_tok()?;
                break;
            }
            TokKind::Eof => {
                return Err(lex.err(
                    open_line,
                    format!("unclosed `cell ({})` block before end of file", cell.name),
                ));
            }
            TokKind::Semi => {
                lex.next_tok()?;
            }
            _ => {
                let k_tok = lex.next_tok()?;
                let key = tok_as_word(lex, k_tok)?;
                match &lex.peek()?.kind {
                    TokKind::Colon => {
                        let val = parse_simple_attr(lex)?;
                        apply_cell_attr(&mut cell, &key, &val);
                    }
                    TokKind::LParen => {
                        let args = parse_arg_list(lex)?;
                        match &lex.peek()?.kind {
                            TokKind::LBrace => {
                                let grp_line = lex.expect_lbrace()?;
                                match key.as_str() {
                                    "pg_pin" => {
                                        let pg_name = args.first().cloned().unwrap_or_default();
                                        let pg = parse_pg_pin(
                                            lex,
                                            pg_name,
                                            local_voltages,
                                            db,
                                            grp_line,
                                        )?;
                                        cell.pg_pins.push(pg);
                                    }
                                    "pin" => {
                                        let p_name = args.first().cloned().unwrap_or_default();
                                        let p = parse_signal_pin(
                                            lex,
                                            p_name,
                                            false,
                                            local_bus_types,
                                            db,
                                            grp_line,
                                        )?;
                                        cell.pins.push(p);
                                    }
                                    "bus" | "bundle" => {
                                        let p_name = args.first().cloned().unwrap_or_default();
                                        let p = parse_signal_pin(
                                            lex,
                                            p_name,
                                            true,
                                            local_bus_types,
                                            db,
                                            grp_line,
                                        )?;
                                        cell.pins.push(p);
                                    }
                                    "ff" | "ff_bank" => {
                                        cell.has_ff = true;
                                        lex.skip_braced_body(grp_line)?;
                                    }
                                    "latch" | "latch_bank" => {
                                        cell.has_latch = true;
                                        lex.skip_braced_body(grp_line)?;
                                    }
                                    _ => {
                                        lex.skip_braced_body(grp_line)?;
                                    }
                                }
                            }
                            _ => {
                                if matches!(lex.peek()?.kind, TokKind::Semi) {
                                    lex.next_tok()?;
                                }
                                apply_cell_complex_attr(&mut cell, &key, &args);
                            }
                        }
                    }
                    other => {
                        let desc = tok_desc(other);
                        let line = lex.line();
                        return Err(lex.err(
                            line,
                            format!(
                                "expected `:` or `(` after `{key}` in cell `{}`, found {desc}",
                                cell.name,
                            ),
                        ));
                    }
                }
            }
        }
    }
    Ok(cell)
}

fn apply_cell_attr(cell: &mut LibertyCell, key: &str, val: &str) {
    match key {
        "is_isolation_cell" => cell.is_isolation_cell = parse_bool(val),
        "isolation_enable_condition" => {
            cell.isolation_enable_condition = Some(val.trim().to_string());
        }
        "is_level_shifter" => cell.is_level_shifter = parse_bool(val),
        "level_shifter_type" => cell.level_shifter_type = LevelShifterType::parse(val),
        "valid_location" => cell.valid_location = Some(val.trim().to_string()),
        "always_on" | "is_always_on_cell" => cell.is_always_on = parse_bool(val),
        "retention_cell" => {
            if !val.trim().eq_ignore_ascii_case("false") {
                cell.retention_cell = Some(val.trim().to_string());
            }
        }
        "is_retention_cell" => {
            if parse_bool(val) && cell.retention_cell.is_none() {
                cell.retention_cell = Some("RETAIN".to_string());
            }
        }
        "switch_cell_type" => cell.switch_cell_type = Some(val.trim().to_string()),
        "is_macro_cell" | "memory" if parse_bool(val) => {
            cell.is_macro_cell = true;
        }
        _ => {}
    }
}

fn apply_cell_complex_attr(cell: &mut LibertyCell, key: &str, args: &[String]) {
    match key {
        "input_voltage_range" if args.len() >= 2 => {
            if let (Ok(lo), Ok(hi)) = (args[0].parse::<f64>(), args[1].parse::<f64>()) {
                cell.input_voltage_range = Some((lo, hi));
            }
        }
        "output_voltage_range" if args.len() >= 2 => {
            if let (Ok(lo), Ok(hi)) = (args[0].parse::<f64>(), args[1].parse::<f64>()) {
                cell.output_voltage_range = Some((lo, hi));
            }
        }
        _ => {}
    }
}

fn parse_pg_pin(
    lex: &mut Lexer<'_>,
    name: String,
    local_voltages: &HashMap<String, f64>,
    db: &LibertyDb,
    open_line: usize,
) -> Result<PgPinDef, LibertyError> {
    let mut pg = PgPinDef {
        name,
        pg_type: PgType::PrimaryPower,
        voltage_name: None,
        nominal_voltage: None,
        switch_function: None,
        pg_function: None,
        std_cell_main_rail: false,
    };
    loop {
        match &lex.peek()?.kind {
            TokKind::RBrace => {
                lex.next_tok()?;
                break;
            }
            TokKind::Eof => {
                return Err(lex.err(
                    open_line,
                    format!("unclosed `pg_pin ({})` block before end of file", pg.name),
                ));
            }
            TokKind::Semi => {
                lex.next_tok()?;
            }
            _ => {
                let k_tok = lex.next_tok()?;
                let key = tok_as_word(lex, k_tok)?;
                match &lex.peek()?.kind {
                    TokKind::Colon => {
                        let val = parse_simple_attr(lex)?;
                        match key.as_str() {
                            "pg_type" => pg.pg_type = PgType::parse(&val),
                            "voltage_name" => {
                                let vn = val.trim().to_string();
                                pg.nominal_voltage = local_voltages
                                    .get(&vn)
                                    .or_else(|| db.voltage_map.get(&vn))
                                    .copied();
                                pg.voltage_name = Some(vn);
                            }
                            "switch_function" => pg.switch_function = Some(val.trim().to_string()),
                            "pg_function" => pg.pg_function = Some(val.trim().to_string()),
                            "std_cell_main_rail" => pg.std_cell_main_rail = parse_bool(&val),
                            _ => {}
                        }
                    }
                    TokKind::LParen => {
                        skip_complex_or_group(lex)?;
                    }
                    other => {
                        let desc = tok_desc(other);
                        let line = lex.line();
                        return Err(lex.err(
                            line,
                            format!("unexpected token {desc} inside `pg_pin ({})`", pg.name),
                        ));
                    }
                }
            }
        }
    }
    Ok(pg)
}

fn parse_signal_pin(
    lex: &mut Lexer<'_>,
    name: String,
    is_bus: bool,
    local_bus_types: &HashMap<String, BusType>,
    db: &LibertyDb,
    open_line: usize,
) -> Result<SignalPinDef, LibertyError> {
    let mut pin = SignalPinDef {
        name,
        is_bus,
        width: 1,
        ..SignalPinDef::default()
    };
    loop {
        match &lex.peek()?.kind {
            TokKind::RBrace => {
                lex.next_tok()?;
                break;
            }
            TokKind::Eof => {
                return Err(lex.err(
                    open_line,
                    format!("unclosed `pin ({})` block before end of file", pin.name),
                ));
            }
            TokKind::Semi => {
                lex.next_tok()?;
            }
            _ => {
                let k_tok = lex.next_tok()?;
                let key = tok_as_word(lex, k_tok)?;
                match &lex.peek()?.kind {
                    TokKind::Colon => {
                        let val = parse_simple_attr(lex)?;
                        match key.as_str() {
                            "direction" => pin.direction = PinDir::parse(&val),
                            "function" => pin.function = Some(val.trim().to_string()),
                            "bus_type" => {
                                if let Some(bt) = local_bus_types
                                    .get(val.trim())
                                    .or_else(|| db.bus_types.get(val.trim()))
                                {
                                    pin.width = bt.bit_width.max(1);
                                }
                            }
                            "related_power_pin" => {
                                pin.related_power_pin = Some(val.trim().to_string());
                            }
                            "related_ground_pin" => {
                                pin.related_ground_pin = Some(val.trim().to_string());
                            }
                            "related_bias_pin" => {
                                pin.related_bias_pin = Some(val.trim().to_string());
                            }
                            "power_down_function" => {
                                pin.power_down_function = Some(val.trim().to_string());
                            }
                            "clock" => pin.is_clock = parse_bool(&val),
                            "always_on" => pin.is_always_on = parse_bool(&val),
                            "isolation_cell_data_pin" => pin.is_iso_data_pin = parse_bool(&val),
                            "isolation_cell_enable_pin" | "isolation_enable_pin" => {
                                pin.is_iso_enable_pin = parse_bool(&val);
                            }
                            "level_shifter_data_pin" => pin.is_ls_data_pin = parse_bool(&val),
                            "level_shifter_enable_pin" => pin.is_ls_enable_pin = parse_bool(&val),
                            "switch_pin" => pin.is_switch_pin = parse_bool(&val),
                            _ => {}
                        }
                    }
                    TokKind::LParen => {
                        let args = parse_arg_list(lex)?;
                        if matches!(lex.peek()?.kind, TokKind::LBrace) {
                            let grp_line = lex.expect_lbrace()?;
                            // Fast-skip timing / internal_power / nested per-bit bus pins
                            lex.skip_braced_body(grp_line)?;
                        } else {
                            if matches!(lex.peek()?.kind, TokKind::Semi) {
                                lex.next_tok()?;
                            }
                            if key == "retention_pin" && !args.is_empty() {
                                let kind = RetentionPinKind::parse(&args[0]);
                                let sense = args.get(1).cloned();
                                pin.retention_pin = Some((kind, sense));
                            }
                        }
                    }
                    other => {
                        let desc = tok_desc(other);
                        let line = lex.line();
                        return Err(lex.err(
                            line,
                            format!("unexpected token {desc} inside `pin ({})`", pin.name),
                        ));
                    }
                }
            }
        }
    }
    Ok(pin)
}

fn parse_simple_attr(lex: &mut Lexer<'_>) -> Result<String, LibertyError> {
    let _colon = lex.next_tok()?; // `:`
    let mut parts: Vec<String> = Vec::new();
    loop {
        match &lex.peek()?.kind {
            TokKind::Semi => {
                lex.next_tok()?;
                break;
            }
            TokKind::RBrace | TokKind::Eof => break,
            TokKind::Quoted(_) => {
                if let TokKind::Quoted(s) = lex.next_tok()?.kind {
                    parts.push(s);
                }
            }
            TokKind::Atom(_) => {
                if let TokKind::Atom(s) = lex.next_tok()?.kind {
                    parts.push(s.to_string());
                }
            }
            TokKind::Comma => {
                lex.next_tok()?;
                parts.push(",".to_string());
            }
            other => {
                let desc = tok_desc(other);
                let line = lex.line();
                return Err(lex.err(
                    line,
                    format!("unexpected token {desc} in attribute value"),
                ));
            }
        }
    }
    Ok(parts.join(" "))
}

fn parse_arg_list(lex: &mut Lexer<'_>) -> Result<Vec<String>, LibertyError> {
    let open_line = lex.expect_lparen()?;
    let mut args = Vec::new();
    let mut cur = String::new();
    loop {
        match &lex.peek()?.kind {
            TokKind::RParen => {
                lex.next_tok()?;
                if !cur.is_empty() {
                    args.push(cur);
                }
                return Ok(args);
            }
            TokKind::Comma => {
                lex.next_tok()?;
                args.push(std::mem::take(&mut cur));
            }
            TokKind::Quoted(_) => {
                if let TokKind::Quoted(s) = lex.next_tok()?.kind {
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(&s);
                }
            }
            TokKind::Atom(_) => {
                if let TokKind::Atom(s) = lex.next_tok()?.kind {
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(s);
                }
            }
            TokKind::LBrace => {
                let line = lex.line();
                return Err(lex.err(
                    line,
                    "expected `)` to close group header before `{`",
                ));
            }
            TokKind::Eof => {
                return Err(lex.err(open_line, "unclosed `(` before end of file"));
            }
            other => {
                let desc = tok_desc(other);
                let line = lex.line();
                return Err(lex.err(line, format!("expected argument or `)`, found {desc}")));
            }
        }
    }
}

fn skip_complex_or_group(lex: &mut Lexer<'_>) -> Result<(), LibertyError> {
    let _args = parse_arg_list(lex)?;
    if matches!(lex.peek()?.kind, TokKind::LBrace) {
        let line = lex.expect_lbrace()?;
        lex.skip_braced_body(line)?;
    } else if matches!(lex.peek()?.kind, TokKind::Semi) {
        lex.next_tok()?;
    }
    Ok(())
}

fn tok_as_word(lex: &Lexer<'_>, tok: crate::lexer::Tok<'_>) -> Result<String, LibertyError> {
    match tok.kind {
        TokKind::Atom(s) => Ok(s.to_string()),
        TokKind::Quoted(s) => Ok(s),
        other => Err(lex.err(
            tok.line,
            format!("expected identifier, found {}", tok_desc(&other)),
        )),
    }
}

fn parse_bool(val: &str) -> bool {
    matches!(
        val.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    )
}
