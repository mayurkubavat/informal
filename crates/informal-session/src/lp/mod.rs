//! Static Power-Intent & Structural Verification (Stages 1–2) and Low-Power Verification state for `Session`.

pub mod finder_bridge;
pub mod formal;
pub mod report;
pub mod rules;
pub mod static_rtl;
pub mod static_upf;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use informal_upf::{
    glob, load_upf_files, resolve, ObjectFinder, PowerIntent, PstSat, PstWitness,
    ResolvedPowerIntent, SupplyLogicConstraint, UpfFileSpec, UpfLoc, UpfVersion,
    DEFAULT_VOLTAGE_TOL_UV,
};
use serde::{Deserialize, Serialize};

use crate::session::Session;
use finder_bridge::DesignFinder;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LpStage {
    Upf,
    Rtl,
    Netlist,
    Pg,
    Formal,
    All,
}

impl LpStage {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "upf" => Ok(Self::Upf),
            "rtl" | "design" => Ok(Self::Rtl),
            "netlist" => Ok(Self::Netlist),
            "pg" => Ok(Self::Pg),
            "formal" => Ok(Self::Formal),
            "all" => Ok(Self::All),
            _ => Err(format!(
                "check_lp: invalid -stage `{s}` (expected upf|rtl|design|netlist|pg|formal|all)"
            )),
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Upf => "upf",
            Self::Rtl => "rtl",
            Self::Netlist => "netlist",
            Self::Pg => "pg",
            Self::Formal => "formal",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LpSeverity {
    Error,
    Warning,
    Note,
}

impl LpSeverity {
    pub fn word(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LpStatus {
    Open,
    Waived,
    FormallyWaived,
    #[serde(rename = "cex")]
    FormalCex,
}

impl LpStatus {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(Self::Open),
            "waived" => Ok(Self::Waived),
            "formally_waived" => Ok(Self::FormallyWaived),
            "cex" | "formal_cex" => Ok(Self::FormalCex),
            _ => Err(format!(
                "report_lp: invalid -status `{s}` (expected open|waived|formally_waived|cex)"
            )),
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Waived => "waived",
            Self::FormallyWaived => "formally_waived",
            Self::FormalCex => "cex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpAutoFormalMeta {
    pub engine: String,
    pub ms: u64,
    pub assumptions: Vec<String>,
    pub stopats: Vec<String>,
    pub reset: Option<String>,
    pub task: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpViolation {
    pub id: usize,
    pub rule_id: String,
    pub severity: LpSeverity,
    pub stage: String,
    pub domain_src: Option<String>,
    pub domain_dst: Option<String>,
    pub signal: String,
    pub width_bits: u32,
    pub raw_crossings: usize,
    pub message: String,
    pub upf_loc: Option<UpfLoc>,
    pub rtl_loc: Option<String>,
    pub pst_witness: Option<PstWitness>,
    pub status: LpStatus,
    pub waiver_id: Option<usize>,
    pub formal_prop: Option<String>,
    pub trace: Option<String>,
    pub auto_formal_meta: Option<LpAutoFormalMeta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpWaiver {
    pub id: usize,
    pub rule_glob: String,
    pub signal_glob: Option<String>,
    pub src_domain: Option<String>,
    pub dst_domain: Option<String>,
    pub comment: Option<String>,
}

impl LpWaiver {
    pub fn matches(&self, v: &LpViolation) -> bool {
        if !glob(&self.rule_glob, &v.rule_id) {
            return false;
        }
        if let Some(ref sig_pat) = self.signal_glob {
            if !glob(sig_pat, &v.signal) {
                return false;
            }
        }
        if let Some(ref sd_pat) = self.src_domain {
            match v.domain_src.as_deref() {
                Some(sd) if glob(sd_pat, sd) => {}
                _ => return false,
            }
        }
        if let Some(ref dd_pat) = self.dst_domain {
            match v.domain_dst.as_deref() {
                Some(dd) if glob(dd_pat, dd) => {}
                _ => return false,
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LpLoadSpec {
    pub path: PathBuf,
    pub scope: Option<String>,
    pub version: Option<UpfVersion>,
    pub is_cpf: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LpBenchStats {
    pub struct_graph_build_ms: f64,
    pub pst_sat_build_ms: f64,
    pub stage1_ms: f64,
    pub stage2_ms: f64,
    pub cnf_vars: usize,
    pub cnf_clauses: usize,
    pub raw_crossings: usize,
    pub grouped_crossings: usize,
    pub compression_ratio: f64,
}

#[derive(Debug, Clone)]
pub struct CheckLpOpts {
    pub stage: LpStage,
    pub tol_uv: i64,
    pub prove_corruption: bool,
    pub prove_waivers: bool,
    pub sec: bool,
    pub lec: bool,
}

impl Default for CheckLpOpts {
    fn default() -> Self {
        Self {
            stage: LpStage::All,
            tol_uv: DEFAULT_VOLTAGE_TOL_UV,
            prove_corruption: false,
            prove_waivers: false,
            sec: false,
            lec: false,
        }
    }
}

#[derive(Serialize)]
pub struct LpState {
    pub upf_files: Vec<LpLoadSpec>,
    pub liberty_files: Vec<PathBuf>,
    #[serde(skip)]
    pub intent: Option<PowerIntent>,
    #[serde(skip)]
    pub resolved: Option<ResolvedPowerIntent>,
    pub pst_consistent: Option<bool>,
    #[serde(skip)]
    pub logic_constraints: Vec<SupplyLogicConstraint>,
    pub waivers: Vec<LpWaiver>,
    pub next_waiver_id: usize,
    pub violations: Vec<LpViolation>,
    pub stages_run: Vec<String>,
    pub last_stats: Option<LpBenchStats>,
}

impl Default for LpState {
    fn default() -> Self {
        Self {
            upf_files: Vec::new(),
            liberty_files: Vec::new(),
            intent: None,
            resolved: None,
            pst_consistent: None,
            logic_constraints: Vec::new(),
            waivers: Vec::new(),
            next_waiver_id: 1,
            violations: Vec::new(),
            stages_run: Vec::new(),
            last_stats: None,
        }
    }
}

impl LpState {
    pub fn is_empty(&self) -> bool {
        self.upf_files.is_empty()
            && self.liberty_files.is_empty()
            && self.waivers.is_empty()
            && self.violations.is_empty()
    }

    pub fn clear_results(&mut self) {
        self.resolved = None;
        self.pst_consistent = None;
        self.logic_constraints.clear();
        self.violations.clear();
        self.stages_run.clear();
        self.last_stats = None;
    }

    pub fn invalidate_design(&mut self) {
        self.resolved = None;
        self.pst_consistent = None;
        self.logic_constraints.clear();
        self.violations.clear();
        self.stages_run.clear();
        self.last_stats = None;
    }

    pub fn invalidate_formal_waivers(&mut self) {
        for v in &mut self.violations {
            if v.status == LpStatus::FormallyWaived {
                v.status = LpStatus::Open;
                v.auto_formal_meta = None;
            }
        }
        self.reapply_waivers();
    }

    pub fn add_waiver(
        &mut self,
        rule_glob: String,
        signal_glob: Option<String>,
        src_domain: Option<String>,
        dst_domain: Option<String>,
        comment: Option<String>,
    ) -> usize {
        let id = self.next_waiver_id.max(1);
        self.next_waiver_id = id + 1;
        self.waivers.push(LpWaiver {
            id,
            rule_glob,
            signal_glob,
            src_domain,
            dst_domain,
            comment,
        });
        self.reapply_waivers();
        id
    }

    pub fn remove_waiver(&mut self, id: usize) -> bool {
        let before = self.waivers.len();
        self.waivers.retain(|w| w.id != id);
        if self.waivers.len() == before {
            return false;
        }
        self.reapply_waivers();
        true
    }

    pub fn reapply_waivers(&mut self) {
        for v in &mut self.violations {
            if v.status == LpStatus::Waived {
                v.status = LpStatus::Open;
                v.waiver_id = None;
            }
            if v.status == LpStatus::Open {
                if let Some(w) = self.waivers.iter().find(|w| w.matches(v)) {
                    v.status = LpStatus::Waived;
                    v.waiver_id = Some(w.id);
                }
            }
        }
    }
}

/// Loads a UPF or CPF file into `s.lp`, resolving immediately against the live
/// elaborated design if present (or standalone if not yet elaborated).
pub fn load_upf(
    s: &mut Session,
    path: &Path,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    is_cpf: bool,
) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("load_upf: file not found: {}", path.display()));
    }
    let finder = Arc::new(DesignFinder::from_compiled(
        s.compiled.as_ref(),
        s.design.top.as_deref(),
    ));
    let spec = LpLoadSpec {
        path: path.to_path_buf(),
        scope: scope.map(String::from),
        version,
        is_cpf,
    };
    let next_specs = if scope.is_none() {
        vec![spec]
    } else {
        let mut v = s.lp.upf_files.clone();
        v.push(spec);
        v
    };
    let upf_specs: Vec<UpfFileSpec> = next_specs
        .iter()
        .map(|sp| UpfFileSpec {
            path: sp.path.clone(),
            scope: sp.scope.clone(),
            version: sp.version,
            is_cpf: sp.is_cpf,
        })
        .collect();
    let mut intent =
        load_upf_files(&upf_specs, finder.clone()).map_err(|e| format!("load_upf: {e}"))?;

    let resolved = resolve(&mut intent, finder.as_ref());
    s.lp.upf_files = next_specs;
    s.lp.intent = Some(intent);
    s.lp.resolved = Some(resolved);
    s.lp.pst_consistent = None;
    s.lp.logic_constraints.clear();
    s.lp.violations.clear();
    s.lp.stages_run.clear();
    s.lp.last_stats = None;
    Ok(())
}

/// Records `.lib` file path(s) in `s.lp.liberty_files` (parsed in WS5 Stage 3).
pub fn read_liberty(s: &mut Session, paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() {
        return Err("read_liberty: at least one .lib path is required".to_string());
    }
    for p in paths {
        if !p.is_file() {
            return Err(format!("read_liberty: file not found: {}", p.display()));
        }
        if !s.lp.liberty_files.contains(p) {
            s.lp.liberty_files.push(p.clone());
        }
    }
    Ok(())
}

/// Runs static power-intent/structural and formal Low-Power checks for `opts`.
pub fn check_lp(s: &mut Session, opts: &CheckLpOpts) -> Result<(), String> {
    if s.lp.upf_files.is_empty() && s.lp.intent.is_none() {
        return Err("check_lp: no UPF/CPF loaded; run `load_upf` first".to_string());
    }
    if opts.sec
        || opts.lec
        || opts.prove_corruption
        || opts.prove_waivers
        || opts.stage == LpStage::Formal
    {
        return formal::run_formal_lp(s, opts);
    }
    match opts.stage {
        LpStage::Upf => {
            let t0 = Instant::now();
            let finder = DesignFinder::from_compiled(s.compiled.as_ref(), s.design.top.as_deref());
            let resolved = ensure_resolved_with_finder(s, &finder)?;
            let t_sat = Instant::now();
            let mut sat = PstSat::build_with_width_provider(&resolved, opts.tol_uv, &finder);
            let pst_sat_build_ms = t_sat.elapsed().as_secs_f64() * 1000.0;
            let (cnf_vars, cnf_clauses) = sat.cnf_stats();
            let mut violations = static_upf::run_stage1(&resolved, &mut sat);
            for (idx, v) in violations.iter_mut().enumerate() {
                v.id = idx + 1;
            }
            let stage1_ms = t0.elapsed().as_secs_f64() * 1000.0;
            s.lp.pst_consistent = Some(sat.is_state_space_consistent());
            s.lp.logic_constraints = sat.export_logic_constraints();
            s.lp.resolved = Some(resolved);
            s.lp.violations = violations;
            s.lp.stages_run = vec!["upf".to_string()];
            s.lp.last_stats = Some(LpBenchStats {
                pst_sat_build_ms,
                stage1_ms,
                cnf_vars,
                cnf_clauses,
                ..LpBenchStats::default()
            });
            s.lp.reapply_waivers();
            Ok(())
        }
        LpStage::Rtl | LpStage::All => static_rtl::run_stage2(s, opts),
        LpStage::Netlist | LpStage::Pg => Err(format!(
            "check_lp: `-stage {}` requires Liberty/PG Stage 3–4 support (scheduled in WS5)",
            opts.stage.word()
        )),
        LpStage::Formal => formal::run_formal_lp(s, opts),
    }
}

pub(crate) fn ensure_resolved_with_finder(
    s: &mut Session,
    finder: &DesignFinder,
) -> Result<ResolvedPowerIntent, String> {
    if let Some(ref resolved) = s.lp.resolved {
        if resolved.has_design == finder.has_design() {
            return Ok(resolved.clone());
        }
    }
    if !s.lp.upf_files.is_empty() {
        let f_arc = Arc::new(finder.clone());
        let upf_specs: Vec<UpfFileSpec> =
            s.lp.upf_files
                .iter()
                .map(|sp| UpfFileSpec {
                    path: sp.path.clone(),
                    scope: sp.scope.clone(),
                    version: sp.version,
                    is_cpf: sp.is_cpf,
                })
                .collect();
        let mut intent = load_upf_files(&upf_specs, f_arc).map_err(|e| format!("check_lp: {e}"))?;
        let resolved = resolve(&mut intent, finder);
        s.lp.intent = Some(intent);
        return Ok(resolved);
    }
    let mut intent =
        s.lp.intent
            .clone()
            .ok_or_else(|| "check_lp: no UPF/CPF loaded".to_string())?;
    Ok(resolve(&mut intent, finder))
}
