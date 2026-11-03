//! Word-level engines on Bitwuzla. Stage 1 of the round-2 plan has a
//! word-level BMC here: Btor2 is unrolled frame by frame into Bitwuzla
//! terms, arrays included, which the bit-level engines cannot take.
//! IC3SA and counterexample-guided prophecy come in stage 4.

use fdv_engine::{
    Capabilities, CheckRequest, Engine, EngineResult, ModelFormat, Progress, RunContext, Verdict,
};
use fdv_ir::{Line, Model, Op, Sort};
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[cfg(fdv_bitwuzla)]
mod ffi {
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::atomic::AtomicBool;
    extern "C" {
        pub fn fdv_bw_new(timeout_ms: i64, cancel: *const AtomicBool) -> *mut c_void;
        pub fn fdv_bw_free(p: *mut c_void);
        pub fn fdv_bw_error(p: *mut c_void) -> *const c_char;
        pub fn fdv_bw_bv_sort(p: *mut c_void, w: u64) -> c_int;
        pub fn fdv_bw_array_sort(p: *mut c_void, i: c_int, e: c_int) -> c_int;
        pub fn fdv_bw_const(p: *mut c_void, sort: c_int, name: *const c_char) -> c_int;
        pub fn fdv_bw_value(p: *mut c_void, sort: c_int, bits: *const c_char) -> c_int;
        pub fn fdv_bw_const_array(p: *mut c_void, sort: c_int, value: c_int) -> c_int;
        pub fn fdv_bw_op(
            p: *mut c_void,
            op: *const c_char,
            n: c_int,
            args: *const c_int,
            i0: u64,
            i1: u64,
        ) -> c_int;
        pub fn fdv_bw_assert(p: *mut c_void, t: c_int);
        pub fn fdv_bw_check(p: *mut c_void, n: c_int, assume: *const c_int) -> c_int;
    }
}

/// True when this build links Bitwuzla.
pub fn available() -> bool {
    cfg!(fdv_bitwuzla)
}

type T = i32;

#[cfg(fdv_bitwuzla)]
struct Bz {
    p: *mut std::ffi::c_void,
}

#[cfg(fdv_bitwuzla)]
impl Drop for Bz {
    fn drop(&mut self) {
        unsafe { ffi::fdv_bw_free(self.p) }
    }
}

#[cfg(fdv_bitwuzla)]
impl Bz {
    fn new(timeout_ms: i64, cancel: &AtomicBool) -> Self {
        Bz {
            p: unsafe { ffi::fdv_bw_new(timeout_ms, cancel) },
        }
    }
    fn check(&self, r: i32) -> anyhow::Result<i32> {
        if r < 0 {
            let e = unsafe { CStr::from_ptr(ffi::fdv_bw_error(self.p)) }
                .to_string_lossy()
                .into_owned();
            anyhow::bail!("bitwuzla: {e}");
        }
        Ok(r)
    }
    fn bv_sort(&self, w: u32) -> i32 {
        unsafe { ffi::fdv_bw_bv_sort(self.p, w as u64) }
    }
    fn array_sort(&self, i: i32, e: i32) -> i32 {
        unsafe { ffi::fdv_bw_array_sort(self.p, i, e) }
    }
    fn konst(&self, sort: i32, name: &str) -> T {
        let n = CString::new(name.replace('\0', "")).unwrap();
        unsafe { ffi::fdv_bw_const(self.p, sort, n.as_ptr()) }
    }
    fn value(&self, sort: i32, bits: &str) -> anyhow::Result<T> {
        let b = CString::new(bits).unwrap();
        self.check(unsafe { ffi::fdv_bw_value(self.p, sort, b.as_ptr()) })
    }
    fn const_array(&self, sort: i32, v: T) -> T {
        unsafe { ffi::fdv_bw_const_array(self.p, sort, v) }
    }
    fn op(&self, op: &str, args: &[T], i0: u64, i1: u64) -> anyhow::Result<T> {
        let o = CString::new(op).unwrap();
        self.check(unsafe {
            ffi::fdv_bw_op(
                self.p,
                o.as_ptr() as *const c_char,
                args.len() as i32,
                args.as_ptr(),
                i0,
                i1,
            )
        })
    }
    fn assert(&self, t: T) {
        unsafe { ffi::fdv_bw_assert(self.p, t) }
    }
    fn solve(&self, assume: &[T]) -> anyhow::Result<i32> {
        self.check(unsafe { ffi::fdv_bw_check(self.p, assume.len() as i32, assume.as_ptr()) })
    }
}

/// Bits (MSB first) of a Btor2 constant line.
fn const_bits(l: &Line, w: u32) -> anyhow::Result<String> {
    let w = w as usize;
    let lsb_first: Vec<bool> = match l.op {
        Op::Zero => vec![false; w],
        Op::One => (0..w).map(|i| i == 0).collect(),
        Op::Ones => vec![true; w],
        Op::Const => {
            let s = l.nums[0].as_bytes();
            (0..w)
                .map(|i| s.len() > i && s[s.len() - 1 - i] == b'1')
                .collect()
        }
        Op::Consth => {
            let mut b = Vec::new();
            for ch in l.nums[0].chars().rev() {
                let d = ch
                    .to_digit(16)
                    .ok_or_else(|| anyhow::anyhow!("bad hex constant"))?;
                b.extend((0..4).map(|i| (d >> i) & 1 == 1));
            }
            b.resize(w, false);
            b
        }
        Op::Constd => {
            let (neg, digits) = match l.nums[0].strip_prefix('-') {
                Some(d) => (true, d),
                None => (false, l.nums[0].as_str()),
            };
            let mut m = vec![false; w];
            for ch in digits.chars() {
                let mut carry = ch
                    .to_digit(10)
                    .ok_or_else(|| anyhow::anyhow!("bad decimal constant"))?
                    as u64;
                for b in m.iter_mut() {
                    let v = *b as u64 * 10 + carry;
                    *b = v & 1 == 1;
                    carry = v >> 1;
                }
            }
            if neg {
                let mut carry = true;
                for b in m.iter_mut() {
                    let v = !*b;
                    *b = v ^ carry;
                    carry = v && carry;
                }
            }
            m
        }
        _ => unreachable!(),
    };
    Ok(lsb_first
        .iter()
        .rev()
        .map(|&b| if b { '1' } else { '0' })
        .collect())
}

/// Word-level BMC on Bitwuzla.
pub struct WordBmc {
    pub max_depth: Option<usize>,
}

impl Engine for WordBmc {
    fn name(&self) -> &str {
        "fdv-wbmc"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            formats: vec![ModelFormat::Btor2],
            finds_bugs: true,
            ..Default::default()
        }
    }
    fn check(&self, req: &CheckRequest, ctx: &RunContext) -> anyhow::Result<EngineResult> {
        let t0 = Instant::now();
        let text = std::fs::read_to_string(&req.model)?;
        let m = fdv_ir::parse(&text)?;
        let verdict = run(&m, ctx, self.max_depth, self.name())?;
        Ok(EngineResult {
            engine: self.name().into(),
            verdict,
            wall_ms: t0.elapsed().as_millis(),
            witness: None,
        })
    }
}

#[cfg(not(fdv_bitwuzla))]
fn run(_: &Model, _: &RunContext, _: Option<usize>, _: &str) -> anyhow::Result<Verdict> {
    Ok(Verdict::Unknown {
        reason: "built without Bitwuzla".into(),
    })
}

#[cfg(fdv_bitwuzla)]
fn run(m: &Model, ctx: &RunContext, max: Option<usize>, name: &str) -> anyhow::Result<Verdict> {
    if m.with_op(Op::Bad).next().is_none() {
        return Ok(Verdict::Unknown {
            reason: "model has no bad-state property".into(),
        });
    }
    if m.with_op(Op::Fair).next().is_some() || m.with_op(Op::Justice).next().is_some() {
        return Ok(Verdict::Unknown {
            reason: "liveness is not supported by this engine".into(),
        });
    }
    // Cancellation reaches the solver through a flag a watcher thread sets.
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (cancel, done, token) = (cancel.clone(), done.clone(), ctx.cancel.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if token.is_cancelled() {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let left = ctx
        .deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as i64;
    let bz = Bz::new(left, &cancel);
    let result = unroll(m, &bz, ctx, max, name);
    done.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    result
}

#[cfg(fdv_bitwuzla)]
fn unroll(
    m: &Model,
    bz: &Bz,
    ctx: &RunContext,
    max: Option<usize>,
    name: &str,
) -> anyhow::Result<Verdict> {
    // Sorts.
    let mut sorts: HashMap<u32, i32> = HashMap::new();
    for e in m.entries() {
        if let fdv_ir::Entry::Sort { id, sort, .. } = e {
            let s = match *sort {
                Sort::BitVec(w) => bz.bv_sort(w),
                Sort::Array { index, element } => bz.array_sort(sorts[&index], sorts[&element]),
            };
            sorts.insert(*id, s);
        }
    }
    let next: HashMap<u32, i64> = m
        .with_op(Op::Next)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let init: HashMap<u32, i64> = m
        .with_op(Op::Init)
        .map(|l| (l.args[0] as u32, l.args[1]))
        .collect();
    let mut prev: HashMap<u32, T> = HashMap::new();
    let mut k = 0usize;
    loop {
        if ctx.should_stop() {
            return Ok(Verdict::Unknown {
                reason: "timeout".into(),
            });
        }
        if max.is_some_and(|d| k > d) {
            return Ok(Verdict::Unknown {
                reason: format!("bound {} reached", k - 1),
            });
        }
        let mut f: HashMap<u32, T> = HashMap::new();
        let get = |f: &HashMap<u32, T>, a: i64| -> anyhow::Result<T> {
            let t = *f
                .get(&(a.unsigned_abs() as u32))
                .ok_or_else(|| anyhow::anyhow!("node {} used before definition", a.abs()))?;
            if a < 0 {
                bz.op("not", &[t], 0, 0)
            } else {
                Ok(t)
            }
        };
        for l in m.lines() {
            let sort = l.sort.map(|s| sorts[&s]);
            let width = l.sort.and_then(|s| m.width(s));
            let t = match l.op {
                Op::State => {
                    if k == 0 {
                        bz.konst(sort.unwrap(), l.symbol.as_deref().unwrap_or(""))
                    } else {
                        match next.get(&l.id) {
                            Some(&n) => {
                                let p = *prev.get(&(n.unsigned_abs() as u32)).unwrap();
                                if n < 0 {
                                    bz.op("not", &[p], 0, 0)?
                                } else {
                                    p
                                }
                            }
                            None => bz.konst(sort.unwrap(), ""),
                        }
                    }
                }
                Op::Input => bz.konst(sort.unwrap(), l.symbol.as_deref().unwrap_or("")),
                Op::Zero | Op::One | Op::Ones | Op::Const | Op::Constd | Op::Consth => {
                    bz.value(sort.unwrap(), &const_bits(l, width.unwrap())?)?
                }
                Op::Sext | Op::Uext => {
                    bz.op(l.op.name(), &[get(&f, l.args[0])?], l.nums[0].parse()?, 0)?
                }
                Op::Slice => bz.op(
                    "slice",
                    &[get(&f, l.args[0])?],
                    l.nums[0].parse()?,
                    l.nums[1].parse()?,
                )?,
                Op::Init
                | Op::Next
                | Op::Bad
                | Op::Constraint
                | Op::Output
                | Op::Fair
                | Op::Justice => continue,
                op => {
                    let args = l
                        .args
                        .iter()
                        .map(|&a| get(&f, a))
                        .collect::<anyhow::Result<Vec<_>>>()?;
                    bz.op(op.name(), &args, 0, 0)?
                }
            };
            f.insert(l.id, t);
        }
        if k == 0 {
            for (&s, &v) in &init {
                let st = f[&s];
                let mut val = get(&f, v)?;
                // A bit-vector init of an array state is a constant array.
                let ss = m.line(s).and_then(|l| l.sort).unwrap();
                if matches!(m.sort(ss), Some(Sort::Array { .. }))
                    && m.line(v.unsigned_abs() as u32)
                        .and_then(|l| l.sort)
                        .is_some_and(|vs| m.width(vs).is_some())
                {
                    val = bz.const_array(sorts[&ss], val);
                }
                let eq = bz.op("eq", &[st, val], 0, 0)?;
                bz.assert(eq);
            }
        }
        for l in m.with_op(Op::Constraint) {
            bz.assert(get(&f, l.args[0])?);
        }
        let bads: Vec<T> = m
            .with_op(Op::Bad)
            .map(|l| get(&f, l.args[0]))
            .collect::<anyhow::Result<_>>()?;
        let mut any = bads[0];
        for &b in &bads[1..] {
            any = bz.op("or", &[any, b], 0, 0)?;
        }
        match bz.solve(&[any])? {
            10 => {
                return Ok(Verdict::Unsafe {
                    depth: Some(k as u32),
                })
            }
            20 => {
                let nb = bz.op("not", &[any], 0, 0)?;
                bz.assert(nb);
            }
            _ => {
                return Ok(Verdict::Unknown {
                    reason: "timeout".into(),
                })
            }
        }
        ctx.report(Progress {
            engine: name.into(),
            bound: Some(k as u32),
            note: String::new(),
        });
        prev = f;
        k += 1;
    }
}

/// The word-level engines (empty without Bitwuzla).
pub fn engines() -> Vec<Arc<dyn Engine>> {
    if !available() {
        return Vec::new();
    }
    vec![Arc::new(WordBmc { max_depth: None })]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn check(text: &str, depth: usize) -> Verdict {
        let dir = std::env::temp_dir().join(format!("fdv-wbmc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(format!("m{}.btor2", text.len()));
        std::fs::write(&p, text).unwrap();
        let req = CheckRequest {
            model: p,
            format: ModelFormat::Btor2,
            timeout: Duration::from_secs(30),
        };
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
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/models");
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        assert_eq!(
            check(&read("counter.btor2"), 20),
            Verdict::Unsafe { depth: Some(7) }
        );
        // Arrays: write-then-read never fails.
        assert!(matches!(
            check(&read("memory.btor2"), 10),
            Verdict::Unknown { .. }
        ));
    }

    #[test]
    fn array_bug_is_found() {
        if !available() {
            return;
        }
        // mem starts all zero; each step writes 1 at address `a`; bad when
        // mem[b] is 1: reachable at depth 1.
        let m = "1 sort bitvec 2\n2 sort bitvec 1\n3 sort array 1 2\n4 zero 2\n5 state 3 mem\n6 init 3 5 4\n\
                 7 input 1 a\n8 input 1 b\n9 one 2\n10 write 3 5 7 9\n11 next 3 5 10\n12 read 2 5 8\n13 bad 12\n";
        assert_eq!(check(m, 5), Verdict::Unsafe { depth: Some(1) });
    }
}
