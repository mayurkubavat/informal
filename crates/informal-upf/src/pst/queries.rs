//! Symbolic PST queries, voltage relation analysis, state enumeration, witness
//! extraction, and `set_equivalent` coherence verification.

use std::collections::{BTreeMap, BTreeSet};

use informal_sat::{Answer, Sat};

use crate::ir::{DiagSeverity, SupplySetId, UpfDiag, UpfLoc, VoltageSpec};
use crate::resolve::ResolvedPowerIntent;

use super::cnf::CnfBuilder;
use super::{LsRequirement, PstSat, PstWitness, StateEntry};

impl PstSat {
    /// True if `src` can be in a combinational-corrupting state while `dst` is `NORMAL`.
    pub fn can_be_off_while_on(
        &mut self,
        src: SupplySetId,
        dst: SupplySetId,
    ) -> Option<PstWitness> {
        let s = src.0 as usize;
        let d = dst.0 as usize;
        if s >= self.ss_entries.len() || d >= self.ss_entries.len() {
            return None;
        }
        if self.func_equiv_class[s] == self.func_equiv_class[d] || !self.consistent {
            return None;
        }
        let ans = self
            .cnf
            .sat
            .solve(&[self.lit_corrupt_comb[s], self.lit_normal[d]]);
        if ans == Answer::Sat {
            Some(self.extract_witness())
        } else {
            None
        }
    }

    /// True if `sup` can be in a combinational-corrupting state while `other` is also
    /// in a combinational-corrupting state.
    pub fn can_be_off_while_off(
        &mut self,
        sup: SupplySetId,
        other: SupplySetId,
    ) -> Option<PstWitness> {
        let s = sup.0 as usize;
        let o = other.0 as usize;
        if s >= self.lit_corrupt_comb.len() || o >= self.lit_corrupt_comb.len() || !self.consistent
        {
            return None;
        }
        if self
            .cnf
            .sat
            .solve(&[self.lit_corrupt_comb[s], self.lit_corrupt_comb[o]])
            == Answer::Sat
        {
            Some(self.extract_witness())
        } else {
            None
        }
    }

    /// True if `sup` can be in any non-`NORMAL` state (combinational or sequential
    /// corruption, including `CORRUPT_STATE_ON_ACTIVITY` and `CORRUPT_STATE_ON_CHANGE`)
    /// while `other` is also in a non-`NORMAL` state.
    pub fn can_be_non_normal_while_off(
        &mut self,
        sup: SupplySetId,
        other: SupplySetId,
    ) -> Option<PstWitness> {
        let s = sup.0 as usize;
        let o = other.0 as usize;
        if s >= self.lit_normal.len() || o >= self.lit_normal.len() || !self.consistent {
            return None;
        }
        if self
            .cnf
            .sat
            .solve(&[-self.lit_normal[s], -self.lit_normal[o]])
            == Answer::Sat
        {
            Some(self.extract_witness())
        } else {
            None
        }
    }

    /// True if `src` can be in any non-`NORMAL` state (combinational or sequential
    /// corruption) while `dst` is `NORMAL`.
    pub fn can_corrupt_state_while_on(
        &mut self,
        src: SupplySetId,
        dst: SupplySetId,
    ) -> Option<PstWitness> {
        let s = src.0 as usize;
        let d = dst.0 as usize;
        if s >= self.ss_entries.len() || d >= self.ss_entries.len() {
            return None;
        }
        if self.func_equiv_class[s] == self.func_equiv_class[d] || !self.consistent {
            return None;
        }
        if self
            .cnf
            .sat
            .solve(&[-self.lit_normal[s], self.lit_normal[d]])
            == Answer::Sat
        {
            Some(self.extract_witness())
        } else {
            None
        }
    }

    /// Determines whether `src` and `dst` can simultaneously be `NORMAL` at
    /// voltages differing by more than `tol_uv`.
    pub fn voltage_relation(
        &mut self,
        src: SupplySetId,
        dst: SupplySetId,
        tol_uv: i64,
    ) -> LsRequirement {
        let s = src.0 as usize;
        let d = dst.0 as usize;
        if s >= self.ss_entries.len() || d >= self.ss_entries.len() {
            return LsRequirement::None;
        }
        if self.equiv_class[s] == self.equiv_class[d] || !self.consistent {
            return LsRequirement::None;
        }
        if self.unconstrained.contains(&src) || self.unconstrained.contains(&dst) {
            return LsRequirement::Undetermined {
                reason: "unconstrained supply set (missing port/power states)".to_string(),
            };
        }

        let mut hl_witness: Option<PstWitness> = None;
        let mut lh_witness: Option<PstWitness> = None;
        let mut saw_undetermined = false;

        let s_states: Vec<StateEntry> = self.ss_entries[s]
            .iter()
            .filter(|e| e.simstate.is_normal())
            .cloned()
            .collect();
        let d_states: Vec<StateEntry> = self.ss_entries[d]
            .iter()
            .filter(|e| e.simstate.is_normal())
            .cloned()
            .collect();

        for se in &s_states {
            for de in &d_states {
                for &(sp_v, sp_lit) in &se.power_vs {
                    for &(dp_v, dp_lit) in &de.power_vs {
                        for &(sg_v, sg_lit) in &se.ground_vs {
                            for &(dg_v, dg_lit) in &de.ground_vs {
                                let mut assumptions = Vec::with_capacity(6);
                                assumptions.push(se.var);
                                assumptions.push(de.var);
                                if let Some(l) = sp_lit {
                                    assumptions.push(l);
                                }
                                if let Some(l) = dp_lit {
                                    assumptions.push(l);
                                }
                                if let Some(l) = sg_lit {
                                    assumptions.push(l);
                                }
                                if let Some(l) = dg_lit {
                                    assumptions.push(l);
                                }

                                let (Some((slo, shi)), Some((dlo, dhi))) =
                                    (sp_v.bounds_uv(), dp_v.bounds_uv())
                                else {
                                    if !saw_undetermined
                                        && self.cnf.sat.solve(&assumptions) == Answer::Sat
                                    {
                                        saw_undetermined = true;
                                    }
                                    continue;
                                };
                                let gnd_diff = match (sg_v.bounds_uv(), dg_v.bounds_uv()) {
                                    (Some((gs_lo, gs_hi)), Some((gd_lo, gd_hi))) => {
                                        gs_hi > gd_lo + tol_uv || gd_hi > gs_lo + tol_uv
                                    }
                                    _ => false,
                                };
                                let need_hl = shi > dlo + tol_uv || gnd_diff;
                                let need_lh = dhi > slo + tol_uv || gnd_diff;
                                if !need_hl && !need_lh {
                                    continue;
                                }
                                if ((need_hl && hl_witness.is_none())
                                    || (need_lh && lh_witness.is_none()))
                                    && self.cnf.sat.solve(&assumptions) == Answer::Sat
                                {
                                    let w = self.extract_witness();
                                    if need_hl && hl_witness.is_none() {
                                        hl_witness = Some(w.clone());
                                    }
                                    if need_lh && lh_witness.is_none() {
                                        lh_witness = Some(w);
                                    }
                                }
                            }
                        }
                    }
                }
                if hl_witness.is_some() && lh_witness.is_some() {
                    break;
                }
            }
            if hl_witness.is_some() && lh_witness.is_some() {
                break;
            }
        }

        match (hl_witness, lh_witness) {
            (Some(hl_witness), Some(lh_witness)) => LsRequirement::Both {
                hl_witness,
                lh_witness,
            },
            (Some(witness), None) => LsRequirement::HighToLow { witness },
            (None, Some(witness)) => LsRequirement::LowToHigh { witness },
            (None, None) if saw_undetermined => LsRequirement::Undetermined {
                reason: "NORMAL state with UNDETERMINED voltage".to_string(),
            },
            (None, None) => LsRequirement::None,
        }
    }

    /// Verifies that `sup` is `NORMAL` in every legal state where `ref_sup` is `NORMAL`.
    pub fn is_supply_always_on_when(
        &mut self,
        sup: SupplySetId,
        ref_sup: SupplySetId,
    ) -> Result<(), PstWitness> {
        let s = sup.0 as usize;
        let r = ref_sup.0 as usize;
        if s >= self.lit_normal.len() || r >= self.lit_normal.len() || !self.consistent {
            return Ok(());
        }
        if self
            .cnf
            .sat
            .solve(&[-self.lit_normal[s], self.lit_normal[r]])
            == Answer::Sat
        {
            Err(self.extract_witness())
        } else {
            Ok(())
        }
    }

    /// Verifies that `sup` is `NORMAL` in every legal state where `ref_sup` can
    /// corrupt combinationally (`lit_corrupt_comb`).
    pub fn is_supply_always_on_when_corrupting(
        &mut self,
        sup: SupplySetId,
        ref_sup: SupplySetId,
    ) -> Result<(), PstWitness> {
        let s = sup.0 as usize;
        let r = ref_sup.0 as usize;
        if s >= self.lit_normal.len() || r >= self.lit_corrupt_comb.len() || !self.consistent {
            return Ok(());
        }
        if self
            .cnf
            .sat
            .solve(&[-self.lit_normal[s], self.lit_corrupt_comb[r]])
            == Answer::Sat
        {
            Err(self.extract_witness())
        } else {
            Ok(())
        }
    }

    /// Verifies that `iso_sup` is `NORMAL` whenever `src` corrupts AND `dst` is `NORMAL`.
    pub fn is_supply_always_on_when_crossing(
        &mut self,
        iso_sup: SupplySetId,
        src: SupplySetId,
        dst: SupplySetId,
    ) -> Result<(), PstWitness> {
        let i = iso_sup.0 as usize;
        let s = src.0 as usize;
        let d = dst.0 as usize;
        if i >= self.lit_normal.len()
            || s >= self.lit_corrupt_comb.len()
            || d >= self.lit_normal.len()
            || !self.consistent
        {
            return Ok(());
        }
        if self.cnf.sat.solve(&[
            -self.lit_normal[i],
            self.lit_corrupt_comb[s],
            self.lit_normal[d],
        ]) == Answer::Sat
        {
            Err(self.extract_witness())
        } else {
            Ok(())
        }
    }

    /// Enumerates up to `limit` distinct legal symbolic power-state assignments
    /// on a clean temporary solver clone (zero mutation of primary solver).
    pub fn enumerate_symbolic_states(&self, limit: usize) -> Vec<PstWitness> {
        if !self.consistent || limit == 0 {
            return Vec::new();
        }
        let mut temp = self.cnf.clone_solver();
        let mut out = Vec::new();
        while out.len() < limit && temp.solve(&[]) == Answer::Sat {
            let w = self.extract_witness_from(temp.as_ref());
            let mut block = Vec::with_capacity(self.ss_entries.len());
            for entries in &self.ss_entries {
                if let Some(chosen) = entries.iter().find(|e| temp.value(e.var)) {
                    block.push(-chosen.var);
                }
            }
            out.push(w);
            if block.is_empty() {
                break;
            }
            temp.add_clause(&block);
        }
        out
    }

    pub(crate) fn check_equivalence_coherence(&mut self, resolved: &ResolvedPowerIntent) {
        let mut reported: BTreeSet<(usize, usize)> = BTreeSet::new();
        for (group, loc) in &resolved.explicit_equiv_groups {
            self.check_equiv_group(group, loc, false, &mut reported);
        }
        for (group, loc) in &resolved.function_only_equiv_groups {
            self.check_equiv_group(group, loc, true, &mut reported);
        }
    }

    fn check_equiv_group(
        &mut self,
        group: &[SupplySetId],
        loc: &Option<UpfLoc>,
        function_only: bool,
        reported: &mut BTreeSet<(usize, usize)>,
    ) {
        for a_idx in 0..group.len() {
            for b_idx in (a_idx + 1)..group.len() {
                let sa = group[a_idx];
                let sb = group[b_idx];
                if self.unconstrained.contains(&sa) || self.unconstrained.contains(&sb) {
                    continue;
                }
                let pair_key = (sa.0.min(sb.0) as usize, sa.0.max(sb.0) as usize);
                if reported.contains(&pair_key) {
                    continue;
                }
                let a_entries = self.ss_entries[sa.0 as usize].clone();
                let b_entries = self.ss_entries[sb.0 as usize].clone();
                'pair_loop: for ea in &a_entries {
                    for eb in &b_entries {
                        if let Some((va, vb)) = states_equiv_incompatible(
                            ea,
                            eb,
                            self.tol_uv,
                            function_only,
                            &mut self.cnf,
                        ) {
                            reported.insert(pair_key);
                            self.diagnostics.push(UpfDiag {
                                rule_id: "UPF_EQUIV_MISMATCH".to_string(),
                                severity: DiagSeverity::Error,
                                message: format!(
                                    "set_equivalent mismatch between `{}` (state `{}` {:?}) and `{}` (state `{}` {:?})",
                                    self.ss_names[sa.0 as usize],
                                    ea.state_name,
                                    va,
                                    self.ss_names[sb.0 as usize],
                                    eb.state_name,
                                    vb
                                ),
                                loc: loc.clone(),
                            });
                            break 'pair_loop;
                        }
                    }
                }
            }
        }
    }

    fn extract_witness(&self) -> PstWitness {
        self.extract_witness_from(self.cnf.sat.as_ref())
    }

    fn extract_witness_from(&self, sat: &dyn Sat) -> PstWitness {
        let mut supply_states = BTreeMap::new();
        for (idx, entries) in self.ss_entries.iter().enumerate() {
            if let Some(chosen) = entries.iter().find(|e| sat.value(e.var)) {
                supply_states.insert(self.ss_names[idx].clone(), chosen.state_name.clone());
            }
        }
        let mut logic_signals = BTreeMap::new();
        for (name, bits) in &self.signal_bits {
            if bits.len() == 1 {
                logic_signals.insert(name.clone(), sat.value(bits[0]));
            } else {
                for (bit_idx, &lit) in bits.iter().enumerate() {
                    logic_signals.insert(format!("{name}[{bit_idx}]"), sat.value(lit));
                }
            }
        }
        PstWitness {
            supply_states,
            logic_signals,
        }
    }
}

fn states_equiv_incompatible(
    a: &StateEntry,
    b: &StateEntry,
    tol_uv: i64,
    function_only: bool,
    cnf: &mut CnfBuilder,
) -> Option<(VoltageSpec, VoltageSpec)> {
    if a.simstate.is_normal() != b.simstate.is_normal() {
        if cnf.sat.solve(&[a.var, b.var]) == Answer::Sat {
            return Some((a.power_v, b.power_v));
        }
        return None;
    }
    if function_only || !a.simstate.is_normal() {
        return None;
    }
    for &(ap, ap_lit) in &a.power_vs {
        for &(bp, bp_lit) in &b.power_vs {
            if voltages_conflict(ap, bp, tol_uv) {
                let mut assump = vec![a.var, b.var];
                if let Some(l) = ap_lit {
                    assump.push(l);
                }
                if let Some(l) = bp_lit {
                    assump.push(l);
                }
                if cnf.sat.solve(&assump) == Answer::Sat {
                    return Some((ap, bp));
                }
            }
        }
    }
    for &(ag, ag_lit) in &a.ground_vs {
        for &(bg, bg_lit) in &b.ground_vs {
            if voltages_conflict(ag, bg, tol_uv) {
                let mut assump = vec![a.var, b.var];
                if let Some(l) = ag_lit {
                    assump.push(l);
                }
                if let Some(l) = bg_lit {
                    assump.push(l);
                }
                if cnf.sat.solve(&assump) == Answer::Sat {
                    return Some((ag, bg));
                }
            }
        }
    }
    None
}

fn voltages_conflict(a: VoltageSpec, b: VoltageSpec, tol_uv: i64) -> bool {
    match (a.bounds_uv(), b.bounds_uv()) {
        (Some((alo, ahi)), Some((blo, bhi))) => alo > bhi + tol_uv || blo > ahi + tol_uv,
        _ => false,
    }
}
