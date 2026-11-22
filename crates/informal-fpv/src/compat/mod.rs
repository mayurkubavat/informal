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

pub(super) use crate::session::glob;
use crate::session::{Kind, Session, Status};
use informal_tcl::{Interp, TclError, Value};
use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;

mod apps;
pub mod check;
mod design;
pub mod help;
mod lp;
mod props;
mod prove_cmd;
mod tasks;

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
    active_flavor: Rc<Cell<Flavor>>,
}

impl Shell {
    pub fn new(session: Session, flavor: Flavor) -> Self {
        let interp = Interp::new();
        let session = Rc::new(RefCell::new(session));
        let active_flavor = Rc::new(Cell::new(flavor));
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
        for ns in ["informal", "jg", "vcf"] {
            let s = session.clone();
            let af = active_flavor.clone();
            interp.register(
                &format!("::{ns}::help"),
                Box::new(move |args| {
                    let sess = s.try_borrow().map_err(|_| "session busy".to_string())?;
                    let r = help::eval_help(args, af.get()).map(Value::Str);
                    if !sess.dry_run {
                        return r;
                    }
                    check::used("help", r.is_ok());
                    r.or_else(|e| {
                        check::finding(check::Kind::Error, "help", &e);
                        Ok(Value::Str(String::new()))
                    })
                }),
            );
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
            active_flavor,
        };
        shell.activate(flavor);
        shell
    }

    /// Import a flavor's commands at global level (the other's go away).
    pub fn activate(&self, flavor: Flavor) {
        self.active_flavor.set(flavor);
        let names: Vec<&str> = COMMON
            .iter()
            .map(|c| c.0)
            .chain(JG.iter().map(|c| c.0))
            .chain(VCF.iter().map(|c| c.0))
            .chain(["include", "clock", "help"])
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

pub(super) fn note(msg: &str) {
    if check::active() {
        let cmd = msg.split([':', ' ']).next().unwrap_or("");
        check::finding(check::Kind::Ignored, cmd, msg);
    } else {
        eprintln!("informal: {msg}");
    }
}

/// Command words after the name, with `-opt value` pairs pulled out.
pub(super) struct Args {
    pub(super) cmd: String,
    pub(super) pos: Vec<String>,
    pub(super) opts: Vec<(String, Vec<String>)>,
}

impl Args {
    /// `arity` gives the number of values each option takes; options in
    /// `ignore` (with their arity) are dropped with a note.
    pub(super) fn parse(
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

    pub(super) fn flag(&self, o: &str) -> bool {
        self.opts.iter().any(|(k, _)| k == o)
    }
    pub(super) fn val(&self, o: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(k, _)| k == o)
            .and_then(|(_, v)| v.first())
            .map(String::as_str)
    }
    pub(super) fn all<'a>(&'a self, o: &'a str) -> impl Iterator<Item = &'a Vec<String>> + 'a {
        self.opts
            .iter()
            .filter(move |(k, _)| k == o)
            .map(|(_, v)| v)
    }
}

/// Expand `$VAR` and `${VAR}` from the environment, as filelists allow.
pub(super) fn expand_env(s: &str) -> String {
    let re = regex::Regex::new(r"\$\{(\w+)\}|\$(\w+)").unwrap();
    re.replace_all(s, |c: &regex::Captures| {
        let k = c.get(1).or(c.get(2)).unwrap().as_str();
        std::env::var(k).unwrap_or_default()
    })
    .into_owned()
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
    ("set_lowering", design::set_lowering),
    ("check_assumptions", tasks::check_assumptions),
    ("set_vacuity", tasks::set_vacuity),
    ("set_engines", props::set_engines),
    ("set_prove_cache", tasks::set_prove_cache),
    ("set_prove_cache_path", tasks::set_prove_cache_path),
    ("set_prove_cache_mode", tasks::set_prove_cache_mode),
    ("check_conn", apps::check_conn),
    ("check_cov", apps::check_cov),
    ("check_xprop", apps::check_xprop),
    ("check_spv", apps::check_spv),
    ("check_sec", apps::check_sec),
    ("check_csr", apps::check_csr),
    ("check_protocol", apps::check_protocol),
    ("why_cex", apps::why_cex),
    ("get_fv_complexity", apps::get_fv_complexity),
    ("blackbox_assistant", apps::blackbox_assistant),
    ("check_fv_setup", apps::check_fv_setup),
    ("report_fv_setup", apps::check_fv_setup),
    ("get_reset_info", apps::get_reset_info),
    ("abstract", apps::abstract_cmd),
    ("report_abstractions", apps::report_abstractions),
    ("set_abstractions", apps::set_abstractions),
    ("load_upf", lp::load_upf),
    ("read_liberty", lp::read_liberty),
    ("check_lp", lp::check_lp),
    ("report_lp", lp::report_lp),
    ("set_lp_waiver", lp::set_lp_waiver),
    ("compare_upf", lp::compare_upf),
    ("check_lpv", lp::check_lpv),
];

const JG: &[(&str, Body)] = &[
    ("clear", design::clear),
    ("analyze", design::jg_analyze),
    ("elaborate", design::jg_elaborate),
    ("clock_cmd", design::clock_cmd),
    ("reset", design::reset),
    ("assert", |s, a| props::jg_prop(s, a, Kind::Assert)),
    ("assume", |s, a| props::jg_prop(s, a, Kind::Assume)),
    ("cover", |s, a| props::jg_prop(s, a, Kind::Cover)),
    ("prove", prove_cmd::prove),
    ("get_property_list", props::get_property_list),
    ("get_status", props::get_status),
    ("set_prove_time_limit", prove_cmd::set_prove_time_limit),
    (
        "set_prove_per_property_time_limit",
        prove_cmd::set_prove_per_property_time_limit,
    ),
    (
        "set_prove_total_time_limit",
        prove_cmd::set_prove_total_time_limit,
    ),
    ("set_prove_effort", prove_cmd::set_prove_effort),
    (
        "set_prove_orchestration",
        prove_cmd::set_prove_orchestration,
    ),
    ("set_engine_mode", props::set_engine_mode),
    ("report", props::report),
    ("get_design_info", design::get_design_info),
    ("get_property_info", props::get_property_info),
    ("get_stats", props::get_stats),
    ("stopat", tasks::stopat),
    ("task", tasks::task),
    ("get_task_list", tasks::get_task_list),
    ("visualize", props::visualize),
    ("set_proofgrid_per_engine_max_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_per_engine_max_local_jobs", |_, _| {
        Ok(().into())
    }),
    ("set_proofgrid_max_local_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_max_jobs", |_, _| Ok(().into())),
    ("set_proofgrid_mode", |_, _| Ok(().into())),
    ("set_max_trace_length", prove_cmd::set_max_trace_length),
];

const VCF: &[(&str, Body)] = &[
    ("set_blackbox", design::set_blackbox),
    ("set_fml_appmode", design::set_fml_appmode),
    ("set_fml_var", prove_cmd::set_fml_var),
    ("read_file", design::vcf_read_file),
    ("analyze", design::vcf_analyze),
    ("elaborate", design::vcf_elaborate),
    ("create_clock", design::create_clock),
    ("snip_driver", tasks::stopat),
    ("create_reset", design::create_reset),
    ("sim_run", |_, _| Ok(().into())),
    ("sim_save_reset", |_, _| Ok(().into())),
    ("fvassert", |s, a| props::vcf_prop(s, a, Kind::Assert)),
    ("fvassume", |s, a| props::vcf_prop(s, a, Kind::Assume)),
    ("fvcover", |s, a| props::vcf_prop(s, a, Kind::Cover)),
    ("fvdisable", |s, a| props::set_enabled(s, &a[1..], false)),
    ("fvenable", |s, a| props::set_enabled(s, &a[1..], true)),
    ("check_fv", prove_cmd::check_fv),
    ("report_fv", props::report_fv),
    ("get_props", props::get_props),
];

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
             elaborate -top top -loop_limit 64\n\
             clock clk\n\
             foreach p {a b} {\n\
                 assert -name p_$p -frob {req |=> gnt}\n\
                 assert -name q_$p {req |=> gnt}\n\
             }\n\
             check_cdc -create\n\
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
            "set_jg_gui_layout u.sig\ncover -name c1 {done}\n",
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
                (Unknown, 9, "check_cdc"),
                (Ignored, 10, "prove"),
                (Unknown, 1, "set_jg_gui_layout"),
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
        let e = sh.eval("assert -name r_empty {   }").unwrap_err();
        assert!(e.message.contains("empty property"), "{}", e.message);
        // `implies`, `iff`, `if..else`, ranges, goto repetition, negation,
        // followed-by and aborts are taken: the front end compiles them at prove.
        sh.eval("assert -name r_imp {a implies b}").unwrap();
        sh.eval("assert -name r {a |-> ##[1:3] b}").unwrap();
        sh.eval("assert -name r_goto {a |-> b [->2]}").unwrap();
        sh.eval("assert -name r_not {not (a ##1 b)}").unwrap();
        sh.eval("assert -name r_fby {(a ##1 b) #-# c}").unwrap();
        sh.eval("assert -name r_abort {accept_on (c) (a |-> b)}")
            .unwrap();
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

    #[test]
    fn help_catalog_matches_all_registered_commands() {
        use std::collections::BTreeSet;
        let expected: BTreeSet<(help::DocFlavor, &str)> = COMMON
            .iter()
            .map(|(n, _)| (help::DocFlavor::Common, *n))
            .chain(std::iter::once((help::DocFlavor::Common, "help")))
            .chain(JG.iter().map(|(n, _)| {
                let name = if *n == "clock_cmd" { "clock" } else { *n };
                (help::DocFlavor::Jg, name)
            }))
            .chain(std::iter::once((help::DocFlavor::Jg, "include")))
            .chain(VCF.iter().map(|(n, _)| (help::DocFlavor::Vcf, *n)))
            .collect();
        let documented: BTreeSet<(help::DocFlavor, &str)> = help::all_docs()
            .iter()
            .map(|d| (d.flavor, d.name))
            .collect();
        let missing: Vec<_> = expected.difference(&documented).collect();
        let extra: Vec<_> = documented.difference(&expected).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "help catalog drift: missing={missing:?}, extra={extra:?}"
        );
        assert_eq!(
            help::all_docs().len(),
            documented.len(),
            "duplicate (flavor, name) entry in COMMAND_DOCS"
        );
    }

    #[test]
    fn interactive_help_lists_and_describes_commands() {
        let jg = shell(Flavor::Jg);
        let summary = jg.eval("help").unwrap();
        assert!(summary.contains("active flavor: jg"), "{summary}");
        assert!(summary.contains("prove"), "{summary}");
        assert!(summary.contains("check_protocol"), "{summary}");
        let apps = jg.eval("help -apps").unwrap();
        assert!(
            apps.contains("check_csr") && !apps.contains("get_version"),
            "{apps}"
        );
        let detail = jg.eval("help prove").unwrap();
        assert!(
            detail.contains("-with_proof_core") && detail.contains("check_fv"),
            "{detail}"
        );
        let jg_an = jg.eval("help analyze").unwrap();
        assert!(jg_an.contains("analyze [jg]"), "{jg_an}");
        let vcf_an = jg.eval("help vcf::analyze").unwrap();
        assert!(vcf_an.contains("analyze [vcf]"), "{vcf_an}");
        let js: serde_json::Value = serde_json::from_str(&jg.eval("help -json").unwrap()).unwrap();
        assert_eq!(js.as_array().unwrap().len(), help::all_docs().len());
        let err = jg.eval("help no_such_cmd").unwrap_err();
        assert!(err.message.contains("unknown command"), "{}", err.message);

        let vcf = shell(Flavor::Vcf);
        let vcf_sum = vcf.eval("help").unwrap();
        assert!(
            vcf_sum.contains("active flavor: vcf") && vcf_sum.contains("check_fv"),
            "{vcf_sum}"
        );
        let vcf_a = vcf.eval("help analyze").unwrap();
        assert!(vcf_a.contains("analyze [vcf]"), "{vcf_a}");
    }

    #[test]
    fn prove_all_logs_hint_when_current_task_is_empty_but_named_task_has_properties() {
        use crate::progress::{ProveProgress, ProveSink};
        use std::sync::{Arc, Mutex};

        struct Capture(Arc<Mutex<Vec<String>>>);
        impl ProveSink for Capture {
            fn progress(&self, _: &ProveProgress) {}
            fn line(&self, text: &str, _: bool) {
                self.0.lock().unwrap().push(text.to_string());
            }
        }

        let dir = std::env::temp_dir().join(format!("informal-empty-task-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sv = dir.join("top.sv");
        std::fs::write(
            &sv,
            "module top(input logic clk, input logic rst_n, output logic y); assign y = 1'b1; endmodule\n",
        )
        .unwrap();
        let sh = shell(Flavor::Jg);
        let lines = Arc::new(Mutex::new(Vec::new()));
        sh.session.borrow_mut().sink = Some(Arc::new(Capture(lines.clone())));
        sh.eval(&format!(
            "analyze -sv {{{}}}; elaborate -top top; clock clk; reset -expression {{!rst_n}}",
            sv.display()
        ))
        .unwrap();
        sh.eval("task -create Protocol").unwrap();
        sh.eval("assert -task Protocol -name a_ok {y == 1'b1}")
            .unwrap();
        // `<embedded>` is still current with 0 properties
        let _ = sh.eval("prove -all").unwrap();
        let log = lines.lock().unwrap().join("\n");
        assert!(
            log.contains("current task `<embedded>` has 0 provable properties")
                && log.contains("Protocol"),
            "expected empty-task hint in session log, got: {log}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
