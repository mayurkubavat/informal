//! Stage 2 Level-Shifter rules (`LS_MISSING`, `LS_DIR_MISMATCH`, `LS_LOCATION`,
//! `LS_UNISOLATED_INPUT`, `LS_REDUNDANT`).

use informal_svlang::struct_graph::{split_crossing_group_by_cuts, BitRange, CrossingGroup};
use informal_upf::{
    AppliesTo, LevelShifterStrategy, LsRequirement, LsRule, PowerDomainId, StrategyLocation,
    StrategyMatch, SupplySetId,
};

use super::iso::display_signal;
use super::{Stage2Ctx, ViolationSink};

pub(crate) fn check_crossing(
    ctx: &mut Stage2Ctx<'_>,
    group: &CrossingGroup,
    iso_cuts: &[BitRange],
    sink: &mut ViolationSink,
) {
    let Some(src_pd) = ctx.domain_of_cell(group.src_cell) else {
        return;
    };
    let Some(dst_pd) = ctx.domain_of_cell(group.dst_cell) else {
        return;
    };
    if src_pd == dst_pd {
        return;
    }
    let Some(src_ss) = ctx
        .effective_port_supply(group.src_cell, group.src_base_net, false)
        .or_else(|| ctx.resolved.primary_ss(src_pd))
    else {
        return;
    };
    let Some(dst_ss) = ctx
        .effective_port_supply(group.dst_cell, group.dst_base_net, true)
        .or_else(|| ctx.resolved.primary_ss(dst_pd))
    else {
        return;
    };

    let src_name = ctx.domain_name(src_pd);
    let dst_name = ctx.domain_name(dst_pd);
    let sig_disp = display_signal(ctx, group);
    let rtl_loc = ctx.graph.format_loc(group.loc);

    let ls_req = ctx.sat.voltage_relation(src_ss, dst_ss, ctx.tol_uv);
    let ls_match = ctx.match_ls_for_group(Some(src_pd), Some(dst_pd), group);

    let needs_ls = matches!(
        ls_req,
        LsRequirement::HighToLow { .. }
            | LsRequirement::LowToHigh { .. }
            | LsRequirement::Both { .. }
    ) && !ctx.resolved.same_supply_class(src_ss, dst_ss);

    match ls_match {
        StrategyMatch::None => {
            if needs_ls {
                let witness = ls_witness(&ls_req);
                sink.push(
                    "LS_MISSING",
                    None,
                    Some(src_name.clone()),
                    Some(dst_name.clone()),
                    sig_disp.clone(),
                    group.width_bits,
                    group.raw_crossings,
                    format!(
                        "missing level shifter for `{}` crossing from `{src_name}` to `{dst_name}` (requires {})",
                        sig_disp,
                        ls_req.kind_tag()
                    ),
                    None,
                    rtl_loc.clone(),
                    witness,
                );
            }
        }
        StrategyMatch::Suppressed(_) => {
            // `-no_shift` explicitly suppresses `LS_MISSING`.
        }
        StrategyMatch::Matched(ls) => {
            // 1. `LS_REDUNDANT`
            if !needs_ls && !ls.force_shift && !matches!(ls_req, LsRequirement::Undetermined { .. })
            {
                sink.push(
                    "LS_REDUNDANT",
                    None,
                    Some(src_name.clone()),
                    Some(dst_name.clone()),
                    sig_disp.clone(),
                    group.width_bits,
                    group.raw_crossings,
                    format!(
                        "level-shifter strategy `{}` is redundant: `{src_name}` and `{dst_name}` share compatible voltage levels",
                        ls.name
                    ),
                    ls.loc.clone(),
                    rtl_loc.clone(),
                    None,
                );
            }

            // 2. `LS_DIR_MISMATCH`
            if let Some(rule) = ls.rule {
                let dir_bad = matches!(
                    (&ls_req, rule),
                    (LsRequirement::HighToLow { .. }, LsRule::LowToHigh)
                        | (LsRequirement::LowToHigh { .. }, LsRule::HighToLow)
                        | (
                            LsRequirement::Both { .. },
                            LsRule::LowToHigh | LsRule::HighToLow
                        )
                );
                if dir_bad {
                    sink.push(
                        "LS_DIR_MISMATCH",
                        None,
                        Some(src_name.clone()),
                        Some(dst_name.clone()),
                        sig_disp.clone(),
                        group.width_bits,
                        group.raw_crossings,
                        format!(
                            "level-shifter `{}` specifies `-rule {:?}` but PST voltage relation requires `{}`",
                            ls.name,
                            rule,
                            ls_req.kind_tag()
                        ),
                        ls.loc.clone(),
                        rtl_loc.clone(),
                        ls_witness(&ls_req),
                    );
                }
            }

            // 3. `LS_LOCATION`
            check_ls_location(
                ctx, ls, group, &ls_req, src_pd, dst_pd, src_ss, dst_ss, &src_name, &dst_name,
                &sig_disp, &rtl_loc, sink,
            );

            // 4. `LS_UNISOLATED_INPUT`
            if needs_ls && !ctx.resolved.same_func_supply_class(src_ss, dst_ss) {
                if let Some(w) = ctx.sat.can_be_off_while_on(src_ss, dst_ss) {
                    let iso_subs = split_crossing_group_by_cuts(ctx.graph, group, iso_cuts);
                    let uniso_subs: Vec<CrossingGroup> = iso_subs
                        .iter()
                        .filter(|sub| {
                            matches!(
                                ctx.match_iso_for_group(Some(src_pd), Some(dst_pd), sub),
                                StrategyMatch::None
                            )
                        })
                        .cloned()
                        .collect();
                    if uniso_subs.len() == iso_subs.len() && !uniso_subs.is_empty() {
                        sink.push(
                            "LS_UNISOLATED_INPUT",
                            None,
                            Some(src_name),
                            Some(dst_name),
                            sig_disp,
                            group.width_bits,
                            group.raw_crossings,
                            format!(
                                "level-shifter `{}` input from switchable `{}` has no isolation strategy",
                                ls.name,
                                ctx.domain_name(src_pd)
                            ),
                            ls.loc.clone(),
                            rtl_loc,
                            Some(w),
                        );
                    } else {
                        for sub in &uniso_subs {
                            let sub_sig = display_signal(ctx, sub);
                            sink.push(
                                "LS_UNISOLATED_INPUT",
                                None,
                                Some(src_name.clone()),
                                Some(dst_name.clone()),
                                sub_sig,
                                sub.width_bits,
                                sub.raw_crossings,
                                format!(
                                    "level-shifter `{}` input from switchable `{}` has no isolation strategy",
                                    ls.name,
                                    ctx.domain_name(src_pd)
                                ),
                                ls.loc.clone(),
                                rtl_loc.clone(),
                                Some(w.clone()),
                            );
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn check_ls_location(
    ctx: &mut Stage2Ctx<'_>,
    ls: &LevelShifterStrategy,
    group: &CrossingGroup,
    ls_req: &LsRequirement,
    src_pd: PowerDomainId,
    dst_pd: PowerDomainId,
    src_ss: SupplySetId,
    dst_ss: SupplySetId,
    src_name: &str,
    dst_name: &str,
    sig_disp: &str,
    rtl_loc: &Option<String>,
    sink: &mut ViolationSink,
) {
    let mut bad_msg: Option<(String, Option<informal_upf::PstWitness>)> = None;

    if let Some(ref in_s) = ls.input_supply_set {
        if let Some(in_ss) = ctx.resolved.resolve_supply_set(in_s, &ls.scope) {
            if !ctx.resolved.same_supply_class(in_ss, src_ss) {
                bad_msg = Some((
                    format!(
                        "level-shifter `{}` `-input_supply_set {in_s}` does not match source domain `{src_name}` supply",
                        ls.name
                    ),
                    None,
                ));
            }
        }
    }
    if bad_msg.is_none() {
        if let Some(ref out_s) = ls.output_supply_set {
            if let Some(out_ss) = ctx.resolved.resolve_supply_set(out_s, &ls.scope) {
                if !ctx.resolved.same_supply_class(out_ss, dst_ss) {
                    bad_msg = Some((
                        format!(
                            "level-shifter `{}` `-output_supply_set {out_s}` does not match destination domain `{dst_name}` supply",
                            ls.name
                        ),
                        None,
                    ));
                }
            }
        }
    }

    if bad_msg.is_none() {
        if let Some(loc) = ls.location {
            let applies = ls.applies_to.unwrap_or(AppliesTo::Both);
            match loc {
                StrategyLocation::Parent => {
                    let parent_cell = if applies == AppliesTo::Inputs {
                        ctx.graph.cell(group.dst_cell).parent
                    } else {
                        ctx.graph.cell(group.src_cell).parent
                    };
                    if let Some(pcell) = parent_cell {
                        if let Some(parent_ss) = ctx.ss_of_cell(pcell) {
                            if let Err(w) = ctx.sat.is_supply_always_on_when(parent_ss, dst_ss) {
                                bad_msg = Some((
                                    format!(
                                        "level-shifter `{}` uses `-location parent`, but parent `{}` supply can be OFF while `{dst_name}` is NORMAL",
                                        ls.name,
                                        ctx.graph.cell(pcell).upf_path
                                    ),
                                    Some(w),
                                ));
                            }
                        }
                    } else if src_pd != dst_pd && ctx.graph.cell(group.src_cell).is_top {
                        bad_msg = Some((
                            format!(
                                "level-shifter `{}` uses `-location parent` on top-level domain `{src_name}`",
                                ls.name
                            ),
                            None,
                        ));
                    }
                }
                StrategyLocation::Self_ => {
                    // Low-to-high shifter located inside `src` (`Outputs`) without destination `output_supply_set`
                    // lacks the higher destination rail!
                    let strat_on_src =
                        ctx.resolved.resolve_domain(&ls.domain, &ls.scope) == Some(src_pd);
                    if strat_on_src
                        && matches!(applies, AppliesTo::Outputs | AppliesTo::Both)
                        && matches!(
                            ls_req,
                            LsRequirement::LowToHigh { .. } | LsRequirement::Both { .. }
                        )
                        && ls.output_supply_set.is_none()
                    {
                        bad_msg = Some((
                            format!(
                                "level-shifter `{}` uses `-location self` in `{src_name}` for Low-to-High shift without `-output_supply_set`",
                                ls.name
                            ),
                            ls_witness(ls_req),
                        ));
                    }
                }
                _ => {}
            }
        }
    }

    if let Some((msg, w)) = bad_msg {
        sink.push(
            "LS_LOCATION",
            None,
            Some(src_name.to_string()),
            Some(dst_name.to_string()),
            sig_disp.to_string(),
            group.width_bits,
            group.raw_crossings,
            msg,
            ls.loc.clone(),
            rtl_loc.clone(),
            w,
        );
    }
}

fn ls_witness(req: &LsRequirement) -> Option<informal_upf::PstWitness> {
    match req {
        LsRequirement::HighToLow { witness } | LsRequirement::LowToHigh { witness } => {
            Some(witness.clone())
        }
        LsRequirement::Both { hl_witness, .. } => Some(hl_witness.clone()),
        LsRequirement::None | LsRequirement::Undetermined { .. } => None,
    }
}
