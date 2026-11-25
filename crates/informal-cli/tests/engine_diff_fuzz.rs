//! Differential random-circuit fuzzer cross-checking all in-house engines
//! (and available external engines: rIC3, ABC) for soundness disagreements,
//! certificate validity, and witness replayability (decision D1/D8).
//!
//! Deterministic by default (`INFORMAL_FUZZ_SEED`, `INFORMAL_FUZZ_CASES`) so
//! `./scripts/check.sh` runs quickly (~5-10 s), while manual or overnight fuzz
//! sweeps can scale `INFORMAL_FUZZ_CASES=500` with any seed.

use informal_bmc::{certify, load_aig};
use informal_engine::{
    Certificate, CheckRequest, Engine, EngineKind, ModelFormat, RunContext, Verdict,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Deterministic SplitMix64 PRNG (zero external dependencies).
#[derive(Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: usize, hi_excl: usize) -> usize {
        lo + (self.next_u64() as usize % (hi_excl - lo).max(1))
    }
    fn coin(&mut self, num: usize, den: usize) -> bool {
        (self.next_u64() as usize % den) < num
    }
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
        "informal-diff-fuzz-{tag}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    TempDir(dir)
}

fn fuzz_seed() -> u64 {
    std::env::var("INFORMAL_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_cafe_1800_2026)
}

fn fuzz_cases(default: usize) -> usize {
    std::env::var("INFORMAL_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
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
    all.extend(informal_car::engines());
    all.extend(informal_ic3::engines());
    all.extend(informal_swarm::engines());
    all.extend(informal_bitwuzla::engines());
    Some(all)
}

fn available_external_oracles() -> Vec<Arc<dyn Engine>> {
    let cfg = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/engines.toml");
    let Ok(all) = informal_engines_ext::load(&cfg) else {
        return Vec::new();
    };
    let want = ["ric3-ic3", "ric3-bmc"];
    let path_env = std::env::var_os("PATH");
    let has_bin = |bin: &str| {
        path_env
            .as_ref()
            .is_some_and(|p| std::env::split_paths(p).any(|d| d.join(bin).is_file()))
    };
    all.into_iter()
        .filter(|e| want.contains(&e.name()))
        .filter(|e| {
            if e.name().starts_with("ric3") {
                has_bin("ric3")
            } else if e.name().starts_with("abc") {
                has_bin("abc")
            } else {
                false
            }
        })
        .collect()
}

/// Generate a random sequential ASCII AIGER 1.9 safety circuit (optionally with
/// a free-running toggle clock latch and/or invariant constraint).
fn gen_random_aag(rng: &mut Rng) -> String {
    let n_in = rng.range(1, 4);
    let n_latch = rng.range(2, 6);
    let n_and = rng.range(5, 16);
    let has_clock = rng.coin(1, 3);
    let has_constraint = rng.coin(1, 3);

    let max_var = n_in + n_latch + n_and;
    let first_latch = n_in + 1;
    let first_and = n_in + n_latch + 1;

    // Pick random AND gate definitions referencing strictly smaller variables.
    let mut ands = Vec::with_capacity(n_and);
    for idx in 0..n_and {
        let lhs_var = first_and + idx;
        let pick_lit = |rng: &mut Rng| -> usize {
            let v = rng.range(0, lhs_var); // 0 = constant FALSE
            (v * 2) | rng.range(0, 2)
        };
        let a = pick_lit(rng);
        let b = pick_lit(rng);
        ands.push((lhs_var * 2, a, b));
    }

    let any_lit = |rng: &mut Rng| -> usize {
        let v = rng.range(0, max_var + 1);
        (v * 2) | rng.range(0, 2)
    };

    let mut latches = Vec::with_capacity(n_latch);
    for idx in 0..n_latch {
        let lvar = first_latch + idx;
        let llit = lvar * 2;
        if idx == 0 && has_clock {
            // Free-running toggle clock latch (exercises fold_clock + scorr).
            let init = rng.range(0, 2);
            latches.push((llit, llit ^ 1, Some(init)));
        } else {
            let next = any_lit(rng);
            let init = match rng.range(0, 5) {
                0 => None, // free/nondet init (AIGER 1.9: init == latch lit)
                1 => Some(1),
                _ => Some(0),
            };
            latches.push((llit, next, init));
        }
    }

    let bad_lit = any_lit(rng);
    let constr_lit = has_constraint.then(|| any_lit(rng));

    let mut s = format!(
        "aag {max_var} {n_in} {n_latch} 0 {n_and} 1 {}\n",
        usize::from(has_constraint)
    );
    for i in 1..=n_in {
        s.push_str(&format!("{}\n", i * 2));
    }
    for (llit, next, init) in &latches {
        match init {
            Some(0) => s.push_str(&format!("{llit} {next}\n")),
            Some(1) => s.push_str(&format!("{llit} {next} 1\n")),
            None => s.push_str(&format!("{llit} {next} {llit}\n")),
            Some(_) => unreachable!(),
        }
    }
    s.push_str(&format!("{bad_lit}\n"));
    if let Some(c) = constr_lit {
        s.push_str(&format!("{c}\n"));
    }
    for (lhs, a, b) in &ands {
        s.push_str(&format!("{lhs} {a} {b}\n"));
    }
    for i in 0..n_in {
        s.push_str(&format!("i{i} in_{i}\n"));
    }
    for i in 0..n_latch {
        s.push_str(&format!("l{i} reg_{i}\n"));
    }
    s
}

/// Exact bounded state-space BFS oracle for tiny AIGER models (`<= 5` latches,
/// `<= 3` inputs): checks reachability up to `max_depth` steps.
fn exact_bfs_aig(path: &Path, max_depth: usize) -> Option<Verdict> {
    let g = load_aig(path).ok()?;
    if g.latches.len() > 5 || g.inputs.len() > 3 || g.bads.is_empty() {
        return None;
    }
    let n_i = g.inputs.len();
    let mut init_states = vec![0u32];
    for (li, l) in g.latches.iter().enumerate() {
        match l.init {
            informal_aig::Init::Zero => {}
            informal_aig::Init::One => {
                for st in &mut init_states {
                    *st |= 1 << li;
                }
            }
            _ => {
                let cur = init_states.clone();
                for st in cur {
                    init_states.push(st | (1 << li));
                }
            }
        }
    }
    let mut visited: HashSet<u32> = HashSet::new();
    let mut frontier: Vec<u32> = init_states;
    for depth in 0..=max_depth {
        let mut next_frontier = Vec::new();
        for &st in &frontier {
            if !visited.insert(st) {
                continue;
            }
            for inp in 0..(1u32 << n_i) {
                let vals = g.eval(|l| {
                    if let Some(ii) = g.inputs.iter().position(|&x| x == l) {
                        ((inp >> ii) & 1) == 1
                    } else if let Some(li) = g.latches.iter().position(|x| x.lit == l) {
                        ((st >> li) & 1) == 1
                    } else {
                        false
                    }
                });
                let ok_c = g
                    .constraints
                    .iter()
                    .all(|&c| informal_aig::lit_val(&vals, c));
                if !ok_c {
                    continue;
                }
                if g.bads.iter().any(|&b| informal_aig::lit_val(&vals, b)) {
                    return Some(Verdict::Unsafe {
                        depth: Some(depth as u32),
                    });
                }
                let mut nxt = 0u32;
                for (li, l) in g.latches.iter().enumerate() {
                    if informal_aig::lit_val(&vals, l.next) {
                        nxt |= 1 << li;
                    }
                }
                if !visited.contains(&nxt) {
                    next_frontier.push(nxt);
                }
            }
        }
        if next_frontier.is_empty() {
            return Some(Verdict::Safe);
        }
        frontier = next_frontier;
    }
    None
}

/// Generate a random small word-level Btor2 safety model (bitvectors, muxes,
/// arithmetic, optional constraints).
fn gen_random_btor2(rng: &mut Rng) -> String {
    let w = rng.range(2, 5); // 2..=4 bits
    let mut id = 1usize;
    let mut out = String::new();
    let s1 = id;
    id += 1;
    out.push_str(&format!("{s1} sort bitvec 1\n"));
    let sw = id;
    id += 1;
    out.push_str(&format!("{sw} sort bitvec {w}\n"));

    let zero_w = id;
    id += 1;
    out.push_str(&format!("{zero_w} zero {sw}\n"));
    let one_w = id;
    id += 1;
    out.push_str(&format!("{one_w} one {sw}\n"));

    let mut w_nodes = vec![zero_w, one_w];
    let mut b_nodes = Vec::new();

    // Inputs
    let n_in = rng.range(1, 3);
    for i in 0..n_in {
        let nid = id;
        id += 1;
        out.push_str(&format!("{nid} input {sw} in_{i}\n"));
        w_nodes.push(nid);
    }
    let en = id;
    id += 1;
    out.push_str(&format!("{en} input {s1} en\n"));
    b_nodes.push(en);

    // States
    let n_st = rng.range(1, 4);
    let mut states = Vec::new();
    for i in 0..n_st {
        let st = id;
        id += 1;
        out.push_str(&format!("{st} state {sw} r_{i}\n"));
        let init_c = rng.range(0, 1 << w);
        let cid = id;
        id += 1;
        out.push_str(&format!("{cid} constd {sw} {init_c}\n"));
        let iid = id;
        id += 1;
        out.push_str(&format!("{iid} init {sw} {st} {cid}\n"));
        w_nodes.push(st);
        states.push(st);
    }

    // Random word-level & predicate operations
    let n_ops = rng.range(4, 9);
    for _ in 0..n_ops {
        let a = w_nodes[rng.range(0, w_nodes.len())];
        let b = w_nodes[rng.range(0, w_nodes.len())];
        match rng.range(0, 8) {
            0 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} add {sw} {a} {b}\n"));
                w_nodes.push(nid);
            }
            1 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} sub {sw} {a} {b}\n"));
                w_nodes.push(nid);
            }
            2 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} xor {sw} {a} {b}\n"));
                w_nodes.push(nid);
            }
            3 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} and {sw} {a} {b}\n"));
                w_nodes.push(nid);
            }
            4 => {
                let cond = b_nodes[rng.range(0, b_nodes.len())];
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} ite {sw} {cond} {a} {b}\n"));
                w_nodes.push(nid);
            }
            5 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} eq {s1} {a} {b}\n"));
                b_nodes.push(nid);
            }
            6 => {
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} ult {s1} {a} {b}\n"));
                b_nodes.push(nid);
            }
            _ => {
                let c = rng.range(0, 1 << w);
                let cid = id;
                id += 1;
                out.push_str(&format!("{cid} constd {sw} {c}\n"));
                let nid = id;
                id += 1;
                out.push_str(&format!("{nid} eq {s1} {a} {cid}\n"));
                b_nodes.push(nid);
            }
        }
    }

    for &st in &states {
        let nxt = w_nodes[rng.range(0, w_nodes.len())];
        let nid = id;
        id += 1;
        out.push_str(&format!("{nid} next {sw} {st} {nxt}\n"));
    }

    if rng.coin(1, 3) && !b_nodes.is_empty() {
        let c = b_nodes[rng.range(0, b_nodes.len())];
        let nid = id;
        id += 1;
        out.push_str(&format!("{nid} constraint {c}\n"));
    }
    let bad_cond = b_nodes[rng.range(0, b_nodes.len())];
    out.push_str(&format!("{id} bad {bad_cond}\n"));
    out
}

/// Generate a random small liveness Btor2 model (`justice` + optional `fair`).
fn gen_random_liveness_btor2(rng: &mut Rng) -> String {
    let w = 2;
    let bound = rng.range(1, 4);
    let target = rng.range(0, 4);
    let wraps = rng.coin(1, 2);
    // Counter that either wraps below `bound` or saturates at `bound`.
    let next_on_hit = if wraps { "3" } else { "4" }; // 3 = zero, 4 = cnt
    let fair_line = if rng.coin(1, 2) {
        "14 input 2 en\n15 fair 14\n"
    } else {
        ""
    };
    format!(
        "\
1 sort bitvec {w}
2 sort bitvec 1
3 zero 1
4 state 1 cnt
5 init 1 4 3
6 one 1
7 add 1 4 6
8 constd 1 {bound}
9 eq 2 4 8
10 ite 1 9 {next_on_hit} 7
11 next 1 4 10
12 constd 1 {target}
13 eq 2 4 12
{fair_line}16 justice 1 13 j_fuzz
"
    )
}

fn decode_bits(s: &str) -> Vec<bool> {
    s.bytes().map(|b| b == b'1').collect()
}

fn verify_cert_if_present(
    engine: &dyn Engine,
    model: &Path,
    verdict: &Verdict,
    cert: Option<&Certificate>,
    label: &str,
) {
    let Some(cert) = cert else {
        assert!(
            !engine.capabilities().certifies || !verdict.is_definitive(),
            "{label}: certifying engine {} omitted certificate on {:?}",
            engine.name(),
            verdict
        );
        return;
    };
    let orig = load_aig(model).expect("reload model for certificate verification");
    match (verdict, cert) {
        (Verdict::Safe, Certificate::Invariant { clauses }) => {
            certify::check(&orig, clauses, &informal_sat::best).unwrap_or_else(|e| {
                panic!("{label}: {} invariant cert failed: {e}", engine.name())
            });
        }
        (Verdict::Safe, Certificate::TwoStep { clauses }) => {
            certify::check_folded(&orig, clauses, &informal_sat::best)
                .unwrap_or_else(|e| panic!("{label}: {} two-step cert failed: {e}", engine.name()));
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
            .unwrap_or_else(|e| panic!("{label}: {} k-ind cert failed: {e}", engine.name()));
        }
        (Verdict::Safe, Certificate::TwoStepKInduction { k, clauses, .. }) => {
            certify::check_kind_folded(&orig, clauses, *k as usize, &informal_sat::best, &|| false)
                .unwrap_or_else(|e| {
                    panic!("{label}: {} folded k-ind cert failed: {e}", engine.name())
                });
        }
        (Verdict::Unsafe { depth }, Certificate::Trace { init, inputs }) => {
            if engine.capabilities().kind != EngineKind::Liveness {
                let d = depth.expect("unsafe cert depth") as usize;
                let tr = certify::Trace {
                    init: decode_bits(init),
                    inputs: inputs.iter().map(|f| decode_bits(f)).collect(),
                };
                certify::replay_unsafe(&orig, d, &tr).unwrap_or_else(|e| {
                    panic!("{label}: {} trace replay failed: {e}", engine.name())
                });
            }
        }
        (v, c) => panic!(
            "{label}: {} mismatched verdict {:?} and cert {:?}",
            engine.name(),
            v,
            c.kind()
        ),
    }
}

fn check_differential(
    label: &str,
    model: &Path,
    format: ModelFormat,
    engines: &[Arc<dyn Engine>],
    liveness: bool,
    oracle: Option<Verdict>,
) {
    let mut safe_by: Vec<String> = Vec::new();
    let mut unsafe_by: Vec<(String, Option<u32>)> = Vec::new();

    if let Some(ref ov) = oracle {
        match ov {
            Verdict::Safe => safe_by.push("exact-bfs-oracle".into()),
            Verdict::Unsafe { depth } => unsafe_by.push(("exact-bfs-oracle".into(), *depth)),
            Verdict::Unknown { .. } => {}
        }
    }

    for eng in engines {
        let caps = eng.capabilities();
        if !caps.formats.contains(&format) {
            continue;
        }
        if liveness && !caps.liveness {
            continue;
        }
        if !liveness && caps.kind == EngineKind::Liveness {
            continue;
        }
        // Cap pure-BMC engines to a fast bounded check so SAFE circuits do not
        // burn full timeout per BMC worker, while still verifying every bug
        // within depth 20 and exercising every engine in the fleet.
        let (eng_timeout, max_depth) = if !caps.proves {
            (Duration::from_millis(500), Some(20))
        } else {
            (Duration::from_secs(2), None)
        };
        let mut eng_req = CheckRequest::new(model.to_path_buf(), format, eng_timeout);
        eng_req.max_depth = max_depth;
        let ctx = RunContext::new(eng_timeout);
        let Ok(res) = eng.check(&eng_req, &ctx) else {
            continue;
        };
        match &res.verdict {
            Verdict::Safe => {
                if !liveness {
                    verify_cert_if_present(
                        eng.as_ref(),
                        model,
                        &res.verdict,
                        res.certificate.as_ref(),
                        label,
                    );
                }
                safe_by.push(eng.name().to_string());
            }
            Verdict::Unsafe { depth } => {
                if !liveness {
                    verify_cert_if_present(
                        eng.as_ref(),
                        model,
                        &res.verdict,
                        res.certificate.as_ref(),
                        label,
                    );
                }
                unsafe_by.push((eng.name().to_string(), *depth));
            }
            Verdict::Unknown { .. } => {}
        }
    }

    if !safe_by.is_empty() && !unsafe_by.is_empty() {
        let text = std::fs::read_to_string(model).unwrap_or_default();
        panic!(
            "SOUNDNESS CONFLICT on {label}!\n  SAFE by:   {safe_by:?}\n  UNSAFE by: {unsafe_by:?}\n--- model ---\n{text}"
        );
    }
}

#[test]
fn differential_fuzz_random_aiger_circuits() {
    let Some(mut fleet) = in_house_fleet() else {
        return;
    };
    fleet.extend(available_external_oracles());
    let dir = new_temp("aig");
    let mut rng = Rng::new(fuzz_seed());
    let n = fuzz_cases(14);

    for i in 0..n {
        let aag = gen_random_aag(&mut rng);
        let path = dir.0.join(format!("fuzz_{i}.aag"));
        std::fs::write(&path, &aag).unwrap();
        let oracle = exact_bfs_aig(&path, 20);
        check_differential(
            &format!("fuzz_aig_{i}"),
            &path,
            ModelFormat::Aiger,
            &fleet,
            false,
            oracle,
        );
    }
}

#[test]
fn differential_fuzz_random_btor2_and_liveness_circuits() {
    let Some(mut fleet) = in_house_fleet() else {
        return;
    };
    fleet.extend(available_external_oracles());
    let dir = new_temp("btor2");
    let mut rng = Rng::new(fuzz_seed() ^ 0xdead_beef_0001);
    let n_safety = fuzz_cases(8);
    let n_live = (fuzz_cases(4)).min(12);

    for i in 0..n_safety {
        let btor = gen_random_btor2(&mut rng);
        let path = dir.0.join(format!("fuzz_btor_{i}.btor2"));
        std::fs::write(&path, &btor).unwrap();
        check_differential(
            &format!("fuzz_btor_{i}"),
            &path,
            ModelFormat::Btor2,
            &fleet,
            false,
            None,
        );
    }

    for i in 0..n_live {
        let btor = gen_random_liveness_btor2(&mut rng);
        let path = dir.0.join(format!("fuzz_live_{i}.btor2"));
        std::fs::write(&path, &btor).unwrap();
        check_differential(
            &format!("fuzz_live_{i}"),
            &path,
            ModelFormat::Btor2,
            &fleet,
            true,
            None,
        );
    }
}
