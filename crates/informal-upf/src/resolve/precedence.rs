//! IEEE 1801 4-tier strategy precedence matching and same-tier overlap checks.

use std::collections::BTreeMap;

use crate::ir::{
    escape_rtl_brackets, escape_sv_escaped_backslashes_for_glob, glob,
    has_unescaped_glob_wildcards, strip_sv_escapes_in_glob, strip_sv_escapes_in_path,
    unescape_rtl_brackets, AppliesTo, DiagSeverity, IsolationStrategy, LevelShifterStrategy,
    PowerDomainId, PowerIntent, RetentionStrategy, UpfDiag,
};

use super::membership::matches_any_element_in_scope;
use super::refinement::same_domain_ref;
use super::supply_graph::{lookup_scoped, resolve_ss_handle};
use super::{ResolvedPowerIntent, StrategyMatch};

/// Bit-slice-aware endpoint descriptor for crossing strategy matching.
#[derive(Debug, Clone)]
pub struct SignalEndpoint<'a> {
    pub upf_path: &'a str,
    pub full_path: &'a str,
    pub esc_upf_path: Option<&'a str>,
    pub width: u32,
    pub packed_bounds: Option<(i128, i128)>,
    /// Inclusive 0-based physical LSB-0 bit ranges `(lo, hi)`.
    pub ranges: &'a [(u32, u32)],
}

struct TierSpec<'a> {
    elements: &'a [String],
    source_filter: Option<&'a str>,
    sink_filter: Option<&'a str>,
    scope: &'a str,
}

impl ResolvedPowerIntent {
    /// IEEE 1801 4-tier precedence lookup for isolation strategies on a crossing
    /// `signal_path` from `src_domain` to `dst_domain`.
    pub fn match_isolation(
        &self,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        signal_path: &str,
        direction: AppliesTo,
    ) -> StrategyMatch<&IsolationStrategy> {
        let mut best: Option<(u8, u32, &IsolationStrategy)> = None;
        for iso in &self.intent.isolation_strategies {
            let Some(pd_id) = lookup_scoped(&self.domain_by_name, &iso.domain, &iso.scope) else {
                continue;
            };
            if !domain_matches_direction(
                pd_id,
                src_domain,
                dst_domain,
                iso.applies_to,
                AppliesTo::Outputs,
                direction,
            ) {
                continue;
            }
            if matches_exclude_element_in_scope(&iso.exclude_elements, &iso.scope, signal_path) {
                continue;
            }
            if iso.diff_supply_only {
                if let (Some(s_pd), Some(d_pd)) = (src_domain, dst_domain) {
                    if let (Some(s_ss), Some(d_ss)) = (self.primary_ss(s_pd), self.primary_ss(d_pd))
                    {
                        if self.same_supply_class(s_ss, d_ss) {
                            continue;
                        }
                    }
                }
            }

            let tier = match self.compute_tier(
                TierSpec {
                    elements: &iso.elements,
                    source_filter: iso.source_filter.as_deref(),
                    sink_filter: iso.sink_filter.as_deref(),
                    scope: &iso.scope,
                },
                src_domain,
                dst_domain,
                signal_path,
            ) {
                Some(t) => t,
                None => continue,
            };

            let better = match best {
                None => true,
                Some((bt, bp, _)) => tier > bt || (tier == bt && iso.precedence_order > bp),
            };
            if better {
                best = Some((tier, iso.precedence_order, iso));
            }
        }
        match best {
            Some((_, _, iso)) if iso.no_isolation => StrategyMatch::Suppressed(iso),
            Some((_, _, iso)) => StrategyMatch::Matched(iso),
            None => StrategyMatch::None,
        }
    }

    /// IEEE 1801 4-tier precedence lookup for level-shifter strategies.
    pub fn match_level_shifter(
        &self,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        signal_path: &str,
        direction: AppliesTo,
    ) -> StrategyMatch<&LevelShifterStrategy> {
        let mut best: Option<(u8, u32, &LevelShifterStrategy)> = None;
        for ls in &self.intent.level_shifter_strategies {
            let Some(pd_id) = lookup_scoped(&self.domain_by_name, &ls.domain, &ls.scope) else {
                continue;
            };
            if !domain_matches_direction(
                pd_id,
                src_domain,
                dst_domain,
                ls.applies_to,
                AppliesTo::Both,
                direction,
            ) {
                continue;
            }
            if matches_exclude_element_in_scope(&ls.exclude_elements, &ls.scope, signal_path) {
                continue;
            }
            let tier = match self.compute_tier(
                TierSpec {
                    elements: &ls.elements,
                    source_filter: ls.source_filter.as_deref(),
                    sink_filter: ls.sink_filter.as_deref(),
                    scope: &ls.scope,
                },
                src_domain,
                dst_domain,
                signal_path,
            ) {
                Some(t) => t,
                None => continue,
            };
            let better = match best {
                None => true,
                Some((bt, bp, _)) => tier > bt || (tier == bt && ls.precedence_order > bp),
            };
            if better {
                best = Some((tier, ls.precedence_order, ls));
            }
        }
        match best {
            Some((_, _, ls)) if ls.no_shift => StrategyMatch::Suppressed(ls),
            Some((_, _, ls)) => StrategyMatch::Matched(ls),
            None => StrategyMatch::None,
        }
    }

    /// IEEE 1801 precedence lookup for retention strategies on register `reg_path`.
    pub fn match_retention(
        &self,
        domain: PowerDomainId,
        reg_path: &str,
    ) -> StrategyMatch<&RetentionStrategy> {
        let mut best: Option<(u8, u32, &RetentionStrategy)> = None;
        for ret in &self.intent.retention_strategies {
            let Some(pd_id) = lookup_scoped(&self.domain_by_name, &ret.domain, &ret.scope) else {
                continue;
            };
            if pd_id != domain {
                continue;
            }
            if matches_exclude_element_in_scope(&ret.exclude_elements, &ret.scope, reg_path) {
                continue;
            }
            let tier = if !ret.elements.is_empty() {
                if matches_any_element_in_scope(&ret.elements, &ret.scope, reg_path) {
                    4
                } else {
                    continue;
                }
            } else {
                1
            };
            let better = match best {
                None => true,
                Some((bt, bp, _)) => tier > bt || (tier == bt && ret.precedence_order > bp),
            };
            if better {
                best = Some((tier, ret.precedence_order, ret));
            }
        }
        match best {
            Some((_, _, ret)) if ret.no_retention => StrategyMatch::Suppressed(ret),
            Some((_, _, ret)) => StrategyMatch::Matched(ret),
            None => StrategyMatch::None,
        }
    }

    /// Bit-slice-aware IEEE 1801 4-tier precedence lookup for isolation strategies
    /// across a crossing's `src` and `dst` endpoints. Excludes any strategy whose
    /// `exclude_elements` covers/overlaps any `src` or `dst` endpoint range.
    pub fn match_isolation_crossing(
        &self,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        src_eps: &[SignalEndpoint<'_>],
        dst_eps: &[SignalEndpoint<'_>],
        query_dir: AppliesTo,
    ) -> StrategyMatch<&IsolationStrategy> {
        let mut best: Option<(u8, u32, &IsolationStrategy)> = None;
        for iso in &self.intent.isolation_strategies {
            let Some(pd_id) = lookup_scoped(&self.domain_by_name, &iso.domain, &iso.scope) else {
                continue;
            };
            let eff = iso.applies_to.unwrap_or(AppliesTo::Outputs);
            let match_out = matches!(query_dir, AppliesTo::Outputs | AppliesTo::Both)
                && matches!(eff, AppliesTo::Outputs | AppliesTo::Both)
                && src_domain == Some(pd_id);
            let match_in = matches!(query_dir, AppliesTo::Inputs | AppliesTo::Both)
                && matches!(eff, AppliesTo::Inputs | AppliesTo::Both)
                && dst_domain == Some(pd_id);
            if !match_out && !match_in {
                continue;
            }
            if src_eps
                .iter()
                .chain(dst_eps.iter())
                .any(|ep| endpoint_matches_patterns(ep, &iso.scope, &iso.exclude_elements, true))
            {
                continue;
            }
            if iso.diff_supply_only {
                if let (Some(s_pd), Some(d_pd)) = (src_domain, dst_domain) {
                    if let (Some(s_ss), Some(d_ss)) = (self.primary_ss(s_pd), self.primary_ss(d_pd))
                    {
                        if self.same_supply_class(s_ss, d_ss) {
                            continue;
                        }
                    }
                }
            }
            let tier = match self.compute_crossing_tier(
                TierSpec {
                    elements: &iso.elements,
                    source_filter: iso.source_filter.as_deref(),
                    sink_filter: iso.sink_filter.as_deref(),
                    scope: &iso.scope,
                },
                src_domain,
                dst_domain,
                if match_out { src_eps } else { &[] },
                if match_in { dst_eps } else { &[] },
            ) {
                Some(t) => t,
                None => continue,
            };
            let better = match best {
                None => true,
                Some((bt, bp, _)) => tier > bt || (tier == bt && iso.precedence_order > bp),
            };
            if better {
                best = Some((tier, iso.precedence_order, iso));
            }
        }
        match best {
            Some((_, _, iso)) if iso.no_isolation => StrategyMatch::Suppressed(iso),
            Some((_, _, iso)) => StrategyMatch::Matched(iso),
            None => StrategyMatch::None,
        }
    }

    /// Bit-slice-aware IEEE 1801 4-tier precedence lookup for level-shifter strategies
    /// across a crossing's `src` and `dst` endpoints. Excludes any strategy whose
    /// `exclude_elements` covers/overlaps any `src` or `dst` endpoint range.
    pub fn match_level_shifter_crossing(
        &self,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        src_eps: &[SignalEndpoint<'_>],
        dst_eps: &[SignalEndpoint<'_>],
        query_dir: AppliesTo,
    ) -> StrategyMatch<&LevelShifterStrategy> {
        let mut best: Option<(u8, u32, &LevelShifterStrategy)> = None;
        for ls in &self.intent.level_shifter_strategies {
            let Some(pd_id) = lookup_scoped(&self.domain_by_name, &ls.domain, &ls.scope) else {
                continue;
            };
            let eff = ls.applies_to.unwrap_or(AppliesTo::Both);
            let match_out = matches!(query_dir, AppliesTo::Outputs | AppliesTo::Both)
                && matches!(eff, AppliesTo::Outputs | AppliesTo::Both)
                && src_domain == Some(pd_id);
            let match_in = matches!(query_dir, AppliesTo::Inputs | AppliesTo::Both)
                && matches!(eff, AppliesTo::Inputs | AppliesTo::Both)
                && dst_domain == Some(pd_id);
            if !match_out && !match_in {
                continue;
            }
            if src_eps
                .iter()
                .chain(dst_eps.iter())
                .any(|ep| endpoint_matches_patterns(ep, &ls.scope, &ls.exclude_elements, true))
            {
                continue;
            }
            let tier = match self.compute_crossing_tier(
                TierSpec {
                    elements: &ls.elements,
                    source_filter: ls.source_filter.as_deref(),
                    sink_filter: ls.sink_filter.as_deref(),
                    scope: &ls.scope,
                },
                src_domain,
                dst_domain,
                if match_out { src_eps } else { &[] },
                if match_in { dst_eps } else { &[] },
            ) {
                Some(t) => t,
                None => continue,
            };
            let better = match best {
                None => true,
                Some((bt, bp, _)) => tier > bt || (tier == bt && ls.precedence_order > bp),
            };
            if better {
                best = Some((tier, ls.precedence_order, ls));
            }
        }
        match best {
            Some((_, _, ls)) if ls.no_shift => StrategyMatch::Suppressed(ls),
            Some((_, _, ls)) => StrategyMatch::Matched(ls),
            None => StrategyMatch::None,
        }
    }

    fn compute_crossing_tier(
        &self,
        spec: TierSpec<'_>,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        out_eps: &[SignalEndpoint<'_>],
        in_eps: &[SignalEndpoint<'_>],
    ) -> Option<u8> {
        if let Some(sf) = spec.source_filter {
            if !self.filter_matches_domain(sf, spec.scope, src_domain) {
                return None;
            }
        }
        if let Some(df) = spec.sink_filter {
            if !self.filter_matches_domain(df, spec.scope, dst_domain) {
                return None;
            }
        }
        if !spec.elements.is_empty() {
            if out_eps
                .iter()
                .chain(in_eps.iter())
                .any(|ep| endpoint_matches_patterns(ep, spec.scope, spec.elements, false))
            {
                return Some(4);
            }
            return None;
        }
        match (spec.source_filter.is_some(), spec.sink_filter.is_some()) {
            (true, true) => Some(3),
            (true, false) | (false, true) => Some(2),
            (false, false) => Some(1),
        }
    }

    fn compute_tier(
        &self,
        spec: TierSpec<'_>,
        src_domain: Option<PowerDomainId>,
        dst_domain: Option<PowerDomainId>,
        signal_path: &str,
    ) -> Option<u8> {
        if let Some(sf) = spec.source_filter {
            if !self.filter_matches_domain(sf, spec.scope, src_domain) {
                return None;
            }
        }
        if let Some(df) = spec.sink_filter {
            if !self.filter_matches_domain(df, spec.scope, dst_domain) {
                return None;
            }
        }
        if !spec.elements.is_empty() {
            if matches_any_element_in_scope(spec.elements, spec.scope, signal_path) {
                return Some(4);
            }
            return None;
        }
        match (spec.source_filter.is_some(), spec.sink_filter.is_some()) {
            (true, true) => Some(3),
            (true, false) | (false, true) => Some(2),
            (false, false) => Some(1),
        }
    }

    pub fn filter_matches_domain(
        &self,
        filter: &str,
        scope: &str,
        domain: Option<PowerDomainId>,
    ) -> bool {
        let Some(pd_id) = domain else {
            return false;
        };
        if let Some(f_pd) = lookup_scoped(&self.domain_by_name, filter, scope) {
            return f_pd == pd_id;
        }
        if let Some(f_ss) = resolve_ss_handle(
            filter,
            scope,
            &self.intent.domains,
            &self.domain_by_name,
            &self.ss_by_name,
        ) {
            if let Some(pd_ss) = self.primary_ss(pd_id) {
                return self.same_supply_class(f_ss, pd_ss);
            }
        }
        false
    }
}

fn domain_matches_direction(
    strat_pd: PowerDomainId,
    src_domain: Option<PowerDomainId>,
    dst_domain: Option<PowerDomainId>,
    applies_to: Option<AppliesTo>,
    default_applies_to: AppliesTo,
    query_dir: AppliesTo,
) -> bool {
    let eff = applies_to.unwrap_or(default_applies_to);
    match (eff, query_dir) {
        (AppliesTo::Outputs, AppliesTo::Outputs)
        | (AppliesTo::Outputs, AppliesTo::Both)
        | (AppliesTo::Both, AppliesTo::Outputs) => src_domain == Some(strat_pd),
        (AppliesTo::Inputs, AppliesTo::Inputs)
        | (AppliesTo::Inputs, AppliesTo::Both)
        | (AppliesTo::Both, AppliesTo::Inputs) => dst_domain == Some(strat_pd),
        (AppliesTo::Both, AppliesTo::Both) => {
            src_domain == Some(strat_pd) || dst_domain == Some(strat_pd)
        }
        (AppliesTo::Outputs, AppliesTo::Inputs) | (AppliesTo::Inputs, AppliesTo::Outputs) => false,
    }
}

pub(crate) fn check_strategy_overlaps(
    intent: &mut PowerIntent,
    domain_by_name: &BTreeMap<String, PowerDomainId>,
) {
    for i in 0..intent.isolation_strategies.len() {
        for j in (i + 1)..intent.isolation_strategies.len() {
            let a = &intent.isolation_strategies[i];
            let b = &intent.isolation_strategies[j];
            if !same_domain_ref(domain_by_name, &a.domain, &a.scope, &b.domain, &b.scope)
                || a.name == b.name
            {
                continue;
            }
            let same_scope_tier = a.elements == b.elements
                && a.exclude_elements == b.exclude_elements
                && a.source_filter == b.source_filter
                && a.sink_filter == b.sink_filter
                && a.applies_to.unwrap_or(AppliesTo::Outputs)
                    == b.applies_to.unwrap_or(AppliesTo::Outputs);
            if same_scope_tier
                && (a.clamp_value != b.clamp_value
                    || a.applies_to_clamp != b.applies_to_clamp
                    || a.isolation_sense != b.isolation_sense
                    || a.isolation_signal != b.isolation_signal
                    || a.no_isolation != b.no_isolation)
            {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_STRATEGY_OVERLAP".to_string(),
                    severity: DiagSeverity::Warning,
                    message: format!(
                        "isolation strategies `{}` and `{}` overlap on domain `{}` with conflicting attributes",
                        a.name, b.name, a.domain
                    ),
                    loc: b.loc.clone(),
                });
            }
        }
    }

    for i in 0..intent.level_shifter_strategies.len() {
        for j in (i + 1)..intent.level_shifter_strategies.len() {
            let a = &intent.level_shifter_strategies[i];
            let b = &intent.level_shifter_strategies[j];
            if !same_domain_ref(domain_by_name, &a.domain, &a.scope, &b.domain, &b.scope)
                || a.name == b.name
            {
                continue;
            }
            let same_scope_tier = a.elements == b.elements
                && a.exclude_elements == b.exclude_elements
                && a.source_filter == b.source_filter
                && a.sink_filter == b.sink_filter
                && a.applies_to.unwrap_or(AppliesTo::Both)
                    == b.applies_to.unwrap_or(AppliesTo::Both);
            if same_scope_tier
                && (a.rule != b.rule
                    || a.no_shift != b.no_shift
                    || a.threshold_uv != b.threshold_uv
                    || a.force_shift != b.force_shift)
            {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_STRATEGY_OVERLAP".to_string(),
                    severity: DiagSeverity::Warning,
                    message: format!(
                        "level-shifter strategies `{}` and `{}` overlap on domain `{}` with conflicting attributes",
                        a.name, b.name, a.domain
                    ),
                    loc: b.loc.clone(),
                });
            }
        }
    }

    for i in 0..intent.retention_strategies.len() {
        for j in (i + 1)..intent.retention_strategies.len() {
            let a = &intent.retention_strategies[i];
            let b = &intent.retention_strategies[j];
            if !same_domain_ref(domain_by_name, &a.domain, &a.scope, &b.domain, &b.scope)
                || a.name == b.name
            {
                continue;
            }
            let same_scope_tier =
                a.elements == b.elements && a.exclude_elements == b.exclude_elements;
            if same_scope_tier
                && (a.no_retention != b.no_retention
                    || a.save_signal != b.save_signal
                    || a.save_sense != b.save_sense
                    || a.restore_signal != b.restore_signal
                    || a.restore_sense != b.restore_sense
                    || a.save_condition != b.save_condition
                    || a.restore_condition != b.restore_condition
                    || a.retention_condition != b.retention_condition)
            {
                intent.diagnostics.push(UpfDiag {
                    rule_id: "UPF_STRATEGY_OVERLAP".to_string(),
                    severity: DiagSeverity::Warning,
                    message: format!(
                        "retention strategies `{}` and `{}` overlap on domain `{}` with conflicting attributes",
                        a.name, b.name, a.domain
                    ),
                    loc: b.loc.clone(),
                });
            }
        }
    }
}

fn matches_exclude_element_in_scope(patterns: &[String], scope: &str, target: &str) -> bool {
    if matches_any_element_in_scope(patterns, scope, target) {
        return true;
    }
    if let Some((base, _, _)) = parse_bit_slice_expr(target) {
        if matches_any_element_in_scope(patterns, scope, &base) {
            return true;
        }
    }
    false
}

/// Parses a trailing Verilog bit-slice `[msb:lsb]` or single-bit index `[idx]`
/// (including escaped `\[...\]`), returning `(base, msb, lsb)`.
pub fn parse_bit_slice_expr(s: &str) -> Option<(String, i128, i128)> {
    let unesc = unescape_rtl_brackets(s.trim());
    let trimmed = unesc.trim();
    if !trimmed.ends_with(']') {
        return None;
    }
    let open = trimmed.rfind('[')?;
    let inner = &trimmed[open + 1..trimmed.len() - 1];
    if inner.contains('/') {
        return None;
    }
    let base = trimmed[..open].trim_end();
    if base.is_empty() {
        return None;
    }
    if let Some((a, b)) = inner.split_once(':') {
        Some((
            base.to_string(),
            a.trim().parse().ok()?,
            b.trim().parse().ok()?,
        ))
    } else {
        let idx: i128 = inner.trim().parse().ok()?;
        Some((base.to_string(), idx, idx))
    }
}

/// Converts declared `(msb, lsb)` SV bit coordinates into inclusive 0-based physical
/// LSB-0 `(lo, hi)` coordinates for a signal of `width` and `packed_bounds`.
pub fn sv_slice_to_phys_range(
    msb: i128,
    lsb: i128,
    width: u32,
    packed_bounds: Option<(i128, i128)>,
) -> Option<(u32, u32)> {
    if width == 0 {
        return None;
    }
    let (left, right) = packed_bounds.unwrap_or(((width.saturating_sub(1)) as i128, 0));
    if left >= right {
        let lo_sv = msb.min(lsb);
        let hi_sv = msb.max(lsb);
        if lo_sv < right || hi_sv > left {
            return None;
        }
        Some(((lo_sv - right) as u32, (hi_sv - right) as u32))
    } else {
        let lo_sv = msb.max(lsb);
        let hi_sv = msb.min(lsb);
        if hi_sv < left || lo_sv > right {
            return None;
        }
        Some(((right - lo_sv) as u32, (right - hi_sv) as u32))
    }
}

fn top_prefixed_upf(upf_path: &str, full_path: &str, esc_upf_path: Option<&str>) -> Option<String> {
    let upf_norm = upf_path.trim_start_matches('/');
    if upf_norm.is_empty() {
        return None;
    }
    let (top, rest) = full_path.split_once('.')?;
    if top.is_empty() {
        return None;
    }
    let esc_norm = esc_upf_path.map(|s| s.trim_start_matches('/'));
    if rest == upf_norm.replace('/', ".")
        || esc_norm
            .is_some_and(|e| rest == e.replace('/', ".") || rest == e.trim_end().replace('/', "."))
    {
        Some(format!("{top}/{upf_norm}"))
    } else {
        None
    }
}

/// Returns `true` if signal-base pattern `base` (literal, escaped, or glob) matches
/// an endpoint's signal path (`upf_path`, `esc_upf_path`, or `full_path`).
pub fn endpoint_base_matches(
    base: &str,
    upf_path: &str,
    full_path: &str,
    esc_upf_path: Option<&str>,
) -> bool {
    let pat = base.trim_start_matches('/');
    let upf_norm = upf_path.trim_start_matches('/');
    let esc_norm = esc_upf_path.map(|s| s.trim_start_matches('/'));
    let top_slash = top_prefixed_upf(upf_path, full_path, esc_upf_path);
    if has_unescaped_glob_wildcards(pat) {
        let esc = escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(pat));
        let esc_un = strip_sv_escapes_in_glob(&escape_rtl_brackets(pat));
        let glob_hit = |t: &str| glob(&esc, t) || (!t.contains('\\') && glob(&esc_un, t));
        return glob_hit(upf_norm)
            || esc_norm.is_some_and(glob_hit)
            || top_slash.as_deref().is_some_and(glob_hit);
    }
    let lit = unescape_rtl_brackets(pat);
    let lit_un = strip_sv_escapes_in_path(&lit);
    lit == upf_norm
        || lit_un == upf_norm
        || esc_norm.is_some_and(|en| lit == en || lit.trim_end() == en.trim_end())
        || top_slash
            .as_deref()
            .is_some_and(|ts| lit == ts || lit_un == ts)
        || lit == full_path
        || (!lit_un.is_empty()
            && (lit.replace('/', ".") == full_path || lit_un.replace('/', ".") == full_path))
}

/// Evaluates `patterns` against `ep` (`is_exclude = true` triggers on any physical
/// bit-range overlap; `is_exclude = false` requires coverage of all `ep.ranges`).
pub fn endpoint_matches_patterns(
    ep: &SignalEndpoint<'_>,
    scope: &str,
    patterns: &[String],
    is_exclude: bool,
) -> bool {
    let upf_norm = ep.upf_path.trim_start_matches('/');
    let esc_norm = ep.esc_upf_path.map(|s| s.trim_start_matches('/'));
    let top_slash = top_prefixed_upf(ep.upf_path, ep.full_path, ep.esc_upf_path);

    for pat_raw in patterns {
        let pat_slice = std::slice::from_ref(pat_raw);
        let whole_hit = matches_any_element_in_scope(pat_slice, scope, upf_norm)
            || esc_norm.is_some_and(|en| matches_any_element_in_scope(pat_slice, scope, en))
            || top_slash
                .as_deref()
                .is_some_and(|ts| matches_any_element_in_scope(pat_slice, scope, ts));
        if whole_hit {
            return true;
        }
        if let Some((base, msb, lsb)) = parse_bit_slice_expr(pat_raw) {
            if endpoint_base_matches(&base, ep.upf_path, ep.full_path, ep.esc_upf_path) {
                if let Some((phys_lo, phys_hi)) =
                    sv_slice_to_phys_range(msb, lsb, ep.width, ep.packed_bounds)
                {
                    let bit_hit = if is_exclude {
                        ep.ranges
                            .iter()
                            .any(|&(lo, hi)| phys_lo <= hi && lo <= phys_hi)
                    } else {
                        !ep.ranges.is_empty()
                            && ep
                                .ranges
                                .iter()
                                .all(|&(lo, hi)| phys_lo <= lo && phys_hi >= hi)
                    };
                    if bit_hit {
                        return true;
                    }
                }
            }
        }
    }
    false
}
