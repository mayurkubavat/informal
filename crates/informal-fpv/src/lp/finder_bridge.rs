//! Live `ObjectFinder` and `SignalWidthProvider` bridge over
//! `informal_svlang::StructuralGraph` and `svlang::ElaboratedDesign`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use informal_svlang::struct_graph::{normalize_query_path, normalize_sv_leaf};
use informal_svlang::{CompiledDesign, PortDir};
use informal_upf::{glob, ObjectFinder, ObjectType, SignalWidthProvider, SupplyDirection};

/// Pre-indexed `Send + Sync + 'static` bridge from elaborated RTL to `informal-upf`.
#[derive(Debug, Clone, Default)]
pub struct DesignFinder {
    pub has_rtl: bool,
    pub top: String,
    pub scopes: BTreeSet<String>,
    pub insts: Vec<String>,
    pub mods: Vec<String>,
    pub ports: Vec<(String, Option<SupplyDirection>)>,
    pub pins: Vec<(String, Option<SupplyDirection>)>,
    pub nets: Vec<String>,
    pub regs: Vec<String>,
    pub signal_widths: BTreeMap<String, u32>,
    pub signal_bounds: BTreeMap<String, (u32, Option<(i128, i128)>)>,
}

impl DesignFinder {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_compiled(compiled: Option<&Arc<CompiledDesign>>, top: Option<&str>) -> Self {
        let Some(comp) = compiled else {
            return Self::empty();
        };
        let graph = comp.struct_graph();
        let resolved_top = top
            .map(normalize_sv_leaf)
            .or_else(|| graph.top_names.first().cloned())
            .unwrap_or_default();

        let mut f = Self {
            has_rtl: true,
            top: resolved_top.clone(),
            ..Self::default()
        };
        f.scopes.insert(String::new());
        if !resolved_top.is_empty() {
            f.scopes.insert(resolved_top.clone());
        }

        let mut inst_set = BTreeSet::new();
        let mut mod_set = BTreeSet::new();
        let mut net_set = BTreeSet::new();
        let mut reg_set = BTreeSet::new();

        for cell in &graph.cells {
            if !cell.upf_path.is_empty() {
                f.scopes.insert(cell.upf_path.clone());
                inst_set.insert(cell.upf_path.clone());
            }
            if !cell.module.is_empty() {
                mod_set.insert(cell.module.clone());
            }
            for fl in &cell.flops {
                if !fl.upf_path.is_empty() {
                    reg_set.insert(fl.upf_path.clone());
                }
            }
            for la in &cell.latches {
                if !la.upf_path.is_empty() {
                    reg_set.insert(la.upf_path.clone());
                }
            }
        }

        for pin in &graph.pins {
            let dir = pin.dir.and_then(port_dir_to_supply);
            if !pin.upf_path.is_empty() {
                let cell = graph.cell(pin.cell);
                if cell.is_top {
                    f.ports.push((pin.upf_path.clone(), dir));
                } else {
                    f.pins.push((pin.upf_path.clone(), dir));
                    // Expose child instance ports as `Port` too when UPF targets `u_sub/clk`.
                    f.ports.push((pin.upf_path.clone(), dir));
                }
                let pw = pin.width.max(1);
                f.signal_bounds
                    .entry(pin.upf_path.clone())
                    .or_insert((pw, pin.packed_bounds));
                if !f.top.is_empty() {
                    f.signal_bounds
                        .entry(format!("{}/{}", f.top, pin.upf_path))
                        .or_insert((pw, pin.packed_bounds));
                }
            }
        }

        for net in &graph.nets {
            if !net.upf_path.is_empty() {
                net_set.insert(net.upf_path.clone());
                if net.is_seq_state {
                    reg_set.insert(net.upf_path.clone());
                }
                let w = net.width.max(1);
                f.insert_width_aliases(&net.upf_path, &net.full_path, w, net.packed_bounds);
            }
        }

        f.insts = inst_set.into_iter().collect();
        f.mods = mod_set.into_iter().collect();
        f.nets = net_set.into_iter().collect();
        f.regs = reg_set.into_iter().collect();
        f.ports.sort_by(|a, b| a.0.cmp(&b.0));
        f.ports.dedup();
        f.pins.sort_by(|a, b| a.0.cmp(&b.0));
        f.pins.dedup();
        f
    }

    fn insert_width_aliases(
        &mut self,
        upf_path: &str,
        full_dot: &str,
        w: u32,
        packed_bounds: Option<(i128, i128)>,
    ) {
        self.signal_widths.insert(upf_path.to_string(), w);
        self.signal_widths.insert(full_dot.to_string(), w);
        self.signal_bounds
            .insert(upf_path.to_string(), (w, packed_bounds));
        self.signal_bounds
            .insert(full_dot.to_string(), (w, packed_bounds));
        if !self.top.is_empty() {
            let prefixed = format!("{}/{upf_path}", self.top);
            self.signal_widths.insert(prefixed.clone(), w);
            self.signal_bounds.insert(prefixed, (w, packed_bounds));
        }
        let dot_rel = upf_path.replace('/', ".");
        self.signal_widths.entry(dot_rel.clone()).or_insert(w);
        self.signal_bounds
            .entry(dot_rel)
            .or_insert((w, packed_bounds));
    }

    fn normalize_scope_arg(&self, scope: &str) -> String {
        let norm = normalize_query_path(scope);
        if norm == self.top {
            String::new()
        } else if let Some(rest) = norm.strip_prefix(&format!("{}/", self.top)) {
            rest.to_string()
        } else {
            norm
        }
    }

    fn match_pool(
        &self,
        items: &[String],
        scope: &str,
        patterns: &[String],
        transitive: bool,
        out: &mut BTreeSet<String>,
    ) {
        let scope_norm = self.normalize_scope_arg(scope);
        let norm_pats: Vec<String> = patterns.iter().map(|p| normalize_query_path(p)).collect();

        for item in items {
            let rel = if scope_norm.is_empty() {
                item.as_str()
            } else if let Some(rest) = item.strip_prefix(&format!("{scope_norm}/")) {
                rest
            } else {
                continue;
            };
            if !transitive && rel.contains('/') {
                continue;
            }
            let leaf = rel.rsplit('/').next().unwrap_or(rel);
            let matched = patterns.iter().zip(&norm_pats).any(|(raw_p, norm_p)| {
                glob(raw_p, item)
                    || glob(raw_p, rel)
                    || glob(raw_p, leaf)
                    || glob(norm_p, item)
                    || glob(norm_p, rel)
                    || glob(norm_p, leaf)
            });
            if matched {
                out.insert(item.clone());
            }
        }
    }
}

fn port_dir_to_supply(dir: PortDir) -> Option<SupplyDirection> {
    match dir {
        PortDir::Input => Some(SupplyDirection::In),
        PortDir::Output => Some(SupplyDirection::Out),
        PortDir::Inout => Some(SupplyDirection::InOut),
        PortDir::Ref => None,
    }
}

impl ObjectFinder for DesignFinder {
    fn has_design(&self) -> bool {
        self.has_rtl
    }

    fn scope_exists(&self, scope_path: &str) -> bool {
        if !self.has_rtl {
            return false;
        }
        let norm = self.normalize_scope_arg(scope_path);
        self.scopes.contains(&norm)
    }

    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String> {
        self.find_objects_with_direction(scope_path, patterns, obj_type, transitive, None)
    }

    fn find_objects_with_direction(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
        direction: Option<SupplyDirection>,
    ) -> Vec<String> {
        if !self.has_rtl {
            return Vec::new();
        }
        let mut out = BTreeSet::new();
        match obj_type {
            Some(ObjectType::Inst | ObjectType::Cell) => {
                self.match_pool(&self.insts, scope_path, patterns, transitive, &mut out);
            }
            Some(ObjectType::Mod) => {
                self.match_pool(&self.mods, scope_path, patterns, transitive, &mut out);
            }
            Some(ObjectType::Port) => {
                let filtered: Vec<String> = self
                    .ports
                    .iter()
                    .filter(|(_, d)| direction.is_none() || *d == direction)
                    .map(|(n, _)| n.clone())
                    .collect();
                self.match_pool(&filtered, scope_path, patterns, transitive, &mut out);
            }
            Some(ObjectType::Pin) => {
                let filtered: Vec<String> = self
                    .pins
                    .iter()
                    .filter(|(_, d)| direction.is_none() || *d == direction)
                    .map(|(n, _)| n.clone())
                    .collect();
                self.match_pool(&filtered, scope_path, patterns, transitive, &mut out);
            }
            Some(ObjectType::Net) => {
                self.match_pool(&self.nets, scope_path, patterns, transitive, &mut out);
            }
            Some(ObjectType::Reg) => {
                self.match_pool(&self.regs, scope_path, patterns, transitive, &mut out);
            }
            None => {
                // `validate_strategy_elements` queries with `obj_type == None` for instances, ports, nets, regs, and pins.
                self.match_pool(&self.insts, scope_path, patterns, transitive, &mut out);
                let ports: Vec<String> = self
                    .ports
                    .iter()
                    .filter(|(_, d)| direction.is_none() || *d == direction)
                    .map(|(n, _)| n.clone())
                    .collect();
                self.match_pool(&ports, scope_path, patterns, transitive, &mut out);
                self.match_pool(&self.nets, scope_path, patterns, transitive, &mut out);
                self.match_pool(&self.regs, scope_path, patterns, transitive, &mut out);
                let pins: Vec<String> = self
                    .pins
                    .iter()
                    .filter(|(_, d)| direction.is_none() || *d == direction)
                    .map(|(n, _)| n.clone())
                    .collect();
                self.match_pool(&pins, scope_path, patterns, transitive, &mut out);
            }
            Some(
                ObjectType::SupplyPort
                | ObjectType::SupplyNet
                | ObjectType::SupplySet
                | ObjectType::PowerDomain,
            ) => {}
        }
        out.into_iter().collect()
    }

    fn signal_slice_exists(&self, base_path: &str, msb: i128, lsb: i128) -> bool {
        if !self.has_rtl {
            return false;
        }
        let lookup = |k: &str| self.signal_bounds.get(k).copied();
        let entry = lookup(base_path).or_else(|| {
            let norm = normalize_query_path(base_path);
            lookup(&norm).or_else(|| {
                (!self.top.is_empty())
                    .then(|| norm.strip_prefix(&format!("{}/", self.top)))
                    .flatten()
                    .and_then(lookup)
            })
        });
        let Some((width, packed_bounds)) = entry else {
            return false;
        };
        if width == 0 {
            return false;
        }
        let (lo_b, hi_b) = match packed_bounds {
            Some((l, r)) => (l.min(r), l.max(r)),
            None => (0, (width.saturating_sub(1)) as i128),
        };
        msb.min(lsb) >= lo_b && msb.max(lsb) <= hi_b
    }
}

impl SignalWidthProvider for DesignFinder {
    fn width_of(&self, sig: &str) -> Option<u32> {
        if let Some(&w) = self.signal_widths.get(sig) {
            return Some(w);
        }
        let norm = normalize_query_path(sig);
        if let Some(&w) = self.signal_widths.get(&norm) {
            return Some(w);
        }
        if !self.top.is_empty() {
            if let Some(stripped) = norm.strip_prefix(&format!("{}/", self.top)) {
                if let Some(&w) = self.signal_widths.get(stripped) {
                    return Some(w);
                }
            }
        }
        None
    }
}
