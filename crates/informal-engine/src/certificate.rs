//! What an engine hands back with a definitive verdict, so that a program
//! that shares no code with InFormal can check the verdict.
//!
//! Everything here is in terms of the bit-level model as loaded from the
//! file (AIGER as is, Btor2 bit-blasted), before any preprocessing: AIGER
//! literals (`2 * var + negated`) of its latches, and its latches and
//! inputs by index. `informal-bmc` turns a certificate into the files the
//! outside checkers read (`informal_bmc::certificate`).

use serde::{Deserialize, Serialize};

/// A clause over AIGER literals of the loaded model's latches.
pub type Clause = Vec<u32>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Certificate {
    /// SAFE: the conjunction of the clauses holds in every initial state,
    /// is kept by every step, and excludes every bad state (states and
    /// steps that meet the constraints).
    Invariant { clauses: Vec<Clause> },
    /// SAFE by k-induction: `clauses` is an inductive invariant as above
    /// (but need not exclude the bad states); no bad state is reachable in
    /// fewer than `k` steps; and after `k` consecutive good states that
    /// satisfy it, the next state is good. `simple_path` is set when the
    /// last part only holds for paths without a repeated state: such a
    /// proof has no witness circuit yet.
    KInduction {
        k: u32,
        clauses: Vec<Clause>,
        simple_path: bool,
    },
    /// SAFE, proved on a clock-folded model (one step is two of the loaded
    /// model): the clauses hold in every initial state and again after
    /// every two steps; such a state is not bad and neither is the one
    /// after it.
    TwoStep { clauses: Vec<Clause> },
    /// UNSAFE: a path to a bad state. `init` has one character (`0` or
    /// `1`) per latch, used for the latches without a constant initial
    /// value; `inputs` has one string per step, one character per input.
    /// The bad state is in the last step.
    Trace { init: String, inputs: Vec<String> },
}

impl Certificate {
    /// A short name for reports.
    pub fn kind(&self) -> &'static str {
        match self {
            Certificate::Invariant { .. } => "invariant",
            Certificate::KInduction { .. } => "k-induction",
            Certificate::TwoStep { .. } => "two-step",
            Certificate::Trace { .. } => "trace",
        }
    }
}

/// `0` / `1` characters for a row of values.
pub fn bits(values: impl IntoIterator<Item = bool>) -> String {
    values
        .into_iter()
        .map(|b| if b { '1' } else { '0' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificates_round_trip_as_json() {
        let all = [
            Certificate::Invariant {
                clauses: vec![vec![2, 5], vec![]],
            },
            Certificate::KInduction {
                k: 3,
                clauses: vec![vec![4]],
                simple_path: true,
            },
            Certificate::TwoStep { clauses: vec![] },
            Certificate::Trace {
                init: bits([false, true]),
                inputs: vec!["10".into(), "".into()],
            },
        ];
        for c in all {
            let s = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<Certificate>(&s).unwrap(), c, "{s}");
        }
        let s = serde_json::to_string(&Certificate::TwoStep { clauses: vec![] }).unwrap();
        assert_eq!(s, r#"{"kind":"two-step","clauses":[]}"#);
    }
}
