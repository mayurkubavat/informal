//! Fast zero-copy structural Verilog / PG netlist parser for Liberty-backed
//! post-synthesis and physical PG netlists (100k+ cells in < 0.3s).

use std::path::Path;

use svlang::semantic::PortDir;

pub use super::scanner::parse_expr_str;
use super::scanner::{parse_lhs_ref, Scanner};
use crate::struct_graph::{normalize_sv_leaf, NetSegKind};

#[derive(Debug, Clone)]
pub struct ParsedPort {
    pub name: String,
    pub dir: Option<PortDir>,
    pub width: u32,
    pub packed_bounds: Option<(i128, i128)>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedNet {
    pub name: String,
    pub width: u32,
    pub packed_bounds: Option<(i128, i128)>,
    pub kind: NetSegKind,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum ParsedExpr {
    Empty,
    Const {
        is_zero: bool,
        is_all_ones: bool,
    },
    Ref {
        net: String,
        range: Option<(u32, u32)>,
        invert: bool,
    },
    Compound {
        refs: Vec<(String, Option<(u32, u32)>)>,
    },
}

#[derive(Debug, Clone)]
pub struct ParsedAssign {
    pub lhs_net: String,
    pub lhs_range: Option<(u32, u32)>,
    pub rhs: ParsedExpr,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedPortConn {
    /// `Some(port_name)` for `.PIN(expr)`, `None` for positional.
    pub pin_name: Option<String>,
    pub expr: ParsedExpr,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedInst {
    pub module_name: String,
    pub inst_name: String,
    pub raw_name: String,
    pub conns: Vec<ParsedPortConn>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedModule {
    pub name: String,
    pub file_idx: u32,
    pub line: u32,
    pub ports: Vec<ParsedPort>,
    pub nets: Vec<ParsedNet>,
    pub assigns: Vec<ParsedAssign>,
    pub insts: Vec<ParsedInst>,
    pub has_behavioral: bool,
}

/// Parses all structural Verilog modules from a source file on disk.
pub fn parse_verilog_file(path: &Path, file_idx: u32) -> Result<Vec<ParsedModule>, String> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| format!("gate_fast: failed to read `{}`: {e}", path.display()))?;
    parse_verilog_str(&src, file_idx, &path.display().to_string())
}

/// Parses all structural Verilog modules from `src`.
pub fn parse_verilog_str(
    src: &str,
    file_idx: u32,
    file_name: &str,
) -> Result<Vec<ParsedModule>, String> {
    let mut sc = Scanner::new(src, file_name);
    let mut modules = Vec::new();
    while !sc.eof() {
        sc.skip_ws_and_comments();
        if sc.eof() {
            break;
        }
        if sc.peek_byte() == Some(b'`') {
            sc.skip_directive_line();
            continue;
        }
        if sc.starts_with_bytes(b"(*") {
            sc.skip_attr();
            continue;
        }
        let line = sc.line;
        if let Some(word) = sc.read_ident() {
            if word == "module" || word == "macromodule" {
                modules.push(parse_module(&mut sc, file_idx, line)?);
            } else if word == "interface" || word == "package" {
                // Signal non-structural SV construct cleanly
                return Err(format!(
                    "gate_fast: non-structural `{word}` at {file_name}:{line}"
                ));
            } else {
                sc.skip_to_semi_or_eof();
            }
        } else {
            sc.advance_one();
        }
    }
    Ok(modules)
}

fn parse_module(
    sc: &mut Scanner<'_>,
    file_idx: u32,
    mod_line: u32,
) -> Result<ParsedModule, String> {
    sc.skip_ws_and_comments();
    let name_raw = sc
        .read_ident()
        .ok_or_else(|| sc.err("expected module name"))?;
    let name = normalize_sv_leaf(name_raw);

    let mut m = ParsedModule {
        name,
        file_idx,
        line: mod_line,
        ports: Vec::new(),
        nets: Vec::new(),
        assigns: Vec::new(),
        insts: Vec::new(),
        has_behavioral: false,
    };

    sc.skip_ws_and_comments();
    // Optional `#(...)` parameter port list
    if sc.peek_byte() == Some(b'#') {
        sc.advance_one();
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'(') {
            sc.skip_balanced(b'(', b')');
        }
    }

    // Port header `(...)`
    sc.skip_ws_and_comments();
    if sc.peek_byte() == Some(b'(') {
        parse_port_header(sc, &mut m)?;
    }
    sc.skip_ws_and_comments();
    if sc.peek_byte() == Some(b';') {
        sc.advance_one();
    }

    // Module body until `endmodule`
    while !sc.eof() {
        sc.skip_ws_and_comments();
        if sc.eof() {
            break;
        }
        if sc.peek_byte() == Some(b'`') {
            sc.skip_directive_line();
            continue;
        }
        if sc.starts_with_bytes(b"(*") {
            sc.skip_attr();
            continue;
        }
        if sc.peek_byte() == Some(b';') {
            sc.advance_one();
            continue;
        }
        let item_line = sc.line;
        let Some(kw) = sc.read_ident() else {
            sc.advance_one();
            continue;
        };
        match kw {
            "endmodule" => break,
            "input" | "output" | "inout" => {
                let dir = match kw {
                    "input" => PortDir::Input,
                    "output" => PortDir::Output,
                    _ => PortDir::Inout,
                };
                parse_decl_list(sc, Some(dir), NetSegKind::Port(dir), item_line, &mut m);
            }
            "wire" | "tri" | "tri0" | "tri1" | "wand" | "wor" | "logic" | "reg" | "supply0"
            | "supply1" => {
                let kind = match kw {
                    "reg" => NetSegKind::Reg,
                    "logic" => NetSegKind::Var,
                    "supply0" => NetSegKind::Supply0,
                    "supply1" => NetSegKind::Supply1,
                    _ => NetSegKind::Wire,
                };
                parse_decl_list(sc, None, kind, item_line, &mut m);
            }
            "assign" => {
                parse_assign(sc, item_line, &mut m);
            }
            "parameter" | "localparam" | "defparam" | "integer" | "genvar" | "real" | "time" => {
                sc.skip_to_semi_or_eof();
            }
            "specify" => {
                sc.skip_until_keyword("endspecify");
            }
            "initial" | "always" | "always_ff" | "always_comb" | "always_latch" | "function"
            | "task" | "generate" | "property" | "sequence" | "assert" | "assume" | "cover" => {
                m.has_behavioral = true;
                skip_behavioral_item(sc, kw);
            }
            _ => {
                // Module/cell instantiation: `CELL_TYPE [#(...)] inst_name (...), ...;`
                let cell_mod = normalize_sv_leaf(kw);
                parse_instantiation(sc, cell_mod, item_line, &mut m)?;
            }
        }
    }

    Ok(m)
}

fn parse_port_header(sc: &mut Scanner<'_>, m: &mut ParsedModule) -> Result<(), String> {
    sc.advance_one(); // `(`
    let mut cur_dir: Option<PortDir> = None;
    let mut cur_width: u32 = 1;
    let mut cur_bounds: Option<(i128, i128)> = None;

    while !sc.eof() {
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b')') {
            sc.advance_one();
            break;
        }
        if sc.peek_byte() == Some(b',') {
            sc.advance_one();
            continue;
        }
        let line = sc.line;
        let Some(tok) = sc.read_ident() else {
            sc.advance_one();
            continue;
        };
        match tok {
            "input" => {
                cur_dir = Some(PortDir::Input);
                cur_width = 1;
                cur_bounds = None;
            }
            "output" => {
                cur_dir = Some(PortDir::Output);
                cur_width = 1;
                cur_bounds = None;
            }
            "inout" => {
                cur_dir = Some(PortDir::Inout);
                cur_width = 1;
                cur_bounds = None;
            }
            "wire" | "reg" | "logic" | "signed" | "unsigned" | "tri" | "var" => {}
            _ => {
                let pname = normalize_sv_leaf(tok);
                if let Some(dir) = cur_dir {
                    m.ports.push(ParsedPort {
                        name: pname.clone(),
                        dir: Some(dir),
                        width: cur_width,
                        packed_bounds: cur_bounds,
                        line,
                    });
                    m.nets.push(ParsedNet {
                        name: pname,
                        width: cur_width,
                        packed_bounds: cur_bounds,
                        kind: NetSegKind::Port(dir),
                        line,
                    });
                } else {
                    // Non-ANSI port list: direction/width declared in body
                    m.ports.push(ParsedPort {
                        name: pname,
                        dir: None,
                        width: 1,
                        packed_bounds: None,
                        line,
                    });
                }
            }
        }
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'[') {
            if let Some((w, bnd)) = sc.read_range_spec() {
                cur_width = w;
                cur_bounds = Some(bnd);
            }
        }
    }
    Ok(())
}

fn parse_decl_list(
    sc: &mut Scanner<'_>,
    dir: Option<PortDir>,
    kind: NetSegKind,
    line: u32,
    m: &mut ParsedModule,
) {
    let mut width = 1u32;
    let mut bounds: Option<(i128, i128)> = None;

    loop {
        sc.skip_ws_and_comments();
        if sc.eof() || sc.peek_byte() == Some(b';') {
            if sc.peek_byte() == Some(b';') {
                sc.advance_one();
            }
            break;
        }
        if sc.peek_byte() == Some(b'[') {
            if let Some((w, bnd)) = sc.read_range_spec() {
                width = w;
                bounds = Some(bnd);
            }
            continue;
        }
        if sc.peek_byte() == Some(b',') {
            sc.advance_one();
            continue;
        }
        let Some(tok) = sc.read_ident() else {
            sc.advance_one();
            continue;
        };
        if matches!(
            tok,
            "wire" | "reg" | "logic" | "signed" | "unsigned" | "tri" | "var"
        ) {
            continue;
        }
        let name = normalize_sv_leaf(tok);
        if let Some(d) = dir {
            if let Some(existing) = m.ports.iter_mut().find(|p| p.name == name) {
                existing.dir = Some(d);
                existing.width = width;
                existing.packed_bounds = bounds;
            } else {
                m.ports.push(ParsedPort {
                    name: name.clone(),
                    dir: Some(d),
                    width,
                    packed_bounds: bounds,
                    line,
                });
            }
        }
        m.nets.push(ParsedNet {
            name: name.clone(),
            width,
            packed_bounds: bounds,
            kind,
            line,
        });
        // Skip optional unpacked dim or `= init_expr`
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'[') {
            sc.skip_balanced(b'[', b']');
        }
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'=') {
            sc.advance_one();
            let rhs_raw = sc.read_until_comma_or_semi();
            let rhs = parse_expr_str(rhs_raw);
            m.assigns.push(ParsedAssign {
                lhs_net: name,
                lhs_range: None,
                rhs,
                line,
            });
        }
    }
}

fn parse_assign(sc: &mut Scanner<'_>, line: u32, m: &mut ParsedModule) {
    // Optional `#delay`
    sc.skip_ws_and_comments();
    if sc.peek_byte() == Some(b'#') {
        sc.advance_one();
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'(') {
            sc.skip_balanced(b'(', b')');
        } else {
            let _ = sc.read_ident();
        }
    }
    let stmt = sc.read_until_semi();
    if let Some((lhs_str, rhs_str)) = stmt.split_once('=') {
        let (lhs_net, lhs_range) = parse_lhs_ref(lhs_str.trim());
        if !lhs_net.is_empty() {
            let rhs = parse_expr_str(rhs_str.trim());
            m.assigns.push(ParsedAssign {
                lhs_net,
                lhs_range,
                rhs,
                line,
            });
        }
    }
}

fn parse_instantiation(
    sc: &mut Scanner<'_>,
    module_name: String,
    line: u32,
    m: &mut ParsedModule,
) -> Result<(), String> {
    sc.skip_ws_and_comments();
    if sc.peek_byte() == Some(b'#') {
        sc.advance_one();
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'(') {
            sc.skip_balanced(b'(', b')');
        }
    }
    loop {
        sc.skip_ws_and_comments();
        if sc.eof() || sc.peek_byte() == Some(b';') {
            if sc.peek_byte() == Some(b';') {
                sc.advance_one();
            }
            break;
        }
        let Some(raw_inst) = sc.read_ident() else {
            sc.skip_to_semi_or_eof();
            break;
        };
        let raw_name = raw_inst.to_string();
        let inst_name = normalize_sv_leaf(raw_inst);
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b'[') {
            sc.skip_balanced(b'[', b']');
            sc.skip_ws_and_comments();
        }
        let mut conns = Vec::new();
        if sc.peek_byte() == Some(b'(') {
            parse_port_conns(sc, &mut conns)?;
        }
        m.insts.push(ParsedInst {
            module_name: module_name.clone(),
            inst_name,
            raw_name,
            conns,
            line,
        });
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b',') {
            sc.advance_one();
            continue;
        }
        if sc.peek_byte() == Some(b';') {
            sc.advance_one();
        }
        break;
    }
    Ok(())
}

fn parse_port_conns(sc: &mut Scanner<'_>, out: &mut Vec<ParsedPortConn>) -> Result<(), String> {
    sc.advance_one(); // `(`
    while !sc.eof() {
        sc.skip_ws_and_comments();
        if sc.peek_byte() == Some(b')') {
            sc.advance_one();
            break;
        }
        if sc.peek_byte() == Some(b',') {
            sc.advance_one();
            continue;
        }
        let line = sc.line;
        if sc.peek_byte() == Some(b'.') {
            sc.advance_one();
            sc.skip_ws_and_comments();
            let pin_raw = sc
                .read_ident()
                .ok_or_else(|| sc.err("expected port name after `.`"))?;
            let pin_name = normalize_sv_leaf(pin_raw);
            sc.skip_ws_and_comments();
            let expr = if sc.peek_byte() == Some(b'(') {
                let inner = sc.read_balanced_inner(b'(', b')');
                parse_expr_str(inner.trim())
            } else {
                // Implicit `.pin` connection (SV .name)
                ParsedExpr::Ref {
                    net: pin_name.clone(),
                    range: None,
                    invert: false,
                }
            };
            out.push(ParsedPortConn {
                pin_name: Some(pin_name),
                expr,
                line,
            });
        } else {
            let raw = sc.read_until_comma_or_rparen();
            let expr = parse_expr_str(raw.trim());
            out.push(ParsedPortConn {
                pin_name: None,
                expr,
                line,
            });
        }
    }
    Ok(())
}

fn skip_behavioral_item(sc: &mut Scanner<'_>, kw: &str) {
    match kw {
        "function" => sc.skip_until_keyword("endfunction"),
        "task" => sc.skip_until_keyword("endtask"),
        "generate" => sc.skip_until_keyword("endgenerate"),
        "property" => sc.skip_until_keyword("endproperty"),
        "sequence" => sc.skip_until_keyword("endsequence"),
        _ => {
            // Skip a single statement or `begin ... end` block.
            let mut begin_depth = 0usize;
            while !sc.eof() {
                sc.skip_ws_and_comments();
                if sc.eof() {
                    break;
                }
                if sc.peek_byte() == Some(b';') {
                    sc.advance_one();
                    if begin_depth == 0 {
                        break;
                    }
                    continue;
                }
                if let Some(tok) = sc.read_ident() {
                    if tok == "begin" {
                        begin_depth += 1;
                    } else if tok == "end" {
                        begin_depth = begin_depth.saturating_sub(1);
                        if begin_depth == 0 {
                            break;
                        }
                    } else if tok == "endmodule" {
                        break;
                    }
                } else {
                    sc.advance_one();
                }
            }
        }
    }
}
