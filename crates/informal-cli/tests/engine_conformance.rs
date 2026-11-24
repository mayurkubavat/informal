//! Shared engine conformance, certification, cancellation, and differential
//! portfolio harness.
//!
//! Every in-house engine registered in `informal_bmc::engines()` and
//! `informal_bitwuzla::engines()` (plus any future engine crate wired into
//! `informal-cli`) is verified against:
//! 1. A canonical model matrix spanning safety, combinational, clock-folded,
//!    constrained, unblasted array, and liveness models.
//! 2. Mandatory independent certificate / witness verification on the original
//!    loaded model whenever `capabilities().certifies` is true.
//! 3. `CancelToken` stop-latency (< 1 s after cancel on a hard model).
//! 4. Full portfolio differential runs asserting zero verdict conflicts.

use informal_bmc::{certify, load_aig};
use informal_engine::{
    portfolio, selector, Certificate, CheckRequest, Effort, Engine, EngineKind, ModelFormat,
    RunContext, Verdict,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    Safe,
    Unsafe(Option<u32>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Safety,
    Combinational,
    Array,
    /// Unbounded array invariant requiring word-level IC3 + prophecy (e.g. `informal-ic3sa`).
    ArrayUnbounded,
    Liveness,
    /// Multi-phase recurring liveness requiring SCC/monotonic-bit proving (`informal-rlive`).
    LivenessRecurring,
}

struct ModelCase {
    name: &'static str,
    format: ModelFormat,
    path: PathBuf,
    btor_text: Option<String>,
    expected: Expected,
    category: Category,
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn new_temp(tag: &str) -> TempDir {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "informal-conformance-{tag}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    TempDir(dir)
}

fn models_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models")
}

fn write_inline(dir: &Path, name: &str, text: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, text).expect("write inline model");
    p
}

fn canonical_matrix(dir: &Path) -> Vec<ModelCase> {
    let repo = models_dir();
    let repo_case =
        |file: &'static str, format: ModelFormat, expected: Expected, category: Category| {
            let path = repo.join(file);
            let btor_text = (format == ModelFormat::Btor2)
                .then(|| std::fs::read_to_string(&path).expect("read repo model"));
            ModelCase {
                name: file,
                format,
                path,
                btor_text,
                expected,
                category,
            }
        };

    let comb_safe = "\
1 sort bitvec 4
2 sort bitvec 1
3 input 1 a
4 xor 1 3 3
5 zero 1
6 neq 2 4 5
7 bad 6
";
    let comb_unsafe = "\
1 sort bitvec 4
2 sort bitvec 1
3 input 1 a
4 constd 1 9
5 eq 2 3 4
6 bad 5
";
    let wrap_counter_safe = "\
1 sort bitvec 3
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 constd 1 5
9 eq 2 4 8
10 ite 1 9 3 7
11 next 1 4 10
12 constd 1 7
13 eq 2 4 12
14 bad 13
";
    let constrained_safe = "\
1 sort bitvec 2
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 input 2 en
7 not 2 6
8 constraint 7
9 one 1
10 add 1 4 9
11 ite 1 6 10 4
12 next 1 4 11
13 constd 1 2
14 eq 2 4 13
15 bad 14
";
    let array_unsafe = "\
1 sort bitvec 2
2 sort bitvec 1
3 sort array 1 2
4 zero 2
5 state 3 mem
6 init 3 5 4
7 input 1 a
8 input 1 b
9 one 2
10 write 3 5 7 9
11 next 3 5 10
12 read 2 5 8
13 bad 12
";
    let array_write_guard_safe = "\
1 sort bitvec 64
2 sort bitvec 8
3 sort array 1 2
4 sort bitvec 1
5 zero 2
6 one 2
7 constd 1 16
8 state 3 mem
9 init 3 8 5
10 input 1 waddr
11 input 1 raddr
12 ult 4 10 7
13 write 3 8 10 6
14 ite 3 12 13 8
15 next 3 8 14
16 ugte 4 11 7
17 read 2 8 11
18 neq 4 17 5
19 and 4 16 18
20 bad 19
";
    let array_shadow_safe = "\
1 sort bitvec 64
2 sort bitvec 8
3 sort array 1 2
4 sort bitvec 1
5 zero 2
6 ones 2
7 state 3 m1
8 state 3 m2
9 init 3 7 5
10 init 3 8 6
11 input 1 wa
12 input 1 ra
13 input 2 d
14 xor 2 13 6
15 write 3 7 11 13
16 write 3 8 11 14
17 next 3 7 15
18 next 3 8 16
19 read 2 7 12
20 read 2 8 12
21 xor 2 20 6
22 neq 4 19 21
23 bad 22
";
    let liveness_safe = "\
1 sort bitvec 2
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 constd 1 2
9 eq 2 4 8
10 ite 1 9 3 7
11 next 1 4 10
12 constd 1 3
13 eq 2 4 12
14 justice 1 13 never_three
";
    let liveness_multiphase_safe = "\
1 sort bitvec 16
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 60000
6 state 1 cnt
7 init 1 6 3
8 ult 2 6 5
9 add 1 6 4
10 ite 1 8 9 6
11 next 1 6 10
12 slice 2 6 0 0
13 and 2 8 12
14 justice 1 13 j_oscillate_30k
";

    let inline_btor = |name: &'static str, text: &str, expected: Expected, category: Category| {
        let path = write_inline(dir, name, text);
        ModelCase {
            name,
            format: ModelFormat::Btor2,
            path,
            btor_text: Some(text.to_string()),
            expected,
            category,
        }
    };

    vec![
        repo_case(
            "stuck_latch_safe.aag",
            ModelFormat::Aiger,
            Expected::Safe,
            Category::Safety,
        ),
        repo_case(
            "toggle_unsafe.aag",
            ModelFormat::Aiger,
            Expected::Unsafe(Some(1)),
            Category::Safety,
        ),
        repo_case(
            "counter2_unsafe.aag",
            ModelFormat::Aiger,
            Expected::Unsafe(Some(3)),
            Category::Safety,
        ),
        repo_case(
            "delay_safe.aag",
            ModelFormat::Aiger,
            Expected::Safe,
            Category::Safety,
        ),
        repo_case(
            "clocked_safe.aag",
            ModelFormat::Aiger,
            Expected::Safe,
            Category::Safety,
        ),
        repo_case(
            "split_safe.aag",
            ModelFormat::Aiger,
            Expected::Safe,
            Category::Safety,
        ),
        repo_case(
            "split_unsafe.aag",
            ModelFormat::Aiger,
            Expected::Unsafe(Some(2)),
            Category::Safety,
        ),
        repo_case(
            "counter.btor2",
            ModelFormat::Btor2,
            Expected::Unsafe(Some(7)),
            Category::Safety,
        ),
        repo_case(
            "memory.btor2",
            ModelFormat::Btor2,
            Expected::Safe,
            Category::Array,
        ),
        repo_case(
            "liveness.btor2",
            ModelFormat::Btor2,
            Expected::Unsafe(None),
            Category::Liveness,
        ),
        inline_btor(
            "comb_safe.btor2",
            comb_safe,
            Expected::Safe,
            Category::Combinational,
        ),
        inline_btor(
            "comb_unsafe.btor2",
            comb_unsafe,
            Expected::Unsafe(Some(0)),
            Category::Combinational,
        ),
        inline_btor(
            "wrap_counter_safe.btor2",
            wrap_counter_safe,
            Expected::Safe,
            Category::Safety,
        ),
        inline_btor(
            "constrained_safe.btor2",
            constrained_safe,
            Expected::Safe,
            Category::Safety,
        ),
        inline_btor(
            "array_unsafe.btor2",
            array_unsafe,
            Expected::Unsafe(Some(1)),
            Category::Array,
        ),
        inline_btor(
            "array_write_guard_safe.btor2",
            array_write_guard_safe,
            Expected::Safe,
            Category::ArrayUnbounded,
        ),
        inline_btor(
            "array_shadow_safe.btor2",
            array_shadow_safe,
            Expected::Safe,
            Category::ArrayUnbounded,
        ),
        inline_btor(
            "liveness_safe.btor2",
            liveness_safe,
            Expected::Safe,
            Category::Liveness,
        ),
        inline_btor(
            "liveness_multiphase_safe.btor2",
            liveness_multiphase_safe,
            Expected::Safe,
            Category::LivenessRecurring,
        ),
    ]
}

fn in_house_fleet() -> Option<Vec<Arc<dyn Engine>>> {
    let mut all = informal_bmc::engines();
    if all.is_empty() {
        informal_engine::tool_missing("needs CaDiCaL");
        return None;
    }
    all.extend(informal_liveness::engines());
    all.extend(informal_bdd::engines());
    all.extend(informal_imc::engines());
    all.extend(informal_bitwuzla::engines());
    Some(all)
}

fn decode_bits(s: &str) -> Vec<bool> {
    s.bytes().map(|b| b == b'1').collect()
}

/// Independently verify an attached [`Certificate`] against the original loaded
/// model (`load_aig`) and confirm `certificate::write` succeeds cleanly.
fn verify_certificate(
    engine: &dyn Engine,
    case: &ModelCase,
    verdict: &Verdict,
    cert: &Certificate,
    cert_dir: &Path,
) {
    let orig = load_aig(&case.path)
        .unwrap_or_else(|e| panic!("{} / {}: reload failed: {e:#}", engine.name(), case.name));
    match (verdict, cert) {
        (Verdict::Safe, Certificate::Invariant { clauses }) => {
            certify::check(&orig, clauses, &informal_sat::best).unwrap_or_else(|e| {
                panic!(
                    "{} / {}: invariant certificate failed: {e}",
                    engine.name(),
                    case.name
                )
            });
        }
        (Verdict::Safe, Certificate::TwoStep { clauses }) => {
            certify::check_folded(&orig, clauses, &informal_sat::best).unwrap_or_else(|e| {
                panic!(
                    "{} / {}: two-step invariant certificate failed: {e}",
                    engine.name(),
                    case.name
                )
            });
        }
        (Verdict::Safe, Certificate::KInduction { k, clauses, .. }) => {
            certify::check_kind_paths(
                &orig,
                clauses,
                true,
                *k as usize,
                &informal_sat::best,
                &|| false,
            )
            .unwrap_or_else(|e| {
                panic!(
                    "{} / {}: k-induction certificate failed: {e}",
                    engine.name(),
                    case.name
                )
            });
        }
        (Verdict::Safe, Certificate::TwoStepKInduction { k, clauses, .. }) => {
            certify::check_kind_folded(&orig, clauses, *k as usize, &informal_sat::best, &|| false)
                .unwrap_or_else(|e| {
                    panic!(
                        "{} / {}: two-step k-induction certificate failed: {e}",
                        engine.name(),
                        case.name
                    )
                });
        }
        (Verdict::Unsafe { depth }, Certificate::Trace { init, inputs }) => {
            let d = depth.expect("certified UNSAFE must include depth") as usize;
            let trace = certify::Trace {
                init: decode_bits(init),
                inputs: inputs.iter().map(|f| decode_bits(f)).collect(),
            };
            // Liveness-only engines certify lassos against the raw AIG, not the
            // l2s-expanded AIG; safety engines replay directly on `orig`.
            if engine.capabilities().kind != EngineKind::Liveness {
                certify::replay_unsafe(&orig, d, &trace).unwrap_or_else(|e| {
                    panic!(
                        "{} / {}: trace certificate failed replay: {e}",
                        engine.name(),
                        case.name
                    )
                });
            }
        }
        (v, c) => panic!(
            "{} / {}: mismatched verdict {:?} and certificate {:?}",
            engine.name(),
            case.name,
            v,
            c.kind()
        ),
    }

    if engine.capabilities().kind != EngineKind::Liveness {
        informal_bmc::certificate::write(cert_dir, &case.path, engine.name(), verdict, Some(cert))
            .unwrap_or_else(|e| {
                panic!(
                    "{} / {}: certificate::write failed: {e:#}",
                    engine.name(),
                    case.name
                )
            });
    }
}

fn check_verdict_matches(engine: &dyn Engine, case: &ModelCase, verdict: &Verdict) {
    match (case.expected, verdict) {
        (Expected::Safe, Verdict::Safe) => {}
        (Expected::Unsafe(want_depth), Verdict::Unsafe { depth }) => {
            // Stepped BMC (`deep_step` or step > 1) may find a valid bug at a
            // slightly deeper frame than shortest; exact-step engines match depth.
            if let (Some(want), Some(got)) = (want_depth, *depth) {
                if !engine.capabilities().deep_step && !engine.name().ends_with("-10") {
                    assert_eq!(
                        got,
                        want,
                        "{} / {}: expected depth {want}, got {got}",
                        engine.name(),
                        case.name
                    );
                } else {
                    assert!(
                        got >= want,
                        "{} / {}: impossible depth {got} < {want}",
                        engine.name(),
                        case.name
                    );
                }
            }
        }
        (Expected::Safe, Verdict::Unsafe { depth }) => panic!(
            "UNSOUND: {} reported UNSAFE({depth:?}) on Safe model {}",
            engine.name(),
            case.name
        ),
        (Expected::Unsafe(_), Verdict::Safe) => panic!(
            "UNSOUND: {} reported SAFE on Unsafe model {}",
            engine.name(),
            case.name
        ),
        (_, Verdict::Unknown { .. }) => {}
    }
}

#[test]
fn every_in_house_engine_conforms_and_certifies_canonical_matrix() {
    let Some(fleet) = in_house_fleet() else {
        return;
    };
    let dir = new_temp("matrix");
    let cases = canonical_matrix(&dir.0);
    let cert_root = dir.0.join("certs");

    for case in &cases {
        for engine in &fleet {
            let caps = engine.capabilities();
            if !caps.formats.contains(&case.format) {
                continue;
            }
            let is_liveness = matches!(
                case.category,
                Category::Liveness | Category::LivenessRecurring
            );
            if is_liveness && !caps.liveness {
                continue;
            }
            if !is_liveness && caps.kind == EngineKind::Liveness {
                continue;
            }
            if matches!(case.category, Category::Array | Category::ArrayUnbounded) && !caps.arrays {
                continue;
            }
            // Unbounded array-invariant models require word-level IC3 + prophecy;
            // recurring multi-phase liveness requires `informal-rlive`.
            let require_prove = match case.category {
                Category::ArrayUnbounded => caps.kind == EngineKind::Ic3,
                Category::LivenessRecurring => engine.name() == "informal-rlive",
                _ => true,
            };

            let mut req =
                CheckRequest::new(case.path.clone(), case.format, Duration::from_secs(15));
            req.effort = Effort::Standard;
            if (!caps.proves || !require_prove) && case.expected == Expected::Safe {
                req.max_depth = Some(6);
            }
            if !require_prove {
                req.timeout = Duration::from_millis(250);
            }
            let ctx = RunContext::new(req.timeout);
            let res = engine
                .check(&req, &ctx)
                .unwrap_or_else(|e| panic!("{} / {}: error {e:#}", engine.name(), case.name));

            check_verdict_matches(engine.as_ref(), case, &res.verdict);

            // Capability soundness invariants:
            if !caps.proves {
                assert_ne!(
                    res.verdict,
                    Verdict::Safe,
                    "{} declared proves=false but returned Safe on {}",
                    engine.name(),
                    case.name
                );
            }
            if !caps.finds_bugs {
                assert!(
                    !matches!(res.verdict, Verdict::Unsafe { .. }),
                    "{} declared finds_bugs=false but returned Unsafe on {}",
                    engine.name(),
                    case.name
                );
            }

            // Completeness: every eligible engine must solve these canonical models.
            match case.expected {
                Expected::Safe if caps.proves && require_prove => {
                    assert_eq!(
                        res.verdict,
                        Verdict::Safe,
                        "{} failed to prove canonical Safe model {}: {:?}",
                        engine.name(),
                        case.name,
                        res.verdict
                    );
                }
                Expected::Unsafe(_) if caps.finds_bugs => {
                    assert!(
                        matches!(res.verdict, Verdict::Unsafe { .. }),
                        "{} failed to refute canonical Unsafe model {}: {:?}",
                        engine.name(),
                        case.name,
                        res.verdict
                    );
                }
                _ => {}
            }

            // Mandatory certificate check for certifying engines.
            if caps.certifies && res.verdict.is_definitive() {
                let must_attach = caps.kind != EngineKind::Liveness
                    || matches!(res.verdict, Verdict::Unsafe { .. });
                if must_attach {
                    let cert = res.certificate.as_ref().unwrap_or_else(|| {
                        panic!(
                            "{} declared certifies=true but returned {:?} without certificate on {}",
                            engine.name(),
                            res.verdict,
                            case.name
                        )
                    });
                    let out_dir = cert_root.join(format!("{}_{}", engine.name(), case.name));
                    verify_certificate(engine.as_ref(), case, &res.verdict, cert, &out_dir);
                }
            }
        }
    }
}

#[test]
fn every_engine_honors_cancel_token_under_one_second() {
    let Some(fleet) = in_house_fleet() else {
        return;
    };
    let dir = new_temp("cancel");

    // 24-bit counter targeting 2^24 - 1: reachable only at depth 16,777,215,
    // so no engine can solve it in 50ms.
    let hard_safety = write_inline(
        &dir.0,
        "hard_safety.btor2",
        "\
1 sort bitvec 24
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 next 1 4 7
9 ones 1
10 eq 2 4 9
11 bad 10
",
    );

    // 24-bit counter with liveness target (cnt == 2^24 - 1) so liveness engines
    // genuinely work and must respond to CancelToken promptly.
    let hard_liveness = write_inline(
        &dir.0,
        "hard_liveness.btor2",
        "\
1 sort bitvec 24
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 next 1 4 7
9 ones 1
10 eq 2 4 9
11 justice 1 10 reach_max
",
    );

    for engine in &fleet {
        let caps = engine.capabilities();
        let model = if caps.kind == EngineKind::Liveness {
            hard_liveness.clone()
        } else {
            hard_safety.clone()
        };
        let req = CheckRequest::new(model, ModelFormat::Btor2, Duration::from_secs(30));
        let ctx = RunContext::new(req.timeout);
        let cancel = ctx.cancel.clone();

        let (t_cancel, res) = std::thread::scope(|s| {
            let h = s.spawn(move || {
                std::thread::sleep(Duration::from_millis(60));
                let at = Instant::now();
                cancel.cancel();
                at
            });
            let res = engine.check(&req, &ctx);
            let at = h.join().unwrap();
            (at, res)
        });
        let stop_latency = Instant::now().saturating_duration_since(t_cancel);
        let res = res.unwrap_or_else(|e| panic!("{} errored on cancel: {e:#}", engine.name()));

        assert!(
            !res.verdict.is_definitive(),
            "{} returned spurious definitive verdict {:?} on hard 24-bit model",
            engine.name(),
            res.verdict
        );
        assert!(
            stop_latency < Duration::from_secs(1),
            "{} took {:?} after CancelToken::cancel() (>= 1s)",
            engine.name(),
            stop_latency
        );
    }
}

fn program_on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let p = dir.join(bin);
            p.is_file()
        })
    })
}

#[test]
fn portfolio_differential_zero_conflicts_across_canonical_matrix() {
    let Some(mut fleet) = in_house_fleet() else {
        return;
    };
    // Add available external engines (skipping Linux-only `ric3` portfolio on macOS).
    let cfg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/engines.toml");
    if let Ok(ext) = informal_engines_ext::load(&cfg) {
        let has_ric3 = program_on_path("ric3");
        let has_abc = program_on_path("abc");
        let has_pono = program_on_path("pono");
        let has_btormc = program_on_path("btormc");
        for e in ext {
            let name = e.name();
            if cfg!(target_os = "macos") && name == "ric3" {
                continue;
            }
            let available = (name.starts_with("ric3") && has_ric3)
                || (name.starts_with("abc") && has_abc)
                || (name.starts_with("pono") && has_pono)
                || (name == "btormc" && has_btormc);
            if available {
                fleet.push(e);
            }
        }
    }

    let dir = new_temp("portfolio");
    let cases = canonical_matrix(&dir.0);

    for case in &cases {
        let selected = match &case.btor_text {
            Some(text) => {
                selector::select_for_btor_with_effort(&fleet, text, false, Effort::Standard)
            }
            None => fleet
                .iter()
                .filter(|e| {
                    let c = e.capabilities();
                    c.formats.contains(&case.format) && c.kind != EngineKind::Liveness
                })
                .cloned()
                .collect(),
        };
        assert!(
            !selected.is_empty(),
            "selector returned empty fleet for {}",
            case.name
        );

        let req = CheckRequest::new(case.path.clone(), case.format, Duration::from_secs(20));
        let outcome = portfolio::run(&selected, &req, None);
        assert!(
            outcome.conflicts.is_empty(),
            "engine conflict on {}: {:?}",
            case.name,
            outcome.conflicts
        );
        let v = portfolio::verdict(&outcome);
        match (case.expected, &v) {
            (Expected::Safe, Verdict::Safe) => {}
            (Expected::Unsafe(_), Verdict::Unsafe { .. }) => {}
            _ => panic!(
                "portfolio verdict mismatch on {}: expected {:?}, got {:?} (profile: {:?})",
                case.name, case.expected, v, outcome.profile
            ),
        }
    }
}
