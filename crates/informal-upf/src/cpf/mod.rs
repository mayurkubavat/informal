//! Cadence CPF (1.1 / 2.0) Tcl 8.6 front end lowering directly into
//! canonical [`PowerIntent`] IR with Decision D13 strict option checking.

pub mod cmds_domain_mode;
pub mod cmds_rules;
pub mod finalize;
mod finalize_defaults;
mod finalize_pst;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use informal_tcl::{Interp, Value};

use crate::expr_bv::qualify_logic_expr_in_scope;
use crate::ir::{
    normalize_scope, trim_hier_path, PowerIntent, Sense, SupplySet, SupplySetId, UpfLoc,
    UpfVersion, VoltageSpec,
};
use crate::upf::{register_upf_commands, ObjectFinder, UpfError, UpfEvalState};

pub(crate) use finalize::{finalize_cpf, finalize_cpf_ex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpfCondState {
    On,
    Off,
    Standby,
}

#[derive(Debug, Clone)]
pub(crate) struct NominalCondition {
    pub voltage: VoltageSpec,
    pub state: CpfCondState,
    pub pmos_bias: Option<VoltageSpec>,
    pub nmos_bias: Option<VoltageSpec>,
    pub loc: Option<UpfLoc>,
}

#[derive(Debug, Clone)]
pub(crate) struct CpfPowerMode {
    pub name: String,
    pub scope: String,
    pub design_top: Option<String>,
    pub domain_conditions: Vec<(String, String)>,
    pub condition: Option<String>,
    pub condition_scope: String,
    pub is_default: bool,
    pub illegal: bool,
    pub emitted: bool,
    pub loc: Option<UpfLoc>,
}

#[derive(Default)]
pub(crate) struct CpfState {
    /// Keyed by `(normalized_scope, nc_name)` so scoped nominal conditions stay isolated from root.
    pub nominal_conditions: BTreeMap<(String, String), NominalCondition>,
    /// NC keys authored directly via `create_nominal_condition` in the current CPF frame.
    pub direct_nominal_conditions: BTreeSet<(String, String)>,
    /// NC keys authored directly via `create_nominal_condition` in any enclosing ancestor frame.
    pub ancestor_direct_nominal_conditions: BTreeSet<(String, String)>,
    /// NC keys authored in this frame or any nested child frame popped into it.
    pub subtree_nominal_conditions: BTreeSet<(String, String)>,
    /// NC keys carrying explicit-`-scope` overwrite intent from a `-scope` child frame.
    pub explicit_overwrite_nominal_conditions: BTreeSet<(String, String)>,
    /// Keyed by `(normalized_scope, nc_name)` matching `nominal_conditions`.
    pub updated_nominal_conditions: BTreeSet<(String, String)>,
    pub updated_domain_conditions: BTreeSet<String>,
    pub power_modes: Vec<CpfPowerMode>,
    pub domain_shutoff: BTreeMap<String, (String, String)>,
    pub domain_enable: BTreeMap<String, (String, String)>,
    pub cpf_ss_names: BTreeSet<String>,
    pub active_design: Option<String>,
    pub design_stack: Vec<(Option<String>, Option<String>)>,
    pub emitted_fallback_states: BTreeSet<(String, String)>,
    pub emitted_mode_states: BTreeSet<(String, String)>,
    pub finalized: bool,
    pub fallback_applied: bool,
}

pub(crate) fn resolve_nc_in_map<'a>(
    ncs: &'a BTreeMap<(String, String), NominalCondition>,
    scope: &str,
    nc_name: &str,
) -> Option<(&'a str, &'a NominalCondition)> {
    let key = nc_name.trim();
    if let Some(abs) = key.strip_prefix('/') {
        return ncs
            .get_key_value(&(String::new(), abs.to_string()))
            .map(|((sc, _), nc)| (sc.as_str(), nc));
    }
    let mut cur = scope.trim_matches('/');
    loop {
        if let Some(((sc, _), nc)) = ncs.get_key_value(&(cur.to_string(), key.to_string())) {
            return Some((sc.as_str(), nc));
        }
        if cur.is_empty() {
            break;
        }
        cur = match cur.rsplit_once('/') {
            Some((parent, _)) => parent,
            None => "",
        };
    }
    None
}

impl CpfState {
    pub(crate) fn is_empty(&self) -> bool {
        self.nominal_conditions.is_empty()
            && self.direct_nominal_conditions.is_empty()
            && self.subtree_nominal_conditions.is_empty()
            && self.explicit_overwrite_nominal_conditions.is_empty()
            && self.updated_nominal_conditions.is_empty()
            && self.updated_domain_conditions.is_empty()
            && self.power_modes.is_empty()
            && self.domain_shutoff.is_empty()
            && self.domain_enable.is_empty()
            && self.cpf_ss_names.is_empty()
            && self.active_design.is_none()
            && self.design_stack.is_empty()
            && self.emitted_fallback_states.is_empty()
            && self.emitted_mode_states.is_empty()
    }

    pub(crate) fn inherit_from(&mut self, parent: &Self) {
        self.nominal_conditions = parent.nominal_conditions.clone();
        self.updated_nominal_conditions = parent.updated_nominal_conditions.clone();
        self.updated_domain_conditions = parent.updated_domain_conditions.clone();
        self.power_modes = parent.power_modes.clone();
        self.domain_shutoff = parent.domain_shutoff.clone();
        self.domain_enable = parent.domain_enable.clone();
        self.cpf_ss_names = parent.cpf_ss_names.clone();
        self.active_design = parent.active_design.clone();
        self.design_stack = parent.design_stack.clone();
        self.emitted_fallback_states = parent.emitted_fallback_states.clone();
        self.emitted_mode_states = parent.emitted_mode_states.clone();
        self.finalized = parent.finalized;
        self.fallback_applied = parent.fallback_applied;
    }

    pub(crate) fn inherit_nested_child_cpf(&mut self, parent: &Self, inherit_design: bool) {
        self.nominal_conditions = parent.nominal_conditions.clone();
        self.domain_shutoff = parent.domain_shutoff.clone();
        self.domain_enable = parent.domain_enable.clone();
        self.cpf_ss_names = parent.cpf_ss_names.clone();
        self.power_modes = parent.power_modes.clone();
        self.emitted_fallback_states = parent.emitted_fallback_states.clone();
        self.emitted_mode_states = parent.emitted_mode_states.clone();
        if inherit_design {
            self.active_design = parent.active_design.clone();
            self.design_stack = parent.design_stack.clone();
        }
    }

    pub(crate) fn merge_domain_conditions_into(&self, parent: &mut Self) {
        for (k, v) in &self.domain_shutoff {
            parent.domain_enable.remove(k);
            parent.domain_shutoff.insert(k.clone(), v.clone());
        }
        for (k, v) in &self.domain_enable {
            parent.domain_shutoff.remove(k);
            parent.domain_enable.insert(k.clone(), v.clone());
        }
        for k in &self.updated_domain_conditions {
            parent.updated_domain_conditions.insert(k.clone());
        }
        for s in &self.cpf_ss_names {
            parent.cpf_ss_names.insert(s.clone());
        }
    }

    fn allows_nc_overwrite(
        &self,
        k: &(String, String),
        is_sequential_sibling: bool,
        explicit_scope: bool,
        is_explicit_k: bool,
    ) -> bool {
        is_sequential_sibling
            || is_explicit_k
            || (!self.direct_nominal_conditions.contains(k)
                && (!self.explicit_overwrite_nominal_conditions.contains(k)
                    || (explicit_scope && !self.ancestor_direct_nominal_conditions.contains(k))))
    }

    pub(crate) fn merge_nominal_conditions_into(
        &self,
        parent: &mut Self,
        is_sequential_sibling: bool,
        explicit_scope: bool,
    ) {
        let mut newly_inserted: Vec<(String, String)> = Vec::new();
        let mut overwritten_keys: BTreeSet<(String, String)> = BTreeSet::new();
        for (k, v) in &self.nominal_conditions {
            let is_explicit_k = self.explicit_overwrite_nominal_conditions.contains(k)
                || (explicit_scope && self.direct_nominal_conditions.contains(k));
            let allow_overwrite =
                parent.allows_nc_overwrite(k, is_sequential_sibling, explicit_scope, is_explicit_k);
            if allow_overwrite {
                overwritten_keys.insert(k.clone());
                if is_explicit_k {
                    parent
                        .explicit_overwrite_nominal_conditions
                        .insert(k.clone());
                } else if self.subtree_nominal_conditions.contains(k) {
                    parent.explicit_overwrite_nominal_conditions.remove(k);
                }
                match parent.nominal_conditions.insert(k.clone(), v.clone()) {
                    None => newly_inserted.push(k.clone()),
                    Some(prev) => {
                        let changed = prev.voltage != v.voltage
                            || prev.state != v.state
                            || prev.pmos_bias != v.pmos_bias
                            || prev.nmos_bias != v.nmos_bias;
                        if changed {
                            parent.updated_nominal_conditions.insert(k.clone());
                        }
                    }
                }
            } else if let Some(prev) = parent.nominal_conditions.get(k) {
                let changed = prev.voltage != v.voltage
                    || prev.state != v.state
                    || prev.pmos_bias != v.pmos_bias
                    || prev.nmos_bias != v.nmos_bias;
                if changed {
                    let (sc, nc_name) = (&k.0, &k.1);
                    let child_mode_uses_nc = self.power_modes.iter().any(|pm| {
                        pm.domain_conditions.iter().any(|(_, nc)| {
                            nc.trim().trim_start_matches('/') == nc_name.as_str()
                                && resolve_nc_in_map(&self.nominal_conditions, &pm.scope, nc)
                                    .is_some_and(|(def_sc, _)| def_sc == sc.as_str())
                        })
                    });
                    if child_mode_uses_nc {
                        parent.updated_nominal_conditions.insert(k.clone());
                    }
                }
            } else {
                parent.nominal_conditions.insert(k.clone(), v.clone());
                newly_inserted.push(k.clone());
            }
        }
        for k in &self.subtree_nominal_conditions {
            parent.subtree_nominal_conditions.insert(k.clone());
        }
        if explicit_scope {
            for k in &self.direct_nominal_conditions {
                parent
                    .explicit_overwrite_nominal_conditions
                    .insert(k.clone());
            }
        }
        for k in &self.explicit_overwrite_nominal_conditions {
            parent
                .explicit_overwrite_nominal_conditions
                .insert(k.clone());
        }
        for k in newly_inserted {
            let (sc, nc_name) = (&k.0, &k.1);
            let shadows_mode_ref = parent.power_modes.iter().any(|pm| {
                pm.domain_conditions.iter().any(|(_, nc)| {
                    nc.trim().trim_start_matches('/') == nc_name.as_str()
                        && resolve_nc_in_map(&parent.nominal_conditions, &pm.scope, nc)
                            .is_some_and(|(def_sc, _)| def_sc == sc.as_str())
                })
            });
            if shadows_mode_ref {
                parent.updated_nominal_conditions.insert(k);
            }
        }
        for k in &self.updated_nominal_conditions {
            let is_explicit_k = self.explicit_overwrite_nominal_conditions.contains(k)
                || (explicit_scope && self.direct_nominal_conditions.contains(k));
            if overwritten_keys.contains(k)
                || (!self.nominal_conditions.contains_key(k)
                    && parent.allows_nc_overwrite(
                        k,
                        is_sequential_sibling,
                        explicit_scope,
                        is_explicit_k,
                    ))
            {
                parent.updated_nominal_conditions.insert(k.clone());
            }
        }
    }

    pub(crate) fn merge_power_modes_into(&self, parent: &mut Self) {
        for pm in &self.power_modes {
            if let Some(existing) = parent
                .power_modes
                .iter_mut()
                .find(|m| m.name == pm.name && m.scope == pm.scope && m.design_top == pm.design_top)
            {
                *existing = pm.clone();
            } else {
                parent.power_modes.push(pm.clone());
            }
        }
    }

    pub(crate) fn has_pending(&self) -> bool {
        (!self.finalized
            && (!self.power_modes.is_empty()
                || !self.domain_shutoff.is_empty()
                || !self.domain_enable.is_empty()))
            || !self.updated_nominal_conditions.is_empty()
            || !self.updated_domain_conditions.is_empty()
    }
}

/// Evaluates a CPF (1.1 / 2.0) file on disk into canonical [`PowerIntent`].
pub fn load_cpf_file(
    path: &Path,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<PowerIntent, UpfError> {
    let canon = std::fs::canonicalize(path).map_err(|source| UpfError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let interp = Interp::new();
    let state = Rc::new(RefCell::new(UpfEvalState::new(finder)));
    register_upf_commands(&interp, &state)?;
    register_cpf_commands(&interp, &state)?;
    interp
        .eval("namespace eval :: { namespace import -force ::cpf::* }")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    {
        let mut st = state.borrow_mut();
        st.is_cpf_mode = true;
        if let Some(ver) = version {
            st.intent.upf_version = Some(ver);
        }
        if let Some(sc) = scope {
            let norm = normalize_scope(sc, st.intent.hierarchy_separator);
            if st.finder.has_design() && !norm.is_empty() && !st.finder.scope_exists(&norm) {
                return Err(UpfError::Semantic(format!(
                    "load_cpf: unresolved scope `{norm}` in design"
                )));
            }
            st.current_scope = norm;
        }
        st.intent
            .loaded_files
            .push(canon.to_string_lossy().into_owned());
    }

    interp.eval_file(&canon).map_err(|e| UpfError::Tcl {
        message: e.message,
        info: e.info,
    })?;

    {
        let mut st = state.borrow_mut();
        let UpfEvalState {
            intent, cpf_state, ..
        } = &mut *st;
        finalize_cpf(intent, cpf_state);
    }
    let intent = state.borrow().intent.clone();
    Ok(intent)
}

/// Evaluates an inline CPF Tcl script string into canonical [`PowerIntent`].
pub fn parse_cpf_str(
    script: &str,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<PowerIntent, UpfError> {
    let interp = Interp::new();
    let state = Rc::new(RefCell::new(UpfEvalState::new(finder)));
    register_upf_commands(&interp, &state)?;
    register_cpf_commands(&interp, &state)?;
    interp
        .eval("namespace eval :: { namespace import -force ::cpf::* }")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    {
        let mut st = state.borrow_mut();
        st.is_cpf_mode = true;
        if let Some(ver) = version {
            st.intent.upf_version = Some(ver);
        }
        if let Some(sc) = scope {
            let norm = normalize_scope(sc, st.intent.hierarchy_separator);
            if st.finder.has_design() && !norm.is_empty() && !st.finder.scope_exists(&norm) {
                return Err(UpfError::Semantic(format!(
                    "load_cpf: unresolved scope `{norm}` in design"
                )));
            }
            st.current_scope = norm;
        }
    }

    interp.eval(script).map_err(|e| UpfError::Tcl {
        message: e.message,
        info: e.info,
    })?;

    {
        let mut st = state.borrow_mut();
        let UpfEvalState {
            intent, cpf_state, ..
        } = &mut *st;
        finalize_cpf(intent, cpf_state);
    }
    let intent = state.borrow().intent.clone();
    Ok(intent)
}

pub(crate) fn register_cpf_commands(
    interp: &Interp,
    state: &Rc<RefCell<UpfEvalState>>,
) -> Result<(), UpfError> {
    interp
        .eval("namespace eval ::cpf {}")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    register_cpf(
        interp,
        state,
        "::cpf::set_cpf_version",
        cmds_domain_mode::cmd_set_cpf_version,
    );
    register_cpf(
        interp,
        state,
        "::cpf::set_hierarchy_separator",
        cmds_domain_mode::cmd_set_hierarchy_separator,
    );
    register_cpf(
        interp,
        state,
        "::cpf::set_array_naming_style",
        cmds_domain_mode::cmd_set_array_naming_style,
    );
    register_cpf(
        interp,
        state,
        "::cpf::set_design",
        cmds_domain_mode::cmd_set_design,
    );
    register_cpf(
        interp,
        state,
        "::cpf::end_design",
        cmds_domain_mode::cmd_end_design,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_power_nets",
        cmds_domain_mode::cmd_create_power_nets,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_ground_nets",
        cmds_domain_mode::cmd_create_ground_nets,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_global_connection",
        cmds_domain_mode::cmd_create_global_connection,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_power_domain",
        cmds_domain_mode::cmd_create_power_domain,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_power_domain",
        cmds_domain_mode::cmd_update_power_domain,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_nominal_condition",
        cmds_domain_mode::cmd_create_nominal_condition,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_operating_corner",
        cmds_domain_mode::cmd_create_operating_corner,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_power_mode",
        cmds_domain_mode::cmd_create_power_mode,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_power_mode",
        cmds_domain_mode::cmd_update_power_mode,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_power_switch_rule",
        cmds_rules::cmd_create_power_switch_rule,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_power_switch_rule",
        cmds_rules::cmd_update_power_switch_rule,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_isolation_rule",
        cmds_rules::cmd_create_isolation_rule,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_isolation_rules",
        cmds_rules::cmd_update_isolation_rules,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_level_shifter_rule",
        cmds_rules::cmd_create_level_shifter_rule,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_level_shifter_rules",
        cmds_rules::cmd_update_level_shifter_rules,
    );
    register_cpf(
        interp,
        state,
        "::cpf::create_state_retention_rule",
        cmds_rules::cmd_create_state_retention_rule,
    );
    register_cpf(
        interp,
        state,
        "::cpf::update_state_retention_rules",
        cmds_rules::cmd_update_state_retention_rules,
    );
    register_cpf(
        interp,
        state,
        "::cpf::define_isolation_cell",
        cmds_rules::cmd_define_cell_note,
    );
    register_cpf(
        interp,
        state,
        "::cpf::define_level_shifter_cell",
        cmds_rules::cmd_define_cell_note,
    );
    register_cpf(
        interp,
        state,
        "::cpf::define_state_retention_cell",
        cmds_rules::cmd_define_cell_note,
    );
    register_cpf(
        interp,
        state,
        "::cpf::define_power_switch_cell",
        cmds_rules::cmd_define_cell_note,
    );
    register_cpf(
        interp,
        state,
        "::cpf::identify_power_logic",
        cmds_rules::cmd_identify_power_logic,
    );

    interp
        .eval(
            "namespace eval ::cpf { namespace import -force ::upf::load_upf ::upf::find_objects; namespace export * }",
        )
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;
    Ok(())
}

fn register_cpf<F>(interp: &Interp, state: &Rc<RefCell<UpfEvalState>>, name: &str, f: F)
where
    F: Fn(&mut UpfEvalState, &[String], Option<UpfLoc>) -> Result<Value, String> + 'static,
{
    let st = state.clone();
    interp.register(
        name,
        Box::new(move |args| {
            let loc = UpfLoc::from_tcl();
            let mut s = st
                .try_borrow_mut()
                .map_err(|_| "CPF interpreter state busy".to_string())?;
            f(&mut s, args, loc)
        }),
    );
}

pub(crate) fn cpf_ss_name(domain_name: &str) -> String {
    if let Some(abs) = domain_name.strip_prefix('/') {
        format!("/SS_{abs}")
    } else {
        format!("SS_{domain_name}")
    }
}

pub(crate) fn ensure_cpf_domain_supply_set(
    intent: &mut PowerIntent,
    domain_name: &str,
    scope: &str,
    loc: Option<UpfLoc>,
) -> String {
    let ss_name = cpf_ss_name(domain_name);
    let norm_domain = domain_name.trim_start_matches('/');
    let derived_scope = intent
        .domains
        .iter()
        .find(|d| d.name == norm_domain)
        .map(|d| d.scope.clone())
        .unwrap_or_else(|| {
            if !scope.is_empty() {
                scope.to_string()
            } else {
                norm_domain
                    .rsplit_once('/')
                    .map(|(s, _)| s.to_string())
                    .unwrap_or_default()
            }
        });
    if let Some(existing) = intent.supply_sets.iter_mut().find(|s| s.name == ss_name) {
        if existing.scope.is_empty() && !derived_scope.is_empty() {
            existing.scope = derived_scope;
        }
    } else {
        let id = SupplySetId(intent.supply_sets.len() as u32);
        intent.supply_sets.push(SupplySet {
            id,
            name: ss_name.clone(),
            scope: derived_scope,
            power: None,
            ground: None,
            pwell: None,
            nwell: None,
            deeppwell: None,
            deepnwell: None,
            extra_functions: BTreeMap::new(),
            is_implicit: true,
            reference_gnd: None,
            loc,
        });
    }
    ss_name
}

pub(crate) fn strip_outer_balanced_parens(mut s: &str) -> &str {
    loop {
        let t = s.trim();
        if has_outer_balanced_parens(t) {
            s = &t[1..t.len() - 1];
        } else {
            return t;
        }
    }
}

pub(crate) fn parse_cpf_signal_sense(
    expr: &str,
    st: &UpfEvalState,
) -> (Option<String>, Option<Sense>) {
    let sep = st.intent.hierarchy_separator;
    let t = strip_outer_balanced_parens(expr);
    if t.is_empty() {
        return (None, None);
    }
    let (body, sense) = if let Some(rest) = t.strip_prefix('!') {
        let rest_trim = rest.trim();
        if !rest.starts_with('=')
            && (has_outer_balanced_parens(rest_trim)
                || (!rest_trim.is_empty() && !is_compound_bool_expr(rest_trim, sep)))
        {
            (strip_outer_balanced_parens(rest_trim), Sense::Low)
        } else {
            (t, Sense::High)
        }
    } else {
        (t, Sense::High)
    };
    if body.is_empty() {
        return (None, None);
    }
    let qualified = if !is_compound_bool_expr(body, sep) {
        st.qualify_signal_ref(body)
    } else if st.current_scope.is_empty()
        && sep == '/'
        && !body.contains("./")
        && !body.contains("../")
    {
        body.to_string()
    } else {
        qualify_logic_expr_in_scope(body, &st.current_scope, sep)
    };
    (Some(qualified), Some(sense))
}

fn has_outer_balanced_parens(s: &str) -> bool {
    if !(s.starts_with('(') && s.ends_with(')')) {
        return false;
    }
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                let Some(next) = depth.checked_sub(1) else {
                    return false;
                };
                depth = next;
                if depth == 0 && i + 1 < bytes.len() {
                    return false;
                }
            }
            _ => {}
        }
        i += 1;
    }
    depth == 0
}

pub(crate) fn is_compound_bool_expr(s: &str, sep: char) -> bool {
    let mut in_escaped = false;
    let mut bracket_depth = 0usize;
    for ch in s.chars() {
        if in_escaped {
            if ch.is_ascii_whitespace() {
                in_escaped = false;
            }
            continue;
        }
        if ch == '\\' {
            in_escaped = true;
            continue;
        }
        if ch == '[' {
            bracket_depth += 1;
            continue;
        }
        if ch == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            continue;
        }
        if ch.is_ascii_whitespace()
            || (ch == ':' && bracket_depth == 0 && sep != ':')
            || matches!(
                ch,
                '&' | '|' | '^' | '=' | '<' | '>' | '?' | '!' | '~' | '(' | ')'
            )
        {
            return true;
        }
    }
    false
}

pub(crate) fn qualify_cpf_bool_expr(st: &UpfEvalState, expr: &str, scope: &str) -> String {
    let t = trim_hier_path(expr);
    let sep = st.intent.hierarchy_separator;
    if !is_compound_bool_expr(t, sep) && scope == st.current_scope {
        st.qualify_signal_ref(t)
    } else {
        qualify_logic_expr_in_scope(t, scope, sep)
    }
}

pub(crate) fn negate_bool_expr(expr: &str) -> String {
    let t = trim_hier_path(expr);
    if let Some(inner) = t.strip_prefix('!') {
        let inner = trim_hier_path(inner);
        if has_outer_balanced_parens(inner) {
            return trim_hier_path(&inner[1..inner.len() - 1]).to_string();
        }
        if !inner.is_empty() && !is_compound_bool_expr(inner, '/') {
            return inner.to_string();
        }
    }
    if is_compound_bool_expr(t, '/') {
        if t.contains('\\') && !t.ends_with(char::is_whitespace) {
            format!("!({t} )")
        } else {
            format!("!({t})")
        }
    } else {
        format!("!{t}")
    }
}
