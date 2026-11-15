//! Certificates as files that outside checkers read (decision D18).
//!
//! - **UNSAFE:** the counterexample in the AIGER witness format, checked by
//!   `aigsim` (AIGER tools).
//! - **SAFE:** a witness circuit in AIGER, checked by Certifaiger: the
//!   model with one new bad output, `P'`, such that `P'` holds in the
//!   initial states, is kept by every step, and implies the model's
//!   property. Certifaiger turns those three claims into SAT problems of
//!   its own and gives them to a SAT solver.
//!
//! [`crate::certify`] checks the same facts inside the tool, with its own
//! encoding; this module only writes them down. Three kinds of proof
//! become witness circuits ([`witness_circuit`]):
//!
//! - an inductive invariant (IC3): `P'` is "not bad, and the invariant";
//! - k-induction without the simple-path condition: the circuit also
//!   remembers the last `k - 1` states and inputs, and `P'` says that what
//!   it remembers is a path of good states, which starts in an initial
//!   state while it is shorter than `k`. Its inductive step is then the
//!   base and the step of the k-induction proof in one;
//! - a two-step invariant (IC3 on a clock-folded model): the circuit
//!   remembers one state and a bit that says whether the step count is odd;
//!   `P'` is the invariant on even steps, and on odd steps that the
//!   remembered state satisfied it and led here.
//!
//! A witness circuit holds only the part of the model the proof is about:
//! the cone of the bad outputs, the constraints and the invariant's
//! latches. On the CPU models of HWMCC'24, where one file carries one
//! property of hundreds, that is a small part, and Certifaiger compares
//! the two circuits latch by latch. That part is copied gate for gate, its
//! inputs and latches come first, and the symbol table maps each to its
//! literal in the model, which is how Certifaiger knows what is shared.
//!
//! A Btor2 model is checked at bit level: the bit-blasted model is written
//! next to its witness circuit, and the checker confirms the verdict for
//! that file. The bit-blaster stays trusted there (and liveness to safety,
//! when the model has a justice property).

use crate::certify::{self, Trace};
use anyhow::{bail, Context};
use informal_aig::{aiger, neg, var, Aig, Init, Lit, Node, FALSE, TRUE};
use informal_engine::{Certificate, Verdict};
use std::path::{Path, PathBuf};

/// `m`'s literal for the literal `l` of the model it was copied from.
fn at(m: &[Lit], l: Lit) -> Lit {
    m[var(l) as usize] ^ (l & 1)
}

fn or(w: &mut Aig, a: Lit, b: Lit) -> Lit {
    w.or(a, b)
}

fn same(w: &mut Aig, a: Lit, b: Lit) -> Lit {
    neg(w.xor(a, b))
}

/// A new latch of `w` that starts at 0 and takes `next`.
fn latch(w: &mut Aig, next: Lit) -> Lit {
    let l = w.latch(Init::Zero);
    w.latches.last_mut().expect("just added").next = next;
    l
}

/// Latches of `w` that hold the values `leaf` (by variable of `g`: its
/// inputs and latches in `cone`) had one step earlier.
fn remember(w: &mut Aig, g: &Aig, cone: &[bool], leaf: &[Lit]) -> Vec<Lit> {
    let mut held = vec![FALSE; g.nodes.len()];
    for (v, n) in g.nodes.iter().enumerate() {
        if cone[v] && matches!(n, Node::Input | Node::Latch) {
            held[v] = latch(w, leaf[v]);
        }
    }
    held
}

/// A copy of `g`'s logic in `w` (the nodes in `cone`) over other inputs
/// and latches: `leaf` by variable of `g`. Returns `w`'s literal for each
/// variable of `g`.
fn copy(w: &mut Aig, g: &Aig, cone: &[bool], leaf: &[Lit]) -> Vec<Lit> {
    let mut m = vec![FALSE; g.nodes.len()];
    for (v, n) in g.nodes.iter().enumerate() {
        if !cone[v] {
            continue;
        }
        m[v] = match *n {
            Node::Const => FALSE,
            Node::Input | Node::Latch => leaf[v],
            Node::And(a, b) => {
                let (x, y) = (at(&m, a), at(&m, b));
                w.and(x, y)
            }
        };
    }
    m
}

/// No bad output of `g` is true, in the copy `m`.
fn good(w: &mut Aig, g: &Aig, m: &[Lit]) -> Lit {
    let bads: Vec<Lit> = g.bads.iter().map(|&b| at(m, b)).collect();
    neg(w.or_all(&bads))
}

/// Every constraint of `g` holds, in the copy `m`.
fn constrained(w: &mut Aig, g: &Aig, m: &[Lit]) -> Lit {
    let cs: Vec<Lit> = g.constraints.iter().map(|&c| at(m, c)).collect();
    w.and_all(&cs)
}

/// Every clause holds, in the copy `m`.
fn holds(w: &mut Aig, clauses: &[Vec<Lit>], m: &[Lit]) -> Lit {
    let mut all = TRUE;
    for c in clauses {
        let lits: Vec<Lit> = c.iter().map(|&l| at(m, l)).collect();
        let x = w.or_all(&lits);
        all = w.and(all, x);
    }
    all
}

/// The copy `m` is an initial state of `g` (latches in `cone`).
fn initial(w: &mut Aig, g: &Aig, cone: &[bool], m: &[Lit]) -> Lit {
    let mut all = TRUE;
    for l in g.latches.iter().filter(|l| cone[var(l.lit) as usize]) {
        let x = at(m, l.lit);
        let ok = match l.init {
            Init::Zero => neg(x),
            Init::One => x,
            Init::Free => continue,
            Init::Expr(e) => same(w, x, at(m, e)),
        };
        all = w.and(all, ok);
    }
    all
}

/// One step of `g` leads from the copy `from` to the latches of `to`.
fn steps(w: &mut Aig, g: &Aig, cone: &[bool], from: &[Lit], to: &[Lit]) -> Lit {
    let mut all = TRUE;
    for l in g.latches.iter().filter(|l| cone[var(l.lit) as usize]) {
        let eq = same(w, at(to, l.lit), at(from, l.next));
        all = w.and(all, eq);
    }
    all
}

/// The clauses as they are, after checking that they only name latches.
fn latch_clauses(g: &Aig, clauses: &[Vec<Lit>]) -> Result<(), String> {
    for &l in clauses.iter().flatten() {
        if !g.is_latch(l) {
            return Err(format!(
                "certificate literal {l} is not a latch of the model"
            ));
        }
    }
    Ok(())
}

/// A witness circuit and what it shares with the model: the model's
/// literals for the circuit's first inputs and first latches, in order.
#[derive(Debug, Clone)]
pub struct Witness {
    pub circuit: Aig,
    pub inputs: Vec<Lit>,
    pub latches: Vec<Lit>,
}

/// The cone of the model, copied gate for gate, ready to get more latches
/// and its new bad output: the cone's inputs and latches in the model's
/// order, the constraints, no bad output yet. Also the circuit's literal
/// for each variable of `g` in the cone.
fn base(g: &Aig, cone: &[bool]) -> (Witness, Vec<Lit>) {
    let mut w = Aig::new();
    let mut now = vec![FALSE; g.nodes.len()];
    let in_cone = |l: Lit| cone[var(l) as usize];
    let inputs: Vec<Lit> = g.inputs.iter().copied().filter(|&i| in_cone(i)).collect();
    for &i in &inputs {
        now[var(i) as usize] = w.input();
    }
    let latches: Vec<Lit> = g
        .latches
        .iter()
        .map(|l| l.lit)
        .filter(|&l| in_cone(l))
        .collect();
    for &l in &latches {
        now[var(l) as usize] = w.latch(Init::Free);
    }
    for (v, n) in g.nodes.iter().enumerate() {
        if let (true, Node::And(a, b)) = (cone[v], *n) {
            now[v] = w.raw_and(at(&now, a), at(&now, b));
        }
    }
    for (k, l) in g.latches.iter().filter(|l| in_cone(l.lit)).enumerate() {
        w.latches[k].next = at(&now, l.next);
        w.latches[k].init = match l.init {
            Init::Expr(e) => Init::Expr(at(&now, e)),
            other => other,
        };
    }
    w.constraints = g.constraints.iter().map(|&c| at(&now, c)).collect();
    (
        Witness {
            circuit: w,
            inputs,
            latches,
        },
        now,
    )
}

/// The witness circuit of a k-induction proof at depth `k` (`k` good
/// states in a row that satisfy `clauses`, an inductive invariant, are
/// followed by a good state; no bad state within fewer than `k` steps).
/// `k = 1` with clauses that exclude the bad states is a plain inductive
/// invariant.
fn kind_circuit(g: &Aig, clauses: &[Vec<Lit>], k: usize) -> Result<Witness, String> {
    if k == 0 {
        return Err("k-induction at depth 0".into());
    }
    latch_clauses(g, clauses)?;
    let cone = certify::cone(g, clauses);
    let (mut out, now) = base(g, &cone);
    let w = &mut out.circuit;
    // States 0 (now) to k - 1 (the oldest remembered), with whether each
    // is there yet.
    let mut leaf = vec![now.clone()];
    let mut state = vec![now];
    let mut valid = vec![TRUE];
    for i in 1..k {
        valid.push(latch(w, valid[i - 1]));
        let held = remember(w, g, &cone, &leaf[i - 1]);
        state.push(copy(w, g, &cone, &held));
        leaf.push(held);
    }
    let mut p = good(w, g, &state[0]);
    let inv = holds(w, clauses, &state[0]);
    p = w.and(p, inv);
    for i in 1..k {
        // A remembered state is good, meets the constraints and the
        // invariant, and leads to the one after it.
        let mut body = steps(w, g, &cone, &state[i], &state[i - 1]);
        for x in [
            good(w, g, &state[i]),
            constrained(w, g, &state[i]),
            holds(w, clauses, &state[i]),
        ] {
            body = w.and(body, x);
        }
        let x = or(w, neg(valid[i]), body);
        p = w.and(p, x);
        if i + 1 < k {
            let x = or(w, neg(valid[i + 1]), valid[i]);
            p = w.and(p, x);
        }
    }
    for i in 0..k - 1 {
        // While fewer than k states are remembered, the oldest is initial.
        let oldest = w.and(valid[i], neg(valid[i + 1]));
        let init = initial(w, g, &cone, &state[i]);
        let x = or(w, neg(oldest), init);
        p = w.and(p, x);
    }
    w.bads = vec![neg(p)];
    Ok(out)
}

/// The witness circuit of a two-step invariant ([`certify::check_folded`];
/// `clauses` with the fold's `ok` already replaced by true).
fn two_step_circuit(g: &Aig, clauses: &[Vec<Lit>]) -> Result<Witness, String> {
    latch_clauses(g, clauses)?;
    let cone = certify::cone(g, clauses);
    let (mut out, ident) = base(g, &cone);
    let w = &mut out.circuit;
    let odd = w.latch(Init::Zero);
    w.latches.last_mut().expect("just added").next = neg(odd);
    let held = remember(w, g, &cone, &ident);
    let before = copy(w, g, &cone, &held);
    let mut p = good(w, g, &ident);
    let inv = holds(w, clauses, &ident);
    let even = or(w, odd, inv);
    p = w.and(p, even);
    let mut body = steps(w, g, &cone, &before, &ident);
    for x in [constrained(w, g, &before), holds(w, clauses, &before)] {
        body = w.and(body, x);
    }
    let x = or(w, neg(odd), body);
    p = w.and(p, x);
    w.bads = vec![neg(p)];
    Ok(out)
}

/// The witness circuit for a SAFE certificate on the loaded model `g`, or
/// why there is none.
pub fn witness_circuit(g: &Aig, cert: &Certificate) -> Result<Witness, String> {
    match cert {
        Certificate::Invariant { clauses } => kind_circuit(g, clauses, 1),
        Certificate::KInduction {
            simple_path: true, ..
        } => Err("the k-induction proof needs the simple-path condition, \
                  which has no witness circuit yet"
            .into()),
        Certificate::KInduction { k, clauses, .. } => kind_circuit(g, clauses, *k as usize),
        Certificate::TwoStep { clauses } => two_step_circuit(g, clauses),
        Certificate::Trace { .. } => Err("a trace is not a proof".into()),
    }
}

/// A witness circuit for the model `g` as a binary AIGER file for
/// Certifaiger: the circuit, the model's literal for each shared input and
/// latch in the symbol table (`i0 = 2`), and a comment that names the
/// model. The literals are those of the model's file, where inputs and
/// then latches are numbered from 2 in order: the same as in `g` for a
/// model read from AIGER, not for one bit-blasted from Btor2.
pub fn witness_file(g: &Aig, w: &Witness, model_name: &str) -> Vec<u8> {
    let mut out = aiger::write_binary(&w.circuit);
    let mut in_file = std::collections::HashMap::new();
    for (k, &i) in g.inputs.iter().enumerate() {
        in_file.insert(i, 2 * (k + 1));
    }
    for (k, l) in g.latches.iter().enumerate() {
        in_file.insert(l.lit, 2 * (g.inputs.len() + 1 + k));
    }
    let mut text = String::new();
    for (k, l) in w.inputs.iter().enumerate() {
        text += &format!("i{k} = {}\n", in_file[l]);
    }
    for (k, l) in w.latches.iter().enumerate() {
        text += &format!("l{k} = {}\n", in_file[l]);
    }
    text += &format!("c\nWITNESS b0 {model_name}\nwritten by InFormal\n");
    out.extend(text.bytes());
    out
}

fn parse_bits(s: &str, n: usize, what: &str) -> Result<Vec<bool>, String> {
    if s.len() != n || !s.bytes().all(|c| c == b'0' || c == b'1') {
        return Err(format!("trace: {what} is not {n} characters of 0 and 1"));
    }
    Ok(s.bytes().map(|c| c == b'1').collect())
}

/// The trace of a certificate, by latch and input index of `g`.
fn trace_of(g: &Aig, init: &str, inputs: &[String]) -> Result<Trace, String> {
    if inputs.is_empty() {
        return Err("trace: no steps".into());
    }
    Ok(Trace {
        init: parse_bits(init, g.latches.len(), "the initial state")?,
        inputs: inputs
            .iter()
            .map(|f| parse_bits(f, g.inputs.len(), "a step"))
            .collect::<Result<_, _>>()?,
    })
}

/// An UNSAFE certificate as an AIGER witness: `1`, the bad output reached,
/// the initial latch values, the input values of every step, `.`. The
/// trace is replayed on `g` first: one that does not reach a bad state is
/// an error, not a file.
pub fn aiger_witness(g: &Aig, init: &str, inputs: &[String]) -> Result<String, String> {
    if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
        return Err(
            "an initial value given by an expression cannot be written as an AIGER witness".into(),
        );
    }
    let t = trace_of(g, init, inputs)?;
    let bad = certify::replay_unsafe_bad(g, inputs.len() - 1, &t)?;
    let mut s = format!("1\nb{bad}\n");
    for (l, &v) in g.latches.iter().zip(&t.init) {
        s.push(match l.init {
            Init::Zero => '0',
            Init::One => '1',
            _ if v => '1',
            _ => '0',
        });
    }
    s.push('\n');
    for f in inputs {
        s += f;
        s.push('\n');
    }
    s += ".\n";
    Ok(s)
}

/// What [`write`] wrote for one verdict: the summary it leaves in the
/// certificate directory as `<model>.cert.json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Version of this summary's layout.
    pub informal_certificate: u32,
    /// The model the verdict is about, as it was given.
    pub model: String,
    pub verdict: String,
    pub engine: String,
    /// `invariant`, `k-induction`, `two-step`, `trace`, or `none`.
    pub evidence: String,
    /// The file to give the checker, in the same directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// `certifaiger` or `aigsim`; absent when there is nothing to check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
    /// The AIGER model the checker reads when it is not `model` itself: a
    /// Btor2 model bit-blasted by InFormal, in the same directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_level_model: Option<String>,
    /// Why there is no file, or what the check does not cover.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Summary {
    /// One line for the console.
    pub fn line(&self) -> String {
        match (&self.file, &self.checker) {
            (Some(f), Some(c)) => format!("certificate: {f} ({}, for {c})", self.evidence),
            _ => format!(
                "certificate: none ({})",
                self.note.as_deref().unwrap_or("no reason given")
            ),
        }
    }
}

/// Write the certificate of a definitive verdict on `model` into `dir`
/// (created when missing): the file for the outside checker when there is
/// one, and always `<model file name>.cert.json`, which says what was
/// written or why nothing could be. Returns the summary.
pub fn write(
    dir: &Path,
    model: &Path,
    engine: &str,
    verdict: &Verdict,
    cert: Option<&Certificate>,
) -> anyhow::Result<Summary> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = model
        .file_name()
        .and_then(|n| n.to_str())
        .context("the model path has no file name")?
        .to_string();
    let mut s = Summary {
        informal_certificate: 1,
        model: model.display().to_string(),
        verdict: verdict.label().into(),
        engine: engine.into(),
        evidence: cert.map_or("none", |c| c.kind()).into(),
        file: None,
        checker: None,
        bit_level_model: None,
        note: None,
    };
    let put = |file: &str, bytes: &[u8]| -> anyhow::Result<()> {
        let p = dir.join(file);
        std::fs::write(&p, bytes).with_context(|| format!("writing {}", p.display()))
    };
    let outcome: Result<(), String> = (|| {
        let cert = cert.ok_or_else(|| format!("{engine} gives no certificate"))?;
        let g = crate::load_aig(model).map_err(|e| format!("cannot reload the model: {e:#}"))?;
        let aiger = matches!(
            model.extension().and_then(|e| e.to_str()),
            Some("aig" | "aag")
        );
        let mut notes = Vec::new();
        if !aiger {
            if g.latches.iter().any(|l| matches!(l.init, Init::Expr(_))) {
                return Err(
                    "the model has initial values given by expressions, which AIGER cannot hold"
                        .into(),
                );
            }
            notes.push("checked at bit level: the bit-blaster is trusted");
            if g.names
                .values()
                .any(|n| n.starts_with(informal_aig::l2s::HELPER))
            {
                notes.push("a liveness property, checked as safety: that step is trusted too");
            }
        }
        let (file, bytes, checker) = match (verdict, cert) {
            (Verdict::Unsafe { .. }, Certificate::Trace { init, inputs }) => (
                format!("{name}.trace"),
                aiger_witness(&g, init, inputs)?.into_bytes(),
                "aigsim",
            ),
            (Verdict::Safe, c) if !matches!(c, Certificate::Trace { .. }) => {
                let w = witness_circuit(&g, c)?;
                let model_name = if aiger {
                    name.clone()
                } else {
                    format!("{name}.model.aig")
                };
                (
                    format!("{name}.cert.aig"),
                    witness_file(&g, &w, &model_name),
                    "certifaiger",
                )
            }
            _ => return Err("the certificate does not fit the verdict".into()),
        };
        let io = |e: anyhow::Error| format!("{e:#}");
        if !aiger {
            let m = format!("{name}.model.aig");
            put(&m, &aiger::write_binary(&g)).map_err(io)?;
            s.bit_level_model = Some(m);
        }
        put(&file, &bytes).map_err(io)?;
        s.file = Some(file);
        s.checker = Some(checker.into());
        if !notes.is_empty() {
            s.note = Some(notes.join("; "));
        }
        Ok(())
    })();
    if let Err(e) = outcome {
        s.note = Some(e);
    }
    let json = serde_json::to_string_pretty(&s)?;
    put(&format!("{name}.cert.json"), (json + "\n").as_bytes())?;
    Ok(s)
}

/// The summaries in a certificate directory, by file name.
pub fn summaries(dir: &Path) -> anyhow::Result<Vec<(PathBuf, Summary)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = e?.path();
        if !p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".cert.json"))
        {
            continue;
        }
        let text = std::fs::read_to_string(&p)?;
        let Ok(s) = serde_json::from_str(&text) else {
            bail!("{} is not a certificate summary", p.display());
        };
        out.push((p, s));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_sat::Sat;

    fn sat() -> Box<dyn Sat> {
        informal_sat::best()
    }

    /// What Certifaiger asks of a witness circuit for `g`, with this
    /// crate's own checks: the new bad output is unreachable by plain
    /// 1-induction (base and inductive step), and wherever it is false the
    /// model's bad outputs are false too. `w` may be the circuit as built
    /// or as read back from its file: the shared inputs and latches are
    /// its first ones. (Certifaiger's reset and transition conditions
    /// compare the shared latches, which are copies here.)
    fn accepted(g: &Aig, wit: &Witness, w: &Aig) -> Result<(), String> {
        assert_eq!(w.bads.len(), 1);
        assert_eq!(w.constraints.len(), g.constraints.len());
        certify::check_kind_paths(w, &[], false, 1, &sat, &|| false)?;
        // Safety: a state where P' holds but the model is bad. The model's
        // bad outputs are built again here, over the shared leaves.
        let mut leaf = vec![FALSE; g.nodes.len()];
        for (k, &i) in wit.inputs.iter().enumerate() {
            leaf[var(i) as usize] = w.inputs[k];
        }
        for (k, &l) in wit.latches.iter().enumerate() {
            let init = g.latches.iter().find(|x| x.lit == l).unwrap().init;
            assert!(matches!(init, Init::Expr(_)) || w.latches[k].init == init);
            leaf[var(l) as usize] = w.latches[k].lit;
        }
        let mut m = w.clone();
        let cone = certify::cone(g, &[]);
        let model = copy(&mut m, g, &cone, &leaf);
        let model_bad = neg(good(&mut m, g, &model));
        m.bads = vec![m.and(neg(w.bads[0]), model_bad)];
        m.latches.iter_mut().for_each(|l| l.init = Init::Free);
        match certify::check_unsafe(&m, 0, &sat, &|| false) {
            Err(e) if e.starts_with("unsafe fails: no counterexample") => Ok(()),
            Err(e) => Err(e),
            Ok(()) => Err("the witness property does not imply the model's".into()),
        }
    }

    fn check(g: &Aig, cert: &Certificate) -> Result<(), String> {
        let w = witness_circuit(g, cert).unwrap();
        accepted(g, &w, &w.circuit)
    }

    /// Two latches that toggle together from 0; bad = they differ.
    fn twins() -> Aig {
        let mut g = Aig::new();
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        g.set_next(a, neg(a));
        g.set_next(b, neg(b));
        let bad = g.xor(a, b);
        g.bads.push(bad);
        g
    }

    #[test]
    fn invariant_circuits_are_inductive_only_for_real_invariants() {
        let g = twins();
        let (a, b) = (g.latches[0].lit, g.latches[1].lit);
        let cert = |clauses| Certificate::Invariant { clauses };
        assert_eq!(
            check(&g, &cert(vec![vec![neg(a), b], vec![a, neg(b)]])),
            Ok(())
        );
        // Holds at first, not after a step.
        assert_eq!(
            check(&g, &cert(vec![vec![neg(a)], vec![neg(b)]])),
            Err("step fails".into())
        );
        // Does not hold at first.
        assert_eq!(check(&g, &cert(vec![vec![a]])), Err("base fails".into()));
        // A literal that is no latch is refused.
        assert!(witness_circuit(&g, &cert(vec![vec![g.bads[0]]])).is_err());
    }

    /// a' = in & !a, then b, c and d each take the one before: all from
    /// 0. a is never true twice in a row, so neither are two neighbours:
    /// bad = d & c is unreachable. That is 3-inductive and not 2-inductive
    /// (d & c in the next state is b & a two states before it).
    fn delay() -> Aig {
        let mut g = Aig::new();
        let i = g.input();
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        let c = g.latch(Init::Zero);
        let d = g.latch(Init::Zero);
        let n = g.and(i, neg(a));
        g.set_next(a, n);
        g.set_next(b, a);
        g.set_next(c, b);
        g.set_next(d, c);
        let bad = g.and(d, c);
        g.bads.push(bad);
        g
    }

    #[test]
    fn k_induction_circuits_carry_base_and_step() {
        let g = delay();
        let kind = |k| Certificate::KInduction {
            k,
            clauses: vec![],
            simple_path: false,
        };
        // The proof depth itself, checked by certify.
        let never = || false;
        assert!(certify::check_kind_paths(&g, &[], false, 2, &sat, &never).is_err());
        assert_eq!(
            certify::check_kind_paths(&g, &[], false, 3, &sat, &never),
            Ok(0)
        );
        assert_eq!(check(&g, &kind(3)), Ok(()));
        // A larger depth is a proof too.
        assert_eq!(check(&g, &kind(5)), Ok(()));
        // Too shallow: the circuit is not inductive.
        assert_eq!(check(&g, &kind(2)), Err("step fails".into()));
        // With a strengthening invariant, depth 1 is enough.
        let (a, b, c) = (g.latches[0].lit, g.latches[1].lit, g.latches[2].lit);
        let inv = vec![vec![neg(a), neg(b)], vec![neg(b), neg(c)]];
        assert!(certify::check_kind_paths(&g, &[], false, 1, &sat, &never).is_err());
        assert_eq!(
            check(
                &g,
                &Certificate::KInduction {
                    k: 1,
                    clauses: inv,
                    simple_path: false,
                }
            ),
            Ok(())
        );
        // An unsafe variant (bad = d & a, reached at step 4): no depth
        // gives an inductive circuit, because the base is inside it.
        let mut u = delay();
        let (a, d) = (u.latches[0].lit, u.latches[3].lit);
        u.bads = vec![u.and(d, a)];
        assert!(certify::check_unsafe(&u, 4, &sat, &never).is_ok());
        for k in 1..=4 {
            assert!(check(&u, &kind(k)).is_err(), "depth {k}");
        }
        // A proof that needed the simple-path condition has no circuit.
        let sp = Certificate::KInduction {
            k: 3,
            clauses: vec![],
            simple_path: true,
        };
        assert!(witness_circuit(&g, &sp)
            .unwrap_err()
            .contains("simple-path"));
    }

    /// clk toggles from 0; a takes the clock (or an input that the
    /// constraint keeps low), b takes a. b is the clock two steps ago, so
    /// bad = b & !clk is unreachable; it is not inductive in one step.
    fn clocked() -> Aig {
        let mut g = Aig::new();
        let i = g.input();
        let clk = g.latch(Init::Zero);
        let a = g.latch(Init::Zero);
        let b = g.latch(Init::Zero);
        g.set_next(clk, neg(clk));
        let n = g.or(clk, i);
        g.set_next(a, n);
        g.set_next(b, a);
        g.constraints.push(neg(i));
        let bad = g.and(b, neg(clk));
        g.bads.push(bad);
        g
    }

    #[test]
    fn two_step_circuits_follow_the_folded_check() {
        let g = clocked();
        let (clk, b) = (g.latches[0].lit, g.latches[2].lit);
        // On even steps the clock is low and b is too.
        let inv = vec![vec![neg(clk)], vec![neg(b)]];
        assert_eq!(certify::check_folded(&g, &inv, &sat), Ok(()));
        assert_eq!(
            check(
                &g,
                &Certificate::TwoStep {
                    clauses: inv.clone(),
                }
            ),
            Ok(())
        );
        // The same clauses are not a one-step invariant.
        assert_eq!(
            check(&g, &Certificate::Invariant { clauses: inv }),
            Err("step fails".into())
        );
        // Without them nothing ties the remembered state down.
        assert!(certify::check_folded(&g, &[], &sat).is_err());
        assert_eq!(
            check(&g, &Certificate::TwoStep { clauses: vec![] }),
            Err("step fails".into())
        );
    }

    #[test]
    fn traces_become_aiger_witnesses_only_when_they_replay() {
        // counter2: 2-bit counter with enable, bad = (q == 3).
        let mut g = Aig::new();
        let en = g.input();
        let q0 = g.latch(Init::Zero);
        let q1 = g.latch(Init::Free);
        let n0 = g.xor(q0, en);
        let carry = g.and(q0, en);
        let n1 = g.xor(q1, carry);
        g.set_next(q0, n0);
        g.set_next(q1, n1);
        let bad = g.and(q0, q1);
        g.bads.push(bad);
        let steps = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // q1 starts free: from 10 (q1 = 1) one enabled step reaches 3.
        // The constant latch is written with its reset whatever the trace says.
        assert_eq!(
            aiger_witness(&g, "11", &steps(&["1", "0"])).unwrap(),
            "1\nb0\n01\n1\n0\n.\n"
        );
        assert!(aiger_witness(&g, "00", &steps(&["1", "0"]))
            .unwrap_err()
            .contains("does not reach a bad state"));
        assert!(aiger_witness(&g, "0", &steps(&["1"])).is_err());
        assert!(aiger_witness(&g, "01", &[]).is_err());
    }

    #[test]
    fn a_proof_without_a_witness_circuit_leaves_a_summary_that_says_why() {
        let dir = std::env::temp_dir().join(format!("informal-cert-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let model = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/models/delay_safe.aag");
        let cert = Certificate::KInduction {
            k: 3,
            clauses: vec![],
            simple_path: true,
        };
        let s = write(&dir, &model, "informal-kind", &Verdict::Safe, Some(&cert)).unwrap();
        assert_eq!((s.file.clone(), s.checker.clone()), (None, None));
        assert_eq!(s.evidence, "k-induction");
        assert!(s.note.as_deref().unwrap().contains("simple-path"));
        assert!(s
            .line()
            .starts_with("certificate: none (the k-induction proof"));
        assert_eq!(summaries(&dir).unwrap()[0].1, s);
        // A trace for a SAFE verdict is refused the same way.
        let trace = Certificate::Trace {
            init: "0000".into(),
            inputs: vec!["0".into()],
        };
        let s = write(&dir, &model, "x", &Verdict::Safe, Some(&trace)).unwrap();
        assert!(s.note.unwrap().contains("does not fit the verdict"));
        // The same proof without the condition is written.
        let cert = Certificate::KInduction {
            k: 3,
            clauses: vec![],
            simple_path: false,
        };
        let s = write(&dir, &model, "informal-kind", &Verdict::Safe, Some(&cert)).unwrap();
        assert_eq!(s.file.as_deref(), Some("delay_safe.aag.cert.aig"));
        assert!(dir.join("delay_safe.aag.cert.aig").is_file());
        assert_eq!(s.note, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A bit-blasted Btor2 model is not numbered inputs first: the symbol
    /// table must name the literals of the model file that is written.
    #[test]
    fn the_mapping_uses_the_written_models_numbering() {
        let mut g = Aig::new();
        let a = g.latch(Init::One);
        let i = g.input();
        let b = g.latch(Init::Zero);
        let n = g.and(a, neg(i));
        g.set_next(a, n);
        g.set_next(b, neg(a));
        // a can only fall, and b is "a was low": never both.
        let bad = g.and(b, a);
        g.bads.push(bad);
        let inv = vec![vec![neg(a), neg(b)]];
        assert_eq!(certify::check(&g, &inv, &sat), Ok(()));
        let w = witness_circuit(&g, &Certificate::Invariant { clauses: inv }).unwrap();
        let file = aiger::read(&aiger::write_binary(&g)).unwrap();
        let bytes = witness_file(&g, &w, "m.aig");
        let text = String::from_utf8_lossy(&bytes);
        // In the file: input 2, then latches 4 (a, starts at 1) and 6 (b).
        assert!(text.contains("i0 = 2\nl0 = 4\nl1 = 6\nc\n"), "{text}");
        assert_eq!(file.inputs, [2]);
        assert_eq!(
            file.latches
                .iter()
                .map(|l| (l.lit, l.init))
                .collect::<Vec<_>>(),
            [(4, Init::One), (6, Init::Zero)]
        );
        let r = aiger::read(&bytes).unwrap();
        assert_eq!(
            (r.latches[0].init, r.latches[1].init),
            (Init::One, Init::Zero)
        );
    }

    #[test]
    fn witness_files_map_the_model_and_read_back() {
        let g = clocked();
        let w = two_step_circuit(
            &g,
            &[vec![neg(g.latches[0].lit)], vec![neg(g.latches[2].lit)]],
        )
        .unwrap();
        let bytes = witness_file(&g, &w, "clocked.aig");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("i0 = 2\nl0 = 4\nl1 = 6\nl2 = 8\nc\nWITNESS b0 clocked.aig\n"));
        let r = aiger::read(&bytes).unwrap();
        assert_eq!(r.inputs.len(), 1);
        assert_eq!(r.latches.len(), w.circuit.latches.len());
        assert_eq!((r.bads.len(), r.constraints.len()), (1, 1));
        assert_eq!(accepted(&g, &w, &r), Ok(()));
    }
}
