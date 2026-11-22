//! Dynamic Counterexample Root-Cause Slicing (`why_cex`).
//!
//! Given a falsified assertion (`cex`) or reached `cover` (`covered`) and its
//! replayed VCD trace + isolated Btor2 cone, evaluates every word-level Btor2
//! node concretely across frames `0..=T` and walks backward from the failing
//! `bad` node at frame `T` following **only active mux arms (`ite`),
//! controlling boolean operands (`and`/`or`/`implies`), and active register
//! transitions**, pruning untaken datapaths and idle hold cycles.

use informal_ir::Op;
use serde::Serialize;
use std::collections::HashMap;

use crate::session::Session;

mod format;
mod miter_slice;
mod slice;

pub use format::format_text;
use miter_slice::{
    is_helper_name, parse_vcd_bits, resolve_sec_target, resolve_spv_target, resolve_xprop_target,
    ResolvedTarget,
};
use slice::analyze_model_and_vcd;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CausalEvent {
    pub frame: usize,
    pub signal: String,
    pub kind: &'static str,
    pub width: u32,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prev_value: Option<String>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct WhyCexReport {
    pub property: String,
    pub status: String,
    pub failure_frame: usize,
    pub total_frames: usize,
    pub static_cone_signals: usize,
    pub active_slice_signals: usize,
    pub active_signals: Vec<String>,
    pub events: Vec<CausalEvent>,
}

fn resolve_prop_target(
    s: &Session,
    prop_name: &str,
    exact_only: bool,
) -> Option<Result<ResolvedTarget, String>> {
    let prop_idx = if exact_only {
        s.props.iter().position(|p| p.name == prop_name)
    } else {
        s.props
            .iter()
            .position(|p| p.name == prop_name || p.name.ends_with(&format!("::{prop_name}")))
    }?;
    let p = &s.props[prop_idx];
    let Some(tp) = &p.trace else {
        return Some(Err(format!(
            "why_cex: '{}' has no counterexample/witness trace (status is {})",
            p.name,
            p.status.word()
        )));
    };
    Some(Ok((
        p.name.clone(),
        p.status.clone(),
        tp.clone(),
        s.workdir.join(format!("prop_{prop_idx}.trace.btor2")),
        s.workdir.join(format!("prop_{prop_idx}.btor2")),
    )))
}

pub fn slice_property_cex(
    s: &Session,
    prop_name: &str,
    max_events: usize,
) -> Result<WhyCexReport, String> {
    let (resolved_name, resolved_status, trace_path, trace_btor, plain_btor) =
        if let Some(key) = prop_name.strip_prefix("sec::") {
            resolve_sec_target(s, key)
                .or_else(|| resolve_prop_target(s, prop_name, true))
                .ok_or_else(|| format!("why_cex: unknown property '{prop_name}'"))??
        } else if let Some(key) = prop_name.strip_prefix("xprop::") {
            resolve_xprop_target(s, key)
                .or_else(|| resolve_prop_target(s, prop_name, true))
                .ok_or_else(|| format!("why_cex: unknown property '{prop_name}'"))??
        } else if let Some(key) = prop_name.strip_prefix("spv::") {
            resolve_spv_target(s, key)
                .or_else(|| resolve_prop_target(s, prop_name, true))
                .ok_or_else(|| format!("why_cex: unknown property '{prop_name}'"))??
        } else if let Some(res) = resolve_prop_target(s, prop_name, false) {
            res?
        } else if let Some(res) = resolve_sec_target(s, prop_name) {
            res?
        } else if let Some(res) = resolve_xprop_target(s, prop_name) {
            res?
        } else if let Some(res) = resolve_spv_target(s, prop_name) {
            res?
        } else {
            return Err(format!("why_cex: unknown property '{prop_name}'"));
        };
    let vcd_text = std::fs::read_to_string(&trace_path)
        .map_err(|e| format!("why_cex: cannot read '{trace_path}': {e}"))?;
    let vcd = parse_vcd_bits(&vcd_text)?;

    let btor_text = std::fs::read_to_string(&trace_btor)
        .or_else(|_| std::fs::read_to_string(&plain_btor))
        .map_err(|e| format!("why_cex: cannot read Btor2 model for '{resolved_name}': {e}"))?;
    let (core_btor, trace_sec) = informal_svlang::split_trace(&btor_text);
    let m = informal_ir::parse(core_btor).map_err(|e| format!("why_cex: {e}"))?;

    // Also parse `; informal-trace-outputs` alias `output <nid> <name>` lines.
    let mut wire_aliases: HashMap<u32, Vec<String>> = HashMap::new();
    for l in m.with_op(Op::Output) {
        if let (Some(&arg), Some(sym)) = (l.args.first(), &l.symbol) {
            if arg > 0 && !is_helper_name(sym) {
                wire_aliases
                    .entry(arg as u32)
                    .or_default()
                    .push(sym.clone());
            }
        }
    }
    for line in trace_sec.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.len() >= 4 && w[1] == "output" {
            if let Ok(arg) = w[2].parse::<i64>() {
                if arg > 0 && !is_helper_name(w[3]) {
                    wire_aliases
                        .entry(arg as u32)
                        .or_default()
                        .push(w[3].to_string());
                }
            }
        }
    }

    let top_prefix = s.design.top.as_deref().unwrap_or_default();
    analyze_model_and_vcd(
        &resolved_name,
        &resolved_status,
        &m,
        &vcd,
        &wire_aliases,
        top_prefix,
        max_events,
    )
}

#[cfg(test)]
mod tests {
    use super::miter_slice::{lookup_vcd, ParsedVcd};
    use super::*;
    use crate::session::Status;
    use crate::spv::{SpvRule, SpvStatus};
    use crate::xprop::{XpropItem, XpropStatus};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("informal-why-cex-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn helper_filter_and_informal_vcd_lookup() {
        assert!(is_helper_name("miter_first_step"));
        assert!(is_helper_name("miter_post_rst_0"));
        assert!(is_helper_name("m0.miter_first_step"));
        assert!(is_helper_name("informal.foo"));
        assert!(is_helper_name("top._sva_cnt"));
        assert!(!is_helper_name("m0.dut.q"));

        let mut vcd = ParsedVcd {
            frames: 1,
            clocks: HashSet::new(),
            by_name: HashMap::new(),
        };
        vcd.by_name
            .insert("informal.informal_x_0__m0".into(), vec![vec![true]]);
        vcd.by_name.insert("top.r__m0".into(), vec![vec![false]]);
        assert_eq!(
            lookup_vcd(&vcd, "informal_x_0__m0", "dut"),
            Some(&vec![vec![true]])
        );
        assert_eq!(
            lookup_vcd(&vcd, "dut.informal_x_0__m0", "dut"),
            Some(&vec![vec![true]])
        );
        assert_eq!(lookup_vcd(&vcd, "r__m0", ""), Some(&vec![vec![false]]));
    }

    #[test]
    fn slice_resolves_xprop_spv_prefixes_and_no_trace_errors() {
        let dir = temp_dir("resolve");
        let trace_btor = "\
1 sort bitvec 1
2 input 1 secret__m0
3 input 1 secret__m1
4 state 1 miter_first_step
5 one 1
6 zero 1
7 init 1 4 5
8 next 1 4 6
9 neq 1 2 3
10 bad 9 target_bad
; informal-trace-outputs
11 output 2 m0.secret
12 output 3 m1.secret
";
        let vcd_text = "\
$timescale 1ns $end
$scope module dut $end
$var wire 1 ! secret__m0 $end
$var wire 1 \" secret__m1 $end
$upscope $end
$scope module m0 $end
$var wire 1 # secret $end
$upscope $end
$scope module m1 $end
$var wire 1 $ secret $end
$upscope $end
$enddefinitions $end
#0
0!
1\"
0#
1$
#5
#10
";
        let xp_vcd = dir.join("xprop_dut.sig.vcd");
        let xp_tb = dir.join("xprop_dut.sig.trace.btor2");
        std::fs::write(&xp_vcd, vcd_text).unwrap();
        std::fs::write(&xp_tb, trace_btor).unwrap();

        let spv_vcd = dir.join("spv_shared.vcd");
        let spv_tb = dir.join("spv_shared.trace.btor2");
        std::fs::write(&spv_vcd, vcd_text).unwrap();
        std::fs::write(&spv_tb, trace_btor).unwrap();

        let mut s = Session::new(Vec::new(), dir.clone());
        s.design.top = Some("dut".into());
        s.xprop.items.push(XpropItem {
            name: "dut.sig".into(),
            category: "output".into(),
            status: XpropStatus::Cex { depth: Some(0) },
            engine: Some("bmc".into()),
            ms: 1,
            trace: Some(xp_vcd.display().to_string()),
        });
        s.xprop.items.push(XpropItem {
            name: "shared".into(),
            category: "output".into(),
            status: XpropStatus::Cex { depth: Some(0) },
            engine: Some("bmc".into()),
            ms: 1,
            trace: Some(xp_vcd.display().to_string()),
        });
        s.xprop.items.push(XpropItem {
            name: "dut.clean".into(),
            category: "output".into(),
            status: XpropStatus::Proven,
            engine: Some("ic3".into()),
            ms: 1,
            trace: None,
        });
        s.spv.push(SpvRule {
            name: "shared".into(),
            task: "default".into(),
            from: vec!["secret".into()],
            to: vec!["out".into()],
            cond: None,
            status: SpvStatus::Cex { depth: Some(0) },
            engine: Some("bmc".into()),
            ms: 1,
            trace: Some(spv_vcd.display().to_string()),
        });
        s.spv.push(SpvRule {
            name: "clean_spv".into(),
            task: "default".into(),
            from: vec!["secret".into()],
            to: vec!["out".into()],
            cond: None,
            status: SpvStatus::Proven,
            engine: Some("ic3".into()),
            ms: 1,
            trace: None,
        });

        // Short name via `s.design.top` works for xprop, and `miter_first_step` is filtered!
        let rep_xp = slice_property_cex(&s, "sig", 64).unwrap();
        assert_eq!(rep_xp.property, "dut.sig");
        assert_eq!(rep_xp.status, "cex");
        assert!(!rep_xp.active_signals.iter().any(|n| n.contains("miter_")));
        assert!(!rep_xp.events.is_empty());

        // Unprefixed `shared` prefers `xprop` before `spv`, while `spv::shared` selects `spv`.
        let rep_unpref = slice_property_cex(&s, "shared", 64).unwrap();
        assert_eq!(rep_unpref.property, "shared");
        let rep_spv = slice_property_cex(&s, "spv::shared", 64).unwrap();
        assert_eq!(rep_spv.property, "shared");

        // Proven items without trace return explicit errors.
        let err_xp = slice_property_cex(&s, "dut.clean", 64).unwrap_err();
        assert!(err_xp.contains("no counterexample trace"));
        let err_spv = slice_property_cex(&s, "clean_spv", 64).unwrap_err();
        assert!(err_spv.contains("no counterexample trace"));
        assert!(slice_property_cex(&s, "spv::dut.sig", 64).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slice_resolves_sva_props_in_xprop_spv_sec_tasks() {
        use crate::session::{Kind, Property};

        let dir = temp_dir("task-prefix-fallback");
        let trace_btor = "\
1 sort bitvec 1
2 input 1 din
3 bad 2 p_bad
";
        let vcd_text = "\
$timescale 1ns $end
$scope module top $end
$var wire 1 ! din $end
$upscope $end
$enddefinitions $end
#0
1!
#5
#10
";
        let mut s = Session::new(Vec::new(), dir.clone());
        let mk_prop = |name: &str, task: &str, status: Status, trace: Option<String>| Property {
            name: name.into(),
            kind: Kind::Assert,
            expr: "din == 1'b0".into(),
            enabled: true,
            status,
            trace,
            source: None,
            rtl: false,
            engines: Vec::new(),
            ai: false,
            under_ai: Vec::new(),
            vacuity: None,
            witness_trace: None,
            task: task.into(),
            copy_of: None,
            bound: None,
            proof_core: None,
            from_assert: None,
            rtl_from_assert: false,
            stats: None,
        };

        for (idx, (name, task)) in [
            ("xprop::xprop_task_prop", "xprop"),
            ("spv::spv_task_prop", "spv"),
            ("sec::sec_task_prop", "sec"),
        ]
        .into_iter()
        .enumerate()
        {
            let vcd_path = dir.join(format!("prop_{idx}.vcd"));
            let tb_path = dir.join(format!("prop_{idx}.trace.btor2"));
            std::fs::write(&vcd_path, vcd_text).unwrap();
            std::fs::write(&tb_path, trace_btor).unwrap();
            s.props.push(mk_prop(
                name,
                task,
                Status::Cex {
                    engine: "bmc".into(),
                    depth: Some(0),
                    ms: 1,
                },
                Some(vcd_path.display().to_string()),
            ));
        }

        // Proven SVA property in task `xprop` without trace -> `no counterexample/witness trace`.
        s.props.push(mk_prop(
            "xprop::xprop_clean",
            "xprop",
            Status::Proven {
                engine: "ic3".into(),
                ms: 1,
            },
            None,
        ));

        // Unrelated task suffix `other::xprop::nested` must NOT match exact `xprop::nested`.
        let nested_vcd = dir.join("prop_4.vcd");
        let nested_tb = dir.join("prop_4.trace.btor2");
        std::fs::write(&nested_vcd, vcd_text).unwrap();
        std::fs::write(&nested_tb, trace_btor).unwrap();
        s.props.push(mk_prop(
            "other::xprop::nested",
            "other",
            Status::Cex {
                engine: "bmc".into(),
                depth: Some(0),
                ms: 1,
            },
            Some(nested_vcd.display().to_string()),
        ));

        // Colliding X-prop item `collide` + SVA property `xprop::collide`: explicit `xprop::collide`
        // must still resolve the X-prop item when present.
        let xp_vcd = dir.join("xprop_collide.vcd");
        let xp_tb = dir.join("xprop_collide.trace.btor2");
        std::fs::write(&xp_vcd, vcd_text).unwrap();
        std::fs::write(
            &xp_tb,
            "1 sort bitvec 1\n2 input 1 din__m0\n3 input 1 din__m1\n4 neq 1 2 3\n5 bad 4 collide\n",
        )
        .unwrap();
        let sv_vcd = dir.join("prop_5.vcd");
        let sv_tb = dir.join("prop_5.trace.btor2");
        std::fs::write(&sv_vcd, vcd_text).unwrap();
        std::fs::write(&sv_tb, trace_btor).unwrap();
        s.props.push(mk_prop(
            "xprop::collide",
            "xprop",
            Status::Cex {
                engine: "bmc".into(),
                depth: Some(0),
                ms: 1,
            },
            Some(sv_vcd.display().to_string()),
        ));
        s.xprop.items.push(XpropItem {
            name: "collide".into(),
            category: "output".into(),
            status: XpropStatus::Cex { depth: Some(0) },
            engine: Some("bmc".into()),
            ms: 1,
            trace: Some(xp_vcd.display().to_string()),
        });

        for name in [
            "xprop::xprop_task_prop",
            "spv::spv_task_prop",
            "sec::sec_task_prop",
        ] {
            let rep = slice_property_cex(&s, name, 64).unwrap();
            assert_eq!(rep.property, name);
            assert_eq!(rep.status, "cex");
            assert!(!rep.events.is_empty());
        }

        let clean_err = slice_property_cex(&s, "xprop::xprop_clean", 64).unwrap_err();
        assert!(clean_err.contains("no counterexample/witness trace"));

        assert!(slice_property_cex(&s, "xprop::nested", 64).is_err());

        let rep_collide = slice_property_cex(&s, "xprop::collide", 64).unwrap();
        assert_eq!(rep_collide.property, "collide");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
