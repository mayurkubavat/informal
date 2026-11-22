//! Summary, verbose text, and JSON signoff report formatting for `report_lp`.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{LpBenchStats, LpSeverity, LpState, LpStatus, LpViolation, LpWaiver};

#[derive(Debug, Clone, Default)]
pub struct ReportFilter<'a> {
    pub rule_glob: Option<&'a str>,
    pub status: Option<LpStatus>,
    pub stage: Option<&'a str>,
}

impl<'a> ReportFilter<'a> {
    pub fn matches(&self, v: &LpViolation) -> bool {
        if let Some(pat) = self.rule_glob {
            if !informal_upf::glob(pat, &v.rule_id) {
                return false;
            }
        }
        if let Some(st) = self.status {
            if v.status != st {
                return false;
            }
        }
        if let Some(stg) = self.stage {
            if v.stage != stg {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Serialize)]
pub struct LpRuleCount {
    pub rule_id: String,
    pub stage: String,
    pub severity: LpSeverity,
    pub open: usize,
    pub waived: usize,
    pub formally_waived: usize,
    pub cex: usize,
}

#[derive(Debug, Serialize)]
pub struct LpReportSummary {
    pub total: usize,
    pub errors: usize,
    pub warnings: usize,
    pub notes: usize,
    pub open: usize,
    pub waived: usize,
    pub formally_waived: usize,
    pub cex: usize,
    pub by_rule: Vec<LpRuleCount>,
}

#[derive(Debug, Serialize)]
pub struct LpReportJson<'a> {
    pub schema: &'static str,
    pub stages_run: &'a [String],
    pub summary: LpReportSummary,
    pub stats: Option<&'a LpBenchStats>,
    pub violations: Vec<&'a LpViolation>,
    pub waivers: &'a [LpWaiver],
}

pub fn filtered_violations<'a>(lp: &'a LpState, filter: &ReportFilter<'_>) -> Vec<&'a LpViolation> {
    lp.violations.iter().filter(|v| filter.matches(v)).collect()
}

pub fn build_summary(violations: &[&LpViolation]) -> LpReportSummary {
    let mut by_rule_map: BTreeMap<String, LpRuleCount> = BTreeMap::new();
    let (mut errors, mut warnings, mut notes) = (0, 0, 0);
    let (mut open, mut waived, mut formally_waived, mut cex) = (0, 0, 0, 0);

    for &v in violations {
        match v.severity {
            LpSeverity::Error => errors += 1,
            LpSeverity::Warning => warnings += 1,
            LpSeverity::Note => notes += 1,
        }
        match v.status {
            LpStatus::Open => open += 1,
            LpStatus::Waived => waived += 1,
            LpStatus::FormallyWaived => formally_waived += 1,
            LpStatus::FormalCex => cex += 1,
        }
        let entry = by_rule_map
            .entry(v.rule_id.clone())
            .or_insert_with(|| LpRuleCount {
                rule_id: v.rule_id.clone(),
                stage: v.stage.clone(),
                severity: v.severity,
                open: 0,
                waived: 0,
                formally_waived: 0,
                cex: 0,
            });
        match v.status {
            LpStatus::Open => entry.open += 1,
            LpStatus::Waived => entry.waived += 1,
            LpStatus::FormallyWaived => entry.formally_waived += 1,
            LpStatus::FormalCex => entry.cex += 1,
        }
    }

    LpReportSummary {
        total: violations.len(),
        errors,
        warnings,
        notes,
        open,
        waived,
        formally_waived,
        cex,
        by_rule: by_rule_map.into_values().collect(),
    }
}

pub fn format_summary(lp: &LpState, filter: &ReportFilter<'_>) -> String {
    let list = filtered_violations(lp, filter);
    let sum = build_summary(&list);
    let mut out = String::new();
    out.push_str(&format!(
        "LP Signoff Summary (stages: {}): {} total ({} open, {} waived, {} formally_waived, {} cex; {} errors, {} warnings, {} notes)\n",
        if lp.stages_run.is_empty() {
            "none".to_string()
        } else {
            lp.stages_run.join(",")
        },
        sum.total,
        sum.open,
        sum.waived,
        sum.formally_waived,
        sum.cex,
        sum.errors,
        sum.warnings,
        sum.notes,
    ));
    if let Some(st) = &lp.last_stats {
        if st.raw_crossings > 0 || st.grouped_crossings > 0 {
            out.push_str(&format!(
                "  CrossingGroups: {} raw -> {} grouped ({:.2}x compression); Stage1 {:.2}ms, Stage2 {:.2}ms, StructGraph {:.2}ms, PstSat {:.2}ms ({} vars, {} clauses)\n",
                st.raw_crossings,
                st.grouped_crossings,
                st.compression_ratio,
                st.stage1_ms,
                st.stage2_ms,
                st.struct_graph_build_ms,
                st.pst_sat_build_ms,
                st.cnf_vars,
                st.cnf_clauses,
            ));
        }
    }
    if !sum.by_rule.is_empty() {
        out.push_str(
            "  Rule                        Stage    Severity  Open  Waived  FmlWaived  CEX\n",
        );
        out.push_str(
            "  --------------------------  -------  --------  ----  ------  ---------  ---\n",
        );
        for r in &sum.by_rule {
            out.push_str(&format!(
                "  {:<26}  {:<7}  {:<8}  {:>4}  {:>6}  {:>9}  {:>3}\n",
                r.rule_id,
                r.stage,
                r.severity.word(),
                r.open,
                r.waived,
                r.formally_waived,
                r.cex
            ));
        }
    }
    out
}

pub fn format_verbose(lp: &LpState, filter: &ReportFilter<'_>) -> String {
    let mut out = format_summary(lp, filter);
    let list = filtered_violations(lp, filter);
    if list.is_empty() {
        out.push_str("  (no violations matching filter)\n");
        return out;
    }
    out.push('\n');
    for v in list {
        let crossing = match (&v.domain_src, &v.domain_dst) {
            (Some(s), Some(d)) => format!("{s} -> {d}"),
            (Some(s), None) => s.clone(),
            _ => "-".to_string(),
        };
        out.push_str(&format!(
            "[{}] {} ({}, {}, {}) crossing={} signal={} ({}b, {} raw)\n    {}\n",
            v.id,
            v.rule_id,
            v.severity.word(),
            v.stage,
            v.status.word(),
            crossing,
            v.signal,
            v.width_bits,
            v.raw_crossings,
            v.message,
        ));
        if let Some(ref ul) = v.upf_loc {
            out.push_str(&format!("    upf_loc: {}:{}\n", ul.file, ul.line));
        }
        if let Some(ref rl) = v.rtl_loc {
            out.push_str(&format!("    rtl_loc: {rl}\n"));
        }
        if let Some(ref w) = v.pst_witness {
            let states: Vec<String> = w
                .supply_states
                .iter()
                .map(|(k, val)| format!("{k}={val}"))
                .collect();
            out.push_str(&format!("    pst_witness: {{{}}}\n", states.join(", ")));
        }
        if let Some(wid) = v.waiver_id {
            out.push_str(&format!("    waiver_id: {wid}\n"));
        }
    }
    out
}

pub fn format_json(lp: &LpState, filter: &ReportFilter<'_>) -> Result<String, String> {
    let list = filtered_violations(lp, filter);
    let summary = build_summary(&list);
    let payload = LpReportJson {
        schema: "informal.lp/1",
        stages_run: &lp.stages_run,
        summary,
        stats: lp.last_stats.as_ref(),
        violations: list,
        waivers: &lp.waivers,
    };
    serde_json::to_string_pretty(&payload).map_err(|e| format!("report_lp: {e}"))
}
