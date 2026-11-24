//! In-house bit-level verification engines and shared AIG prep/certification.
//!
//! Shared bit-level preparation ([`prep`]) and independent verdict
//! certification ([`verify`]) are exported so each engine — BMC ([`bmc`]),
//! k-induction ([`kind`]), IC3/PDR ([`ic3_engine`]), k-liveness ([`klive`]),
//! and future bit-level engines — lives in its own modular crate/file behind
//! [`informal_engine::Engine`].

pub mod bmc;
pub mod certificate;
pub mod certify;
pub mod cnf;
pub mod deadend;
pub mod ic3;
pub mod ic3_engine;
pub mod invariant;
pub mod kind;
pub mod klive;
pub mod mine;
pub mod prep;
pub mod scorr;
pub mod store;
pub mod unroll;
pub mod verify;

use informal_engine::{Effort, Engine};
use std::sync::Arc;

pub use bmc::Bmc;
pub use ic3_engine::{Ic3Engine, Ic3Mt};
pub use informal_aig::load as load_aig;
pub use kind::{KInduction, KindIc3};
pub use prep::{
    fold_on, prepare_ctx, prepare_ctx_fold, result, scorr_tier, solver, unknown, Checked, Folded,
    Prepared, FORMATS,
};
pub use verify::{
    certified, certify_lemmas, kind_certified, kind_unsafe, mine_into, mine_invariant,
    report_certificate, store_invariant, stored_invariant, unsafe_certified, unsafe_certified_at,
    PreparedTrace,
};

/// The in-house bit-level engines, for adding to a portfolio. Empty when this
/// build has no CaDiCaL: the fallback solver is too slow to be worth a thread.
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if informal_sat::Cadical::signature().is_none() {
        return Vec::new();
    }
    vec![
        Arc::new(Bmc {
            max_depth: None,
            step: 1,
        }),
        Arc::new(Bmc {
            max_depth: None,
            step: 10,
        }),
        Arc::new(Bmc {
            max_depth: None,
            step: 65,
        }),
        Arc::new(KInduction { max_depth: None }),
        Arc::new(Ic3Engine { mini: true }),
        Arc::new(Ic3Engine { mini: false }),
        Arc::new(KindIc3 { ic3_share: 0.5 }),
        Arc::new(Ic3Mt::default()),
        Arc::new(klive::KLive),
    ]
}

/// Structural feature-driven portfolio selector over `engines` for a Btor2 model.
///
/// Delegates to [`informal_engine::selector::select_for_btor`].
pub fn select_for_btor(
    engines: &[Arc<dyn Engine>],
    btor_text: &str,
    explicit_filter: bool,
) -> Vec<Arc<dyn Engine>> {
    informal_engine::selector::select_for_btor(engines, btor_text, explicit_filter)
}

/// Structural feature-driven portfolio selector over `engines` for a Btor2 model
/// at `effort`. Delegates to [`informal_engine::selector::select_for_btor_with_effort`].
pub fn select_for_btor_with_effort(
    engines: &[Arc<dyn Engine>],
    btor_text: &str,
    explicit_filter: bool,
    effort: Effort,
) -> Vec<Arc<dyn Engine>> {
    informal_engine::selector::select_for_btor_with_effort(
        engines,
        btor_text,
        explicit_filter,
        effort,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prep::prepare;
    use crate::unroll::Unroll;
    use informal_aig::Aig;
    use informal_engine::{CheckRequest, ModelFormat, RunContext, Verdict};
    use informal_sat::Answer;
    use std::time::Duration;

    fn check(e: &dyn Engine, text: &str) -> Verdict {
        let dir =
            std::env::temp_dir().join(format!("informal-bmc-{}-{}", std::process::id(), e.name()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("m{}.btor2", text.len()));
        std::fs::write(&path, text).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(60));
        e.check(&req, &RunContext::new(req.timeout))
            .unwrap()
            .verdict
    }

    /// 3-bit counter from 0, bad when it reaches `target`; `wrap` makes it
    /// count to 5 then return to 0.
    fn counter(target: u32, wrap: bool) -> String {
        let mut s = String::from("1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 cnt\n5 init 1 4 3\n6 one 1\n7 add 1 4 6\n");
        if wrap {
            s += "8 constd 1 5\n9 eq 2 4 8\n10 ite 1 9 3 7\n11 next 1 4 10\n";
        } else {
            s += "8 constd 1 0\n9 eq 2 4 8\n11 next 1 4 7\n";
        }
        s += &format!("12 constd 1 {target}\n13 eq 2 4 12\n14 bad 13\n");
        s
    }

    /// H1 (feedback report): asserting "no bad output" on a model with
    /// several bad outputs must rule out each of them.
    #[test]
    fn no_bad_rules_out_every_bad_output() {
        let mut g = Aig::new();
        let (a, b) = (g.input(), g.input());
        g.bads.push(a);
        g.bads.push(b);
        let mut u = Unroll::new(&g, informal_sat::best(), false);
        u.add_frame();
        let x = u.bad(0);
        u.sat.add_clause(&[-x]);
        for l in [a, b] {
            let y = u.lit(0, l);
            assert_eq!(u.sat.solve(&[y]), Answer::Unsat);
        }
    }

    #[test]
    fn bmc_finds_shortest_counterexample() {
        let bmc = Bmc {
            max_depth: Some(10),
            step: 1,
        };
        assert_eq!(
            check(&bmc, &counter(4, false)),
            Verdict::Unsafe { depth: Some(4) }
        );
        assert!(matches!(
            check(&bmc, &counter(7, true)),
            Verdict::Unknown { .. }
        ));
    }

    #[test]
    fn stepped_bmc_finds_bugs_and_respects_bounds() {
        let b = Bmc {
            max_depth: Some(30),
            step: 10,
        };
        assert_eq!(
            check(&b, &counter(4, false)),
            Verdict::Unsafe { depth: Some(4) }
        );
        assert_eq!(
            check(&b, &counter(0, false)),
            Verdict::Unsafe { depth: Some(0) }
        );
        assert!(matches!(
            check(&b, &counter(7, true)),
            Verdict::Unknown { .. }
        ));
    }

    #[test]
    fn kinduction_proves_and_refutes() {
        let kind = KInduction {
            max_depth: Some(20),
        };
        assert_eq!(
            check(&kind, &counter(6, false)),
            Verdict::Unsafe { depth: Some(6) }
        );
        assert_eq!(check(&kind, &counter(7, true)), Verdict::Safe);
        assert_eq!(check(&kind, &counter(6, true)), Verdict::Safe);
        assert_eq!(
            check(&kind, &counter(5, true)),
            Verdict::Unsafe { depth: Some(5) }
        );
    }

    /// Word operators against Rust arithmetic: with constant operands,
    /// `bad = (a op b != expected)` must be unreachable at depth 0.
    #[test]
    fn operators_match_reference() {
        let bmc = Bmc {
            max_depth: Some(0),
            step: 1,
        };
        let w = 6u32;
        let mask = (1u64 << w) - 1;
        let sx = |v: u64| -> i64 { ((v << (64 - w)) as i64) >> (64 - w) };
        type Unsigned = fn(u64, u64, u32) -> u64;
        type Signed = fn(i64, i64) -> i64;
        let cases: &[(&str, Unsigned)] = &[
            ("add", |a, b, _| a + b),
            ("sub", |a, b, _| a.wrapping_sub(b)),
            ("mul", |a, b, _| a * b),
            ("udiv", |a, b, _| a.checked_div(b).unwrap_or(u64::MAX)),
            ("urem", |a, b, _| if b == 0 { a } else { a % b }),
            ("sll", |a, b, w| if b >= w as u64 { 0 } else { a << b }),
            ("srl", |a, b, w| if b >= w as u64 { 0 } else { a >> b }),
        ];
        let signed: &[(&str, Signed)] = &[
            ("sdiv", |a, b| {
                if b == 0 {
                    if a < 0 {
                        1
                    } else {
                        -1
                    }
                } else {
                    a.wrapping_div(b)
                }
            }),
            ("srem", |a, b| if b == 0 { a } else { a.wrapping_rem(b) }),
            // SMT-LIB bvsmod: the sign follows the divisor.
            ("smod", |a, b| {
                if b == 0 {
                    return a;
                }
                let r = a % b;
                if r != 0 && (r < 0) != (b < 0) {
                    r + b
                } else {
                    r
                }
            }),
        ];
        let mut seed = 12345u64;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed & mask
        };
        let model = |op: &str, a: u64, b: u64, c: u64| {
            format!("1 sort bitvec {w}\n2 sort bitvec 1\n3 constd 1 {a}\n4 constd 1 {b}\n5 {op} 1 3 4\n6 constd 1 {c}\n7 neq 2 5 6\n8 bad 7\n")
        };
        for _ in 0..12 {
            let (a, b) = (rnd(), rnd());
            for (op, f) in cases {
                let c = f(a, b, w) & mask;
                assert!(
                    matches!(check(&bmc, &model(op, a, b, c)), Verdict::Unknown { .. }),
                    "{op} {a} {b} -> {c}"
                );
            }
            for (op, f) in signed {
                let (x, y) = (sx(a), sx(b));
                let r = f(x, y);
                let c = (r as u64) & mask;
                assert!(
                    matches!(check(&bmc, &model(op, a, b, c)), Verdict::Unknown { .. }),
                    "{op} {x} {y} -> {r}"
                );
            }
        }
    }

    #[test]
    fn ic3_proves_and_refutes() {
        for e in [Ic3Engine { mini: true }, Ic3Engine { mini: false }] {
            assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
            assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
            assert_eq!(
                check(&e, &counter(5, true)),
                Verdict::Unsafe { depth: Some(5) }
            );
            assert_eq!(
                check(&e, &counter(4, false)),
                Verdict::Unsafe { depth: Some(4) }
            );
            assert_eq!(
                check(&e, &counter(0, false)),
                Verdict::Unsafe { depth: Some(0) }
            );
        }
    }

    #[test]
    fn ic3_mt_proves_and_refutes() {
        let e = Ic3Mt::default();
        for _ in 0..5 {
            assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
            assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
            assert_eq!(
                check(&e, &counter(5, true)),
                Verdict::Unsafe { depth: Some(5) }
            );
            assert_eq!(
                check(&e, &counter(4, false)),
                Verdict::Unsafe { depth: Some(4) }
            );
        }
    }

    /// Two copies of the wrapping counter, bad when they differ or when
    /// the first reaches 7: scorr merges the copies (3 latches left) and
    /// every engine still proves it; a reachable target stays unsafe.
    #[test]
    fn scorr_merges_copies() {
        let twin = |target: u32| {
            format!(
                "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 a\n5 init 1 4 3\n\
                 6 one 1\n7 add 1 4 6\n8 constd 1 5\n9 eq 2 4 8\n10 ite 1 9 3 7\n11 next 1 4 10\n\
                 12 state 1 b\n13 init 1 12 3\n14 add 1 12 6\n15 eq 2 12 8\n16 ite 1 15 3 14\n\
                 17 next 1 12 16\n18 neq 2 4 12\n19 constd 1 {target}\n20 eq 2 4 19\n21 or 2 18 20\n22 bad 21\n"
            )
        };
        let dir = std::env::temp_dir().join(format!("informal-scorr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("twin.btor2");
        std::fs::write(&path, twin(7)).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let ctx = RunContext::new(req.timeout);
        let g = prepare(&req, false).unwrap().aig;
        assert_eq!(g.latches.len(), 6);
        let (h, st, _, _) = scorr::latch_correspondence(&g, solver(&ctx), &ctx).unwrap();
        assert_eq!(st.merged, 3);
        assert_eq!(h.latches.len(), 3);
        let ic3 = Ic3Engine { mini: true };
        let kind = KInduction {
            max_depth: Some(20),
        };
        assert_eq!(check(&ic3, &twin(7)), Verdict::Safe);
        assert_eq!(check(&kind, &twin(7)), Verdict::Safe);
        assert_eq!(check(&ic3, &twin(4)), Verdict::Unsafe { depth: Some(4) });
        assert_eq!(check(&kind, &twin(4)), Verdict::Unsafe { depth: Some(4) });
    }

    /// Two copies of a counter, bad only when they differ. After scorr
    /// merges them the property is constant and no latch is left in its
    /// cone, so the invariant the engines hand over is empty: the
    /// certificate must carry the merges themselves, or a correct SAFE is
    /// withdrawn.
    #[test]
    fn merged_latches_are_part_of_the_certificate() {
        let twin = "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 a\n5 init 1 4 3\n\
                    6 one 1\n7 add 1 4 6\n11 next 1 4 7\n\
                    12 state 1 b\n13 init 1 12 3\n14 add 1 12 6\n17 next 1 12 14\n\
                    18 neq 2 4 12\n22 bad 18\n";
        let engines: [&dyn Engine; 5] = [
            &Ic3Engine { mini: true },
            &Ic3Engine { mini: false },
            &Ic3Mt::default(),
            &KInduction {
                max_depth: Some(20),
            },
            &KindIc3 { ic3_share: 0.5 },
        ];
        for e in engines {
            assert_eq!(check(e, twin), Verdict::Safe, "{}", e.name());
        }
    }

    /// A counter that moves on every other step (a free-running clock
    /// latch gates it), wrapping at 5. The k-induction engines fold the
    /// clock, so their proofs and counterexamples are on the folded model
    /// and certified on the loaded one: 7 is never reached, 5 is reached
    /// at loaded depth 10 (the counter moves on odd steps).
    #[test]
    fn kinduction_folds_a_clock() {
        let clocked = |target: u32| {
            format!(
                "1 sort bitvec 3\n2 sort bitvec 1\n3 zero 1\n4 state 1 cnt\n5 init 1 4 3\n\
                 6 zero 2\n7 state 2 clk\n8 init 2 7 6\n9 not 2 7\n10 next 2 7 9\n\
                 11 one 1\n12 add 1 4 11\n13 constd 1 5\n14 eq 2 4 13\n15 ite 1 14 3 12\n\
                 16 ite 1 7 15 4\n17 next 1 4 16\n18 constd 1 {target}\n19 eq 2 4 18\n20 bad 19\n"
            )
        };
        let engines: [&dyn Engine; 2] = [
            &KInduction {
                max_depth: Some(40),
            },
            &KindIc3 { ic3_share: 0.5 },
        ];
        for e in engines {
            assert_eq!(check(e, &clocked(7)), Verdict::Safe, "{}", e.name());
            assert_eq!(check(e, &clocked(6)), Verdict::Safe, "{}", e.name());
            assert_eq!(
                check(e, &clocked(5)),
                Verdict::Unsafe { depth: Some(10) },
                "{}",
                e.name()
            );
        }
    }

    #[test]
    fn kind_ic3_proves_and_refutes() {
        let e = KindIc3 { ic3_share: 0.5 };
        assert_eq!(check(&e, &counter(7, true)), Verdict::Safe);
        assert_eq!(check(&e, &counter(6, true)), Verdict::Safe);
        assert_eq!(
            check(&e, &counter(5, true)),
            Verdict::Unsafe { depth: Some(5) }
        );
    }

    /// Houdini keeps exactly the inductive lemmas: for the counter that
    /// wraps at 5, "not 6" and "not 7" are inductive together, "not 3" is
    /// not even true.
    #[test]
    fn houdini_keeps_inductive_subset() {
        let dir = std::env::temp_dir().join(format!("informal-houdini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("c.btor2");
        std::fs::write(&path, counter(7, true)).unwrap();
        let g = prepare(
            &CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10)),
            false,
        )
        .unwrap()
        .aig;
        let ctx = RunContext::new(Duration::from_secs(10));
        // Latch bits b0..b2 as AIG literals, lowest bit first.
        let bits: Vec<_> = g.latches.iter().map(|l| l.lit).collect();
        assert_eq!(bits.len(), 3);
        let cube = |v: u32| -> ic3::Cube {
            let mut c: Vec<_> = (0..3)
                .map(|i| {
                    if v >> i & 1 == 1 {
                        bits[i]
                    } else {
                        informal_aig::neg(bits[i])
                    }
                })
                .collect();
            c.sort();
            c
        };
        let inv =
            invariant::houdini(&g, vec![cube(6), cube(7), cube(3)], || solver(&ctx), &ctx).unwrap();
        let mut want = vec![cube(6), cube(7)];
        want.sort();
        assert_eq!(inv, want);
    }

    #[test]
    fn repo_models() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let kind = KInduction {
            max_depth: Some(40),
        };
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        assert_eq!(
            check(&kind, &read("counter.btor2")),
            Verdict::Unsafe { depth: Some(7) }
        );
        // Arrays are reported, not guessed.
        assert!(
            matches!(check(&kind, &read("memory.btor2")), Verdict::Unknown { reason } if reason.contains("arrays"))
        );
        // Liveness is checked as safety (informal_aig::l2s): with req high
        // for ever grant is high for ever, a loop the justice line asks for.
        assert!(matches!(
            check(&kind, &read("liveness.btor2")),
            Verdict::Unsafe { .. }
        ));
    }

    #[test]
    fn d8_unverified_ic3_safe_is_downgraded_and_h3_coupled_push_certifies() {
        if informal_sat::Cadical::signature().is_none() {
            informal_engine::tool_missing("needs CaDiCaL");
            return;
        }
        // Two-latch swap `a <= b; b <= a` starting at `a=1, b=0`, checking `!(a && b)`:
        // `!(a && b)` is inductive and coupled with `a || b`.
        let btor = "\
1 sort bitvec 1
2 zero 1
3 one 1
4 state 1 a
5 state 1 b
6 init 1 4 3
7 init 1 5 2
8 next 1 4 5
9 next 1 5 4
10 and 1 4 5
11 bad 10
";
        let dir = std::env::temp_dir().join(format!("informal-d8h3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("swap.btor2");
        std::fs::write(&path, btor).unwrap();
        let req = CheckRequest::new(path, ModelFormat::Btor2, Duration::from_secs(10));
        let ctx = RunContext::new(req.timeout);
        let p = prepare_ctx_fold(&req, &ctx, false).unwrap();

        // 1. D8 guard: an `Ic3` instance with `proof_level == None` claiming `Verdict::Safe`
        //    must NEVER pass `certified(...)` as `Verdict::Safe`.
        let c1 = ctx.clone();
        let unproved = ic3::Ic3::new(&p, Box::new(move || solver(&c1))).unwrap();
        let checked_bad = certified(&req, &p, &unproved, Verdict::Safe, &ctx);
        assert!(
            matches!(checked_bad.verdict, Verdict::Unknown { .. }),
            "expected unverified Safe to be rejected, got {:?}",
            checked_bad.verdict
        );

        // 2. H3 coupled_push run proves and passes full independent AIG certification:
        let c2 = ctx.clone();
        let mut runner = ic3::Ic3::new(&p, Box::new(move || solver(&c2))).unwrap();
        runner.set_config(ic3::Config {
            coupled_push: true,
            ..ic3::Config::default()
        });
        let v = runner.check(&ctx);
        assert_eq!(v, Verdict::Safe);
        let checked_ok = certified(&req, &p, &runner, v, &ctx);
        assert_eq!(checked_ok.verdict, Verdict::Safe);
        assert!(checked_ok.certificate.is_some());
    }
}
