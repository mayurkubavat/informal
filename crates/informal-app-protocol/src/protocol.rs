//! Formal Bus Protocol VIP SVA Generator (`check_protocol`).
//!
//! Synthesizes complete IEEE 1800-2023 formal protocol verification suites into
//! task `Protocol` (or `-task <name>`) for:
//! - **AMBA AXI4-Lite (`axi4lite`)**
//! - **OpenTitan TileLink-UL (`tlul`)**
//! - **AMBA APB3 / APB4 (`apb`)**
//!
//! Supports `-role master` (assert master rules, assume slave rules), `-role slave`
//! (assume master rules, assert slave rules), and `-role monitor` (assert both
//! master and slave rules), with `-prefix` and `-map KEY=SIG` customization.

use crate::conn::ensure_conn_task;
use crate::session::{Kind, Session};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub const DEFAULT_PROTOCOL_TASK: &str = "Protocol";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BusProtocol {
    Axi4Lite,
    Tlul,
    Apb,
}

impl BusProtocol {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "axi4lite" | "axi4-lite" | "axil" | "axi_lite" => Ok(Self::Axi4Lite),
            "tlul" | "tl-ul" | "tilelink" => Ok(Self::Tlul),
            "apb" | "apb3" | "apb4" => Ok(Self::Apb),
            other => Err(format!(
                "unknown protocol `{other}` (expected `axi4lite`, `tlul`, or `apb`)"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Axi4Lite => "axi4lite",
            Self::Tlul => "tlul",
            Self::Apb => "apb",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BusRole {
    Master,
    Slave,
    Monitor,
}

impl BusRole {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "master" | "host" | "initiator" => Ok(Self::Master),
            "slave" | "device" | "target" | "subordinate" => Ok(Self::Slave),
            "monitor" | "passive" | "both" => Ok(Self::Monitor),
            other => Err(format!(
                "unknown role `{other}` (expected `master`, `slave`, or `monitor`)"
            )),
        }
    }

    fn master_kind(self) -> Kind {
        match self {
            Self::Master | Self::Monitor => Kind::Assert,
            Self::Slave => Kind::Assume,
        }
    }

    fn slave_kind(self) -> Kind {
        match self {
            Self::Slave | Self::Monitor => Kind::Assert,
            Self::Master => Kind::Assume,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProtocolSpec {
    pub name: String,
    pub protocol: BusProtocol,
    pub role: BusRole,
    pub prefix: String,
    pub map: HashMap<String, String>,
    pub task: String,
}

fn resolve_bus_sig(
    known_rels: &HashSet<String>,
    prefix: &str,
    map: &HashMap<String, String>,
    canonical: &str,
    fallbacks: &[&str],
) -> Option<String> {
    if let Some(m) = map.get(canonical) {
        return Some(m.clone());
    }
    let mut cands = vec![format!("{prefix}{canonical}")];
    for fb in fallbacks {
        cands.push(format!("{prefix}{fb}"));
    }
    if known_rels.is_empty() {
        return cands.into_iter().next();
    }
    for c in &cands {
        if known_rels.contains(c) {
            return Some(c.clone());
        }
    }
    for c in &cands {
        if let Some(found) = known_rels.iter().find(|k| k.eq_ignore_ascii_case(c)) {
            return Some(found.clone());
        }
    }
    None
}

/// Creates a formal bus protocol VIP property suite inside `spec.task`.
pub fn create_protocol_vip(s: &mut Session, spec: &ProtocolSpec) -> Result<Vec<String>, String> {
    if !s.elaborated && !s.dry_run {
        s.elaborate(None)
            .map_err(|e| format!("check_protocol: {e:#}"))?;
    }
    let top = s.design.top.clone().unwrap_or_default();
    let top_prefix = format!("{top}.");
    let known_rels: HashSet<String> = s
        .elab
        .as_ref()
        .map(|d| {
            d.signals
                .iter()
                .filter(|sig| !sig.constant)
                .filter_map(|sig| sig.path.strip_prefix(&top_prefix).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let req = |canon: &str, alts: &[&str]| -> Result<String, String> {
        resolve_bus_sig(&known_rels, &spec.prefix, &spec.map, canon, alts).ok_or_else(|| {
            format!(
                "check_protocol ({}): required signal `{}{canon}` not found in `{top}` (use `-prefix` or `-map {canon}=<sig>`)",
                spec.protocol.as_str(),
                spec.prefix
            )
        })
    };
    let opt = |canon: &str, alts: &[&str]| -> Option<String> {
        resolve_bus_sig(&known_rels, &spec.prefix, &spec.map, canon, alts)
    };

    // Build `(short_name, Kind, expr)` items first before mutating `s`.
    let mut items: Vec<(String, Kind, String)> = Vec::new();
    let tag = &spec.name;
    let m_kind = spec.role.master_kind();
    let s_kind = spec.role.slave_kind();

    match spec.protocol {
        BusProtocol::Axi4Lite => {
            let awvalid = req("awvalid", &["s_axi_awvalid", "m_axi_awvalid"])?;
            let awready = req("awready", &["s_axi_awready", "m_axi_awready"])?;
            let awaddr = req("awaddr", &["s_axi_awaddr", "m_axi_awaddr"])?;
            let awprot = opt("awprot", &["s_axi_awprot", "m_axi_awprot"]);

            let wvalid = req("wvalid", &["s_axi_wvalid", "m_axi_wvalid"])?;
            let wready = req("wready", &["s_axi_wready", "m_axi_wready"])?;
            let wdata = req("wdata", &["s_axi_wdata", "m_axi_wdata"])?;
            let wstrb = opt("wstrb", &["s_axi_wstrb", "m_axi_wstrb"]);

            let bvalid = req("bvalid", &["s_axi_bvalid", "m_axi_bvalid"])?;
            let bready = req("bready", &["s_axi_bready", "m_axi_bready"])?;
            let bresp = opt("bresp", &["s_axi_bresp", "m_axi_bresp"]);

            let arvalid = req("arvalid", &["s_axi_arvalid", "m_axi_arvalid"])?;
            let arready = req("arready", &["s_axi_arready", "m_axi_arready"])?;
            let araddr = req("araddr", &["s_axi_araddr", "m_axi_araddr"])?;
            let arprot = opt("arprot", &["s_axi_arprot", "m_axi_arprot"]);

            let rvalid = req("rvalid", &["s_axi_rvalid", "m_axi_rvalid"])?;
            let rready = req("rready", &["s_axi_rready", "m_axi_rready"])?;
            let rdata = req("rdata", &["s_axi_rdata", "m_axi_rdata"])?;
            let rresp = opt("rresp", &["s_axi_rresp", "m_axi_rresp"]);

            // Master rules
            items.push((
                format!("{tag}_aw_valid_hold"),
                m_kind,
                format!("({awvalid} && !{awready}) |=> {awvalid}"),
            ));
            let aw_stab = match awprot {
                Some(p) => format!("($stable({awaddr}) && $stable({p}))"),
                None => format!("$stable({awaddr})"),
            };
            items.push((
                format!("{tag}_aw_payload_stable"),
                m_kind,
                format!("({awvalid} && !{awready}) |=> {aw_stab}"),
            ));

            items.push((
                format!("{tag}_w_valid_hold"),
                m_kind,
                format!("({wvalid} && !{wready}) |=> {wvalid}"),
            ));
            let w_stab = match wstrb {
                Some(st) => format!("($stable({wdata}) && $stable({st}))"),
                None => format!("$stable({wdata})"),
            };
            items.push((
                format!("{tag}_w_payload_stable"),
                m_kind,
                format!("({wvalid} && !{wready}) |=> {w_stab}"),
            ));

            items.push((
                format!("{tag}_ar_valid_hold"),
                m_kind,
                format!("({arvalid} && !{arready}) |=> {arvalid}"),
            ));
            let ar_stab = match arprot {
                Some(p) => format!("($stable({araddr}) && $stable({p}))"),
                None => format!("$stable({araddr})"),
            };
            items.push((
                format!("{tag}_ar_payload_stable"),
                m_kind,
                format!("({arvalid} && !{arready}) |=> {ar_stab}"),
            ));

            // Slave rules
            items.push((
                format!("{tag}_b_valid_hold"),
                s_kind,
                format!("({bvalid} && !{bready}) |=> {bvalid}"),
            ));
            if let Some(br) = bresp {
                items.push((
                    format!("{tag}_b_resp_stable"),
                    s_kind,
                    format!("({bvalid} && !{bready}) |=> $stable({br})"),
                ));
            }
            items.push((
                format!("{tag}_r_valid_hold"),
                s_kind,
                format!("({rvalid} && !{rready}) |=> {rvalid}"),
            ));
            let r_stab = match rresp {
                Some(rr) => format!("($stable({rdata}) && $stable({rr}))"),
                None => format!("$stable({rdata})"),
            };
            items.push((
                format!("{tag}_r_payload_stable"),
                s_kind,
                format!("({rvalid} && !{rready}) |=> {r_stab}"),
            ));

            // Covers
            items.push((
                format!("c_{tag}_wr_txn"),
                Kind::Cover,
                format!("({awvalid} && {awready}) ##[0:4] ({bvalid} && {bready})"),
            ));
            items.push((
                format!("c_{tag}_rd_txn"),
                Kind::Cover,
                format!("({arvalid} && {arready}) ##[0:4] ({rvalid} && {rready})"),
            ));
        }

        BusProtocol::Tlul => {
            let a_valid = req("a_valid", &["tl_i.a_valid"])?;
            let a_ready = req("a_ready", &["tl_o.a_ready"])?;
            let a_opcode = req("a_opcode", &["tl_i.a_opcode"])?;
            let a_address = req("a_address", &["tl_i.a_address"])?;
            let a_data = req("a_data", &["tl_i.a_data"])?;
            let a_mask = opt("a_mask", &["tl_i.a_mask"]);
            let a_size = opt("a_size", &["tl_i.a_size"]);
            let a_source = opt("a_source", &["tl_i.a_source"]);

            let d_valid = req("d_valid", &["tl_o.d_valid"])?;
            let d_ready = req("d_ready", &["tl_i.d_ready"])?;
            let d_opcode = req("d_opcode", &["tl_o.d_opcode"])?;
            let d_data = req("d_data", &["tl_o.d_data"])?;
            let d_source = opt("d_source", &["tl_o.d_source"]);
            let d_error = opt("d_error", &["tl_o.d_error"]);

            items.push((
                format!("{tag}_a_valid_hold"),
                m_kind,
                format!("({a_valid} && !{a_ready}) |=> {a_valid}"),
            ));
            let mut a_fields = vec![
                format!("$stable({a_opcode})"),
                format!("$stable({a_address})"),
                format!("$stable({a_data})"),
            ];
            if let Some(ref m) = a_mask {
                a_fields.push(format!("$stable({m})"));
            }
            if let Some(ref sz) = a_size {
                a_fields.push(format!("$stable({sz})"));
            }
            if let Some(ref sr) = a_source {
                a_fields.push(format!("$stable({sr})"));
            }
            items.push((
                format!("{tag}_a_ctrl_stable"),
                m_kind,
                format!("({a_valid} && !{a_ready}) |=> ({})", a_fields.join(" && ")),
            ));
            items.push((
                format!("{tag}_a_opcode_legal"),
                m_kind,
                format!(
                    "{a_valid} |-> (({a_opcode} == 3'd0) || ({a_opcode} == 3'd1) || ({a_opcode} == 3'd4))"
                ),
            ));
            if let (Some(ref m), Some(ref sz)) = (&a_mask, &a_size) {
                items.push((
                    format!("{tag}_a_putfull_mask"),
                    m_kind,
                    format!(
                        "({a_valid} && ({a_opcode} == 3'd0) && ({sz} == 2'd2)) |-> ({m} == 4'hF)"
                    ),
                ));
            }

            items.push((
                format!("{tag}_d_valid_hold"),
                s_kind,
                format!("({d_valid} && !{d_ready}) |=> {d_valid}"),
            ));
            let mut d_fields = vec![format!("$stable({d_opcode})"), format!("$stable({d_data})")];
            if let Some(ref sr) = d_source {
                d_fields.push(format!("$stable({sr})"));
            }
            if let Some(ref er) = d_error {
                d_fields.push(format!("$stable({er})"));
            }
            items.push((
                format!("{tag}_d_payload_stable"),
                s_kind,
                format!("({d_valid} && !{d_ready}) |=> ({})", d_fields.join(" && ")),
            ));
            items.push((
                format!("{tag}_d_opcode_legal"),
                s_kind,
                format!("{d_valid} |-> (({d_opcode} == 3'd0) || ({d_opcode} == 3'd1))"),
            ));

            items.push((
                format!("c_{tag}_put_ack"),
                Kind::Cover,
                format!(
                    "({a_valid} && {a_ready} && ({a_opcode} != 3'd4)) ##[0:4] ({d_valid} && {d_ready})"
                ),
            ));
            items.push((
                format!("c_{tag}_get_ack"),
                Kind::Cover,
                format!(
                    "({a_valid} && {a_ready} && ({a_opcode} == 3'd4)) ##[0:4] ({d_valid} && {d_ready})"
                ),
            ));
        }

        BusProtocol::Apb => {
            let psel = req("psel", &["pselx"])?;
            let penable = req("penable", &[])?;
            let pwrite = req("pwrite", &[])?;
            let paddr = req("paddr", &[])?;
            let pwdata = req("pwdata", &[])?;
            let pready = req("pready", &[])?;
            let pslverr = opt("pslverr", &[]);

            items.push((
                format!("{tag}_no_orphan_enable"),
                m_kind,
                format!("!{psel} |-> !{penable}"),
            ));
            items.push((
                format!("{tag}_setup_to_access"),
                m_kind,
                format!("({psel} && !{penable}) |=> ({psel} && {penable})"),
            ));
            items.push((
                format!("{tag}_setup_ctrl_stable"),
                m_kind,
                format!(
                    "({psel} && !{penable}) |=> ($stable({paddr}) && $stable({pwrite}) && (!$past({pwrite}) || $stable({pwdata})))"
                ),
            ));
            items.push((
                format!("{tag}_wait_hold"),
                m_kind,
                format!(
                    "({psel} && {penable} && !{pready}) |=> ({psel} && {penable} && $stable({paddr}) && $stable({pwrite}) && (!$past({pwrite}) || $stable({pwdata})))"
                ),
            ));
            items.push((
                format!("{tag}_done_deassert_enable"),
                m_kind,
                format!("({psel} && {penable} && {pready}) |=> !{penable}"),
            ));

            if let Some(err_sig) = pslverr {
                items.push((
                    format!("{tag}_slverr_gated"),
                    s_kind,
                    format!("{err_sig} |-> ({psel} && {penable} && {pready})"),
                ));
            }

            items.push((
                format!("c_{tag}_write"),
                Kind::Cover,
                format!("{psel} && {penable} && {pready} && {pwrite}"),
            ));
            items.push((
                format!("c_{tag}_read"),
                Kind::Cover,
                format!("{psel} && {penable} && {pready} && !{pwrite}"),
            ));
        }
    }

    let task = ensure_conn_task(s, &spec.task)?;
    let mut created = Vec::with_capacity(items.len());
    for (pname, kind, expr) in items {
        let qname = s.add_property_in(&task, kind, Some(pname), &expr)?;
        created.push(qname);
    }
    Ok(created)
}

pub fn format_protocol_report(s: &Session, task: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Bus protocol VIP report (task `{task}`):");
    out.push_str(&s.report_of(Some(task)));
    out
}
