//! Tcl command sets that let existing JasperGold (`jg`) and VC Formal
//! (`vcf`) FPV scripts run unchanged on informal.
//!
//! Both flavors drive the same [`Session`]. Each flavor's commands are
//! defined under its namespace (`jg::analyze`, `vcf::check_fv`) and the
//! active flavor's are also imported at global level, so a script calls
//! them by their usual names. The two tools disagree on `analyze` and
//! `elaborate`, which is why one flavor is active at a time.
//!
//! Options that only tune the vendor engines or GUI are accepted and
//! ignored with a note; options that would change the meaning of a check
//! and are not implemented are errors, so a migrated script never passes
//! silently on a different setup.

use crate::session::{
    glob, parse_duration, qualify, Clock, Kind, Session, SourceLoc, Status, EMBEDDED,
};
use informal_tcl::{Interp, TclError, Value};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub mod check;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    Jg,
    Vcf,
}

impl Flavor {
    pub fn parse(s: &str) -> Option<Flavor> {
        match s {
            "jg" | "jasper" | "jaspergold" => Some(Flavor::Jg),
            "vcf" | "vcformal" | "vc_formal" => Some(Flavor::Vcf),
            _ => None,
        }
    }

    fn ns(self) -> &'static str {
        match self {
            Flavor::Jg => "jg",
            Flavor::Vcf => "vcf",
        }
    }
}

/// Pick the flavor a script was written for from the commands it uses.
pub fn detect_flavor(script: &str) -> Flavor {
    const VCF: &[&str] = &[
        "read_file",
        "create_clock",
        "create_reset",
        "fvassert",
        "fvassume",
        "fvcover",
        "check_fv",
        "report_fv",
        "set_fml_appmode",
        "set_fml_var",
        "sim_run",
        "get_props",
    ];
    let vcf = script.lines().any(|l| {
        let first = l.split_whitespace().next().unwrap_or("");
        VCF.contains(&first)
    });
    if vcf {
        Flavor::Vcf
    } else {
        Flavor::Jg
    }
}

type Shared = Rc<RefCell<Session>>;
type Body = fn(&mut Session, &[String]) -> Result<Value, String>;

/// A Tcl interpreter with one flavor's commands bound to a session.
pub struct Shell {
    pub interp: Interp,
    pub session: Shared,
    pub flavor: Flavor,
}

impl Shell {
    pub fn new(session: Session, flavor: Flavor) -> Self {
        let interp = Interp::new();
        let session = Rc::new(RefCell::new(session));
        // Tcl's own `clock` is kept as `::informal::tcl_clock`; jg's `clock`
        // forwards Tcl subcommands (`clock seconds`) to it.
        interp
            .eval("namespace eval ::informal {}; rename ::clock ::informal::tcl_clock; namespace eval ::jg {}; namespace eval ::vcf {}")
            .expect("Tcl setup");
        let bind = |ns: &str, name: &str, f: Body| {
            let s = session.clone();
            interp.register(
                &format!("::{ns}::{name}"),
                Box::new(move |args| {
                    let mut sess = s.try_borrow_mut().map_err(|_| "session busy".to_string())?;
                    let r = f(&mut sess, args);
                    if !sess.dry_run {
                        return r;
                    }
                    // Check mode: record the outcome and carry on, so one
                    // rejected command does not hide the ones after it.
                    let name = args[0].rsplit("::").next().unwrap_or(&args[0]);
                    check::used(name, r.is_ok());
                    r.or_else(|e| {
                        check::finding(check::Kind::Error, name, &e);
                        Ok(Value::Str(String::new()))
                    })
                }),
            );
        };
        for (name, f) in COMMON {
            bind("informal", name, *f);
            bind("jg", name, *f);
            bind("vcf", name, *f);
        }
        for (name, f) in JG {
            bind("jg", name, *f);
        }
        for (name, f) in VCF {
            bind("vcf", name, *f);
        }
        interp
            .eval(
                r#"
proc ::jg::include {file} { uplevel #0 [list source $file] }
proc ::jg::clock {args} {
    if {[llength $args] && [lindex $args 0] in {seconds milliseconds microseconds clicks format scan add}} {
        return [::informal::tcl_clock {*}$args]
    }
    ::jg::clock_cmd {*}$args
}
namespace eval ::jg { namespace export * }
namespace eval ::vcf { namespace export * }
"#,
            )
            .expect("Tcl setup");
        let shell = Shell {
            interp,
            session,
            flavor,
        };
        shell.activate(flavor);
        shell
    }

    /// Import a flavor's commands at global level (the other's go away).
    pub fn activate(&self, flavor: Flavor) {
        let names: Vec<&str> = COMMON
            .iter()
            .map(|c| c.0)
            .chain(JG.iter().map(|c| c.0))
            .chain(VCF.iter().map(|c| c.0))
            .chain(["include", "clock"])
            .collect();
        for n in names {
            // Only remove global commands we imported (never Tcl builtins).
            let _ = self.interp.eval(&format!(
                "if {{[namespace origin ::{n}] ne {{::{n}}}}} {{ rename ::{n} {{}} }}"
            ));
        }
        self.interp
            .eval(&format!(
                "namespace eval :: {{ namespace import -force ::{}::* }}",
                flavor.ns()
            ))
            .expect("import flavor");
        self.interp.set_var("informal_flavor", flavor.ns());
    }

    /// Check mode: run `script` without elaborating or proving, and report
    /// per command what would be honoured, ignored or rejected. The script
    /// is evaluated one top-level command at a time, so an error does not
    /// stop the check; `source` / `include` are followed the same way.
    pub fn check(&self, script: &Path) -> check::Report {
        self.session.borrow_mut().dry_run = true;
        check::start();
        let reg = |name: &str, f: fn(&[String])| {
            self.interp.register(
                name,
                Box::new(move |a| {
                    f(a);
                    Ok(Value::Str(String::new()))
                }),
            );
        };
        reg("::informal::check_at", |a| {
            check::at(&a[1], a[2].parse().unwrap_or(0), a.get(3).map_or("", |s| s))
        });
        reg("::informal::check_fail", |a| check::tcl_error(&a[1]));
        reg("::informal::check_note", |a| {
            check::finding(check::Kind::Ignored, &a[1], &a[2])
        });
        self.interp.eval(CHECK_DRIVER).expect("check driver");
        self.interp
            .set_var("informal_check_script", &script.to_string_lossy());
        let cmd = "::informal::check_run $::informal_check_script";
        if let Err(e) = self.interp.eval(cmd) {
            check::tcl_error(&e.message);
        }
        self.session.borrow_mut().dry_run = false;
        check::finish()
    }

    pub fn eval(&self, script: &str) -> Result<String, TclError> {
        self.interp.eval(script)
    }

    pub fn source(&self, path: &Path) -> Result<String, TclError> {
        self.interp.eval_file(path)
    }
}

/// The Tcl side of check mode: read a file, evaluate each complete
/// top-level command on its own, and report Tcl errors instead of stopping.
const CHECK_DRIVER: &str = r##"
proc ::informal::check_open {file} {
    if {[llength [info commands ::informal::tcl_open]]} { return [::informal::tcl_open $file r] }
    return [open $file r]
}
proc ::informal::check_run {file} {
    # Relative to the working directory, else to the script that names it.
    set here [file dirname [info script]]
    if {![file exists $file] && [file exists [file join $here $file]]} {
        set file [file join $here $file]
    }
    if {[catch {::informal::check_open $file} ch]} {
        ::informal::check_fail "cannot read $file"
        return
    }
    set text [read $ch]
    close $ch
    set saved [info script]
    info script $file
    set cmd ""
    set start 1
    set n 0
    foreach line [split $text \n] {
        incr n
        if {$cmd eq ""} { set start $n }
        append cmd $line \n
        if {![info complete $cmd] || [string index $line end] eq "\\"} continue
        if {[string trim $cmd] ne "" && [string index [string trimleft $cmd] 0] ne "#"} {
            ::informal::check_at $file $start [string trim $cmd]
            # An environment variable that is not set here is reported once
            # and given a placeholder, so the command is still checked.
            for {set try 0} {$try < 20} {incr try} {
                if {![catch {uplevel #0 $cmd} msg]} break
                if {[regexp {^can't read "(?:::)?env\((\w+)\)": no such variable} $msg -> var]} {
                    set ::env($var) "\${$var}"
                    ::informal::check_fail "environment variable $var is not set"
                    continue
                }
                ::informal::check_fail $msg
                break
            }
        }
        set cmd ""
    }
    if {[string trim $cmd] ne ""} {
        ::informal::check_at $file $start [string trim $cmd]
        ::informal::check_fail "the command is not complete (unbalanced braces or quotes)"
    }
    info script $saved
}
rename ::source ::informal::tcl_source
proc ::source {args} { ::informal::check_run [lindex $args end] }
rename ::exit ::informal::tcl_exit
proc ::exit {args} {}
# A check has no side effects: nothing is executed, written, deleted or
# moved. (This guards against what a flow script does in passing; it is not
# a sandbox for a hostile script.)
foreach c {exec socket load cd} {
    rename ::$c {}
    proc ::$c {args} [list ::informal::check_note $c "$c: not run in check mode"]
}
# The script's own messages are not part of the report.
rename ::puts ::informal::tcl_puts
proc ::puts {args} {
    set a $args
    if {[lindex $a 0] eq "-nonewline"} { set a [lrange $a 1 end] }
    if {[llength $a] == 2 && [lindex $a 0] ni {stdout stderr}} { ::informal::tcl_puts {*}$args }
}
rename ::open ::informal::tcl_open
proc ::open {name {mode r} args} {
    if {$mode in {r RDONLY}} { return [::informal::tcl_open $name r] }
    ::informal::check_note open "open $name $mode: nothing is written in check mode"
    return [::informal::tcl_open /dev/null w]
}
rename ::file ::informal::tcl_file
proc ::file {sub args} {
    foreach w {delete mkdir copy rename link attributes tempfile} {
        if {$sub ne "" && [string first $sub $w] == 0 && $sub ni {d m c r l a t}} {
            ::informal::check_note file "file $sub: not done in check mode"
            return ""
        }
    }
    uplevel 1 [list ::informal::tcl_file $sub {*}$args]
}
"##;

// ---------------------------------------------------------------- helpers

fn note(msg: &str) {
    if check::active() {
        let cmd = msg.split([':', ' ']).next().unwrap_or("");
        check::finding(check::Kind::Ignored, cmd, msg);
    } else {
        eprintln!("informal: {msg}");
    }
}

/// Command words after the name, with `-opt value` pairs pulled out.
struct Args {
    cmd: String,
    pos: Vec<String>,
    opts: Vec<(String, Vec<String>)>,
}

impl Args {
    /// `arity` gives the number of values each option takes; options in
    /// `ignore` (with their arity) are dropped with a note.
    fn parse(
        a: &[String],
        arity: &[(&str, usize)],
        ignore: &[(&str, usize)],
    ) -> Result<Args, String> {
        let cmd = a[0].rsplit("::").next().unwrap_or(&a[0]).to_string();
        let mut out = Args {
            cmd: cmd.clone(),
            pos: Vec::new(),
            opts: Vec::new(),
        };
        let mut i = 1;
        while i < a.len() {
            let w = &a[i];
            if w.starts_with('-')
                && w.len() > 1
                && !w[1..].starts_with(|c: char| c.is_ascii_digit())
            {
                if let Some(&(_, n)) = arity.iter().find(|(o, _)| o == w) {
                    let vals = a
                        .get(i + 1..i + 1 + n)
                        .ok_or_else(|| format!("{cmd}: option {w} needs {n} value(s)"))?;
                    out.opts.push((w.clone(), vals.to_vec()));
                    i += 1 + n;
                    continue;
                }
                if let Some(&(_, n)) = ignore.iter().find(|(o, _)| o == w) {
                    note(&format!("{cmd}: ignoring {w} (vendor engine/GUI option)"));
                    i += 1 + n;
                    continue;
                }
                return Err(format!("{cmd}: unsupported option {w}"));
            }
            out.pos.push(w.clone());
            i += 1;
        }
        Ok(out)
    }

    fn flag(&self, o: &str) -> bool {
        self.opts.iter().any(|(k, _)| k == o)
    }
    fn val(&self, o: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(k, _)| k == o)
            .and_then(|(_, v)| v.first())
            .map(String::as_str)
    }
    fn all<'a>(&'a self, o: &'a str) -> impl Iterator<Item = &'a Vec<String>> + 'a {
        self.opts
            .iter()
            .filter(move |(k, _)| k == o)
            .map(|(_, v)| v)
    }
}

/// Expand `$VAR` and `${VAR}` from the environment, as filelists allow.
fn expand_env(s: &str) -> String {
    let re = regex::Regex::new(r"\$\{(\w+)\}|\$(\w+)").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let k = c.get(1).or(c.get(2)).unwrap().as_str();
        std::env::var(k).unwrap_or_default()
    })
    .into_owned()
}

/// Apply VCS / Jasper style compile words: files, `+define+`, `+incdir+`,
/// `-f list`, `-incdir d`, `-define X`.
fn apply_compile_words(
    s: &mut Session,
    words: &[String],
    base: Option<&Path>,
) -> Result<(), String> {
    // Absolute paths, so that files resolve the same from the work directory.
    let resolve = |w: &str| -> PathBuf {
        let p = PathBuf::from(expand_env(w));
        let p = match base {
            Some(b) if p.is_relative() && !p.exists() && b.join(&p).exists() => b.join(p),
            _ => p,
        };
        std::fs::canonicalize(&p).unwrap_or(p)
    };
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if let Some(rest) = w.strip_prefix("+define+") {
            for d in rest.split('+').filter(|d| !d.is_empty()) {
                let (k, v) = d
                    .split_once('=')
                    .map_or((d, None), |(k, v)| (k, Some(v.to_string())));
                s.design.defines.push((k.to_string(), v));
            }
        } else if let Some(rest) = w.strip_prefix("+incdir+") {
            for d in rest.split('+').filter(|d| !d.is_empty()) {
                s.design.incdirs.push(resolve(d));
            }
        } else if w == "-f" || w == "-F" || w == "-file" {
            let f = resolve(words.get(i + 1).ok_or("-f needs a file")?);
            if s.dry_run && !f.exists() {
                i += 2;
                continue;
            }
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let inner: Vec<String> = text
                .lines()
                .map(|l| l.split("//").next().unwrap_or(""))
                .filter(|l| !l.trim_start().starts_with('#'))
                .flat_map(|l| l.split_whitespace().map(String::from).collect::<Vec<_>>())
                .collect();
            apply_compile_words(s, &inner, f.parent())?;
            i += 1;
        } else if w == "-incdir" {
            s.design.incdirs.push(resolve(
                words.get(i + 1).ok_or("-incdir needs a directory")?,
            ));
            i += 1;
        } else if w == "-define" || w == "-D" {
            let d = words.get(i + 1).ok_or("-define needs a name")?;
            let (k, v) = d
                .split_once('=')
                .map_or((d.as_str(), None), |(k, v)| (k, Some(v.to_string())));
            s.design.defines.push((k.to_string(), v));
            i += 1;
        } else if w == "-v" || w == "-y" {
            note(&format!(
                "ignoring library option {w} {}",
                words.get(i + 1).map_or("", |x| x)
            ));
            i += 1;
        } else if w.starts_with('-') || w.starts_with('+') {
            // VCS switches such as -sverilog, -assert svaext, +libext+.v
            if w == "-assert" || w == "-timescale" {
                i += 1;
            }
        } else {
            let p = resolve(w);
            // (Check mode looks at the script, not at the design files.)
            if !p.exists() && !s.dry_run {
                return Err(format!("file not found: {}", p.display()));
            }
            s.design.files.push(p);
        }
        i += 1;
    }
    s.elaborated = false;
    Ok(())
}

fn add_props(
    s: &mut Session,
    task: &str,
    kind: Kind,
    name: Option<String>,
    expr: &str,
) -> Result<Value, String> {
    let name = s.add_property_in(task, kind, name, expr)?;
    let loc = match informal_tcl::caller_location() {
        Some((file, line)) => Some(SourceLoc::new(&file, line)),
        None => informal_tcl::current_script().and_then(|f| SourceLoc::search(&f, &name)),
    };
    if let Some(p) = s.props.iter_mut().find(|p| p.name == name) {
        p.source = loc;
    }
    Ok(name.into())
}

/// Properties by name or glob. A property of the RTL can also be named
/// without its path (`a_never` for `top.a_never`, `u.a_x` for
/// `top.u.a_x`); a short name that is not a glob must then name exactly one.
fn select(s: &Session, patterns: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let mut m = s.matching(p);
        if m.is_empty() {
            let suffix = format!("*.{p}");
            m = s
                .props
                .iter()
                .enumerate()
                .filter(|(_, q)| q.rtl && glob(&suffix, &q.name))
                .map(|(i, _)| i)
                .collect();
            if m.len() > 1 && !p.contains(['*', '?']) {
                let names: Vec<&str> = m.iter().map(|&i| s.props[i].name.as_str()).collect();
                return Err(format!(
                    "`{p}` is ambiguous: it names {}; give the full name",
                    names.join(", ")
                ));
            }
        }
        if m.is_empty() {
            return Err(format!("no property matches `{p}`"));
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// Properties whose names a regular expression matches anywhere in them,
/// as Tcl's `regexp` does (`-regexp`; anchor with `^` and `$`).
fn select_regexp(s: &Session, patterns: &[String]) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let re = regex::Regex::new(p).map_err(|e| format!("bad regular expression `{p}`: {e}"))?;
        // An embedded property is also matched by its full name,
        // `<embedded>::<name>`, as JasperGold scripts write it.
        let m: Vec<usize> = s
            .props
            .iter()
            .enumerate()
            .filter(|(_, q)| {
                re.is_match(&q.name)
                    || (q.task == EMBEDDED && re.is_match(&format!("{EMBEDDED}::{}", q.name)))
            })
            .map(|(i, _)| i)
            .collect();
        if m.is_empty() {
            return Err(format!("no property matches the regular expression `{p}`"));
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// [`select`] with `-task`: a name is first looked for in that task
/// (`a_x` there is `<task>::a_x`).
fn select_in(s: &Session, task: Option<&str>, patterns: &[String]) -> Result<Vec<usize>, String> {
    let Some(t) = task else {
        return select(s, patterns);
    };
    let mut out = Vec::new();
    for p in patterns.iter().flat_map(|p| p.split_whitespace()) {
        let q = qualify(t, p);
        let mut m: Vec<usize> = s.matching(&q);
        m.retain(|&i| s.props[i].task == t);
        if m.is_empty() {
            m = select(s, &[p.to_string()])?;
        }
        out.extend(m);
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// `task`: JasperGold's tasks (decision D20).
///
/// - `task -create <name> [-source_task <t>] [-copy <names>] [-regexp]
///   [-copy_asserts] [-copy_assumes] [-copy_covers] [-copy_stopats] [-set]`
/// - `task -edit <name>` with the same copy options
/// - `task -set <name>`, `task -list`, `task -delete <name>`
/// - `task` alone: the current task's name.
///
/// A copy is named `<task>::<name>` and starts unprocessed. Copies come
/// from the source task (`-source_task`, else the current task).
fn task(s: &mut Session, a: &[String]) -> Result<Value, String> {
    // `-set <name>` alone switches; with -create or -edit it is a flag.
    if a.len() == 3 && a[1] == "-set" {
        s.set_task(&a[2]).map_err(|e| format!("task: {e}"))?;
        return Ok(a[2].clone().into());
    }
    let a = Args::parse(
        a,
        &[
            ("-create", 1),
            ("-edit", 1),
            ("-delete", 1),
            ("-list", 0),
            ("-set", 0),
            ("-source_task", 1),
            ("-copy", 1),
            ("-regexp", 0),
            ("-copy_asserts", 0),
            ("-copy_assumes", 0),
            ("-copy_covers", 0),
            ("-copy_stopats", 0),
            ("-copy_all", 0),
        ],
        &[
            ("-copy_ratings", 0),
            ("-copy_abstractions", 1),
            ("-copy_related_covers", 0),
            ("-copy_proof_settings", 0),
        ],
    )?;
    if a.flag("-list") {
        return Ok(Value::List(
            s.tasks.iter().map(|t| t.name.clone()).collect(),
        ));
    }
    if let Some(t) = a.val("-delete") {
        s.delete_task(t).map_err(|e| format!("task: {e}"))?;
        return Ok(().into());
    }
    let name = match (a.val("-create"), a.val("-edit")) {
        (Some(n), None) => {
            s.create_task(n).map_err(|e| format!("task: {e}"))?;
            n.to_string()
        }
        (None, Some(n)) => s.task_name(n).map_err(|e| format!("task: {e}"))?,
        (Some(_), Some(_)) => return Err("task: -create and -edit together".into()),
        (None, None) => {
            if !a.pos.is_empty() || a.opts.iter().any(|(k, _)| k != "-set") {
                return Err("task: -create, -edit, -set, -list or -delete is needed".into());
            }
            return Ok(s.current_task.clone().into());
        }
    };
    let source = match a.val("-source_task") {
        Some(t) => s.task_name(t).map_err(|e| format!("task: {e}"))?,
        None => s.current_task.clone(),
    };
    if source == name && a.val("-create").is_some() {
        return Err(format!(
            "task: -source_task {source} is the task being made"
        ));
    }
    let from = s.props_of(&source);
    let all = a.flag("-copy_all");
    let mut which: Vec<usize> = from
        .iter()
        .copied()
        .filter(|&i| match s.props[i].kind {
            Kind::Assert => all || a.flag("-copy_asserts"),
            Kind::Assume => all || a.flag("-copy_assumes"),
            Kind::Cover => all || a.flag("-copy_covers"),
        })
        .collect();
    for pats in a.all("-copy") {
        let words: Vec<String> = pats[0].split_whitespace().map(String::from).collect();
        // Names are looked for in the source task.
        let hit = |p: &crate::session::Property, w: &str| -> Result<bool, String> {
            Ok(if a.flag("-regexp") {
                let re = regex::Regex::new(w)
                    .map_err(|e| format!("task: bad regular expression `{w}`: {e}"))?;
                re.is_match(&p.name) || re.is_match(p.short_name())
            } else {
                glob(w, &p.name) || glob(w, p.short_name())
            })
        };
        for w in &words {
            let mut found = false;
            for &i in &from {
                if hit(&s.props[i], w)? {
                    which.push(i);
                    found = true;
                }
            }
            if !found {
                note(&format!(
                    "task: -copy {w}: no property of task {source} matches"
                ));
            }
        }
    }
    which.sort();
    which.dedup();
    s.copy_into(&name, &which)
        .map_err(|e| format!("task: {e}"))?;
    if all || a.flag("-copy_stopats") {
        s.copy_stopats(&source, &name)
            .map_err(|e| format!("task: {e}"))?;
    }
    if a.flag("-set") {
        s.set_task(&name).map_err(|e| format!("task: {e}"))?;
    }
    Ok(name.into())
}

fn set_enabled(s: &mut Session, patterns: &[String], on: bool) -> Result<Value, String> {
    for i in select(s, patterns)? {
        s.props[i].enabled = on;
    }
    Ok(().into())
}

fn prove_and_report(s: &mut Session, which: &[usize]) -> Result<Value, String> {
    let done = s.prove(which).map_err(|e| format!("{e:#}"))?;
    if s.dry_run {
        return Ok(().into());
    }
    print!("{}", s.report());
    let summary = |w: &str| {
        done.iter()
            .filter(|&&i| s.props[i].status.word() == w)
            .count()
    };
    Ok(format!(
        "proven {} cex {} covered {} unreachable {} undetermined {} error {}",
        summary("proven"),
        summary("cex"),
        summary("covered"),
        summary("unreachable"),
        summary("undetermined"),
        summary("error")
    )
    .into())
}

/// `stopat sig ...` (JasperGold) / `snip_driver sig ...` (VC Formal): cut
/// each signal from its drivers, so it is free on every step. `-clear`
/// removes the named cut points (all without names); `-list` returns them.
fn stopat(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-clear", 0), ("-list", 0), ("-task", 1)],
        &[("-env", 0)],
    )?;
    // The task's own cut points: `-task`, else the current task.
    let task = match a.val("-task") {
        Some(t) => s.task_name(t).map_err(|e| format!("{}: {e}", a.cmd))?,
        None => s.current_task.clone(),
    };
    let names: Vec<String> = a
        .pos
        .iter()
        .flat_map(|w| w.split_whitespace().map(String::from))
        .collect();
    if a.flag("-list") {
        return Ok(Value::List(s.stopats_of(&task).to_vec()));
    }
    let names = names
        .iter()
        .map(|n| s.cut_point(n).map_err(|e| format!("{}: {e}", a.cmd)))
        .collect::<Result<Vec<_>, _>>()?;
    let cuts = s.stopats_mut(&task)?;
    if a.flag("-clear") {
        if names.is_empty() {
            cuts.clear();
        } else {
            cuts.retain(|x| !names.contains(x));
        }
        return Ok(().into());
    }
    if names.is_empty() {
        return Err(format!("{}: signal name(s) required", a.cmd));
    }
    for n in names {
        if !cuts.contains(&n) {
            cuts.push(n);
        }
    }
    Ok(().into())
}

/// Property names filtered by `{key value ...}` pairs on type and status.
fn filter_props(
    s: &Session,
    include: Option<&str>,
    exclude: Option<&str>,
    regexp: bool,
    task: Option<&str>,
) -> Result<Vec<String>, String> {
    let task = task.map(|t| s.task_name(t)).transpose()?;
    let pairs = |spec: Option<&str>| -> Vec<(String, String)> {
        let w: Vec<String> = spec
            .unwrap_or("")
            .split_whitespace()
            .map(String::from)
            .collect();
        w.chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (c[0].clone(), c[1].clone()))
            .collect()
    };
    let (inc, exc) = (pairs(include), pairs(exclude));
    // With -regexp, a name value is a regular expression.
    let mut res = std::collections::HashMap::new();
    if regexp {
        for (k, v) in inc
            .iter()
            .chain(&exc)
            .filter(|(k, _)| k == "name" || k == "-name")
        {
            let re =
                regex::Regex::new(v).map_err(|e| format!("bad regular expression `{v}`: {e}"))?;
            res.insert((k.clone(), v.clone()), re);
        }
    }
    let matches = |p: &crate::session::Property, (k, v): &(String, String)| match k.as_str() {
        "type" | "-type" => p.kind.label() == v || (v == "assertion" && p.kind == Kind::Assert),
        "status" | "-status" => {
            glob(v, p.status.word()) || (v == "falsified" && p.status.word() == "cex")
        }
        "name" | "-name" => match res.get(&(k.clone(), v.clone())) {
            Some(re) => re.is_match(&p.name),
            None => glob(v, &p.name),
        },
        "disabled" | "-disabled" => (v == "1" || v == "true") != p.enabled,
        "task" | "-task" => p.task == *v,
        _ => true,
    };
    Ok(s.props
        .iter()
        .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
        .filter(|p| inc.iter().all(|kv| matches(p, kv)) && !exc.iter().any(|kv| matches(p, kv)))
        .map(|p| p.name.clone())
        .collect())
}

// ---------------------------------------------------------------- commands

const COMMON: &[(&str, Body)] = &[
    ("get_version", |_, _| {
        Ok(format!("informal {}", env!("CARGO_PKG_VERSION")).into())
    }),
    ("echo", |s, a| {
        if !s.dry_run {
            println!("{}", a[1..].join(" "));
        }
        Ok(().into())
    }),
    ("session", |s, _| {
        serde_json::to_string_pretty(s)
            .map(Value::from)
            .map_err(|e| e.to_string())
    }),
    ("set_lowering", |s, a| {
        // informal's own: constructs the front end reports as unsupported
        // unless the user says to leave them out. Nothing is on by default.
        let state = |on: bool| if on { "on" } else { "off" };
        let Some(name) = a.get(1) else {
            return Ok(Value::from(format!(
                "skip_initial_checks {} isunknown_is_false {}",
                state(s.design.skip_initial_checks),
                state(s.design.isunknown_is_false)
            )));
        };
        let option = match name.as_str() {
            "skip_initial_checks" => &mut s.design.skip_initial_checks,
            "isunknown_is_false" => &mut s.design.isunknown_is_false,
            _ => {
                return Err(format!(
                    "set_lowering: unknown option {name}; options: skip_initial_checks, isunknown_is_false"
                ))
            }
        };
        match a.get(2).map(String::as_str) {
            Some("on") => *option = true,
            Some("off") => *option = false,
            None => return Ok(Value::from(state(*option))),
            Some(v) => return Err(format!("set_lowering {name}: on or off, not `{v}`")),
        }
        // The design is read again with the new setting at the next
        // elaborate or prove.
        s.elaborated = false;
        Ok(().into())
    }),
    ("check_assumptions", check_assumptions),
    ("set_vacuity", |s, a| {
        match a.get(1).map(String::as_str) {
            None => {
                return Ok(Value::from(if s.environment_depth > 0 {
                    "on"
                } else {
                    "off"
                }));
            }
            Some("on") => {
                if s.environment_depth == 0 {
                    s.environment_depth = 20;
                }
                s.preconditions = true;
            }
            Some("off") => {
                s.environment_depth = 0;
                s.preconditions = false;
            }
            Some(v) => return Err(format!("set_vacuity: on or off, not `{v}`")),
        }
        Ok(().into())
    }),
    ("set_engines", |s, a| {
        let names: Vec<String> = a[1..]
            .iter()
            .flat_map(|w| w.split_whitespace().map(String::from))
            .collect();
        // A name that matches nothing would otherwise run every engine.
        let known = s.engine_names();
        let unknown: Vec<&str> = names
            .iter()
            .filter(|n| !known.contains(n))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "set_engines: unknown engine(s) {}; available: {}",
                unknown.join(", "),
                known.join(", ")
            ));
        }
        s.engine_filter = names;
        Ok(().into())
    }),
];

const JG: &[(&str, Body)] = &[
    ("clear", |s, a| {
        let a = Args::parse(a, &[("-all", 0)], &[])?;
        let _ = a.flag("-all");
        s.clear();
        Ok(().into())
    }),
    ("task", |s, a| {
        let a = Args::parse(
            a,
            &[("-create", 0), ("-name", 1), ("-assume", 1), ("-task", 1)],
            &[],
        )?;
        if a.flag("-create") {
            let name = a.val("-name").ok_or("task -create requires -name")?;
            s.add_task(name.to_string());
            Ok(format!("task {name} created").into())
        } else if let Some(assume) = a.val("-assume") {
            let task_name = a.val("-task").ok_or("task -assume requires -task")?;
            s.add_assumption_to_task(task_name, assume.to_string())
                .map_err(|e| format!("task: {e}"))?;
            Ok(format!("assumption `{assume}` added to task `{task_name}`").into())
        } else {
            Err("task: use -create -name <name> or -assume <expr> -task <name>".into())
        }
    }),
    ("bind", |s, a| {
        let a = Args::parse(a, &[("-module", 1), ("-target", 1)], &[])?;
        let module = a.val("-module").ok_or("bind requires -module")?;
        let target = a.val("-target").ok_or("bind requires -target")?;
        s.add_bind(module.to_string(), target.to_string());
        Ok(format!("bind `{module}` to `{target}`").into())
    }),
    ("analyze", |s, a| {
        let langs = [
            "-sv", "-sv09", "-sv12", "-sv17", "-sv2009", "-sv2012", "-sv2017", "-v2k", "-verilog",
            "-sva", "-v95",
        ];
        let mut words = Vec::new();
        let mut i = 1;
        while i < a.len() {
            let w = a[i].as_str();
            if langs.contains(&w) {
            } else if w.starts_with("-vhdl") {
                return Err("analyze: VHDL is not supported yet".into());
            } else if w == "-lib" || w == "-L" {
                note(&format!(
                    "analyze: ignoring {w} {}",
                    a.get(i + 1).map_or("", |x| x)
                ));
                i += 1;
            } else if w == "-clear" {
                s.design.files.clear();
            } else {
                words.push(a[i].clone());
            }
            i += 1;
        }
        apply_compile_words(s, &words, None)?;
        Ok(().into())
    }),
    ("elaborate", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-top", 1),
                ("-parameter", 2),
                ("-bbox_m", 1),
                ("-bbox_i", 1),
                ("-create_related_covers", 1),
                ("-no_preconditions", 0),
            ],
            &[
                ("-bbox_mul", 1),
                ("-disable_auto_bbox", 0),
                ("-bbox_a", 1),
                ("-loop_limit", 1),
                ("-multiple_tops", 0),
                ("-extract_covergroup", 0),
                ("-x_value", 1),
                ("-auto_hr_info", 0),
            ],
        )?;
        for p in a.all("-parameter") {
            s.design.params.push((p[0].clone(), p[1].clone()));
        }
        // The black boxes of this elaboration (each option takes a name or
        // a list, and may repeat).
        let list = |o: &str| -> Vec<String> {
            a.all(o)
                .flat_map(|v| {
                    v[0].split_whitespace()
                        .map(String::from)
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        // Precondition covers are made for every proven assertion unless
        // this elaboration asks for none; witness covers are not yet run.
        if a.flag("-no_preconditions") {
            s.preconditions = false;
        }
        for kinds in a.all("-create_related_covers") {
            for k in kinds[0].split_whitespace() {
                match k {
                    "precondition" => s.preconditions = true,
                    "witness" => s.log("elaborate -create_related_covers witness: witness covers are not yet run; precondition covers are (docs/vacuity.md)"),
                    _ => return Err(format!("elaborate -create_related_covers: precondition or witness, not `{k}`")),
                }
            }
        }
        s.design.bbox = list("-bbox_m");
        s.design.bbox_instances = list("-bbox_i");
        s.elaborate(a.val("-top").map(String::from))
            .map_err(|e| format!("{e:#}"))?;
        Ok(s.design.top.clone().unwrap_or_default().into())
    }),
    ("clock_cmd", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-both_edges", 0),
                ("-none", 0),
                ("-clear", 0),
                ("-factor", 1),
                ("-phase", 1),
            ],
            &[("-infer", 0)],
        )?;
        if a.flag("-none") || a.flag("-clear") {
            s.clocks.clear();
            return Ok(().into());
        }
        if a.flag("-both_edges") {
            note("clock: -both_edges is not needed; a design that uses both edges is modelled with two steps per cycle");
        }
        let number = |o: &str, least: u32| -> Result<Option<u32>, String> {
            match a.val(o) {
                None => Ok(None),
                Some(v) => match v.parse::<u32>() {
                    Ok(n) if n >= least && n <= 4096 => Ok(Some(n)),
                    _ => Err(format!(
                        "clock: {o} takes a whole number from {least} to 4096, not `{v}`"
                    )),
                },
            }
        };
        let factor = number("-factor", 1)?.unwrap_or(1);
        let phase = number("-phase", 0)?.unwrap_or(0);
        for sig in &a.pos {
            // Declaring a clock again replaces its ratio.
            s.clocks.retain(|c| &c.signal != sig);
            s.clocks.push(Clock {
                signal: sig.clone(),
                edge: "posedge",
                factor,
                phase,
            });
        }
        if let Err(e) = s.check_reset_and_clocks() {
            s.clocks.truncate(s.clocks.len() - a.pos.len());
            return Err(e);
        }
        if s.clocks.len() > 1 {
            note("clock: several clocks; when the design uses more than one, the model drives them with one step per half period of a -factor 1 clock");
        }
        Ok(().into())
    }),
    ("reset", |s, a| {
        let a = Args::parse(
            a,
            &[("-expression", 1), ("-expr", 1), ("-none", 0)],
            &[("-non_resettable_regs", 1), ("-analyze_only", 0)],
        )?;
        let old = s.reset.clone();
        s.reset = if a.flag("-none") {
            None
        } else if let Some(e) = a.val("-expression").or(a.val("-expr")) {
            Some(e.to_string())
        } else if !a.pos.is_empty() {
            Some(a.pos.join(" "))
        } else {
            return Ok(s.reset.clone().unwrap_or_default().into());
        };
        if let Err(e) = s.check_reset_and_clocks() {
            s.reset = old;
            return Err(e);
        }
        Ok(().into())
    }),
    ("assert", |s, a| jg_prop(s, a, Kind::Assert)),
    ("assume", |s, a| jg_prop(s, a, Kind::Assume)),
    ("cover", |s, a| jg_prop(s, a, Kind::Cover)),
    ("prove", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-all", 0),
                ("-property", 1),
                ("-time_limit", 1),
                ("-task", 1),
            ],
            &[
                ("-bg", 0),
                ("-with_proof_core", 0),
                ("-engine_mode", 1),
                ("-per_property_time_limit", 1),
                ("-dump_trace", 0),
                ("-sst", 1),
                ("-orchestration", 1),
            ],
        )?;
        if let Some(t) = a.val("-time_limit") {
            s.time_limit = parse_duration(t)?;
        }
        let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
        let which: Vec<String> = a.all("-property").flatten().cloned().collect();
        let which = match (&task, which.is_empty()) {
            // -task alone: every enabled assertion and cover of that task.
            (Some(t), true) => {
                let members: Vec<usize> = s
                    .props_of(t)
                    .into_iter()
                    .filter(|&i| s.props[i].enabled && s.props[i].kind != Kind::Assume)
                    .collect();
                if members.is_empty() {
                    note(&format!("prove -task {t}: the task has nothing to prove"));
                    return Ok(().into());
                }
                members
            }
            (_, true) => Vec::new(),
            (t, false) => select_in(s, t.as_deref(), &which)?,
        };
        prove_and_report(s, &which)
    }),
    ("get_property_list", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-include", 1),
                ("-exclude", 1),
                ("-regexp", 0),
                ("-task", 1),
            ],
            &[],
        )?;
        Ok(filter_props(
            s,
            a.val("-include"),
            a.val("-exclude"),
            a.flag("-regexp"),
            a.val("-task"),
        )?
        .into())
    }),
    ("get_status", |s, a| {
        let n = a.get(1).ok_or("get_status: property name required")?;
        let i = *select(s, std::slice::from_ref(n))?.first().unwrap();
        Ok(s.props[i].status.word().into())
    }),
    ("set_prove_time_limit", |s, a| {
        s.time_limit = parse_duration(a.get(1).ok_or("set_prove_time_limit: time required")?)?;
        Ok(().into())
    }),
    // informal's limit is per property already.
    ("set_prove_per_property_time_limit", |s, a| {
        s.time_limit = parse_duration(
            a.get(1)
                .ok_or("set_prove_per_property_time_limit: time required")?,
        )?;
        Ok(().into())
    }),
    ("set_engine_mode", |s, a| {
        // Jasper engine letters to the closest informal engines.
        let mut names = Vec::new();
        for m in a[1..].iter().flat_map(|w| w.split_whitespace()) {
            let mapped: &[&str] = match m {
                "Ht" | "Hp" | "Hps" | "Hts" | "Mp" | "AM" | "AD" | "M" | "L" | "R" => &[
                    "informal-kind-ic3",
                    "informal-ic3-mt",
                    "ric3-ic3",
                    "abc-pdr",
                ],
                "B" | "Bm" | "K" | "C" | "C2" | "I" | "Tri" | "Q3" | "U" => {
                    &["informal-bmc", "informal-wbmc", "ric3-bmc"]
                }
                "N" | "D" | "G" | "G2" => &["informal-kind", "ric3"],
                _ => {
                    note(&format!("set_engine_mode: no informal engine for `{m}`"));
                    &[]
                }
            };
            names.extend(mapped.iter().map(|n| n.to_string()));
        }
        names.dedup();
        // Vendor engine letters are a hint: keep the ones this build has.
        let known = s.engine_names();
        names.retain(|n| known.contains(n));
        if names.is_empty() {
            note("set_engine_mode: none of the matching engines is available; using all engines");
        } else {
            note(&format!("set_engine_mode: using {}", names.join(", ")));
        }
        s.engine_filter = names;
        Ok(().into())
    }),
    ("report", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-summary", 0),
                ("-results", 0),
                ("-csv", 0),
                ("-file", 1),
                ("-force", 0),
                ("-task", 1),
                ("-design", 0),
                ("-detail", 0),
                ("-property", 1),
            ],
            &[],
        )?;
        let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
        let r = if a.flag("-design") {
            if s.dry_run {
                return Ok(().into());
            }
            s.design_info().text()
        } else if a.flag("-detail") || a.val("-property").is_some() {
            // One block per property: what its last prove measured.
            let which: Vec<usize> = match a.val("-property") {
                Some(p) => select_in(s, task.as_deref(), &[p.to_string()])?,
                None => (0..s.props.len())
                    .filter(|&i| task.as_deref().is_none_or(|t| s.props[i].task == t))
                    .collect(),
            };
            which
                .iter()
                .map(|&i| s.property_info(i, false).text())
                .collect::<Vec<_>>()
                .join("\n")
        } else if a.flag("-csv") {
            s.report_csv_of(task.as_deref())
        } else {
            s.report_of(task.as_deref())
        };
        if s.dry_run {
            return Ok(r.into());
        }
        match a.val("-file") {
            Some(f) => {
                let path = Path::new(f);
                if path.exists() && !a.flag("-force") {
                    return Err(format!("report: {f} exists; add -force to overwrite it"));
                }
                std::fs::write(path, &r).map_err(|e| format!("report: {f}: {e}"))?;
            }
            None => print!("{r}"),
        }
        Ok(r.into())
    }),
    ("get_design_info", get_design_info),
    ("get_property_info", get_property_info),
    ("get_stats", |s, a| {
        let a = Args::parse(a, &[("-task", 1)], &[])?;
        let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
        let rows: Vec<serde_json::Value> = s
            .props
            .iter()
            .filter(|p| task.as_deref().is_none_or(|t| p.task == t))
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "task": p.task,
                    "kind": p.kind.label(),
                    "status": p.status.word(),
                    "vacuity": p.vacuity.map(|v| v.word()),
                    "stats": p.stats,
                })
            })
            .collect();
        serde_json::to_string_pretty(&rows)
            .map(Value::from)
            .map_err(|e| e.to_string())
    }),
    ("stopat", stopat),
    ("task", task),
    ("get_task_list", |s, _| {
        Ok(Value::List(
            s.tasks.iter().map(|t| t.name.clone()).collect(),
        ))
    }),
    ("visualize", visualize),
    ("set_proofgrid_per_engine_max_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_per_engine_max_local_jobs", |_, _| {
        Ok(().into())
    }),
    ("set_proofgrid_max_local_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_max_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_mode", |_, _| Ok(().into())),
    ("set_max_trace_length", |_, _| Ok(().into())),
];

/// `get_design_info [-json] [-list instance|memory|clock|reset [-filter P
/// [-regexp]]]`: the design's counts and the current task's model size
/// (stats.rs). The table by default, JSON with `-json`, names with `-list`.
fn get_design_info(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-json", 0), ("-list", 1), ("-filter", 1), ("-regexp", 0)],
        &[("-verbosity", 1), ("-silent", 0)],
    )?;
    if s.dry_run {
        return Ok(().into());
    }
    if let Some(what) = a.val("-list") {
        let d = s.elab.as_ref().ok_or("get_design_info: elaborate first")?;
        let stats = d.stats.clone().unwrap_or_default();
        let names: Vec<String> = match what {
            "instance" | "instances" => d.instances.iter().map(|i| i.path.clone()).collect(),
            "memory" | "memories" => stats.memories.iter().map(|m| m.name.clone()).collect(),
            "clock" | "clocks" => stats.clocks.iter().map(|c| c.signal.clone()).collect(),
            "reset" | "resets" => stats.resets.iter().map(|r| r.signal.clone()).collect(),
            other => {
                return Err(format!(
                "get_design_info -list {other}: not supported yet (instance, memory, clock, reset)"
            ))
            }
        };
        let keep = |n: &str| -> Result<bool, String> {
            match (a.val("-filter"), a.flag("-regexp")) {
                (None, _) => Ok(true),
                (Some(f), true) => regex::Regex::new(f)
                    .map(|re| re.is_match(n))
                    .map_err(|e| format!("get_design_info: bad regular expression `{f}`: {e}")),
                (Some(f), false) => Ok(glob(f, n)),
            }
        };
        let mut out = Vec::new();
        for n in names {
            if keep(&n)? {
                out.push(n);
            }
        }
        return Ok(Value::List(out));
    }
    let info = s.design_info();
    if a.flag("-json") {
        return serde_json::to_string_pretty(&info)
            .map(Value::from)
            .map_err(|e| e.to_string());
    }
    let text = info.text();
    print!("{text}");
    Ok(text.into())
}

/// `get_property_info <name> [-json] [-cone]`: what the last prove
/// measured for one property; `-cone` adds its cone of influence (it
/// lowers the design).
fn get_property_info(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-json", 0), ("-cone", 0), ("-task", 1)], &[])?;
    let [name] = a.pos.as_slice() else {
        return Err("get_property_info: name one property".into());
    };
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let i = match select_in(s, task.as_deref(), std::slice::from_ref(name))?.as_slice() {
        [i] => *i,
        many => {
            return Err(format!(
                "get_property_info: `{name}` names {} properties; name one",
                many.len()
            ))
        }
    };
    let info = s.property_info(i, a.flag("-cone"));
    if a.flag("-json") {
        return serde_json::to_string_pretty(&info)
            .map(Value::from)
            .map_err(|e| e.to_string());
    }
    Ok(info.text().into())
}

/// `check_assumptions [-depth N]`: the environment check on its own (it
/// also runs before every prove): whether the assumptions and the reset
/// allow traces of N model steps. Returns the number of steps they allow.
/// `-conflict` is what it checks; `-dead_end` is not done yet.
fn check_assumptions(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-depth", 1), ("-conflict", 0)], &[])?;
    let depth = match a.val("-depth") {
        None => s.environment_depth.max(1),
        Some(v) => v
            .parse::<u32>()
            .ok()
            .filter(|n| (1..=10_000).contains(n))
            .ok_or_else(|| format!("check_assumptions: -depth takes 1 to 10000, not `{v}`"))?,
    };
    if s.dry_run {
        return Ok(().into());
    }
    let env = s
        .environment_now(depth)
        .map_err(|e| format!("check_assumptions: {e}"))?;
    match env.allowed {
        // -conflict: the assumptions that conflict, as a list (empty when
        // none do).
        Some(_) if a.flag("-conflict") => Ok(Value::List(env.conflict_set)),
        Some(n) => Ok(Value::from(n.to_string())),
        None => Err("check_assumptions: could not decide (no in-house BMC, or out of time)".into()),
    }
}

/// `visualize -violation|-cover -property P`: the trace of a CEX or cover,
/// as the VCD file prove wrote. It is opened with the program named by
/// INFORMAL_WAVE_VIEWER (`surfer`, `gtkwave`) when that is set; the path is
/// returned either way, for the desktop UI's trace view or any VCD viewer.
fn visualize(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-violation", 0), ("-cover", 0), ("-property", 1)],
        &[
            ("-new_window", 0),
            ("-window", 1),
            ("-bg", 0),
            ("-min_length", 0),
            ("-max_length", 1),
            ("-task", 1),
        ],
    )?;
    let names: Vec<String> = a
        .val("-property")
        .map(String::from)
        .into_iter()
        .chain(a.pos.iter().cloned())
        .collect();
    let [name] = names.as_slice() else {
        return Err("visualize: name one property with -property".into());
    };
    if s.dry_run {
        return Ok(().into());
    }
    let i = match select(s, std::slice::from_ref(name))?.as_slice() {
        [i] => *i,
        many => {
            return Err(format!(
                "visualize: `{name}` names {} properties; name one",
                many.len()
            ))
        }
    };
    let p = &s.props[i];
    let want = if a.flag("-cover") { "covered" } else { "cex" };
    if p.status.word() != want {
        return Err(format!(
            "visualize: {} is {}; there is a trace only for a {}",
            p.name,
            p.status.word(),
            if want == "cex" {
                "failing assertion (-violation)"
            } else {
                "covered cover (-cover)"
            }
        ));
    }
    let Some(path) = p.trace.clone() else {
        return Err(format!(
            "visualize: no trace was written for {} (see the prove log)",
            p.name
        ));
    };
    match std::env::var("INFORMAL_WAVE_VIEWER") {
        Ok(viewer) if !viewer.trim().is_empty() => {
            let mut words = viewer.split_whitespace();
            let prog = words.next().unwrap_or_default();
            std::process::Command::new(prog)
                .args(words)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| format!("visualize: starting {prog}: {e}"))?;
            note(&format!("visualize: {path} opened in {prog}"));
        }
        _ => note(&format!(
            "visualize: trace in {path} (open it in the InFormal UI or a VCD viewer; set INFORMAL_WAVE_VIEWER to open it from here)"
        )),
    }
    Ok(path.into())
}

fn jg_prop(s: &mut Session, a: &[String], kind: Kind) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-name", 1),
            ("-disable", 0),
            ("-enable", 0),
            ("-remove", 0),
            ("-env", 0),
            ("-bound", 1),
            ("-task", 1),
            ("-regexp", 0),
        ],
        &[("-severity", 1), ("-tag", 1), ("-from_property", 1)],
    )?;
    let task = a.val("-task").map(|t| s.task_name(t)).transpose()?;
    let pick = |s: &Session| {
        let found = if a.flag("-regexp") {
            select_regexp(s, &a.pos)
        } else {
            select_in(s, task.as_deref(), &a.pos)
        }?;
        Ok::<_, String>(match &task {
            Some(t) => found
                .into_iter()
                .filter(|&i| &s.props[i].task == t)
                .collect(),
            None => found,
        })
    };
    if a.flag("-disable") || a.flag("-enable") {
        for i in pick(s)? {
            s.props[i].enabled = a.flag("-enable");
        }
        return Ok(().into());
    }
    if a.flag("-remove") {
        let drop = pick(s)?;
        let mut i = 0;
        s.props.retain(|_| {
            i += 1;
            !drop.contains(&(i - 1))
        });
        return Ok(().into());
    }
    if a.flag("-bound") {
        return Err(format!("{}: -bound is not supported yet", a.cmd));
    }
    if a.flag("-regexp") {
        return Err(format!(
            "{}: -regexp selects properties with -enable, -disable or -remove",
            a.cmd
        ));
    }
    let expr = match a.pos.as_slice() {
        [e] => e.clone(),
        _ => {
            return Err(format!(
                "{}: expected one property expression in braces",
                a.cmd
            ))
        }
    };
    // `assert -env` turns an assertion into an assumption.
    let kind = if a.flag("-env") && kind == Kind::Assert {
        Kind::Assume
    } else {
        kind
    };
    let task = task.unwrap_or_else(|| s.current_task.clone());
    add_props(s, &task, kind, a.val("-name").map(String::from), &expr)
}

const VCF: &[(&str, Body)] = &[
    // Before `elaborate` / `read_file`: the black boxes of the design.
    ("set_blackbox", |s, a| {
        let a = Args::parse(a, &[("-designs", 1), ("-cells", 1), ("-clear", 0)], &[])?;
        if a.flag("-clear") {
            s.design.bbox.clear();
            s.design.bbox_instances.clear();
        }
        let list = |o: &str| -> Vec<String> {
            a.all(o)
                .flat_map(|v| {
                    v[0].split_whitespace()
                        .map(String::from)
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        s.design.bbox.extend(list("-designs"));
        s.design.bbox_instances.extend(list("-cells"));
        if s.elaborated {
            note("set_blackbox: takes effect at the next elaborate / read_file");
        }
        Ok(().into())
    }),
    ("set_fml_appmode", |_, a| {
        match a.get(1).map(String::as_str) {
            Some("FPV") | Some("fpv") => Ok(().into()),
            Some(m) => Err(format!(
                "set_fml_appmode: only FPV is supported yet (got {m})"
            )),
            None => Ok("FPV".into()),
        }
    }),
    ("set_fml_var", |s, a| {
        let (k, v) = (
            a.get(1).ok_or("set_fml_var: name required")?,
            a.get(2).ok_or("set_fml_var: value required")?,
        );
        match k.as_str() {
            "fml_max_time" => s.time_limit = parse_duration(v)?,
            _ => note(&format!("set_fml_var: ignoring {k}")),
        }
        Ok(().into())
    }),
    ("read_file", |s, a| {
        let a = Args::parse(
            a,
            &[
                ("-top", 1),
                ("-format", 1),
                ("-sva", 0),
                ("-vcs", 1),
                ("-define", 1),
            ],
            &[("-cov", 1), ("-inject_fault", 1), ("-work", 1)],
        )?;
        vcf_format(&a)?;
        vcf_compile(s, &a)?;
        s.elaborate(a.val("-top").map(String::from))
            .map_err(|e| format!("{e:#}"))?;
        Ok(s.design.top.clone().unwrap_or_default().into())
    }),
    ("analyze", |s, a| {
        let a = Args::parse(
            a,
            &[("-format", 1), ("-vcs", 1), ("-define", 1)],
            &[("-library", 1), ("-work", 1)],
        )?;
        vcf_format(&a)?;
        vcf_compile(s, &a)?;
        Ok(().into())
    }),
    ("elaborate", |s, a| {
        let a = Args::parse(
            a,
            &[("-sva", 0), ("-vcs", 1), ("-parameters", 1)],
            &[("-cov", 1), ("-work", 1), ("-library", 1)],
        )?;
        if let Some(v) = a.val("-vcs") {
            let words: Vec<String> = v.split_whitespace().map(String::from).collect();
            for w in &words {
                if let Some(p) = w.strip_prefix("-pvalue+") {
                    if let Some((k, val)) = p.split_once('=') {
                        s.design.params.push((k.to_string(), val.to_string()));
                    }
                }
            }
        }
        s.elaborate(a.pos.first().cloned())
            .map_err(|e| format!("{e:#}"))?;
        Ok(s.design.top.clone().unwrap_or_default().into())
    }),
    ("create_clock", |s, a| {
        let a = Args::parse(a, &[("-period", 1), ("-initial", 1)], &[("-name", 1)])?;
        let sig = a.pos.first().ok_or("create_clock: clock signal required")?;
        s.clocks.push(Clock {
            signal: sig.clone(),
            edge: "posedge",
            factor: 1,
            phase: 0,
        });
        Ok(().into())
    }),
    ("snip_driver", stopat),
    ("create_reset", |s, a| {
        let a = Args::parse(
            a,
            &[("-sense", 1), ("-low", 0), ("-high", 0)],
            &[("-name", 1)],
        )?;
        let sig = a.pos.first().ok_or("create_reset: reset signal required")?;
        let low = a.flag("-low") || a.val("-sense") == Some("low");
        s.reset = Some(if low { format!("!{sig}") } else { sig.clone() });
        Ok(().into())
    }),
    ("sim_run", |_, _| Ok(().into())),
    ("sim_save_reset", |_, _| Ok(().into())),
    ("fvassert", |s, a| vcf_prop(s, a, Kind::Assert)),
    ("fvassume", |s, a| vcf_prop(s, a, Kind::Assume)),
    ("fvcover", |s, a| vcf_prop(s, a, Kind::Cover)),
    ("fvdisable", |s, a| set_enabled(s, &a[1..], false)),
    ("fvenable", |s, a| set_enabled(s, &a[1..], true)),
    ("check_fv", |s, a| {
        let a = Args::parse(
            a,
            &[("-property", 1)],
            &[("-block", 0), ("-run_finish", 1), ("-break", 1)],
        )?;
        let which: Vec<String> = a.all("-property").flatten().cloned().collect();
        let which = if which.is_empty() {
            Vec::new()
        } else {
            select(s, &which)?
        };
        prove_and_report(s, &which)
    }),
    ("report_fv", |s, a| {
        let _ = Args::parse(a, &[("-list", 0), ("-verbose", 0)], &[("-property", 1)])?;
        let r = s.report();
        if !s.dry_run {
            print!("{r}");
        }
        Ok(r.into())
    }),
    ("get_props", |s, a| {
        let a = Args::parse(a, &[("-type", 1), ("-status", 1)], &[])?;
        let mut inc = String::new();
        if let Some(t) = a.val("-type") {
            inc += &format!("type {t} ");
        }
        if let Some(st) = a.val("-status") {
            let st = if st == "falsified" { "cex" } else { st };
            inc += &format!("status {st}");
        }
        let mut names = filter_props(s, Some(&inc), None, false, None)?;
        if let Some(p) = a.pos.first() {
            names.retain(|n| glob(p, n));
        }
        Ok(names.into())
    }),
];

fn vcf_format(a: &Args) -> Result<(), String> {
    match a.val("-format") {
        None | Some("sverilog") | Some("verilog") => Ok(()),
        Some(f) => Err(format!("{}: format {f} is not supported yet", a.cmd)),
    }
}

fn vcf_compile(s: &mut Session, a: &Args) -> Result<(), String> {
    let mut words: Vec<String> = a
        .pos
        .iter()
        .flat_map(|w| w.split_whitespace().map(String::from))
        .collect();
    if let Some(v) = a.val("-vcs") {
        words.extend(v.split_whitespace().map(String::from));
    }
    for d in a.all("-define") {
        for w in d[0].split_whitespace() {
            words.push(format!("+define+{w}"));
        }
    }
    apply_compile_words(s, &words, None)
}

fn vcf_prop(s: &mut Session, a: &[String], kind: Kind) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-expr", 1), ("-name", 1)],
        &[("-type", 1), ("-usage", 1)],
    )?;
    let (name, expr) = match (a.val("-expr"), a.pos.as_slice()) {
        (Some(e), [n]) => (Some(n.clone()), e.to_string()),
        (Some(e), []) => (a.val("-name").map(String::from), e.to_string()),
        _ => {
            return Err(format!(
                "{}: usage `{} <name> -expr {{expression}}`",
                a.cmd, a.cmd
            ))
        }
    };
    let task = s.current_task.clone();
    add_props(s, &task, kind, name, &expr)
}

/// Command names a flavor defines (its map plus the commands shared by
/// both), sorted: what the console completes a first word against.
pub fn command_names(flavor: Flavor) -> Vec<&'static str> {
    let own: &[(&str, Body)] = match flavor {
        Flavor::Jg => JG,
        Flavor::Vcf => VCF,
    };
    let mut names: Vec<&str> = COMMON.iter().chain(own).map(|c| c.0).collect();
    if flavor == Flavor::Jg {
        names.extend(["include", "clock"]);
    }
    names.retain(|n| !n.ends_with("_cmd"));
    names.sort_unstable();
    names.dedup();
    names
}

/// Console Tab completion: command names of `flavor`, then property names,
/// that start with `prefix` (each list sorted, no duplicates). An empty
/// prefix matches nothing, so Tab on an empty prompt does not dump the map.
pub fn complete(flavor: Flavor, props: &[String], prefix: &str) -> Vec<String> {
    if prefix.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = command_names(flavor)
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect();
    let mut ps: Vec<&String> = props.iter().filter(|p| p.starts_with(prefix)).collect();
    ps.sort();
    ps.dedup();
    for p in ps {
        if !out.contains(p) {
            out.push(p.clone());
        }
    }
    out
}

/// Status words for use by callers that want the result of a whole script.
pub fn summary(s: &Session) -> Vec<(String, &'static str)> {
    s.props
        .iter()
        .filter(|p| p.kind != Kind::Assume)
        .map(|p| {
            (
                p.name.clone(),
                if matches!(p.status, Status::Unprocessed) {
                    "unprocessed"
                } else {
                    p.status.word()
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(flavor: Flavor) -> Shell {
        Shell::new(
            Session::new(
                Vec::new(),
                std::env::temp_dir().join("informal-compat-unit"),
            ),
            flavor,
        )
    }

    #[test]
    fn completes_commands_then_properties() {
        let props = vec![
            "a_max".to_string(),
            "prove_me".to_string(),
            "a_hold".to_string(),
        ];
        assert_eq!(complete(Flavor::Jg, &props, "prov"), ["prove", "prove_me"]);
        assert_eq!(complete(Flavor::Jg, &props, "a_"), ["a_hold", "a_max"]);
        assert!(complete(Flavor::Jg, &props, "").is_empty());
        // Each flavor completes its own map only.
        assert!(complete(Flavor::Vcf, &[], "check").contains(&"check_fv".to_string()));
        assert!(!complete(Flavor::Jg, &[], "check").contains(&"check_fv".to_string()));
        assert!(command_names(Flavor::Jg).contains(&"clock"));
        assert!(!command_names(Flavor::Jg).contains(&"clock_cmd"));
    }

    #[test]
    fn detects_flavor() {
        assert_eq!(
            detect_flavor("analyze -sv a.sv\nelaborate -top t\nprove -all"),
            Flavor::Jg
        );
        assert_eq!(
            detect_flavor("read_file -top t -format sverilog -vcs {a.sv}\ncheck_fv"),
            Flavor::Vcf
        );
    }

    #[test]
    fn jg_properties_and_queries() {
        let sh = shell(Flavor::Jg);
        sh.eval("clock clk; reset -expression {!rst_n}").unwrap();
        sh.eval("assert -name a1 {req |-> ##1 ack}; assert {x}; cover -name c1 {done}")
            .unwrap();
        sh.eval("assert -env -name m1 {!(a && b)}").unwrap();
        assert_eq!(
            sh.eval("get_property_list -include {type assert}").unwrap(),
            "a1 assert_1"
        );
        assert_eq!(
            sh.eval("get_property_list -include {type assume}").unwrap(),
            "m1"
        );
        assert_eq!(sh.eval("get_status a1").unwrap(), "unprocessed");
        sh.eval("assert -disable a*").unwrap();
        let s = sh.session.borrow();
        assert!(!s.props[0].enabled && !s.props[1].enabled && s.props[2].enabled);
        assert_eq!(s.reset.as_deref(), Some("!rst_n"));
        assert_eq!(s.clocks[0].signal, "clk");
    }

    /// Check mode goes through the whole script, past every error, and
    /// says per line what informal would do with it.
    #[test]
    fn check_mode_reports_every_line_without_running() {
        let dir = std::env::temp_dir().join(format!("informal-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.tcl");
        std::fs::write(
            &script,
            "# comment\n\
             analyze -sv12 $env(INFORMAL_NO_SUCH_VAR)/top.sv\n\
             elaborate -top top -bbox_mul 64\n\
             clock clk\n\
             foreach p {a b} {\n\
                 assert -name p_$p -frob {req |=> gnt}\n\
                 assert -name q_$p {req |=> gnt}\n\
             }\n\
             check_spv -create\n\
             prove -all -bg\n\
             include sub.tcl\n\
             set f [open written.txt w]; puts $f x; close $f\n\
             exec touch executed.txt\n\
             file delete sub.tcl\n\
             exit\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("sub.tcl"),
            "blackbox_assistant u.sig\ncover -name c1 {done}\n",
        )
        .unwrap();
        let sh = shell(Flavor::Jg);
        let r = sh.check(&script);
        let got: Vec<(check::Kind, u32, &str)> = r
            .findings
            .iter()
            .map(|f| (f.kind, f.line, f.command.as_str()))
            .collect();
        use check::Kind::*;
        assert_eq!(
            got,
            [
                (Tcl, 2, "analyze"),
                (Ignored, 3, "elaborate"),
                (Error, 5, "assert"),
                (Error, 5, "assert"),
                (Unknown, 9, "check_spv"),
                (Ignored, 10, "prove"),
                (Unknown, 1, "blackbox_assistant"),
                (Ignored, 12, "open"),
                (Ignored, 13, "exec"),
                (Ignored, 14, "file"),
            ],
            "{}",
            r.text()
        );
        assert!(r.findings[6].file.ends_with("sub.tcl"));
        assert!(!r.clean());
        // No side effects: nothing written, executed or deleted.
        assert!(!dir.join("written.txt").exists() && !Path::new("written.txt").exists());
        assert!(!dir.join("executed.txt").exists() && !Path::new("executed.txt").exists());
        assert!(dir.join("sub.tcl").exists());
        assert_eq!(
            r.commands["assert"],
            check::Uses {
                honoured: 2,
                rejected: 2
            }
        );
        // Nothing ran: the properties exist, unprocessed; the shell is usable after.
        assert_eq!(sh.eval("get_property_list").unwrap(), "q_a q_b c1");
        assert!(!sh.session.borrow().dry_run);
    }

    #[test]
    fn lowering_switches_are_off_until_asked_for() {
        let sh = shell(Flavor::Jg);
        assert_eq!(
            sh.eval("set_lowering").unwrap(),
            "skip_initial_checks off isunknown_is_false off"
        );
        sh.eval("set_lowering isunknown_is_false on").unwrap();
        assert_eq!(sh.eval("set_lowering isunknown_is_false").unwrap(), "on");
        assert_eq!(sh.eval("set_lowering skip_initial_checks").unwrap(), "off");
        assert!(sh.session.borrow().design.isunknown_is_false);
        sh.eval("set_lowering isunknown_is_false off").unwrap();
        assert!(!sh.session.borrow().design.isunknown_is_false);
        let e = sh.eval("set_lowering everything on").unwrap_err();
        assert!(
            e.message.contains("unknown option everything"),
            "{}",
            e.message
        );
        let e = sh
            .eval("set_lowering skip_initial_checks maybe")
            .unwrap_err();
        assert!(e.message.contains("on or off"), "{}", e.message);
    }

    #[test]
    fn rejects_what_it_cannot_honour() {
        let sh = shell(Flavor::Jg);
        let e = sh.eval("assert -name r {a |-> b [->2]}").unwrap_err();
        assert!(e.message.contains("[->"), "{}", e.message);
        // A bounded range is taken: the front end compiles it at prove.
        sh.eval("assert -name r {a |-> ##[1:3] b}").unwrap();
        assert!(sh
            .session
            .borrow()
            .props
            .iter()
            .any(|p| p.name == "r" && !p.rtl));
        let e = sh.eval("prove -all -frobnicate").unwrap_err();
        assert!(e.message.contains("unsupported option -frobnicate"));
        // Engine-tuning options are accepted.
        sh.eval("set_engine_mode {Ht B N}").unwrap();
        // (kept only for engines this session has: none here)
        assert!(sh.session.borrow().engine_filter.is_empty());
        // A name that matches no engine is an error, not "run them all".
        let e = sh.eval("informal::set_engines no-such-engine").unwrap_err();
        assert!(
            e.message.contains("unknown engine(s) no-such-engine"),
            "{}",
            e.message
        );
    }

    #[test]
    fn vcf_commands_and_flavor_switch() {
        let sh = shell(Flavor::Vcf);
        sh.eval("create_clock clk -period 10; create_reset rst_n -sense low")
            .unwrap();
        sh.eval("fvassert p1 -expr {a |=> b}; fvcover c1 -expr {b}; set_fml_var fml_max_time 2H")
            .unwrap();
        assert_eq!(sh.eval("get_props -type assert").unwrap(), "p1");
        assert_eq!(sh.session.borrow().time_limit.as_secs(), 7200);
        assert_eq!(sh.session.borrow().reset.as_deref(), Some("!rst_n"));
        // jg-only commands are not visible in vcf mode, but stay reachable by namespace.
        assert!(sh.eval("prove -all").is_err());
        sh.activate(Flavor::Jg);
        assert!(sh.eval("info commands ::check_fv").unwrap().is_empty());
        assert_eq!(sh.eval("get_property_list").unwrap(), "p1 c1");
    }

    #[test]
    fn short_names_regexp_and_small_commands() {
        let sh = shell(Flavor::Jg);
        sh.eval("assert -name a1 {x}; assert -name a2 {y}; cover -name c1 {z}")
            .unwrap();
        // Make two of them look like properties of the RTL.
        {
            let mut s = sh.session.borrow_mut();
            for (i, n) in ["top.a_never", "top.u.a_never"].iter().enumerate() {
                s.props[i].name = n.to_string();
                s.props[i].rtl = true;
            }
        }
        assert_eq!(sh.eval("get_status u.a_never").unwrap(), "unprocessed");
        let e = sh.eval("get_status a_never").unwrap_err();
        assert!(e.message.contains("ambiguous"), "{}", e.message);
        // A glob may match several.
        sh.eval("assert -disable a_n*").unwrap();
        assert!(sh.session.borrow().props[..2].iter().all(|p| !p.enabled));
        // -regexp matches anywhere in the name, as Tcl's regexp does.
        sh.eval("assert -enable -regexp {^top\\.u\\.}").unwrap();
        assert!(sh.session.borrow().props[1].enabled && !sh.session.borrow().props[0].enabled);
        assert_eq!(
            sh.eval("get_property_list -regexp -include {name a_n.*r$}")
                .unwrap(),
            "top.a_never top.u.a_never"
        );
        assert!(sh.eval("assert -regexp {x}").is_err());
        // Small commands.
        assert!(sh.eval("get_version").unwrap().starts_with("informal "));
        sh.eval("set_prove_time_limit 500ms").unwrap();
        assert_eq!(sh.session.borrow().time_limit.as_millis(), 500);
        sh.eval("set_prove_per_property_time_limit 2m").unwrap();
        assert_eq!(sh.session.borrow().time_limit.as_secs(), 120);
        sh.eval("reset -expr {!rst_n}").unwrap();
        assert_eq!(sh.session.borrow().reset.as_deref(), Some("!rst_n"));
        sh.eval("set_proofgrid_max_local_jobs 4; set_proofgrid_per_engine_max_local_jobs 2")
            .unwrap();
        let csv = sh.eval("report -csv").unwrap();
        assert!(
            csv.starts_with("property,type,status,engine,detail\n"),
            "{csv}"
        );
        assert!(csv.contains("c1,cover,unprocessed,,"), "{csv}");
        // No trace before a prove: said, not a silent nothing.
        let e = sh
            .eval("visualize -violation -property u.a_never")
            .unwrap_err();
        assert!(e.message.contains("is unprocessed"), "{}", e.message);
    }

    #[test]
    fn filelists_defines_and_incdirs() {
        let dir = std::env::temp_dir().join(format!("informal-fl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("inc")).unwrap();
        std::fs::write(dir.join("a.sv"), "module a; endmodule\n").unwrap();
        std::fs::write(
            dir.join("files.f"),
            "// list\n+incdir+inc\n+define+W=8+FAST\na.sv\n",
        )
        .unwrap();
        let sh = shell(Flavor::Jg);
        sh.eval(&format!("analyze -sv -f {}", dir.join("files.f").display()))
            .unwrap();
        let s = sh.session.borrow();
        assert_eq!(
            s.design.files,
            [std::fs::canonicalize(dir.join("a.sv")).unwrap()]
        );
        assert_eq!(
            s.design.incdirs,
            [std::fs::canonicalize(dir.join("inc")).unwrap()]
        );
        assert_eq!(
            s.design.defines,
            [
                ("W".to_string(), Some("8".to_string())),
                ("FAST".to_string(), None)
            ]
        );
    }
}
