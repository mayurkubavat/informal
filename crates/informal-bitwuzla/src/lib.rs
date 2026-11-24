//! Word-level engines on Bitwuzla: bounded model checking ([`WordBmc`]),
//! k-induction ([`WordKind`]), and syntax-guided word-level IC3 with
//! counterexample-guided prophecy ([`ic3sa`]).

mod ffi;
pub mod ic3sa;
pub mod wbmc;
pub mod wkind;

use informal_engine::Engine;
use std::sync::Arc;

pub use ic3sa::Ic3Sa;
pub use wbmc::WordBmc;
pub use wkind::WordKind;

/// True when this build links Bitwuzla.
pub fn available() -> bool {
    cfg!(informal_bitwuzla)
}

/// The word-level engines (empty without Bitwuzla).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if !available() {
        return Vec::new();
    }
    vec![
        Arc::new(WordBmc { max_depth: None }),
        Arc::new(WordKind { max_depth: None }),
        Arc::new(Ic3Sa),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use informal_engine::{CheckRequest, ModelFormat, RunContext, Verdict};
    use std::path::Path;
    use std::time::Duration;

    fn check(text: &str, depth: usize) -> Verdict {
        let dir = std::env::temp_dir().join(format!("informal-wbmc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("m{}.btor2", text.len()));
        std::fs::write(&p, text).unwrap();
        let req = CheckRequest::new(p, ModelFormat::Btor2, Duration::from_secs(30));
        WordBmc {
            max_depth: Some(depth),
        }
        .check(&req, &RunContext::new(req.timeout))
        .unwrap()
        .verdict
    }

    #[test]
    fn counter_and_memory() {
        if !available() {
            informal_engine::tool_missing("needs Bitwuzla");
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        assert_eq!(
            check(&read("counter.btor2"), 20),
            Verdict::Unsafe { depth: Some(7) }
        );
        // Arrays: write-then-read never fails; wbmc reaches its bound, wkind proves Safe!
        assert!(matches!(
            check(&read("memory.btor2"), 10),
            Verdict::Unknown { .. }
        ));
        let p = root.join("memory.btor2");
        let req = CheckRequest::new(p, ModelFormat::Btor2, Duration::from_secs(30));
        assert_eq!(
            WordKind {
                max_depth: Some(10)
            }
            .check(&req, &RunContext::new(req.timeout))
            .unwrap()
            .verdict,
            Verdict::Safe
        );
    }

    #[test]
    fn array_bug_is_found() {
        if !available() {
            informal_engine::tool_missing("needs Bitwuzla");
            return;
        }
        let m = "1 sort bitvec 2\n2 sort bitvec 1\n3 sort array 1 2\n4 zero 2\n5 state 3 mem\n6 init 3 5 4\n\
                 7 input 1 a\n8 input 1 b\n9 one 2\n10 write 3 5 7 9\n11 next 3 5 10\n12 read 2 5 8\n13 bad 12\n";
        assert_eq!(check(m, 5), Verdict::Unsafe { depth: Some(1) });
    }

    #[cfg(informal_bitwuzla)]
    #[test]
    fn shim_models_push_pop_and_unsat_assumptions() {
        use std::sync::atomic::AtomicBool;
        let cancel = AtomicBool::new(false);
        let bz = ffi::Bz::new_ic3(5_000, &cancel);
        let bv4 = bz.bv_sort(4).unwrap();
        let x = bz.konst(bv4, "x").unwrap();
        let five = bz.value(bv4, "0101").unwrap();
        let six = bz.value(bv4, "0110").unwrap();
        let eq5 = bz.op("eq", &[x, five], 0, 0).unwrap();
        let eq6 = bz.op("eq", &[x, six], 0, 0).unwrap();
        let y = bz.konst(bv4, "y").unwrap();
        let eqy5 = bz.op("eq", &[y, five], 0, 0).unwrap();

        bz.push(1).unwrap();
        assert_eq!(bz.solve(&[eq5]).unwrap(), 10);
        assert_eq!(bz.get_value_bits(x).unwrap(), "0101");
        assert_eq!(bz.get_value_bits(eq5).unwrap(), "1");
        bz.pop(1).unwrap();

        // eq5 & eq6 is UNSAT; eqy5 is irrelevant and must NOT be in the unsat core.
        assert_eq!(bz.solve(&[eq5, eq6, eqy5]).unwrap(), 20);
        assert!(bz.is_unsat_assumption(eq5).unwrap());
        assert!(bz.is_unsat_assumption(eq6).unwrap());
        assert!(!bz.is_unsat_assumption(eqy5).unwrap());
    }

    fn check_ic3sa(tag: &str, text: &str) -> Verdict {
        let dir = std::env::temp_dir().join(format!("informal-ic3sa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("{tag}.btor2"));
        std::fs::write(&p, text).unwrap();
        let req = CheckRequest::new(p, ModelFormat::Btor2, Duration::from_secs(30));
        Ic3Sa
            .check(&req, &RunContext::new(req.timeout))
            .unwrap()
            .verdict
    }

    #[test]
    fn ic3sa_proves_unbounded_64bit_arrays_and_wide_datapaths() {
        if !available() {
            informal_engine::tool_missing("needs Bitwuzla");
            return;
        }
        // 1. 64-bit addressed memory initialized to 0, writes 0 at arbitrary waddr;
        //    bad: read(mem, raddr) != 0 (unbounded, requires universal prophecy invariant).
        let mem_preserve = "\
1 sort bitvec 64
2 sort bitvec 8
3 sort array 1 2
4 sort bitvec 1
5 zero 2
6 state 3 mem
7 init 3 6 5
8 input 1 waddr
9 input 1 raddr
10 write 3 6 8 5
11 next 3 6 10
12 read 2 6 9
13 neq 4 12 5
14 bad 13
";
        assert_eq!(check_ic3sa("mem_preserve", mem_preserve), Verdict::Safe);

        // 2. 64-bit addressed memory initialized to 0; writes 1 only when waddr < 16;
        //    bad: (raddr >= 16) && (read(mem, raddr) != 0).
        let write_guard = "\
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
        assert_eq!(check_ic3sa("write_guard", write_guard), Verdict::Safe);

        // 3. Two 64-bit shadow arrays: m1 init 0, m2 init 0xFF; m1' = write(m1, wa, d),
        //    m2' = write(m2, wa, d ^ 0xFF); bad: read(m1, ra) != (read(m2, ra) ^ 0xFF).
        let shadow_arrays = "\
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
        assert_eq!(check_ic3sa("shadow_arrays", shadow_arrays), Verdict::Safe);

        // 4. Wide 64-bit counter wrapping at 1_000_000: bad when cnt > 1_000_000.
        let wide_counter = "\
1 sort bitvec 64
2 sort bitvec 1
3 zero 1
4 one 1
5 constd 1 1000000
6 state 1 cnt
7 init 1 6 3
8 add 1 6 4
9 eq 2 6 5
10 ite 1 9 3 8
11 next 1 6 10
12 ugt 2 6 5
13 bad 12
";
        assert_eq!(check_ic3sa("wide_counter", wide_counter), Verdict::Safe);

        // 5. Unsafe array bug caught at exact depth 1.
        let arr_bug = "\
1 sort bitvec 64
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
        assert_eq!(
            check_ic3sa("arr_bug", arr_bug),
            Verdict::Unsafe { depth: Some(1) }
        );
    }
}
