//! A real Tcl 8.6 interpreter, embedded through the C API.
//!
//! JasperGold and VC Formal scripts are ordinary Tcl (procs, `foreach`,
//! `expr`, `source`, namespaces), so the compatibility layer runs them in
//! the real interpreter rather than a look-alike, and only adds the tool
//! commands. Commands are Rust closures that take their words as strings
//! and return a string result or an error message.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::Path;

#[allow(non_camel_case_types)]
mod ffi {
    use super::*;

    #[repr(C)]
    pub struct Tcl_Interp {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct Tcl_Obj {
        _private: [u8; 0],
    }

    pub type ObjCmdProc = unsafe extern "C" fn(
        data: *mut c_void,
        interp: *mut Tcl_Interp,
        objc: c_int,
        objv: *const *mut Tcl_Obj,
    ) -> c_int;
    pub type CmdDeleteProc = unsafe extern "C" fn(data: *mut c_void);

    pub const TCL_OK: c_int = 0;
    pub const TCL_ERROR: c_int = 1;
    pub const TCL_GLOBAL_ONLY: c_int = 1;
    pub const TCL_EVAL_GLOBAL: c_int = 0x020000;

    extern "C" {
        pub fn Tcl_FindExecutable(argv0: *const c_char);
        pub fn Tcl_CreateInterp() -> *mut Tcl_Interp;
        pub fn Tcl_DeleteInterp(interp: *mut Tcl_Interp);
        pub fn Tcl_Init(interp: *mut Tcl_Interp) -> c_int;
        pub fn Tcl_EvalEx(
            interp: *mut Tcl_Interp,
            script: *const c_char,
            len: c_int,
            flags: c_int,
        ) -> c_int;
        pub fn Tcl_EvalFile(interp: *mut Tcl_Interp, path: *const c_char) -> c_int;
        pub fn Tcl_GetStringResult(interp: *mut Tcl_Interp) -> *const c_char;
        pub fn Tcl_ResetResult(interp: *mut Tcl_Interp);
        pub fn Tcl_SetObjResult(interp: *mut Tcl_Interp, obj: *mut Tcl_Obj);
        pub fn Tcl_NewStringObj(bytes: *const c_char, len: c_int) -> *mut Tcl_Obj;
        pub fn Tcl_NewListObj(objc: c_int, objv: *const *mut Tcl_Obj) -> *mut Tcl_Obj;
        pub fn Tcl_GetStringFromObj(obj: *mut Tcl_Obj, len: *mut c_int) -> *const c_char;
        pub fn Tcl_GetVar2(
            interp: *mut Tcl_Interp,
            name: *const c_char,
            index: *const c_char,
            flags: c_int,
        ) -> *const c_char;
        pub fn Tcl_SetVar2(
            interp: *mut Tcl_Interp,
            name: *const c_char,
            index: *const c_char,
            value: *const c_char,
            flags: c_int,
        ) -> *const c_char;
        pub fn Tcl_CreateObjCommand(
            interp: *mut Tcl_Interp,
            name: *const c_char,
            proc_: ObjCmdProc,
            data: *mut c_void,
            delete: Option<CmdDeleteProc>,
        ) -> *mut c_void;
        pub fn Tcl_CommandComplete(cmd: *const c_char) -> c_int;
    }
}

/// What a command returns to Tcl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    /// A proper Tcl list, so scripts can `foreach` over it.
    List(Vec<String>),
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Str(s)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Str(s.into())
    }
}
impl From<()> for Value {
    fn from(_: ()) -> Self {
        Value::Str(String::new())
    }
}
impl From<Vec<String>> for Value {
    fn from(v: Vec<String>) -> Self {
        Value::List(v)
    }
}

/// A command body: gets the command words (`args[0]` is the name).
pub type Command = Box<dyn FnMut(&[String]) -> Result<Value, String>>;

/// A Tcl error from `eval`, with Tcl's `errorInfo` stack trace.
#[derive(Debug, Clone)]
pub struct TclError {
    pub message: String,
    pub info: String,
}

impl std::fmt::Display for TclError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.info)
    }
}
impl std::error::Error for TclError {}

/// One Tcl interpreter. Not `Send`: Tcl interpreters belong to one thread.
pub struct Interp {
    raw: *mut ffi::Tcl_Interp,
}

fn cstr(s: &str) -> CString {
    // Tcl strings cannot hold NUL; replace rather than fail.
    CString::new(s.replace('\0', "\\0")).expect("NUL removed")
}

unsafe fn obj_string(obj: *mut ffi::Tcl_Obj) -> String {
    let mut len: c_int = 0;
    let p = ffi::Tcl_GetStringFromObj(obj, &mut len);
    let bytes = std::slice::from_raw_parts(p as *const u8, len as usize);
    String::from_utf8_lossy(bytes).into_owned()
}

unsafe fn new_obj(s: &str) -> *mut ffi::Tcl_Obj {
    ffi::Tcl_NewStringObj(s.as_ptr() as *const c_char, s.len() as c_int)
}

thread_local! {
    /// The interpreter running the innermost Rust command, for
    /// [`caller_location`]. Commands can nest (a command that evaluates
    /// Tcl which calls another), so the previous value is restored after.
    static CURRENT: std::cell::Cell<*mut ffi::Tcl_Interp> =
        const { std::cell::Cell::new(std::ptr::null_mut()) };
}

/// Where the Rust command now running was called from: the script file and
/// line, from Tcl's `info frame` (Tcl 8.6 tracks lines for code read by
/// `source`, including loop and proc bodies defined in that file). `None`
/// outside a command callback, or when the call did not come from a file
/// (a console line, `eval` of a built string).
pub fn caller_location() -> Option<(String, u32)> {
    let raw = CURRENT.with(|c| c.get());
    if raw.is_null() {
        return None;
    }
    // The evaluated `info frame` runs one level below the command, so -1
    // is the command's own frame. The interp result is reset by the
    // command's return value afterwards, so clobbering it here is fine.
    let script = "info frame -1";
    let code = unsafe {
        ffi::Tcl_EvalEx(
            raw,
            script.as_ptr() as *const c_char,
            script.len() as c_int,
            0,
        )
    };
    let text = unsafe { CStr::from_ptr(ffi::Tcl_GetStringResult(raw)) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::Tcl_ResetResult(raw) };
    if code != ffi::TCL_OK {
        return None;
    }
    let dict = list_words(&text);
    let get = |k: &str| {
        dict.chunks(2)
            .find(|kv| kv.len() == 2 && kv[0] == k)
            .map(|kv| kv[1].clone())
    };
    let file = get("file")?;
    let line = get("line")?.parse().ok()?;
    Some((file, line))
}

/// The script being sourced (`info script`) while a Rust command runs;
/// `None` outside a command or when no `source` is in progress.
pub fn current_script() -> Option<String> {
    let raw = CURRENT.with(|c| c.get());
    if raw.is_null() {
        return None;
    }
    let script = "info script";
    let code = unsafe {
        ffi::Tcl_EvalEx(
            raw,
            script.as_ptr() as *const c_char,
            script.len() as c_int,
            0,
        )
    };
    let text = unsafe { CStr::from_ptr(ffi::Tcl_GetStringResult(raw)) }
        .to_string_lossy()
        .into_owned();
    unsafe { ffi::Tcl_ResetResult(raw) };
    (code == ffi::TCL_OK && !text.is_empty()).then_some(text)
}

/// Split a Tcl list the simple way `info frame` needs: words separated by
/// spaces, `{...}` groups (nested) kept whole without the outer braces.
fn list_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut w = String::new();
        if c == '{' {
            chars.next();
            let mut depth = 1;
            for c in chars.by_ref() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                w.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                if c == '\\' {
                    chars.next();
                }
                if let Some(c) = chars.next() {
                    w.push(c);
                }
            }
        }
        out.push(w);
    }
    out
}

/// A registered command, with a flag that is set while it runs: a command
/// that reaches itself again (through `eval` or `source`) must not get a
/// second `&mut` to its closure.
struct Slot {
    busy: std::cell::Cell<bool>,
    cmd: std::cell::UnsafeCell<Command>,
}

unsafe extern "C" fn trampoline(
    data: *mut c_void,
    interp: *mut ffi::Tcl_Interp,
    objc: c_int,
    objv: *const *mut ffi::Tcl_Obj,
) -> c_int {
    let slot = &*(data as *const Slot);
    let args: Vec<String> = (0..objc as usize)
        .map(|i| obj_string(*objv.add(i)))
        .collect();
    if slot.busy.replace(true) {
        ffi::Tcl_SetObjResult(
            interp,
            new_obj(&format!("{}: called from inside itself", args[0])),
        );
        return ffi::TCL_ERROR;
    }
    // SAFETY: `busy` was clear, so this is the only reference to the closure.
    let cmd = &mut *slot.cmd.get();
    let prev = CURRENT.with(|c| c.replace(interp));
    // A panic must not unwind into C.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cmd(&args)))
        .unwrap_or_else(|_| Err(format!("internal error in command {}", args[0])));
    slot.busy.set(false);
    CURRENT.with(|c| c.set(prev));
    match out {
        Ok(Value::Str(s)) => {
            ffi::Tcl_SetObjResult(interp, new_obj(&s));
            ffi::TCL_OK
        }
        Ok(Value::List(items)) => {
            let objs: Vec<_> = items.iter().map(|s| new_obj(s)).collect();
            ffi::Tcl_SetObjResult(
                interp,
                ffi::Tcl_NewListObj(objs.len() as c_int, objs.as_ptr()),
            );
            ffi::TCL_OK
        }
        Err(e) => {
            ffi::Tcl_SetObjResult(interp, new_obj(&e));
            ffi::TCL_ERROR
        }
    }
}

unsafe extern "C" fn drop_command(data: *mut c_void) {
    drop(Box::from_raw(data as *mut Slot));
}

impl Interp {
    /// A fresh interpreter with the Tcl library loaded (so `parray`,
    /// `auto_load` and friends work when the library is installed).
    pub fn new() -> Self {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.to_str().map(cstr))
                .unwrap_or_else(|| cstr("informal"));
            unsafe { ffi::Tcl_FindExecutable(exe.as_ptr()) };
        });
        let raw = unsafe { ffi::Tcl_CreateInterp() };
        assert!(!raw.is_null(), "Tcl_CreateInterp failed");
        // A missing init.tcl only loses library procs, not the core language.
        unsafe { ffi::Tcl_Init(raw) };
        Interp { raw }
    }

    /// Define (or replace) a command.
    pub fn register(&self, name: &str, cmd: Command) {
        let data = Box::into_raw(Box::new(Slot {
            busy: std::cell::Cell::new(false),
            cmd: std::cell::UnsafeCell::new(cmd),
        })) as *mut c_void;
        let name = cstr(name);
        unsafe {
            ffi::Tcl_CreateObjCommand(
                self.raw,
                name.as_ptr(),
                trampoline,
                data,
                Some(drop_command),
            )
        };
    }

    fn finish(&self, code: c_int) -> Result<String, TclError> {
        let result = unsafe { CStr::from_ptr(ffi::Tcl_GetStringResult(self.raw)) }
            .to_string_lossy()
            .into_owned();
        if code == ffi::TCL_ERROR {
            let info = self.get_var("errorInfo").unwrap_or_else(|| result.clone());
            unsafe { ffi::Tcl_ResetResult(self.raw) };
            return Err(TclError {
                message: result,
                info,
            });
        }
        Ok(result)
    }

    /// Evaluate a script at global level.
    pub fn eval(&self, script: &str) -> Result<String, TclError> {
        let code = unsafe {
            ffi::Tcl_EvalEx(
                self.raw,
                script.as_ptr() as *const c_char,
                script.len() as c_int,
                ffi::TCL_EVAL_GLOBAL,
            )
        };
        self.finish(code)
    }

    /// `source` a file (sets `info script`, so relative paths inside work
    /// the way the vendor tools resolve them).
    pub fn eval_file(&self, path: &Path) -> Result<String, TclError> {
        let p = cstr(&path.to_string_lossy());
        let code = unsafe { ffi::Tcl_EvalFile(self.raw, p.as_ptr()) };
        self.finish(code)
    }

    pub fn get_var(&self, name: &str) -> Option<String> {
        let n = cstr(name);
        let p = unsafe {
            ffi::Tcl_GetVar2(self.raw, n.as_ptr(), std::ptr::null(), ffi::TCL_GLOBAL_ONLY)
        };
        (!p.is_null()).then(|| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }

    pub fn set_var(&self, name: &str, value: &str) {
        let (n, v) = (cstr(name), cstr(value));
        unsafe {
            ffi::Tcl_SetVar2(
                self.raw,
                n.as_ptr(),
                std::ptr::null(),
                v.as_ptr(),
                ffi::TCL_GLOBAL_ONLY,
            )
        };
    }

    /// True when `script` is a complete command (no open braces or quotes);
    /// the REPL keeps reading lines until it is.
    pub fn is_complete(script: &str) -> bool {
        let s = cstr(script);
        unsafe { ffi::Tcl_CommandComplete(s.as_ptr()) != 0 }
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Interp {
    fn drop(&mut self) {
        unsafe { ffi::Tcl_DeleteInterp(self.raw) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[test]
    fn evaluates_tcl() {
        let i = Interp::new();
        assert_eq!(i.eval("expr {6 * 7}").unwrap(), "42");
        assert_eq!(
            i.eval("proc sq {x} {expr {$x*$x}}; set r {}; foreach v {1 2 3} {lappend r [sq $v]}; set r")
                .unwrap(),
            "1 4 9"
        );
    }

    #[test]
    fn rust_commands_get_words_and_return_lists() {
        let i = Interp::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        i.register(
            "assert",
            Box::new(move |a| {
                s.borrow_mut().push(a.to_vec());
                Ok(vec!["p 1".to_string(), "q".to_string()].into())
            }),
        );
        let out = i
            .eval("set n x; assert -name $n {a |-> b}; llength [assert]")
            .unwrap();
        assert_eq!(out, "2");
        assert_eq!(seen.borrow()[0], ["assert", "-name", "x", "a |-> b"]);
    }

    /// A command that evaluates Tcl which calls the same command gets an
    /// error from the inner call (not a second `&mut` to its closure).
    #[test]
    fn a_command_that_reaches_itself_is_an_error() {
        let i = Interp::new();
        i.register(
            "again",
            Box::new(|_| {
                let interp = CURRENT.with(|c| c.get());
                let script = "again";
                let code = unsafe {
                    ffi::Tcl_EvalEx(
                        interp,
                        script.as_ptr() as *const c_char,
                        script.len() as c_int,
                        0,
                    )
                };
                Ok(Value::Str(format!("inner {}", code == ffi::TCL_ERROR)))
            }),
        );
        assert_eq!(i.eval("again").unwrap(), "inner true");
        // And it is callable again afterwards.
        assert_eq!(i.eval("again").unwrap(), "inner true");
    }

    #[test]
    fn errors_carry_the_message_and_stack() {
        let i = Interp::new();
        i.register("boom", Box::new(|_| Err("bad option".into())));
        let e = i.eval("proc f {} {boom}; f").unwrap_err();
        assert_eq!(e.message, "bad option");
        assert!(e.info.contains("\"f\""), "{}", e.info);
        // The interpreter is still usable after an error.
        assert_eq!(i.eval("set x 1").unwrap(), "1");
    }

    #[test]
    fn caller_location_from_sourced_files() {
        let dir = std::env::temp_dir().join(format!("informal-tcl-loc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.tcl");
        std::fs::write(
            &f,
            "# header\nhere a\n\nforeach x {1 2} {\n    here b$x\n}\nproc p {} {\n  here c\n}\np\n",
        )
        .unwrap();
        let i = Interp::new();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let s = seen.clone();
        i.register(
            "here",
            Box::new(move |a| {
                s.borrow_mut().push((a[1].clone(), caller_location()));
                Ok(().into())
            }),
        );
        i.eval_file(&f).unwrap();
        i.eval("here d").unwrap();
        let file = f.to_string_lossy().into_owned();
        let at = |n| Some((file.clone(), n));
        assert_eq!(
            *seen.borrow(),
            [
                ("a".to_string(), at(2)),
                ("b1".to_string(), at(5)),
                ("b2".to_string(), at(5)),
                ("c".to_string(), at(8)),
                ("d".to_string(), None),
            ]
        );
        assert_eq!(caller_location(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completeness() {
        assert!(Interp::is_complete("set a {b c}"));
        assert!(!Interp::is_complete("proc f {} {"));
    }
}
