//! Canonical Static & Formal Power Verification (`lp_static` / `lp_formal`) rule catalog for Stages 1–4.

use super::{LpSeverity, LpStage};

#[derive(Debug, Clone, Copy)]
pub struct LpRuleMeta {
    pub id: &'static str,
    pub stage: LpStage,
    pub default_severity: LpSeverity,
    pub summary: &'static str,
}

pub const RULES: &[LpRuleMeta] = &[
    // Stage 1: UPF / CPF & PST
    LpRuleMeta {
        id: "UPF_D13_UNSUPPORTED",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Unsupported semantic UPF/CPF option (Decision D13)",
    },
    LpRuleMeta {
        id: "UPF_D13_NOTE",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Note,
        summary: "Accepted layout/synthesis hint",
    },
    LpRuleMeta {
        id: "UPF_UNRESOLVED_TARGET",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Unresolved domain, supply net/set/port, or strategy target",
    },
    LpRuleMeta {
        id: "UPF_DOMAIN_CONFLICT",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Instance explicitly assigned to multiple power domains",
    },
    LpRuleMeta {
        id: "UPF_SUPPLY_CYCLE",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Cycle in supply net topology across power switches",
    },
    LpRuleMeta {
        id: "UPF_SUPPLY_UNCONNECTED",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Warning,
        summary: "Supply net has no driver or domain primary supply is incomplete",
    },
    LpRuleMeta {
        id: "UPF_DANGLING_NET",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Warning,
        summary: "Supply port or net is declared but unconnected to any supply set or switch",
    },
    LpRuleMeta {
        id: "UPF_DOMAIN_NO_SUPPLY",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Power domain has no resolved primary supply set or power rail",
    },
    LpRuleMeta {
        id: "UPF_STRATEGY_OVERLAP",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Warning,
        summary: "Conflicting same-tier strategies on the same power domain",
    },
    LpRuleMeta {
        id: "UPF_EXPR_SYNTAX",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Malformed -supply_expr, -logic_expr, or condition expression",
    },
    LpRuleMeta {
        id: "UPF_EQUIV_MISMATCH",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "set_equivalent supply sets disagree on simstate or voltage",
    },
    LpRuleMeta {
        id: "PST_INCONSISTENT",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Symbolic PST state space is unsatisfiable or empty",
    },
    LpRuleMeta {
        id: "PST_UNREACHABLE_STATE",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "PST row or state cannot be satisfied under legal constraints",
    },
    LpRuleMeta {
        id: "PST_MISSING_PORT_STATE",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Warning,
        summary: "Supply net or supply set lacks declared power/port states",
    },
    // Stage 2: Isolation
    LpRuleMeta {
        id: "ISO_MISSING",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Signal crosses from corruptible domain to always-on/ON sink without isolation",
    },
    LpRuleMeta {
        id: "ISO_REDUNDANT",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Isolation strategy specified where source cannot be off while sink is on",
    },
    LpRuleMeta {
        id: "ISO_LOCATION",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Isolation -location domain is unpowered or incompatible with -applies_to",
    },
    LpRuleMeta {
        id: "ISO_SUPPLY_MISMATCH",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Isolation supply can be off while crossing is active",
    },
    LpRuleMeta {
        id: "ISO_CTRL_UNPOWERED",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Isolation enable driver or buffer domain can be off while crossing is active",
    },
    LpRuleMeta {
        id: "ISO_CTRL_POLARITY_MISMATCH",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "RTL clamp gate active sense disagrees with UPF -isolation_sense",
    },
    LpRuleMeta {
        id: "ISO_CTRL_MIXED_POLARITY",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Isolation control cone has reconvergent mixed polarity",
    },
    LpRuleMeta {
        id: "ISO_CLAMP_UNSPECIFIED",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Isolation strategy does not specify a clamp value",
    },
    LpRuleMeta {
        id: "ISO_CLAMP_MISMATCH",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Inline RTL clamp value disagrees with UPF -clamp_value",
    },
    LpRuleMeta {
        id: "ISO_FEEDTHROUGH",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Pure-wire feedthrough crosses an intermediate switchable domain",
    },
    LpRuleMeta {
        id: "ISO_FEEDTHROUGH_BUFFER",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Buffered or combinational feedthrough traverses a switchable domain without AON repeater",
    },
    // Stage 2: Level Shifters
    LpRuleMeta {
        id: "LS_MISSING",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Cross-voltage domain crossing lacks a level-shifter strategy",
    },
    LpRuleMeta {
        id: "LS_DIRECTION",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Level-shifter -rule direction conflicts with PST voltage relation",
    },
    LpRuleMeta {
        id: "LS_LOCATION",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Level-shifter -location or input/output supply is incompatible with crossing",
    },
    LpRuleMeta {
        id: "LS_UNISOLATED_INPUT",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Level-shifter input comes from switchable domain without isolation/ELS",
    },
    LpRuleMeta {
        id: "LS_REDUNDANT",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Level-shifter strategy specified where source and sink share voltage",
    },
    // Stage 2: Retention, Power Switch, Port Attributes
    LpRuleMeta {
        id: "RET_BACKUP_OFF",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Retention backup supply can be off while retained domain is non-NORMAL",
    },
    LpRuleMeta {
        id: "RET_CTRL_UNPOWERED",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Retention save/restore control domain can be off when retention is active",
    },
    LpRuleMeta {
        id: "RET_NO_ELEMENTS",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Retention strategy matches zero sequential elements or switchable domain has unretained flops",
    },
    LpRuleMeta {
        id: "RET_ASYNC_RST_UNISOLATED",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Retained flop async reset crosses from switchable domain without isolation",
    },
    LpRuleMeta {
        id: "PSW_CTRL_UNPOWERED",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Error,
        summary: "Power switch control signal is driven by a corruptible or self-switched domain",
    },
    LpRuleMeta {
        id: "PSW_SUPPLY_UNCONNECTED",
        stage: LpStage::Upf,
        default_severity: LpSeverity::Error,
        summary: "Power switch input/output supply port or net is missing or unresolved",
    },
    LpRuleMeta {
        id: "PORT_ATTR_MISSING",
        stage: LpStage::Rtl,
        default_severity: LpSeverity::Warning,
        summary: "Top-level port in a switchable domain lacks driver_supply/receiver_supply attribute",
    },
    // Stage 3: Post-Synthesis Gate Netlist + Liberty
    LpRuleMeta {
        id: "ISO_CELL_MISSING",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Cross-domain crossing lacks an instantiated Liberty isolation cell",
    },
    LpRuleMeta {
        id: "ISO_CELL_MISMATCH",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Instantiated isolation cell clamp/enable polarity/location disagrees with UPF strategy",
    },
    LpRuleMeta {
        id: "ISO_LOGIC_BEFORE_CELL",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Non-AON logic gate in sink domain precedes the isolation cell on a crossing",
    },
    LpRuleMeta {
        id: "LS_CELL_MISSING",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Cross-voltage crossing lacks an instantiated Liberty level-shifter cell",
    },
    LpRuleMeta {
        id: "LS_DIR_CELL_MISMATCH",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Level-shifter cell direction (HL/LH) mismatches PST voltage relation",
    },
    LpRuleMeta {
        id: "LS_VRANGE_CELL_MISMATCH",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Level-shifter cell input/output voltage range does not cover PST voltages",
    },
    LpRuleMeta {
        id: "LS_ORDER_VIOLATION",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Level shifter precedes isolation cell without combined ELS support",
    },
    LpRuleMeta {
        id: "AON_NON_AON_BUFFER",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Regular non-always-on buffer or gate on isolation/retention control or AON feedthrough inside switchable domain",
    },
    LpRuleMeta {
        id: "RET_CELL_UNMAPPED",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Sequential cell matching set_retention_elements is not mapped to a Liberty retention cell",
    },
    LpRuleMeta {
        id: "RET_PIN_UNCONNECTED",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Retention cell save/restore pin is unconnected or tied improperly",
    },
    LpRuleMeta {
        id: "MACRO_PIN_SUPPLY_CROSSING",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Multi-rail macro pin related_power_pin domain crosses external driver/load without isolation or level shifter",
    },
    // Stage 4: Physical Power/Ground (PG) Netlist
    LpRuleMeta {
        id: "PG_PIN_UNCONNECTED",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Liberty pg_pin on an instantiated cell is unconnected in the PG netlist",
    },
    LpRuleMeta {
        id: "PG_SUPPLY_MISMATCH",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Cell primary power/ground PG pin is connected to a supply net other than its UPF domain primary supply",
    },
    LpRuleMeta {
        id: "PG_BACKUP_MISMATCH",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Cell backup/always-on power pin is connected to a switchable or wrong supply net",
    },
    LpRuleMeta {
        id: "PG_SHORT",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Two incompatible supply nets or rails are physically shorted together",
    },
    LpRuleMeta {
        id: "PG_BIAS_MISMATCH",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Cell well/bias pin (nwell/pwell) connection mismatches UPF supply set",
    },
    LpRuleMeta {
        id: "PG_SWITCH_CHAIN_BROKEN",
        stage: LpStage::Pg,
        default_severity: LpSeverity::Error,
        summary: "Power switch cell supply chain or switch_function connection is broken",
    },
    // Stage 5: Formal Low-Power Verification
    LpRuleMeta {
        id: "ISO_CORRUPTION_LEAK",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "2-copy formal corruption proof found power-off or activity corruption reaching an active output",
    },
    LpRuleMeta {
        id: "RET_POST_WAKEUP_X",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "Unretained sequential register corrupted during power-off leaks to an active output after wakeup",
    },
    LpRuleMeta {
        id: "LP_PMU_SEQ_FAIL",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "PMU power-control sequence violated isolation or retention ordering assertion",
    },
    LpRuleMeta {
        id: "LP_PST_ILLEGAL_REACHED",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "PMU hardware reached an illegal power state table combination",
    },
    LpRuleMeta {
        id: "LP_RET_VALUE_CORRUPTED",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "Retained register value was not preserved across sleep/restore cycle",
    },
    LpRuleMeta {
        id: "LP_SEC_MISMATCH",
        stage: LpStage::Formal,
        default_severity: LpSeverity::Error,
        summary: "Power-aware sequential equivalence check found output divergence against golden RTL",
    },
    LpRuleMeta {
        id: "LP_LEC_MISMATCH",
        stage: LpStage::Netlist,
        default_severity: LpSeverity::Error,
        summary: "Power-aware RTL+UPF vs Netlist+UPF equivalence check found clamp or logic mismatch",
    },
];

pub fn lookup_rule(id: &str) -> Option<&'static LpRuleMeta> {
    RULES.iter().find(|r| r.id == id)
}
