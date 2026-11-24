//! Safe Rust wrapper around Bitwuzla's C++ shim and shared Btor2 helpers.

#[cfg(informal_bitwuzla)]
use informal_engine::RunContext;
#[cfg(informal_bitwuzla)]
use informal_ir::{Line, Model, Op, Sort};
#[cfg(informal_bitwuzla)]
use std::collections::HashMap;
#[cfg(informal_bitwuzla)]
use std::ffi::{c_char, CStr, CString};
#[cfg(informal_bitwuzla)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(informal_bitwuzla)]
use std::sync::Arc;
#[cfg(informal_bitwuzla)]
use std::time::{Duration, Instant};

#[cfg(informal_bitwuzla)]
mod raw {
    use std::ffi::{c_char, c_int, c_void};
    use std::sync::atomic::AtomicBool;
    extern "C" {
        pub fn informal_bw_new(timeout_ms: i64, cancel: *const AtomicBool) -> *mut c_void;
        pub fn informal_bw_new_opts(
            timeout_ms: i64,
            cancel: *const AtomicBool,
            produce_models: c_int,
            produce_unsat_assumptions: c_int,
        ) -> *mut c_void;
        pub fn informal_bw_free(p: *mut c_void);
        pub fn informal_bw_error(p: *mut c_void) -> *const c_char;
        pub fn informal_bw_bv_sort(p: *mut c_void, w: u64) -> c_int;
        pub fn informal_bw_array_sort(p: *mut c_void, i: c_int, e: c_int) -> c_int;
        pub fn informal_bw_const(p: *mut c_void, sort: c_int, name: *const c_char) -> c_int;
        pub fn informal_bw_value(p: *mut c_void, sort: c_int, bits: *const c_char) -> c_int;
        pub fn informal_bw_const_array(p: *mut c_void, sort: c_int, value: c_int) -> c_int;
        pub fn informal_bw_op(
            p: *mut c_void,
            op: *const c_char,
            n: c_int,
            args: *const c_int,
            i0: u64,
            i1: u64,
        ) -> c_int;
        pub fn informal_bw_assert(p: *mut c_void, t: c_int) -> c_int;
        pub fn informal_bw_check(p: *mut c_void, n: c_int, assume: *const c_int) -> c_int;
        pub fn informal_bw_push(p: *mut c_void, levels: u32) -> c_int;
        pub fn informal_bw_pop(p: *mut c_void, levels: u32) -> c_int;
        pub fn informal_bw_is_unsat_assumption(p: *mut c_void, t: c_int) -> c_int;
        pub fn informal_bw_get_value_bits(p: *mut c_void, t: c_int) -> *const c_char;
    }
}

#[cfg(informal_bitwuzla)]
pub(crate) type T = i32;

#[cfg(informal_bitwuzla)]
pub(crate) struct Bz {
    p: *mut std::ffi::c_void,
}

#[cfg(informal_bitwuzla)]
impl Drop for Bz {
    fn drop(&mut self) {
        unsafe { raw::informal_bw_free(self.p) }
    }
}

#[cfg(informal_bitwuzla)]
impl Bz {
    pub(crate) fn new(timeout_ms: i64, cancel: &AtomicBool) -> Self {
        Bz {
            p: unsafe { raw::informal_bw_new(timeout_ms, cancel) },
        }
    }

    pub(crate) fn new_ic3(timeout_ms: i64, cancel: &AtomicBool) -> Self {
        Bz {
            p: unsafe { raw::informal_bw_new_opts(timeout_ms, cancel, 1, 1) },
        }
    }

    fn last_error(&self) -> String {
        unsafe { CStr::from_ptr(raw::informal_bw_error(self.p)) }
            .to_string_lossy()
            .into_owned()
    }

    fn check(&self, r: i32) -> anyhow::Result<i32> {
        if r < 0 {
            anyhow::bail!("bitwuzla: {}", self.last_error());
        }
        Ok(r)
    }

    pub(crate) fn bv_sort(&self, w: u32) -> anyhow::Result<i32> {
        self.check(unsafe { raw::informal_bw_bv_sort(self.p, w as u64) })
    }

    pub(crate) fn array_sort(&self, i: i32, e: i32) -> anyhow::Result<i32> {
        self.check(unsafe { raw::informal_bw_array_sort(self.p, i, e) })
    }

    pub(crate) fn konst(&self, sort: i32, name: &str) -> anyhow::Result<T> {
        let n = CString::new(name.replace('\0', "")).unwrap();
        self.check(unsafe { raw::informal_bw_const(self.p, sort, n.as_ptr()) })
    }

    pub(crate) fn value(&self, sort: i32, bits: &str) -> anyhow::Result<T> {
        let b = CString::new(bits).unwrap();
        self.check(unsafe { raw::informal_bw_value(self.p, sort, b.as_ptr()) })
    }

    pub(crate) fn const_array(&self, sort: i32, v: T) -> anyhow::Result<T> {
        self.check(unsafe { raw::informal_bw_const_array(self.p, sort, v) })
    }

    pub(crate) fn op(&self, op: &str, args: &[T], i0: u64, i1: u64) -> anyhow::Result<T> {
        let o = CString::new(op).unwrap();
        self.check(unsafe {
            raw::informal_bw_op(
                self.p,
                o.as_ptr() as *const c_char,
                args.len() as i32,
                args.as_ptr(),
                i0,
                i1,
            )
        })
    }

    pub(crate) fn assert(&self, t: T) -> anyhow::Result<()> {
        self.check(unsafe { raw::informal_bw_assert(self.p, t) })
            .map(|_| ())
    }

    pub(crate) fn solve(&self, assume: &[T]) -> anyhow::Result<i32> {
        self.check(unsafe { raw::informal_bw_check(self.p, assume.len() as i32, assume.as_ptr()) })
    }

    #[allow(dead_code)]
    pub(crate) fn push(&self, levels: u32) -> anyhow::Result<()> {
        self.check(unsafe { raw::informal_bw_push(self.p, levels) })
            .map(|_| ())
    }

    #[allow(dead_code)]
    pub(crate) fn pop(&self, levels: u32) -> anyhow::Result<()> {
        self.check(unsafe { raw::informal_bw_pop(self.p, levels) })
            .map(|_| ())
    }

    #[allow(dead_code)]
    pub(crate) fn is_unsat_assumption(&self, t: T) -> anyhow::Result<bool> {
        Ok(self.check(unsafe { raw::informal_bw_is_unsat_assumption(self.p, t) })? != 0)
    }

    #[allow(dead_code)]
    pub(crate) fn get_value_bits(&self, t: T) -> anyhow::Result<String> {
        let ptr = unsafe { raw::informal_bw_get_value_bits(self.p, t) };
        if ptr.is_null() {
            anyhow::bail!("bitwuzla: {}", self.last_error());
        }
        Ok(unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned())
    }
}

/// Run `f(left_ms, &cancel_flag)` with a watcher thread forwarding `ctx.cancel`
/// and memory-budget shedding into `cancel_flag`.
#[cfg(informal_bitwuzla)]
pub(crate) fn with_cancel<R>(
    ctx: &RunContext,
    f: impl FnOnce(i64, &AtomicBool) -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    let cancel = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (cancel, done, token) = (cancel.clone(), done.clone(), ctx.cancel.clone());
        std::thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if token.is_cancelled() || informal_engine::memory::exceeded() {
                    cancel.store(true, Ordering::Relaxed);
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let left = ctx
        .deadline
        .saturating_duration_since(Instant::now())
        .as_millis() as i64;
    let res = f(left.max(1), &cancel);
    done.store(true, Ordering::Relaxed);
    let _ = watcher.join();
    res
}

/// Bits (MSB first) of a Btor2 constant line.
#[cfg(informal_bitwuzla)]
pub(crate) fn const_bits(l: &Line, w: u32) -> anyhow::Result<String> {
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

#[cfg(informal_bitwuzla)]
pub(crate) fn build_sorts(m: &Model, bz: &Bz) -> anyhow::Result<HashMap<u32, i32>> {
    let mut sorts: HashMap<u32, i32> = HashMap::new();
    for e in m.entries() {
        if let informal_ir::Entry::Sort { id, sort, .. } = e {
            let s = match *sort {
                Sort::BitVec(w) => bz.bv_sort(w)?,
                Sort::Array { index, element } => bz.array_sort(sorts[&index], sorts[&element])?,
            };
            sorts.insert(*id, s);
        }
    }
    Ok(sorts)
}

#[cfg(informal_bitwuzla)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn unroll_step(
    m: &Model,
    bz: &Bz,
    sorts: &HashMap<u32, i32>,
    init: &HashMap<u32, i64>,
    next: &HashMap<u32, i64>,
    prev: &HashMap<u32, T>,
    k: usize,
    assert_init: bool,
) -> anyhow::Result<(HashMap<u32, T>, T)> {
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
                    bz.konst(sort.unwrap(), l.symbol.as_deref().unwrap_or(""))?
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
                        None => bz.konst(sort.unwrap(), "")?,
                    }
                }
            }
            Op::Input => bz.konst(sort.unwrap(), l.symbol.as_deref().unwrap_or(""))?,
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
    if k == 0 && assert_init {
        for (&s, &v) in init {
            let st = f[&s];
            let mut val = get(&f, v)?;
            let ss = m.line(s).and_then(|l| l.sort).unwrap();
            if matches!(m.sort(ss), Some(Sort::Array { .. }))
                && m.line(v.unsigned_abs() as u32)
                    .and_then(|l| l.sort)
                    .is_some_and(|vs| m.width(vs).is_some())
            {
                val = bz.const_array(sorts[&ss], val)?;
            }
            let eq = bz.op("eq", &[st, val], 0, 0)?;
            bz.assert(eq)?;
        }
    }
    for l in m.with_op(Op::Constraint) {
        bz.assert(get(&f, l.args[0])?)?;
    }
    let bads: Vec<T> = m
        .with_op(Op::Bad)
        .map(|l| get(&f, l.args[0]))
        .collect::<anyhow::Result<_>>()?;
    let mut any = bads[0];
    for &b in &bads[1..] {
        any = bz.op("or", &[any, b], 0, 0)?;
    }
    Ok((f, any))
}
