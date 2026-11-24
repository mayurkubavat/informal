//! Stage 2 Isolation rules (`ISO_*`), control-cone 4-valued parity lattice
//! checks, inline RTL clamp verification, and feedthrough detection.

use informal_svlang::struct_graph::{
    BitRange, CrossingGroup, CtrlCone, InlineClampInfo, NetSegId, NetSegKind, Polarity,
};
use informal_upf::{
    AppliesTo, ClampValue, IsolationStrategy, PowerDomainId, Sense, StrategyLocation, StrategyMatch,
};

use super::{resolve_upf_signal_range_in_scope, Stage2Ctx, ViolationSink};

pub(crate) fn check_crossing(
    ctx: &mut Stage2Ctx<'_>,
    group: &CrossingGroup,
    sink: &mut ViolationSink,
) {
    let Some(src_pd) = ctx.domain_of_cell(group.src_cell) else {
        return;
    };
    let Some(dst_pd) = ctx.domain_of_cell(group.dst_cell) else {
        return;
    };
    if src_pd == dst_pd && group.mid_cell.is_none() {
        return;
    }

    let Some(src_ss) = ctx.resolved.primary_ss(src_pd) else {
        return;
    };
    let Some(dst_ss) = ctx.resolved.primary_ss(dst_pd) else {
        return;
    };

    let src_name = ctx.domain_name(src_pd);
    let dst_name = ctx.domain_name(dst_pd);
    let sig_disp = display_signal(ctx, group);
    let rtl_loc = ctx.graph.format_loc(group.loc);

    // 1. Feedthrough checks (`ISO_FEEDTHROUGH` / `ISO_FEEDTHROUGH_BUFFER`).
    super::feedthrough_iso::check_feedthrough(
        ctx, group, src_pd, dst_pd, src_ss, dst_ss, &sig_disp, &rtl_loc, sink,
    );
    if src_pd == dst_pd {
        return;
    }

    let iso_match = ctx.match_iso_for_group(Some(src_pd), Some(dst_pd), group);
    let corrupt_witness = if ctx.resolved.same_func_supply_class(src_ss, dst_ss) {
        None
    } else {
        ctx.sat.can_be_off_while_on(src_ss, dst_ss)
    };

    match iso_match {
        StrategyMatch::None => {
            if let Some(w) = corrupt_witness {
                sink.push(
                    "ISO_MISSING",
                    None,
                    Some(src_name),
                    Some(dst_name),
                    sig_disp,
                    group.width_bits,
                    group.raw_crossings,
                    format!(
                        "unisolated crossing from `{}` to `{}` while source can be OFF/CORRUPT and sink is NORMAL",
                        ctx.domain_name(src_pd),
                        ctx.domain_name(dst_pd)
                    ),
                    None,
                    rtl_loc,
                    Some(w),
                );
            }
        }
        StrategyMatch::Suppressed(_) => {
            // `-no_isolation` explicitly suppresses `ISO_MISSING`.
        }
        StrategyMatch::Matched(iso) => {
            // 2. `ISO_REDUNDANT` (do not flag when source has activity-triggered corruption states).
            let src_has_act_corrupt = ctx
                .resolved
                .ss_states
                .get(src_ss.0 as usize)
                .is_some_and(|states| {
                    states.iter().any(|st| {
                        !st.illegal
                            && st.simstate.is_some_and(|s| {
                                matches!(
                                    s,
                                    informal_upf::SimState::CorruptOnActivity
                                        | informal_upf::SimState::CorruptStateOnActivity
                                        | informal_upf::SimState::CorruptStateOnChange
                                )
                            })
                    })
                });
            if corrupt_witness.is_none() && !iso.force_isolation && !src_has_act_corrupt {
                sink.push(
                    "ISO_REDUNDANT",
                    None,
                    Some(src_name.clone()),
                    Some(dst_name.clone()),
                    sig_disp.clone(),
                    group.width_bits,
                    group.raw_crossings,
                    format!(
                        "isolation strategy `{}` is redundant: `{}` cannot be OFF while `{}` is NORMAL",
                        iso.name, src_name, dst_name
                    ),
                    iso.loc.clone(),
                    rtl_loc.clone(),
                    None,
                );
            }

            // 3. `ISO_SUPPLY_MISMATCH`
            let iso_ss_opt = ctx.resolved.effective_iso_supply(iso);
            match iso_ss_opt {
                None => {
                    sink.push(
                        "ISO_SUPPLY_MISMATCH",
                        None,
                        Some(src_name.clone()),
                        Some(dst_name.clone()),
                        sig_disp.clone(),
                        group.width_bits,
                        group.raw_crossings,
                        format!(
                            "isolation strategy `{}` has no resolved isolation supply set",
                            iso.name
                        ),
                        iso.loc.clone(),
                        rtl_loc.clone(),
                        None,
                    );
                }
                Some(iso_ss) => {
                    if let Err(w) = ctx
                        .sat
                        .is_supply_always_on_when_crossing(iso_ss, src_ss, dst_ss)
                    {
                        sink.push(
                            "ISO_SUPPLY_MISMATCH",
                            None,
                            Some(src_name.clone()),
                            Some(dst_name.clone()),
                            sig_disp.clone(),
                            group.width_bits,
                            group.raw_crossings,
                            format!(
                                "isolation supply `{}` for strategy `{}` can be OFF while `{}` corrupts and `{}` is NORMAL",
                                ctx.resolved.intent.supply_sets[iso_ss.0 as usize].name,
                                iso.name,
                                src_name,
                                dst_name
                            ),
                            iso.loc.clone(),
                            rtl_loc.clone(),
                            Some(w),
                        );
                    }
                }
            }

            // 4. `ISO_LOCATION`
            check_iso_location(
                ctx, iso, group, src_pd, dst_pd, src_ss, dst_ss, iso_ss_opt, &src_name, &dst_name,
                &sig_disp, &rtl_loc, sink,
            );

            // 5. `ISO_CLAMP_UNSPECIFIED`
            if iso.clamp_value.is_none() && iso.applies_to_clamp.is_none() {
                sink.push(
                    "ISO_CLAMP_UNSPECIFIED",
                    None,
                    Some(src_name.clone()),
                    Some(dst_name.clone()),
                    sig_disp.clone(),
                    group.width_bits,
                    group.raw_crossings,
                    format!("isolation strategy `{}` has no `-clamp_value`", iso.name),
                    iso.loc.clone(),
                    rtl_loc.clone(),
                    None,
                );
            }

            // 6. Control-cone & inline RTL clamp checks (`ISO_CTRL_*`, `ISO_CLAMP_MISMATCH`).
            check_iso_ctrl_and_clamp(
                ctx, iso, group, src_ss, dst_ss, &src_name, &dst_name, &sig_disp, &rtl_loc, sink,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_iso_location(
    ctx: &mut Stage2Ctx<'_>,
    iso: &IsolationStrategy,
    group: &CrossingGroup,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: informal_upf::SupplySetId,
    dst_ss: informal_upf::SupplySetId,
    iso_ss_opt: Option<informal_upf::SupplySetId>,
    src_name: &str,
    dst_name: &str,
    sig_disp: &str,
    rtl_loc: &Option<String>,
    sink: &mut ViolationSink,
) {
    let Some(loc) = iso.location else {
        return;
    };
    let applies = iso.applies_to.unwrap_or(AppliesTo::Outputs);
    let mut bad_reason: Option<(String, Option<informal_upf::PstWitness>)> = None;

    match loc {
        StrategyLocation::Self_ => {
            if applies == AppliesTo::Inputs {
                let check_ss = iso_ss_opt.unwrap_or(dst_ss);
                if let Some(w) = ctx.sat.can_be_off_while_on(check_ss, src_ss) {
                    bad_reason = Some((
                        format!(
                            "isolation `{}` uses `-location self` on destination `{dst_name}` without an always-on isolation supply",
                            iso.name
                        ),
                        Some(w),
                    ));
                }
            } else if applies == AppliesTo::Outputs {
                // In switchable source domain with `-location self`, if no explicit AON isolation supply is set
                // (or it equals `src_ss`), isolation resides on switchable rail without backup!
                if iso.isolation_supply_set.is_none() || iso_ss_opt == Some(src_ss) {
                    if let Some(w) = ctx.sat.can_be_off_while_on(src_ss, dst_ss) {
                        bad_reason = Some((
                            format!(
                                "isolation `{}` uses `-location self` in switchable domain `{src_name}` without an always-on isolation supply",
                                iso.name
                            ),
                            Some(w),
                        ));
                    }
                }
            }
        }
        StrategyLocation::Parent => {
            let parent_cell = if applies == AppliesTo::Inputs {
                ctx.graph.cell(group.dst_cell).parent
            } else {
                ctx.graph.cell(group.src_cell).parent
            };
            if let Some(pcell) = parent_cell {
                if let Some(parent_ss) = ctx.ss_of_cell(pcell) {
                    if let Err(w) = ctx
                        .sat
                        .is_supply_always_on_when_crossing(parent_ss, src_ss, dst_ss)
                    {
                        bad_reason = Some((
                            format!(
                                "isolation `{}` uses `-location parent`, but parent cell `{}` domain can be OFF during crossing",
                                iso.name,
                                ctx.graph.cell(pcell).upf_path
                            ),
                            Some(w),
                        ));
                    }
                }
            } else if src_pd != dst_pd && ctx.graph.cell(group.src_cell).is_top {
                bad_reason = Some((
                    format!(
                        "isolation `{}` uses `-location parent` on top-level domain `{src_name}` which has no parent",
                        iso.name
                    ),
                    None,
                ));
            }
        }
        StrategyLocation::Fanout | StrategyLocation::Other if applies == AppliesTo::Inputs => {
            bad_reason = Some((
                format!(
                    "isolation `{}` combines `-applies_to inputs` with incompatible `-location {:?}`",
                    iso.name, loc
                ),
                None,
            ));
        }
        _ => {}
    }

    if let Some((msg, w)) = bad_reason {
        sink.push(
            "ISO_LOCATION",
            None,
            Some(src_name.to_string()),
            Some(dst_name.to_string()),
            sig_disp.to_string(),
            group.width_bits,
            group.raw_crossings,
            msg,
            iso.loc.clone(),
            rtl_loc.clone(),
            w,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn check_iso_ctrl_and_clamp(
    ctx: &mut Stage2Ctx<'_>,
    iso: &IsolationStrategy,
    group: &CrossingGroup,
    src_ss: informal_upf::SupplySetId,
    dst_ss: informal_upf::SupplySetId,
    src_name: &str,
    dst_name: &str,
    sig_disp: &str,
    rtl_loc: &Option<String>,
    sink: &mut ViolationSink,
) {
    let Some(ref iso_sig_raw) = iso.isolation_signal else {
        sink.push(
            "ISO_CTRL_UNPOWERED",
            None,
            Some(src_name.to_string()),
            Some(dst_name.to_string()),
            sig_disp.to_string(),
            group.width_bits,
            group.raw_crossings,
            format!(
                "isolation strategy `{}` specifies no `-isolation_signal`",
                iso.name
            ),
            iso.loc.clone(),
            rtl_loc.clone(),
            None,
        );
        return;
    };

    let ctrl_scope = iso.ctrl_scope.as_deref().unwrap_or(&iso.scope);
    let iso_sig_res = resolve_upf_signal_range_in_scope(ctx, ctrl_scope, iso_sig_raw);
    let iso_net_opt = iso_sig_res.map(|(n, _)| n);
    let mut iso_cone: Option<CtrlCone> = None;

    let mut sigs: Vec<(NetSegId, BitRange)> = Vec::new();
    if let Some(nr) = iso_sig_res {
        sigs.push(nr);
    } else if let Ok(sig_refs) = informal_upf::collect_logic_expr_signal_refs(iso_sig_raw) {
        for sig in sig_refs {
            if let Some(nr) = resolve_upf_signal_range_in_scope(ctx, ctrl_scope, &sig) {
                if !sigs.contains(&nr) {
                    sigs.push(nr);
                }
            }
        }
    }

    let mut reported_mixed = false;
    let mut reported_unpowered = false;
    for (ctrl_net, ctrl_range) in sigs {
        let cone = ctx.graph.trace_ctrl_cone(ctrl_net, ctrl_range, 64);

        // Check `ISO_CTRL_MIXED_POLARITY`.
        if !reported_mixed && cone.roots.iter().any(|r| r.polarity == Polarity::Mixed) {
            reported_mixed = true;
            sink.push(
                "ISO_CTRL_MIXED_POLARITY",
                None,
                Some(src_name.to_string()),
                Some(dst_name.to_string()),
                sig_disp.to_string(),
                group.width_bits,
                group.raw_crossings,
                format!(
                    "isolation signal `{iso_sig_raw}` for `{}` has reconvergent mixed polarity in its control cone",
                    iso.name
                ),
                iso.loc.clone(),
                rtl_loc.clone(),
                None,
            );
        }

        // Check `ISO_CTRL_UNPOWERED`: every cell along `cone_cells` and `roots` must be AON during crossing!
        if !reported_unpowered {
            for &cid in &cone.cone_cells {
                if let Some(ctrl_ss) = ctx.ss_of_cell(cid) {
                    if let Err(w) = ctx
                        .sat
                        .is_supply_always_on_when_crossing(ctrl_ss, src_ss, dst_ss)
                    {
                        let c_dom = ctx
                            .domain_of_cell(cid)
                            .map(|d| ctx.domain_name(d))
                            .unwrap_or_else(|| "unknown".to_string());
                        sink.push(
                            "ISO_CTRL_UNPOWERED",
                            None,
                            Some(src_name.to_string()),
                            Some(dst_name.to_string()),
                            sig_disp.to_string(),
                            group.width_bits,
                            group.raw_crossings,
                            format!(
                                "isolation control `{iso_sig_raw}` traverses/originates in domain `{c_dom}` which can be OFF while crossing `{src_name} -> {dst_name}` is active"
                            ),
                            iso.loc.clone(),
                            rtl_loc.clone(),
                            Some(w),
                        );
                        reported_unpowered = true;
                        break;
                    }
                }
            }
        }
        if iso_sig_res.is_some() {
            iso_cone = Some(cone);
        }
    }

    // Inline RTL clamp gate checks (`ISO_CTRL_POLARITY_MISMATCH` & `ISO_CLAMP_MISMATCH`).
    let clamps = collect_relevant_clamps(ctx, group);
    for ic in clamps {
        let (ctrl_related, eff_sense_opt) =
            analyze_clamp_ctrl(ctx, &ic, iso_net_opt, iso_cone.as_ref());
        if let Some(iso_sense) = iso.isolation_sense {
            if let Some(upf_active_high) = match iso_sense {
                Sense::High => Some(true),
                Sense::Low => Some(false),
                _ => None,
            } {
                if let Some(eff_active_high) = eff_sense_opt {
                    if eff_active_high != upf_active_high {
                        sink.push(
                            "ISO_CTRL_POLARITY_MISMATCH",
                            None,
                            Some(src_name.to_string()),
                            Some(dst_name.to_string()),
                            sig_disp.to_string(),
                            group.width_bits,
                            group.raw_crossings,
                            format!(
                                "inline RTL clamp on `{sig_disp}` clamps on `{}` while UPF `{}` specifies `-isolation_sense {:?}`",
                                if eff_active_high { "high (1)" } else { "low (0)" },
                                iso.name,
                                iso_sense
                            ),
                            iso.loc.clone(),
                            rtl_loc.clone(),
                            None,
                        );
                    }
                }
            }
        }

        let upf_clamp = iso.clamp_value.or(iso.applies_to_clamp);
        if ctrl_related {
            if let Some(cv) = upf_clamp {
                let mismatch = matches!(
                    (cv, ic.clamp_one),
                    (ClampValue::Zero, true) | (ClampValue::One, false)
                );
                if mismatch {
                    sink.push(
                        "ISO_CLAMP_MISMATCH",
                        None,
                        Some(src_name.to_string()),
                        Some(dst_name.to_string()),
                        sig_disp.to_string(),
                        group.width_bits,
                        group.raw_crossings,
                        format!(
                            "inline RTL clamp on `{sig_disp}` clamps to `{}` while UPF `{}` specifies `-clamp_value {:?}`",
                            if ic.clamp_one { "1" } else { "0" },
                            iso.name,
                            cv
                        ),
                        iso.loc.clone(),
                        rtl_loc.clone(),
                        None,
                    );
                }
            }
        }
    }
}

fn analyze_clamp_ctrl(
    ctx: &Stage2Ctx<'_>,
    ic: &InlineClampInfo,
    iso_net_opt: Option<NetSegId>,
    iso_cone: Option<&CtrlCone>,
) -> (bool, Option<bool>) {
    let Some(iso_net) = iso_net_opt else {
        return (false, None);
    };
    let map_pol = |pol: Polarity| match pol {
        Polarity::Positive => Some(ic.active_high),
        Polarity::Negative => Some(!ic.active_high),
        Polarity::Mixed | Polarity::Unknown => None,
    };
    if ic.ctrl_net == iso_net {
        return (true, Some(ic.active_high));
    }
    if let Some(cone) = iso_cone {
        if let Some(pol) = cone.polarity_from(ic.ctrl_net) {
            return (true, map_pol(pol));
        }
    }
    let gate_ctrl_w = ctx.graph.net(ic.ctrl_net).width;
    let gate_cone = ctx
        .graph
        .trace_ctrl_cone(ic.ctrl_net, BitRange::full(gate_ctrl_w), 64);
    if let Some(pol) = gate_cone.polarity_from(iso_net) {
        return (true, map_pol(pol));
    }
    if let Some(cone) = iso_cone {
        let mut shared_found = false;
        let mut agreed: Option<bool> = None;
        let mut ambiguous = false;
        let mut has_mixed = false;
        for ir in &cone.roots {
            if ir.is_const {
                continue;
            }
            if let Some(gr) = gate_cone
                .roots
                .iter()
                .find(|r| !r.is_const && r.net == ir.net)
            {
                shared_found = true;
                if ir.polarity == Polarity::Mixed || gr.polarity == Polarity::Mixed {
                    has_mixed = true;
                    continue;
                }
                let eff = match (ir.polarity, gr.polarity) {
                    (Polarity::Positive, Polarity::Positive)
                    | (Polarity::Negative, Polarity::Negative) => Some(ic.active_high),
                    (Polarity::Positive, Polarity::Negative)
                    | (Polarity::Negative, Polarity::Positive) => Some(!ic.active_high),
                    _ => None,
                };
                if let Some(e) = eff {
                    match agreed {
                        None => agreed = Some(e),
                        Some(prev) if prev != e => ambiguous = true,
                        _ => {}
                    }
                }
            }
        }
        if agreed.is_none() && !has_mixed && !ambiguous {
            for &nid in &cone.cone_nets {
                let kind = ctx.graph.net(nid).kind;
                if matches!(kind, NetSegKind::Supply0 | NetSegKind::Supply1) {
                    continue;
                }
                if let (Some(ip), Some(gp)) =
                    (cone.polarity_from(nid), gate_cone.polarity_from(nid))
                {
                    shared_found = true;
                    let eff = match (ip, gp) {
                        (Polarity::Positive, Polarity::Positive)
                        | (Polarity::Negative, Polarity::Negative) => Some(ic.active_high),
                        (Polarity::Positive, Polarity::Negative)
                        | (Polarity::Negative, Polarity::Positive) => Some(!ic.active_high),
                        _ => None,
                    };
                    if let Some(e) = eff {
                        match agreed {
                            None => agreed = Some(e),
                            Some(prev) if prev != e => ambiguous = true,
                            _ => {}
                        }
                    }
                }
            }
        }
        if shared_found {
            return (true, if has_mixed || ambiguous { None } else { agreed });
        }
    }
    (false, None)
}

fn collect_relevant_clamps(ctx: &Stage2Ctx<'_>, group: &CrossingGroup) -> Vec<InlineClampInfo> {
    let mut out = group.inline_clamps.clone();
    for drv in &ctx.graph.net(group.src_base_net).drivers {
        if group.src_ranges.iter().any(|r| drv.dst_range.overlaps(*r)) {
            if let Some(ic) = drv.inline_clamp {
                if !out.contains(&ic) {
                    out.push(ic);
                }
            }
        }
    }
    for drv in &ctx.graph.net(group.dst_base_net).drivers {
        if group.dst_ranges.iter().any(|r| drv.dst_range.overlaps(*r)) {
            if let Some(ic) = drv.inline_clamp {
                if !out.contains(&ic) {
                    out.push(ic);
                }
            }
        }
    }
    for load in &ctx.graph.net(group.dst_base_net).loads {
        if group.dst_ranges.iter().any(|r| load.src_range.overlaps(*r)) {
            if let Some(dn) = load.dst_net {
                for drv in &ctx.graph.net(dn).drivers {
                    if let Some(ic) = drv.inline_clamp {
                        if ic.data_net == group.dst_base_net
                            && group.dst_ranges.iter().any(|r| ic.data_range.overlaps(*r))
                            && !out.contains(&ic)
                        {
                            out.push(ic);
                        }
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn display_signal(ctx: &Stage2Ctx<'_>, group: &CrossingGroup) -> String {
    let _ = ctx;
    if group.src_upf_display.is_empty() {
        group.src_display.clone()
    } else {
        group.src_upf_display.clone()
    }
}
