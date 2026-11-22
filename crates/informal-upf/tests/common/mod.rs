#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use informal_upf::{
    glob, parse_upf_and_resolve, MapWidthProvider, NullObjectFinder, NullWidthProvider,
    ObjectFinder, ObjectType, PstSat, PstWitness, SignalWidthProvider, DEFAULT_VOLTAGE_TOL_UV,
};

pub struct MockDesignFinder {
    pub scopes: BTreeSet<String>,
    pub insts: Vec<String>,
}

impl MockDesignFinder {
    pub fn new(scopes: &[&str], insts: &[&str]) -> Self {
        Self {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            insts: insts.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ObjectFinder for MockDesignFinder {
    fn has_design(&self) -> bool {
        true
    }

    fn scope_exists(&self, scope_path: &str) -> bool {
        let norm = scope_path.trim_matches('/');
        norm.is_empty() || self.scopes.contains(norm)
    }

    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String> {
        if let Some(t) = obj_type {
            if !matches!(t, ObjectType::Inst | ObjectType::Reg | ObjectType::Cell) {
                return Vec::new();
            }
        }
        let norm_scope = scope_path.trim_matches('/');
        self.insts
            .iter()
            .filter(|inst| {
                let rel = if norm_scope.is_empty() {
                    inst.as_str()
                } else if let Some(r) = inst.strip_prefix(&format!("{norm_scope}/")) {
                    r
                } else {
                    return false;
                };
                if !transitive && rel.contains('/') {
                    return false;
                }
                let leaf = rel.rsplit('/').next().unwrap_or(rel);
                patterns
                    .iter()
                    .any(|p| glob(p, inst) || glob(p, rel) || glob(p, leaf))
            })
            .cloned()
            .collect()
    }
}

pub struct RelOnlyFinder {
    pub scopes: BTreeSet<String>,
    pub insts: Vec<String>,
}

impl RelOnlyFinder {
    pub fn new(scopes: &[&str], insts: &[&str]) -> Self {
        Self {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            insts: insts.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ObjectFinder for RelOnlyFinder {
    fn has_design(&self) -> bool {
        true
    }

    fn scope_exists(&self, scope_path: &str) -> bool {
        let norm = scope_path.trim_matches('/');
        norm.is_empty() || self.scopes.contains(norm)
    }

    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String> {
        if let Some(t) = obj_type {
            if !matches!(t, ObjectType::Inst | ObjectType::Reg | ObjectType::Cell) {
                return Vec::new();
            }
        }
        let norm_scope = scope_path.trim_matches('/');
        self.insts
            .iter()
            .filter(|inst| {
                let rel = if norm_scope.is_empty() {
                    inst.as_str()
                } else if let Some(r) = inst.strip_prefix(&format!("{norm_scope}/")) {
                    r
                } else {
                    return false;
                };
                if !transitive && rel.contains('/') {
                    return false;
                }
                let leaf = rel.rsplit('/').next().unwrap_or(rel);
                patterns.iter().any(|p| glob(p, rel) || glob(p, leaf))
            })
            .cloned()
            .collect()
    }
}

pub struct NoUnescapeFinder {
    pub scopes: BTreeSet<String>,
    pub insts: Vec<String>,
}

impl NoUnescapeFinder {
    pub fn new(scopes: &[&str], insts: &[&str]) -> Self {
        Self {
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            insts: insts.iter().map(|s| s.to_string()).collect(),
        }
    }
}

impl ObjectFinder for NoUnescapeFinder {
    fn has_design(&self) -> bool {
        true
    }

    fn scope_exists(&self, scope_path: &str) -> bool {
        let norm = scope_path.trim_matches('/');
        norm.is_empty() || self.scopes.contains(norm)
    }

    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String> {
        if let Some(t) = obj_type {
            if !matches!(t, ObjectType::Inst | ObjectType::Reg | ObjectType::Cell) {
                return Vec::new();
            }
        }
        if patterns.iter().any(|p| p.contains('\\')) {
            return Vec::new();
        }
        let norm_scope = scope_path.trim_matches('/');
        self.insts
            .iter()
            .filter(|inst| {
                let rel = if norm_scope.is_empty() {
                    inst.as_str()
                } else if let Some(r) = inst.strip_prefix(&format!("{norm_scope}/")) {
                    r
                } else {
                    return false;
                };
                if !transitive && rel.contains('/') {
                    return false;
                }
                let leaf = rel.rsplit('/').next().unwrap_or(rel);
                patterns
                    .iter()
                    .any(|p| glob(p, inst) || glob(p, rel) || glob(p, leaf))
            })
            .cloned()
            .collect()
    }
}

pub fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "informal-upf-test-{}-{tag}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

pub fn check_logic_with_provider(
    expr: &str,
    provider: &dyn SignalWidthProvider,
    expected: bool,
) -> Option<PstWitness> {
    let upf = format!(
        r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {{power VDD}} -function {{ground VSS}}
        create_power_domain PD -include_scope -supply {{primary SS}}
        add_power_state SS -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{{expr}}} -simstate NORMAL}}
    "#
    );
    let res = parse_upf_and_resolve(&upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat = PstSat::build_with_width_provider(&res, DEFAULT_VOLTAGE_TOL_UV, provider);
    assert!(
        !sat.diagnostics()
            .iter()
            .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"),
        "unexpected syntax diag for `{expr}`: {:?}",
        sat.diagnostics()
    );
    assert_eq!(
        sat.is_state_space_consistent(),
        expected,
        "wrong SAT verdict for `{expr}`"
    );
    if expected {
        sat.enumerate_symbolic_states(1).into_iter().next()
    } else {
        None
    }
}

pub fn check_logic(expr: &str, expected: bool) -> Option<PstWitness> {
    check_logic_with_provider(expr, &NullWidthProvider, expected)
}

pub fn check_logic_with_widths(
    expr: &str,
    widths: &[(&str, u32)],
    expected: bool,
) -> Option<PstWitness> {
    let mut provider = MapWidthProvider::new();
    for &(name, width) in widths {
        provider.insert(name, width);
    }
    check_logic_with_provider(expr, &provider, expected)
}

pub fn check_illegal(on_expr: &str, ill_expr: &str, expected: bool) {
    check_illegal_with_widths(on_expr, ill_expr, &[], expected);
}

pub fn check_illegal_with_widths(
    on_expr: &str,
    ill_expr: &str,
    widths: &[(&str, u32)],
    expected: bool,
) {
    let upf = format!(
        r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {{power VDD}} -function {{ground VSS}}
        create_power_domain PD -include_scope -supply {{primary SS}}
        add_power_state SS -state {{ON -supply_expr {{power == {{FULL_ON 0.9}} && ground == {{FULL_ON 0.0}}}} -logic_expr {{{on_expr}}} -simstate NORMAL}}
        add_power_state SS -state {{ILL -illegal -logic_expr {{{ill_expr}}}}}
    "#
    );
    let res = parse_upf_and_resolve(&upf, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat = if widths.is_empty() {
        PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV)
    } else {
        let mut provider = MapWidthProvider::new();
        for &(name, width) in widths {
            provider.insert(name, width);
        }
        PstSat::build_with_width_provider(&res, DEFAULT_VOLTAGE_TOL_UV, &provider)
    };
    assert!(
        !sat.diagnostics()
            .iter()
            .any(|d| d.rule_id == "UPF_EXPR_SYNTAX"),
        "unexpected syntax diag for ON=`{on_expr}` ILL=`{ill_expr}`: {:?}",
        sat.diagnostics()
    );
    assert_eq!(
        sat.is_state_space_consistent(),
        expected,
        "unexpected SAT result for ON=`{on_expr}` ILL=`{ill_expr}`"
    );
}
