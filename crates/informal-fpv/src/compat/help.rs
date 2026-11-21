//! Authoritative Tcl command metadata and interactive `help` command for
//! `informal shell`.
//!
//! This catalog is the single source of truth for:
//! 1. Interactive `help [cmd|-all|-jg|-vcf|-apps|-json]` inside `informal shell`.
//! 2. Auto-generated Markdown reference manuals (`informal docgen`:
//!    `docs/reference/tcl-commands.md` and `docs/reference/jg-vcf-crosswalk.md`).
//!
//! A unit test in `compat.rs` enforces 1:1 coverage against `COMMON`, `JG`, and
//! `VCF` so a new Tcl command cannot ship without documentation.

use super::{glob, Flavor};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DocFlavor {
    Common,
    Jg,
    Vcf,
}

impl DocFlavor {
    pub fn label(self) -> &'static str {
        match self {
            DocFlavor::Common => "common",
            DocFlavor::Jg => "jg",
            DocFlavor::Vcf => "vcf",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    DesignSetup,
    PropertiesAndTasks,
    ProvingAndEngines,
    DebugAndComplexity,
    FormalApps,
    EnvironmentAndCache,
    Utilities,
}

impl Category {
    pub const ALL: &'static [Category] = &[
        Category::DesignSetup,
        Category::PropertiesAndTasks,
        Category::ProvingAndEngines,
        Category::DebugAndComplexity,
        Category::FormalApps,
        Category::EnvironmentAndCache,
        Category::Utilities,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::DesignSetup => "Design Setup (Analyze, Elaborate, Clocks, Reset, Blackbox)",
            Category::PropertiesAndTasks => "Properties, Cut Points, and Tasks",
            Category::ProvingAndEngines => "Proving, Engines, and Reporting",
            Category::DebugAndComplexity => "Debugging, Complexity, and Abstraction",
            Category::FormalApps => "Formal Apps",
            Category::EnvironmentAndCache => "Environment, Vacuity, Lowering, and Proof Cache",
            Category::Utilities => "Shell Utilities",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct OptDoc {
    pub flag: &'static str,
    pub desc: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CmdDoc {
    pub name: &'static str,
    pub flavor: DocFlavor,
    pub category: Category,
    pub summary: &'static str,
    pub usage: &'static str,
    pub options: &'static [OptDoc],
    pub ignored_options: &'static [&'static str],
    pub equivalent: Option<&'static str>,
    pub example: &'static str,
    pub notes: &'static str,
}

const fn opt(flag: &'static str, desc: &'static str) -> OptDoc {
    OptDoc { flag, desc }
}

pub fn all_docs() -> &'static [CmdDoc] {
    COMMAND_DOCS
}

pub fn to_json() -> serde_json::Value {
    serde_json::to_value(COMMAND_DOCS).expect("CmdDoc serializes cleanly")
}

/// Format a categorized command list for interactive `help`.
pub fn format_summary(flavor: Option<DocFlavor>, apps_only: bool, pattern: Option<&str>) -> String {
    let mut out = String::new();
    let header = match (flavor, apps_only, pattern) {
        (_, true, _) => "InFormal Formal Apps (available in all flavors):".to_string(),
        (Some(DocFlavor::Jg), false, None) => {
            "InFormal Tcl Commands (active flavor: jg + common):".to_string()
        }
        (Some(DocFlavor::Vcf), false, None) => {
            "InFormal Tcl Commands (active flavor: vcf + common):".to_string()
        }
        (None, false, None) => "InFormal Tcl Commands (all flavors: common, jg, vcf):".to_string(),
        (_, false, Some(p)) => format!("InFormal Tcl Commands matching `{p}`:"),
        (Some(DocFlavor::Common), false, None) => "InFormal Common Commands:".to_string(),
    };
    out.push_str(&header);
    out.push('\n');

    let mut matched = 0usize;
    for &cat in Category::ALL {
        if apps_only && cat != Category::FormalApps {
            continue;
        }
        let items: Vec<&CmdDoc> = COMMAND_DOCS
            .iter()
            .filter(|d| d.category == cat)
            .filter(|d| match flavor {
                None => true,
                Some(DocFlavor::Common) => d.flavor == DocFlavor::Common,
                Some(f) => d.flavor == f || d.flavor == DocFlavor::Common,
            })
            .filter(|d| match pattern {
                None => true,
                Some(p) => {
                    let pat = if p.contains('*') || p.contains('?') {
                        p.to_string()
                    } else {
                        format!("*{p}*")
                    };
                    glob(&pat, d.name)
                        || d.summary
                            .to_ascii_lowercase()
                            .contains(&p.to_ascii_lowercase())
                }
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{}\n", cat.title()));
        for d in items {
            matched += 1;
            let tag = match d.flavor {
                DocFlavor::Common => "",
                DocFlavor::Jg if flavor.is_none() => " [jg]",
                DocFlavor::Vcf if flavor.is_none() => " [vcf]",
                _ => "",
            };
            out.push_str(&format!("  {:<38} {}{}\n", d.name, d.summary, tag));
        }
    }
    if matched == 0 {
        out.push_str("  (no matching commands)\n");
    } else {
        out.push_str("\nTip: run `help <command>` for options and examples, or `help -all` / `help -apps`.\n");
    }
    out
}

/// Format detailed documentation for a single command (supporting `jg::cmd` / `vcf::cmd`).
pub fn format_command(raw_name: &str, preferred: Flavor) -> Result<String, String> {
    let (explicit_flavor, name) = if let Some(rest) = raw_name.strip_prefix("jg::") {
        (Some(DocFlavor::Jg), rest)
    } else if let Some(rest) = raw_name.strip_prefix("vcf::") {
        (Some(DocFlavor::Vcf), rest)
    } else if let Some(rest) = raw_name.strip_prefix("informal::") {
        (Some(DocFlavor::Common), rest)
    } else {
        (None, raw_name)
    };
    let name = name.strip_prefix("::").unwrap_or(name);
    let pref = match preferred {
        Flavor::Jg => DocFlavor::Jg,
        Flavor::Vcf => DocFlavor::Vcf,
    };

    let matches: Vec<&CmdDoc> = COMMAND_DOCS
        .iter()
        .filter(|d| d.name == name)
        .filter(|d| match explicit_flavor {
            Some(f) => d.flavor == f || (f != DocFlavor::Common && d.flavor == DocFlavor::Common),
            None => true,
        })
        .collect();

    if matches.is_empty() {
        let near: Vec<&str> = COMMAND_DOCS
            .iter()
            .filter(|d| d.name.contains(name) || name.contains(d.name))
            .map(|d| d.name)
            .collect();
        let hint = if near.is_empty() {
            "Run `help -all` to list all commands.".to_string()
        } else {
            format!("Similar commands: {}", near.join(", "))
        };
        return Err(format!("help: unknown command `{raw_name}`. {hint}"));
    }

    let doc = matches
        .iter()
        .copied()
        .find(|d| d.flavor == pref)
        .or_else(|| {
            matches
                .iter()
                .copied()
                .find(|d| d.flavor == DocFlavor::Common)
        })
        .unwrap_or(matches[0]);

    let mut out = String::new();
    out.push_str(&format!(
        "{} [{}] — {}\n",
        doc.name,
        doc.flavor.label(),
        doc.summary
    ));
    out.push_str(&format!("Category: {}\n", doc.category.title()));
    out.push_str(&format!("\nUsage:\n  {}\n", doc.usage));
    if !doc.options.is_empty() {
        out.push_str("\nOptions:\n");
        for o in doc.options {
            out.push_str(&format!("  {:<30} {}\n", o.flag, o.desc));
        }
    }
    if !doc.ignored_options.is_empty() {
        out.push_str(&format!(
            "\nAccepted & ignored vendor options:\n  {}\n",
            doc.ignored_options.join(", ")
        ));
    }
    if let Some(eq) = doc.equivalent {
        out.push_str(&format!("\nCross-flavor equivalent:\n  {eq}\n"));
    }
    if !doc.notes.is_empty() {
        out.push_str(&format!("\nNotes:\n  {}\n", doc.notes));
    }
    if !doc.example.is_empty() {
        out.push_str(&format!(
            "\nExample:\n  {}\n",
            doc.example.replace('\n', "\n  ")
        ));
    }
    if explicit_flavor.is_none() && matches.len() > 1 {
        let others: Vec<String> = matches
            .iter()
            .filter(|d| d.flavor != doc.flavor)
            .map(|d| format!("{}::{}", d.flavor.label(), d.name))
            .collect();
        out.push_str(&format!(
            "\nAlso defined in another flavor: {} (run `help {}` to inspect).\n",
            others.join(", "),
            others[0]
        ));
    }
    Ok(out)
}

/// Evaluate the `help` Tcl command.
pub fn eval_help(args: &[String], active: Flavor) -> Result<String, String> {
    let active_doc = match active {
        Flavor::Jg => DocFlavor::Jg,
        Flavor::Vcf => DocFlavor::Vcf,
    };
    let mut filter: Option<Option<DocFlavor>> = None;
    let mut apps_only = false;
    let mut json_mode = false;
    let mut pos: Vec<&str> = Vec::new();

    for w in &args[1..] {
        match w.as_str() {
            "-all" => filter = Some(None),
            "-jg" => filter = Some(Some(DocFlavor::Jg)),
            "-vcf" => filter = Some(Some(DocFlavor::Vcf)),
            "-common" => filter = Some(Some(DocFlavor::Common)),
            "-apps" => apps_only = true,
            "-json" => json_mode = true,
            "-verbose" => {}
            other if other.starts_with('-') => {
                return Err(format!(
                    "help: unsupported option `{other}` (use -all, -jg, -vcf, -common, -apps, -verbose, -json)"
                ));
            }
            other => pos.push(other),
        }
    }

    if json_mode {
        return serde_json::to_string_pretty(&to_json()).map_err(|e| e.to_string());
    }

    if let [single] = pos.as_slice() {
        if !single.contains('*') && !single.contains('?') && filter.is_none() && !apps_only {
            return format_command(single, active);
        }
        return Ok(format_summary(
            filter.unwrap_or(Some(active_doc)),
            apps_only,
            Some(single),
        ));
    }

    if !pos.is_empty() {
        return Err("help: expected at most one command name or pattern".into());
    }

    Ok(format_summary(
        filter.unwrap_or(Some(active_doc)),
        apps_only,
        None,
    ))
}

pub const COMMAND_DOCS: &[CmdDoc] = &[
    // ============================== COMMON ==================================
    CmdDoc {
        name: "help",
        flavor: DocFlavor::Common,
        category: Category::Utilities,
        summary: "Show Tcl command reference, options, and examples",
        usage: "help [command|pattern] [-all|-jg|-vcf|-common|-apps|-verbose|-json]",
        options: &[
            opt("-all", "List commands across common, JasperGold (jg), and VC Formal (vcf) flavors"),
            opt("-jg", "List JasperGold-flavor and common commands"),
            opt("-vcf", "List VC Formal-flavor and common commands"),
            opt("-common", "List flavor-independent InFormal commands only"),
            opt("-apps", "List Formal Apps (`check_*`) only"),
            opt("-verbose", "Show full detail for a command (accepted for vendor compatibility)"),
            opt("-json", "Emit the full command catalog as JSON"),
        ],
        ignored_options: &[],
        equivalent: Some("Available in both `jg` and `vcf` flavors"),
        example: "help\nhelp prove\nhelp -apps\nhelp *cache*",
        notes: "Use `jg::<cmd>` or `vcf::<cmd>` to disambiguate commands that exist in both flavors (such as `analyze` or `elaborate`).",
    },
    CmdDoc {
        name: "get_version",
        flavor: DocFlavor::Common,
        category: Category::Utilities,
        summary: "Return the InFormal version string",
        usage: "get_version",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "puts [get_version]",
        notes: "",
    },
    CmdDoc {
        name: "echo",
        flavor: DocFlavor::Common,
        category: Category::Utilities,
        summary: "Print arguments separated by spaces to standard output",
        usage: "echo [word ...]",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "echo Starting FPV run",
        notes: "Suppressed in `informal shell --check` dry-run mode.",
    },
    CmdDoc {
        name: "session",
        flavor: DocFlavor::Common,
        category: Category::Utilities,
        summary: "Dump the complete FPV session state as pretty-printed JSON",
        usage: "session",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set js [informal::session]",
        notes: "Useful for debugging scripts or driving external tooling.",
    },
    CmdDoc {
        name: "set_lowering",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Toggle opt-in SystemVerilog lowering relaxations",
        usage: "set_lowering [skip_initial_checks|isunknown_is_false [on|off]]",
        options: &[
            opt("skip_initial_checks", "Omit time-0 checks in `initial` blocks instead of rejecting them"),
            opt("isunknown_is_false", "Lower `$isunknown(x)` as constant `0` (2-state) instead of rejecting it"),
        ],
        ignored_options: &[],
        equivalent: Some("CLI: `informal lower --skip-initial-checks --isunknown-is-false`"),
        example: "set_lowering isunknown_is_false on",
        notes: "Both switches default to `off` (decision D13: never guess silently). Changing a switch invalidates elaboration so the next `elaborate` or `prove` re-lowers.",
    },
    CmdDoc {
        name: "check_assumptions",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Check assumption satisfiability, UNSAT conflict cores, and 2QBF dead-end trap states",
        usage: "check_assumptions [-depth N] [-conflict] [-dead_end]",
        options: &[
            opt("-depth N", "Unrolling depth for environment consistency check (default 20)"),
            opt("-conflict", "Compute a minimal conflicting subset of assumptions when overconstrained"),
            opt("-dead_end", "Run 2QBF trap-state check detecting reachable states with no legal next input"),
        ],
        ignored_options: &[],
        equivalent: None,
        example: "check_assumptions -depth 20 -conflict -dead_end",
        notes: "With `-dead_end`, returns `alive` or `dead_end at step ...`; with `-conflict`, returns a list of conflicting assumptions; otherwise returns the allowed trace depth. Automatically run before `prove` when vacuity checking is on.",
    },
    CmdDoc {
        name: "set_vacuity",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Enable or disable automatic environment and precondition vacuity checks",
        usage: "set_vacuity [on|off]",
        options: &[
            opt("on", "Enable environment consistency (depth 20) and precondition vacuity covers"),
            opt("off", "Disable automatic vacuity checks during `prove`"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `elaborate -no_preconditions`"),
        example: "set_vacuity on",
        notes: "On by default. Call with no arguments to query current state.",
    },
    CmdDoc {
        name: "set_engines",
        flavor: DocFlavor::Common,
        category: Category::ProvingAndEngines,
        summary: "Restrict the proof portfolio to named engines",
        usage: "set_engines [engine_name ...]",
        options: &[],
        ignored_options: &[],
        equivalent: Some("JG: `set_engine_mode`"),
        example: "set_engines informal-bmc informal-kind informal-ic3",
        notes: "Pass an empty list to re-enable all configured engines. Unknown engine names are rejected with the available list.",
    },
    CmdDoc {
        name: "set_prove_cache",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Enable or disable the content-addressed SHA-256 proof cache",
        usage: "set_prove_cache [on|off]",
        options: &[
            opt("on", "Enable caching of proven and unreachable verdicts"),
            opt("off", "Disable proof cache lookup and writes"),
        ],
        ignored_options: &[],
        equivalent: None,
        example: "set_prove_cache on",
        notes: "Keys are computed from the exact lowered Btor2 model (design + active assumptions + cut points + property).",
    },
    CmdDoc {
        name: "set_prove_cache_path",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Get or set the proof cache directory path",
        usage: "set_prove_cache_path [dir]",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_prove_cache_path .informal_cache",
        notes: "Share across CI runs to skip re-proving unchanged properties.",
    },
    CmdDoc {
        name: "set_prove_cache_mode",
        flavor: DocFlavor::Common,
        category: Category::EnvironmentAndCache,
        summary: "Set proof cache read/write policy",
        usage: "set_prove_cache_mode [read_write|read_only|write_only]",
        options: &[
            opt("read_write", "Read hits and store new proofs (default)"),
            opt("read_only", "Reuse cached verdicts without writing new entries"),
            opt("write_only", "Always re-prove and refresh cache entries"),
        ],
        ignored_options: &[],
        equivalent: None,
        example: "set_prove_cache_mode read_write",
        notes: "",
    },
    CmdDoc {
        name: "check_conn",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Formal Connectivity App: verify pin-to-pin / muxed / pipelined CSV connectivity maps",
        usage: "check_conn (-load <csv> | -prove | -report | -list | -clear | -init | -validate) [options]",
        options: &[
            opt("-load <csv>", "Load OpenTitan/JasperGold CONNECTION/CONDITION/LATENCY CSV into task"),
            opt("-prove", "Prove all loaded connectivity assertions"),
            opt("-report", "Print connectivity verification summary (use `-file F [-force]` to save)"),
            opt("-list", "List loaded connectivity rules"),
            opt("-task <T>", "Target task name (default `Connectivity`)"),
            opt("-time_limit <dur>", "Per-property time limit"),
            opt("-clear", "Clear loaded connectivity rules"),
            opt("-init", "Initialize connectivity app state"),
            opt("-file <F>", "Write report to file (with `-force` to overwrite)"),
        ],
        ignored_options: &["-bg", "-silent", "-generate_candidates", "-validate"],
        equivalent: Some("JasperGold Connectivity App (`check_conn`)"),
        example: "check_conn -load conn.csv\ncheck_conn -prove\ncheck_conn -report",
        notes: "Supports `CONNECTION,name,src,dst`, `CONDITION,name,cond`, and `LATENCY,name,cycles` CSV directives.",
    },
    CmdDoc {
        name: "check_cov",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Formal Coverage & UNR App: COI observability, branch/toggle dead-code detection, and waivers",
        usage: "check_cov (-init | -measure | -report | -waiver | -clear) [options]",
        options: &[
            opt("-init", "Initialize coverage models (`-model {coi branch toggle functional}`)"),
            opt("-model <list>", "Coverage models to track: `coi`, `branch`, `toggle`, `functional`"),
            opt("-measure", "Run formal reachability/COI measurement on active cover items"),
            opt("-report", "Print coverage summary (`-html -report_file F -force` for HTML report)"),
            opt("-waiver", "Manage waivers (`-add|-remove|-list`, `-type`, `-instance`, `-comment`, `-cid`)"),
            opt("-task <T> / -tasks <L>", "Restrict measurement or reporting to specific task(s)"),
            opt("-time_limit <dur>", "Time limit per UNR check"),
            opt("-clear", "Reset coverage state"),
        ],
        ignored_options: &["-bg", "-silent", "-no_return", "-view", "-exclude_bind_hierarchies", "-enable_prove_based_proof_core"],
        equivalent: Some("JasperGold Coverage App (`check_cov`)"),
        example: "check_cov -init -model {coi branch toggle}\ncheck_cov -measure\ncheck_cov -report",
        notes: "Proves unreachable branches/toggles (UNR) and reports Cone-of-Influence observability.",
    },
    CmdDoc {
        name: "check_xprop",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Exact 2-copy self-composition Formal X-Propagation App",
        usage: "check_xprop (-init | -prove | -report | -list | -waiver | -clear) [options]",
        options: &[
            opt("-init", "Generate X-propagation targets (`-targets {outputs|flops|all}`, `-x_source`)"),
            opt("-targets <list>", "Observation targets: `outputs`, `flops`, `all`, or explicit signal list"),
            opt("-x_source / -source <list>", "X injection sources: reset-less flops, X literals, undriven nets"),
            opt("-to <signals>", "Explicit target signals to check for X reachability"),
            opt("-prove", "Prove all generated X-propagation checks"),
            opt("-report", "Print X-prop summary (`-file F [-force]`)"),
            opt("-waiver", "Add/remove/list X-prop waivers (`-add -instance <sig> -comment <text>`)"),
            opt("-task <T>", "Target task name (default `XProp`)"),
            opt("-time_limit <dur>", "Per-property time limit"),
            opt("-clear", "Clear X-prop state"),
        ],
        ignored_options: &["-bg", "-silent"],
        equivalent: Some("JasperGold X-Prop App (`check_xprop`)"),
        example: "check_xprop -init -targets {outputs} -prove\ncheck_xprop -report",
        notes: "Uses exact 2-copy self-composition (avoiding false mux X-pessimism).",
    },
    CmdDoc {
        name: "check_spv",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Formal Security Path Verification (SPV) App: 2-safety non-interference and taint flow",
        usage: "check_spv (-create | -prove | -report | -list | -clear) [options]",
        options: &[
            opt("-create", "Define a taint/isolation check (`-name N -from S -to O [-cond C]`)"),
            opt("-name <N>", "Property name for the security path check"),
            opt("-from <sig>", "Sensitive source signal (e.g., key register or port)"),
            opt("-to <sig>", "Observable sink signal (e.g., public or debug output)"),
            opt("-cond / -precond <expr>", "Optional guard condition under which isolation must hold"),
            opt("-prove", "Prove all defined SPV checks"),
            opt("-report", "Print SPV results (`-file F [-force]`)"),
            opt("-list", "List defined SPV checks"),
            opt("-task <T>", "Target task name (default `SPV`)"),
            opt("-time_limit <dur>", "Per-property time limit"),
            opt("-clear", "Clear SPV checks"),
        ],
        ignored_options: &["-bg", "-silent"],
        equivalent: Some("JasperGold SPV App (`check_spv`)"),
        example: "check_spv -create -name iso_key -from secret_key -to debug_o -cond {!dbg_en}\ncheck_spv -prove\ncheck_spv -report",
        notes: "Detects both direct combinational and multi-cycle covert storage leaks via 2-copy miter.",
    },
    CmdDoc {
        name: "check_sec",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Formal Sequential (SEC) and Combinational (LEC) Equivalence Checking App",
        usage: "check_sec (-setup | -map | -prove | -report | -list | -clear) [options]",
        options: &[
            opt("-setup", "Configure spec vs. implementation designs (`-spec`, `-imp`, `-clock`, `-reset`)"),
            opt("-spec / -spec_files <F>", "Reference specification RTL file(s) (`-spec_top`, `-spec_param`, `-spec_define`, `-spec_incdir`)"),
            opt("-imp / -imp_files <F>", "Implementation RTL file(s) (`-imp_top`, `-imp_param`, `-imp_define`, `-imp_incdir`)"),
            opt("-lec", "Run combinational keypoint equivalence mode (match state/outputs directly)"),
            opt("-latency <N>", "Retiming pipeline latency offset (`imp` delayed by N cycles relative to `spec`)"),
            opt("-clock <clk> / -reset <expr>", "Clock and reset expression for sequential equivalence"),
            opt("-map", "Explicitly map `-input`, `-output`, `-state`, or `-output_latency` pairs, or add `-assume`"),
            opt("-prove", "Prove equivalence across all mapped outputs"),
            opt("-report", "Print SEC/LEC report (`-json`, `-file F [-force]`)"),
            opt("-task <T>", "Target task name (default `SEC`)"),
            opt("-time_limit <dur>", "Per-output time limit"),
            opt("-clear", "Clear SEC setup"),
        ],
        ignored_options: &["-auto"],
        equivalent: Some("JasperGold SEC App (`check_sec`)"),
        example: "check_sec -setup -spec spec.sv -imp imp.sv -clock clk -reset {!rst_n} -prove\ncheck_sec -report",
        notes: "Generates side-by-side `spec`/`imp` mismatch VCD traces on non-equivalence.",
    },
    CmdDoc {
        name: "check_csr",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "Formal CSR / RAL Verification App from OpenTitan HJSON, CSV, or UVM RAL `.sv`",
        usage: "check_csr (-load <spec> | -prove | -report | -list | -clear) [options]",
        options: &[
            opt("-load <file>", "Load register spec (`.hjson`, `.csv`, or `uvm_reg_block` `.sv`) and generate checks"),
            opt("-prefix <P>", "Signal prefix for bus ports (`-addr`, `-we`, `-re`, `-wdata`, `-rdata`, `-read_latency`)"),
            opt("-prove", "Prove all generated CSR reset, access (`RW`/`RO`/`W1C`/`W1S`/`W0C`/`WO`/`RC`), `regwen`, and non-aliasing checks"),
            opt("-report", "Print CSR verification summary (`-json`, `-file F [-force]`)"),
            opt("-list", "List generated CSR checks"),
            opt("-task <T>", "Target task name (default `CSR`)"),
            opt("-time_limit <dur>", "Per-property time limit"),
            opt("-clear", "Clear CSR app state"),
        ],
        ignored_options: &["-bg", "-silent"],
        equivalent: Some("JasperGold CSR App (`check_csr`)"),
        example: "check_csr -load regs.hjson -prove\ncheck_csr -report",
        notes: "Auto-infers standard bus port names (`addr`, `we`, `re`, `wdata`, `rdata`) unless overridden.",
    },
    CmdDoc {
        name: "check_protocol",
        flavor: DocFlavor::Common,
        category: Category::FormalApps,
        summary: "1-command Formal Bus Protocol VIP Generator (`tlul`, `axi4lite`, `apb`)",
        usage: "check_protocol (-create | -prove | -report | -list | -clear) [options]",
        options: &[
            opt("-create", "Instantiate protocol assertions/assumptions (`-protocol`, `-role`, `-prefix`, `-map`)"),
            opt("-protocol <name>", "Protocol standard: `apb`, `axi4lite`, or `tlul`"),
            opt("-role <role>", "DUT role: `slave` (assert responses, assume requests), `master`, or `monitor`"),
            opt("-prefix <P>", "Optional signal name prefix on the top module"),
            opt("-map <pairs>", "Explicit port overrides `{proto_sig dut_sig ...}`"),
            opt("-name <N>", "Instance label for generated protocol properties"),
            opt("-prove", "Prove all generated protocol properties"),
            opt("-report", "Print protocol verification report (`-json`, `-file F [-force]`)"),
            opt("-list", "List generated protocol checks"),
            opt("-task <T>", "Target task name (default `Protocol`)"),
            opt("-time_limit <dur>", "Per-property time limit"),
            opt("-clear", "Clear protocol app state"),
        ],
        ignored_options: &["-bg", "-silent"],
        equivalent: Some("JasperGold ABVIP / VC Formal FVIP"),
        example: "check_protocol -create -protocol apb -role slave -prove\ncheck_protocol -report",
        notes: "Generates both protocol safety assertions and legal bus master/slave environment assumptions matched to `-role`.",
    },
    CmdDoc {
        name: "why_cex",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Compute a dynamic backward causal root-cause slice of a CEX or cover trace",
        usage: "why_cex [-property P] [-max_events N] [-json] [property_name]",
        options: &[
            opt("-property <P>", "Failing assertion or covered witness property name"),
            opt("-max_events <N>", "Limit reported causal events"),
            opt("-json", "Emit structured JSON causal slice"),
        ],
        ignored_options: &[],
        equivalent: Some("JasperGold Visualize QuietTrace / VC Formal RCA"),
        example: "why_cex -property a_never9",
        notes: "Walks backward from failure cycle T to 0, pruning untaken mux arms and non-controlling logic operands.",
    },
    CmdDoc {
        name: "get_fv_complexity",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Analyze cone-of-influence state bits, memories, and wide-arithmetic proof hotspots",
        usage: "get_fv_complexity [-property P] [-task T] [-json]",
        options: &[
            opt("-property <P>", "Restrict complexity analysis to one property's COI"),
            opt("-task <T>", "Target task (default current task)"),
            opt("-json", "Return structured JSON breakdown"),
        ],
        ignored_options: &[],
        equivalent: Some("JasperGold Proof Profiling / Complexity Analyzer"),
        example: "get_fv_complexity -property a_fifo_order",
        notes: "Highlights wide counters, multipliers, and unpacked arrays that stall bit-level IC3/k-induction.",
    },
    CmdDoc {
        name: "blackbox_assistant",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Recommend or apply complexity-driven black-boxing of heavy submodules",
        usage: "blackbox_assistant [-threshold_bits N] [-apply] [-json]",
        options: &[
            opt("-threshold_bits <N>", "Minimum flop/memory bit threshold to flag a candidate"),
            opt("-apply", "Automatically add recommended modules to the blackbox list"),
            opt("-json", "Return recommendations as JSON"),
        ],
        ignored_options: &[],
        equivalent: Some("JasperGold Complexity Manager"),
        example: "blackbox_assistant -threshold_bits 128",
        notes: "Run before `prove` or when `get_fv_complexity` flags deep arithmetic/storage blocks.",
    },
    CmdDoc {
        name: "check_fv_setup",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Audit formal setup completeness (clocks, resets, reset-less flops, undriven nets, abstractions)",
        usage: "check_fv_setup [-verbose] [-json] [-file F [-force]]",
        options: &[
            opt("-verbose", "Include detailed signal lists for each finding"),
            opt("-json", "Emit audit report as JSON"),
            opt("-file <F>", "Write setup report to file (`-force` to overwrite)"),
        ],
        ignored_options: &[],
        equivalent: Some("Alias: `report_fv_setup`"),
        example: "check_fv_setup -verbose",
        notes: "Run right after `elaborate`, `clock`, and `reset` to catch missing resets or undriven internal nets early.",
    },
    CmdDoc {
        name: "report_fv_setup",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Alias for `check_fv_setup`: report formal setup audit findings",
        usage: "report_fv_setup [-verbose] [-json] [-file F [-force]]",
        options: &[
            opt("-verbose", "Include detailed signal lists"),
            opt("-json", "Emit audit report as JSON"),
            opt("-file <F>", "Write setup report to file (`-force` to overwrite)"),
        ],
        ignored_options: &[],
        equivalent: Some("Alias: `check_fv_setup`"),
        example: "report_fv_setup",
        notes: "",
    },
    CmdDoc {
        name: "get_reset_info",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Report declared reset expression, async resets found, and reset-less registers",
        usage: "get_reset_info [-json]",
        options: &[opt("-json", "Emit reset analysis as JSON")],
        ignored_options: &[],
        equivalent: Some("JG: `reset -analyze_only`"),
        example: "get_reset_info",
        notes: "",
    },
    CmdDoc {
        name: "abstract",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Apply sound over-approximate counter jump abstractions to accelerate deep proofs",
        usage: "abstract -counter <sig> [-threshold K] [-init_value V] [-task T] | abstract -list | abstract -clear",
        options: &[
            opt("-counter <sig>", "Counter register signal to abstract"),
            opt("-threshold <K>", "Fast-forward jump threshold (`K - 1`)"),
            opt("-init_value <V>", "Optional initial/reset value hint"),
            opt("-task <T>", "Apply abstraction to a specific task"),
            opt("-list", "List active abstractions"),
            opt("-clear", "Remove active abstractions"),
        ],
        ignored_options: &[],
        equivalent: Some("JasperGold `abstract -counter`"),
        example: "abstract -counter timeout_cnt_q -threshold 1000",
        notes: "Sound over-approximation: a proof under counter abstraction implies the concrete design holds; spurious CEXs can be refined.",
    },
    CmdDoc {
        name: "report_abstractions",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "List active formal abstractions and their parameters",
        usage: "report_abstractions [-json]",
        options: &[opt("-json", "Emit active abstractions as JSON")],
        ignored_options: &[],
        equivalent: Some("`abstract -list`"),
        example: "report_abstractions",
        notes: "",
    },
    CmdDoc {
        name: "set_abstractions",
        flavor: DocFlavor::Common,
        category: Category::DebugAndComplexity,
        summary: "Clear or disable active formal abstractions",
        usage: "set_abstractions (-clear | -off)",
        options: &[
            opt("-clear", "Remove all active abstractions"),
            opt("-off", "Disable abstractions"),
        ],
        ignored_options: &[],
        equivalent: Some("`abstract -clear`"),
        example: "set_abstractions -clear",
        notes: "",
    },
    // ============================== JG ======================================
    CmdDoc {
        name: "clear",
        flavor: DocFlavor::Jg,
        category: Category::DesignSetup,
        summary: "Reset the current session, clearing design files, elaboration, and properties",
        usage: "clear [-all]",
        options: &[opt("-all", "Clear all design state, tasks, and properties")],
        ignored_options: &[],
        equivalent: Some("VCF: start a new session"),
        example: "clear -all",
        notes: "",
    },
    CmdDoc {
        name: "analyze",
        flavor: DocFlavor::Jg,
        category: Category::DesignSetup,
        summary: "Collect SystemVerilog source files, filelists, defines, and include paths",
        usage: "analyze [-sv|-sv09|-sv12|-sv17|-v2k|-verilog] [-clear] [-bbox_m {M ...}] [-f list] [+define+K=V] [+incdir+D] file ...",
        options: &[
            opt("-sv / -sv12 / -sv17 ...", "Language standard flags (accepted; parsed as IEEE 1800-2023 SystemVerilog)"),
            opt("-bbox_m <mods>", "Mark module name(s) as source-less black boxes"),
            opt("-clear", "Clear previously analyzed files and black boxes"),
            opt("-f / -F <filelist>", "Read compile arguments and files from a VCS/Jasper filelist (`$VAR` expanded)"),
            opt("+define+K[=V] / -define / -D", "Define preprocessor macro(s)"),
            opt("+incdir+D / -incdir", "Add `include` search directory"),
        ],
        ignored_options: &["-lib", "-L"],
        equivalent: Some("VCF: `analyze -format sverilog` or `read_file`"),
        example: "analyze -sv12 +incdir+rtl +define+FORMAL rtl/counter.sv",
        notes: "VHDL (`-vhdl*`) is rejected with a clear error.",
    },
    CmdDoc {
        name: "elaborate",
        flavor: DocFlavor::Jg,
        category: Category::DesignSetup,
        summary: "Elaborate the analyzed SystemVerilog design with `svlang`",
        usage: "elaborate [-top M] [-parameter P V] [-bbox_m {M ...}] [-bbox_i {inst ...}] [-bbox_mul W] [-bbox_a N] [-disable_auto_bbox] [-no_preconditions] [-create_related_covers {precondition witness}]",
        options: &[
            opt("-top <module>", "Top module name (inferred if omitted and unambiguous)"),
            opt("-parameter / -param <P> <V>", "Override top-level parameter `P` with value `V` (repeatable)"),
            opt("-bbox_m <mods>", "Black-box module definition(s) (outputs become free inputs)"),
            opt("-bbox_i <insts>", "Black-box specific instance hierarchy path(s)"),
            opt("-bbox_mul <bits>", "Auto-blackbox multipliers at or above `<bits>` width"),
            opt("-bbox_a <words>", "Auto-blackbox unpacked arrays/memories at or above `<words>` depth"),
            opt("-disable_auto_bbox", "Disable automatic complexity black-boxing"),
            opt("-no_preconditions", "Skip automatic precondition vacuity covers"),
            opt("-create_related_covers <kinds>", "Enable `precondition` and/or `witness` covers"),
        ],
        ignored_options: &["-loop_limit", "-multiple_tops", "-extract_covergroup", "-x_value", "-auto_hr_info"],
        equivalent: Some("VCF: `elaborate <top>` / `read_file -top <top>`"),
        example: "elaborate -top fifo -parameter DEPTH 8 -create_related_covers {precondition witness}",
        notes: "Extracts all RTL concurrent and immediate assertions, assumptions, and covers into `<embedded>`.",
    },
    CmdDoc {
        name: "clock",
        flavor: DocFlavor::Jg,
        category: Category::DesignSetup,
        summary: "Declare primary or derived design clocks (or forward Tcl's built-in `clock`)",
        usage: "clock <sig> [-factor N] [-phase P] [-both_edges] | clock -clear | clock -none | clock -infer",
        options: &[
            opt("-factor <N>", "Clock period multiplier relative to the primary clock"),
            opt("-phase <P>", "Clock phase offset"),
            opt("-both_edges", "Accepted for compatibility (both edges handled by netlist)"),
            opt("-clear", "Clear declared clocks"),
            opt("-none", "Mark design as combinational (no clock)"),
            opt("-infer", "Auto-infer clocks from elaborated sequential elements"),
        ],
        ignored_options: &[],
        equivalent: Some("VCF: `create_clock`"),
        example: "clock clk",
        notes: "Tcl's standard `clock seconds` / `clock format` subcommands are automatically forwarded to `::informal::tcl_clock`.",
    },
    CmdDoc {
        name: "reset",
        flavor: DocFlavor::Jg,
        category: Category::DesignSetup,
        summary: "Declare the formal reset condition held in cycle 0 and released thereafter",
        usage: "reset -expression {<expr>} | reset <expr> | reset -none | reset -analyze_only",
        options: &[
            opt("-expression / -expr <expr>", "Active-high reset predicate (e.g., `{!rst_n}` for active-low reset)"),
            opt("-none", "Run without an explicit reset constraint"),
            opt("-analyze_only", "Report reset structure without changing the session reset"),
            opt("-non_resettable_regs", "List registers without reset drivers"),
        ],
        ignored_options: &[],
        equivalent: Some("VCF: `create_reset`"),
        example: "reset -expression {!rst_n}",
        notes: "Expression identifiers are checked against the elaborated top module.",
    },
    CmdDoc {
        name: "assert",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Add, enable, disable, or remove SVA assertions in a task",
        usage: "assert [-name N] [-task T] [-bound K] [-env] {<sva_expr>} | assert (-disable|-enable|-remove) [-task T] [-regexp] <pattern ...>",
        options: &[
            opt("-name <N>", "Property name (auto-generated if omitted)"),
            opt("-task <T>", "Target task (default current task)"),
            opt("-bound <K>", "Cap proof depth at `K` cycles (reports `bounded_proven` if unviolated)"),
            opt("-env", "Treat this property as an environment `assume` instead of an assertion"),
            opt("-disable / -enable / -remove", "Toggle or delete matching properties (glob or `-regexp`)"),
            opt("-regexp", "Interpret selection pattern as a regular expression"),
        ],
        ignored_options: &["-severity", "-tag", "-from_property"],
        equivalent: Some("VCF: `fvassert`, `fvdisable`, `fvenable`"),
        example: "assert -name a_max {q <= 4'd9}\nassert -disable {a_debug_*}",
        notes: "Identifiers are validated against top-module declarations immediately; unsupported SVA operators report cleanly on `prove`.",
    },
    CmdDoc {
        name: "assume",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Add environment assumptions, promote helper assertions, or toggle/remove assumptions",
        usage: "assume [-name N] [-task T] {<sva_expr>} | assume -from_assert <pat> [-name N] [-task T] [-regexp] | assume (-disable|-enable|-remove) [-task T] [-regexp] <pattern ...>",
        options: &[
            opt("-name <N>", "Assumption name"),
            opt("-task <T>", "Target task (default current task)"),
            opt("-from_assert <pat>", "Promote helper assertion(s) into assumptions with provenance tracking"),
            opt("-disable / -enable / -remove", "Toggle or delete matching assumptions (including RTL assumes per task)"),
            opt("-regexp", "Interpret pattern as a regular expression"),
        ],
        ignored_options: &["-severity", "-tag", "-from_property"],
        equivalent: Some("VCF: `fvassume`"),
        example: "assume -name m_no_ovf {!(full && push)}\nassume -from_assert a_helper_inv",
        notes: "`-from_assert` never circularly assumes assertion P while proving P itself.",
    },
    CmdDoc {
        name: "cover",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Add, enable, disable, or remove SVA reachability cover properties",
        usage: "cover [-name N] [-task T] [-bound K] {<sva_expr>} | cover (-disable|-enable|-remove) [-task T] [-regexp] <pattern ...>",
        options: &[
            opt("-name <N>", "Cover property name"),
            opt("-task <T>", "Target task (default current task)"),
            opt("-bound <K>", "Cap search depth at `K` cycles"),
            opt("-disable / -enable / -remove", "Toggle or delete matching covers"),
            opt("-regexp", "Interpret selection pattern as a regular expression"),
        ],
        ignored_options: &["-severity", "-tag", "-from_property"],
        equivalent: Some("VCF: `fvcover`"),
        example: "cover -name c_full {full ##1 pop}",
        notes: "A witness trace proves reachability (`covered`); an unbounded safety proof means `unreachable`.",
    },
    CmdDoc {
        name: "prove",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Run the engine portfolio on properties or tasks and print a verdict table",
        usage: "prove [-all] [-task T] [-property {P ...}] [-effort low|standard|high|max] [-orchestration auto|on|off] [-time_limit D] [-total_time_limit D] [-bound K] [-with_proof_core]",
        options: &[
            opt("-all", "Prove all enabled properties in the current task"),
            opt("-task <T>", "Prove all enabled properties in task `T`"),
            opt("-property <list>", "Prove only the named property or properties"),
            opt("-effort <level>", "Override proof effort (`low`/`quick`, `standard`, `high`/`deep`, `max`/`signoff`)"),
            opt("-orchestration <mode>", "Override two-pass sweep/converge scheduling (`auto`, `on`, `off`)"),
            opt("-time_limit / -per_property_time_limit <D>", "Override per-property timeout (e.g. `30s`, `5m`, `1h`)"),
            opt("-total_time_limit <D>", "Cap whole-batch wall-clock time across all targets (`0`/`off` disables)"),
            opt("-bound <K>", "Bound BMC/safety proofs to `K` cycles (`bounded_proven`)"),
            opt("-with_proof_core", "Extract minimal needed assumptions and state support for proven assertions"),
            opt("-engine_mode <modes>", "Temporarily map Jasper engine letters (`Ht`, `Hp`, `B`, `N`)"),
        ],
        ignored_options: &["-bg", "-dump_trace", "-sst"],
        equivalent: Some("VCF: `check_fv`"),
        example: "prove -all\nprove -task Safety -effort high -time_limit 60s -with_proof_core",
        notes: "Runs environment consistency check first (when vacuity is on), sweeps multi-property tasks at `low` effort before converging undetermined targets, and runs precondition/witness covers after proven assertions.",
    },
    CmdDoc {
        name: "get_property_list",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Query property names filtered by type, status, task, name, or disabled flag",
        usage: "get_property_list [-task T] [-include {k v ...}] [-exclude {k v ...}] [-regexp]",
        options: &[
            opt("-task <T>", "Restrict to properties in task `T`"),
            opt("-include <pairs>", "Key/value filter pairs (`type`, `status`, `name`, `disabled`, `task`)"),
            opt("-exclude <pairs>", "Key/value exclusion pairs"),
            opt("-regexp", "Treat `name` filter values as regular expressions"),
        ],
        ignored_options: &[],
        equivalent: Some("VCF: `get_props`"),
        example: "get_property_list -include {type assert status cex}",
        notes: "Returns a Tcl list of matching property names.",
    },
    CmdDoc {
        name: "get_status",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Return the verification status string of a single property",
        usage: "get_status <property_name>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set st [get_status a_max]",
        notes: "Returns one of `proven`, `bounded_proven`, `cex`, `covered`, `unreachable`, `undetermined`, `error`, or `unprocessed`.",
    },
    CmdDoc {
        name: "set_prove_time_limit",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set the session default per-property proof time limit",
        usage: "set_prove_time_limit <duration>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("VCF: `set_fml_var fml_max_time`"),
        example: "set_prove_time_limit 5m",
        notes: "Accepts durations with `ms`, `s`, `m`, `h`, or `d` suffixes.",
    },
    CmdDoc {
        name: "set_prove_per_property_time_limit",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set the per-property proof time limit (alias for `set_prove_time_limit`)",
        usage: "set_prove_per_property_time_limit <duration>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("VCF: `set_fml_var fml_max_time`"),
        example: "set_prove_per_property_time_limit 30s",
        notes: "",
    },
    CmdDoc {
        name: "set_prove_total_time_limit",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set or query the whole-batch wall-clock time limit across a `prove` run",
        usage: "set_prove_total_time_limit [<duration|0|off>]",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_prove_total_time_limit 15m",
        notes: "Pass `0` or `off` to clear the batch cap; call with no arguments to query the current setting.",
    },
    CmdDoc {
        name: "set_prove_effort",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set or query the session engine proof effort level (`low`, `standard`, `high`, `max`)",
        usage: "set_prove_effort [<low|standard|high|max>]",
        options: &[
            opt("low / quick", "Skip `scorr` & simulation mining; run lightweight 1-step BMC/k-ind/IC3 subset"),
            opt("standard / medium", "Default balanced portfolio and `scorr` budget"),
            opt("high / deep", "Enable simulation + Houdini 2-literal mining, deeper CTG (`8`/`3`), derived predicates, and IC3 slice restarts"),
            opt("max / signoff", "`high` plus 6-worker diverse `Ic3Mt` pool and automatic witness cover + minimal proof-core signoff"),
        ],
        ignored_options: &[],
        equivalent: Some("VCF: `set_fml_var fml_effort`"),
        example: "set_prove_effort high",
        notes: "Call with no arguments to return the current effort level.",
    },
    CmdDoc {
        name: "set_prove_orchestration",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set or query two-pass proof orchestration (`auto`, `on`, `off`)",
        usage: "set_prove_orchestration [<auto|on|off>]",
        options: &[
            opt("auto", "Enable two-pass sweep/converge automatically on multi-property runs (default)"),
            opt("on", "Always run a fast `low`-effort sweep pass before full-effort convergence"),
            opt("off", "Run every property in a single pass at the configured effort"),
        ],
        ignored_options: &[],
        equivalent: None,
        example: "set_prove_orchestration auto",
        notes: "Pass 1 sweeps targets for `min(2s, max(250ms, time_limit / 10))` at `low` effort so easy properties settle immediately before Pass 2 focuses full effort on remaining undetermined targets.",
    },
    CmdDoc {
        name: "set_engine_mode",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Map JasperGold engine mode letters (`Ht`, `Hp`, `B`, `N`, ...) to InFormal engines",
        usage: "set_engine_mode {<mode> ...}",
        options: &[],
        ignored_options: &[],
        equivalent: Some("Common: `set_engines`"),
        example: "set_engine_mode {Ht B}",
        notes: "Maps `Ht`/`Hp` -> IC3/PDR, `B` -> BMC, `N` -> portfolio and prints the selected engines.",
    },
    CmdDoc {
        name: "report",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Print or export property tables, CSV summaries, design info, or per-property details",
        usage: "report [-task T] [-summary|-results|-detail|-design] [-property P] [-csv] [-file F [-force]]",
        options: &[
            opt("-task <T>", "Restrict report to task `T`"),
            opt("-summary / -results", "Print property verdict table (default)"),
            opt("-detail", "Print detailed per-property engine, depth, vacuity, and memory metrics"),
            opt("-design", "Print design structure and lowered model sizes (`get_design_info`)"),
            opt("-property <P>", "Limit `-detail` output to one property"),
            opt("-csv", "Format table as CSV"),
            opt("-file <F>", "Write output to file (`-force` overwrites existing file)"),
        ],
        ignored_options: &[],
        equivalent: Some("VCF: `report_fv`"),
        example: "report -csv -file results.csv -force",
        notes: "",
    },
    CmdDoc {
        name: "get_design_info",
        flavor: DocFlavor::Jg,
        category: Category::DebugAndComplexity,
        summary: "Inspect elaborated design counts (flops, latches, memories, clocks) and model sizes",
        usage: "get_design_info [-json] [-list instance|memory|clock|reset [-filter P [-regexp]]]",
        options: &[
            opt("-json", "Return structured JSON design metrics"),
            opt("-list <kind>", "Return Tcl list of `instance`, `memory`, `clock`, or `reset` names"),
            opt("-filter <pat>", "Filter `-list` output by glob (or `-regexp`)"),
            opt("-regexp", "Interpret `-filter` pattern as regular expression"),
        ],
        ignored_options: &["-silent", "-verbosity"],
        equivalent: Some("JG: `report -design`"),
        example: "get_design_info\nget_design_info -list memory",
        notes: "Reports exact counts from `svlang` netlist plus the current task's Btor2 and AIG sizes.",
    },
    CmdDoc {
        name: "get_property_info",
        flavor: DocFlavor::Jg,
        category: Category::DebugAndComplexity,
        summary: "Inspect one property's status, engine stats, COI cone, or minimal proof core",
        usage: "get_property_info <property> [-task T] [-json] [-cone] [-proof_core]",
        options: &[
            opt("-task <T>", "Look up property in task `T`"),
            opt("-json", "Return structured JSON"),
            opt("-cone", "Include inputs and registers in the property's cone of influence"),
            opt("-proof_core", "Include minimal needed assumptions/state support (requires `prove -with_proof_core`)"),
        ],
        ignored_options: &[],
        equivalent: None,
        example: "get_property_info a_max -cone",
        notes: "",
    },
    CmdDoc {
        name: "get_stats",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Return per-property proof statistics (engine, wall/CPU time, depth, peak memory) as JSON",
        usage: "get_stats [-task T]",
        options: &[opt("-task <T>", "Restrict statistics to task `T`")],
        ignored_options: &[],
        equivalent: None,
        example: "puts [get_stats]",
        notes: "",
    },
    CmdDoc {
        name: "stopat",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Cut signals from their RTL drivers so they become unconstrained free inputs in a task",
        usage: "stopat [-task T] <sig ...> | stopat -clear [-task T] [sig ...] | stopat -list [-task T]",
        options: &[
            opt("-task <T>", "Apply cut points to task `T` (default current task)"),
            opt("-clear", "Remove listed cut points, or all cut points in the task"),
            opt("-list", "Return active cut points as a Tcl list"),
        ],
        ignored_options: &["-env"],
        equivalent: Some("VCF: `snip_driver`"),
        example: "stopat -task FreeFsm u_alu.mul_res",
        notes: "Cut points are task-scoped: cutting a signal in task `T` leaves it driven in `<embedded>` and other tasks.",
    },
    CmdDoc {
        name: "task",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Create, edit, switch, list, or delete isolated formal verification tasks",
        usage: "task [-create T|-edit T|-set T|-delete T|-list] [-source_task S] [-copy {pat}] [-copy_asserts] [-copy_assumes] [-copy_covers] [-copy_stopats] [-copy_all] [-regexp]",
        options: &[
            opt("-create <T>", "Create task `T` (optionally with `-set` and copy flags)"),
            opt("-edit <T>", "Copy additional properties or cut points into existing task `T`"),
            opt("-set <T>", "Make `T` the current active task"),
            opt("-delete <T>", "Delete task `T` (cannot delete `<embedded>`)"),
            opt("-list", "Return Tcl list of task names (`<embedded>` first)"),
            opt("-source_task <S>", "Copy source task (default current task)"),
            opt("-copy <pats>", "Copy properties matching glob(s) or `-regexp`"),
            opt("-copy_asserts / -copy_assumes / -copy_covers", "Copy all properties of the specified kind"),
            opt("-copy_stopats / -copy_all", "Copy cut points, or copy all properties and cut points"),
        ],
        ignored_options: &["-copy_ratings", "-copy_abstractions", "-copy_related_covers", "-copy_proof_settings"],
        equivalent: Some("JG: `get_task_list`"),
        example: "task -create FastPath -copy {a_fast_*} -copy_assumes -set",
        notes: "Call `task` with no arguments to return the current task name. Each task is lowered and proved independently.",
    },
    CmdDoc {
        name: "get_task_list",
        flavor: DocFlavor::Jg,
        category: Category::PropertiesAndTasks,
        summary: "Return the Tcl list of all tasks in the session",
        usage: "get_task_list",
        options: &[],
        ignored_options: &[],
        equivalent: Some("JG: `task -list`"),
        example: "get_task_list",
        notes: "",
    },
    CmdDoc {
        name: "visualize",
        flavor: DocFlavor::Jg,
        category: Category::DebugAndComplexity,
        summary: "Return the VCD trace path for a CEX or cover and optionally launch `$INFORMAL_WAVE_VIEWER`",
        usage: "visualize (-violation|-cover) -property <P> [-task T]",
        options: &[
            opt("-violation", "Open the counterexample trace for a failing assertion"),
            opt("-cover", "Open the witness trace for a covered property"),
            opt("-property <P>", "Property name"),
            opt("-task <T>", "Task holding the property"),
        ],
        ignored_options: &["-bg", "-window", "-new_window", "-min_length", "-max_length"],
        equivalent: None,
        example: "visualize -violation -property a_never9",
        notes: "If `$INFORMAL_WAVE_VIEWER` (e.g., `surfer` or `gtkwave`) is set, spawns the viewer on the VCD file.",
    },
    CmdDoc {
        name: "include",
        flavor: DocFlavor::Jg,
        category: Category::Utilities,
        summary: "Source another Tcl file in the global scope (JasperGold alias for `source`)",
        usage: "include <file.tcl>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("Standard Tcl: `source`"),
        example: "include common_env.tcl",
        notes: "Followed recursively by `informal shell --check`.",
    },
    CmdDoc {
        name: "set_proofgrid_per_engine_max_jobs",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Vendor proof-grid tuning command (accepted and ignored with a note)",
        usage: "set_proofgrid_per_engine_max_jobs <N>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_proofgrid_per_engine_max_jobs 4",
        notes: "Accepted so existing JasperGold setup scripts run unmodified.",
    },
    CmdDoc {
        name: "set_proofgrid_per_engine_max_local_jobs",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Vendor proof-grid tuning command (accepted and ignored with a note)",
        usage: "set_proofgrid_per_engine_max_local_jobs <N>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_proofgrid_per_engine_max_local_jobs 4",
        notes: "Accepted so existing JasperGold setup scripts run unmodified.",
    },
    CmdDoc {
        name: "set_proofgrid_max_local_jobs",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Vendor proof-grid tuning command (accepted and ignored with a note)",
        usage: "set_proofgrid_max_local_jobs <N>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_proofgrid_max_local_jobs 8",
        notes: "Accepted so existing JasperGold setup scripts run unmodified.",
    },
    CmdDoc {
        name: "set_proofgrid_max_jobs",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Vendor proof-grid tuning command (accepted and ignored with a note)",
        usage: "set_proofgrid_max_jobs <N>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_proofgrid_max_jobs 8",
        notes: "Accepted so existing JasperGold setup scripts run unmodified.",
    },
    CmdDoc {
        name: "set_proofgrid_mode",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Vendor proof-grid mode command (accepted and ignored with a note)",
        usage: "set_proofgrid_mode <mode>",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_proofgrid_mode local",
        notes: "Accepted so existing JasperGold setup scripts run unmodified.",
    },
    CmdDoc {
        name: "set_max_trace_length",
        flavor: DocFlavor::Jg,
        category: Category::ProvingAndEngines,
        summary: "Set or query the persistent BMC unrolling bound (`0`/`off` clears)",
        usage: "set_max_trace_length [<N|0|off>]",
        options: &[],
        ignored_options: &[],
        equivalent: Some("VCF: `set_fml_var fml_max_depth`"),
        example: "set_max_trace_length 200",
        notes: "Caps `informal-bmc` / `informal-wbmc` depth while still allowing inductive engines (`kind`, `ic3`, `wkind`) to prove unboundedly (`proven`, `∞`). Assertions cleared cleanly to bound `N` without an unbounded proof report `bounded_proven`.",
    },
    // ============================== VCF =====================================
    CmdDoc {
        name: "set_blackbox",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Mark modules or cell instances as black boxes before elaboration",
        usage: "set_blackbox [-designs {M ...}] [-cells {path ...}] [-clear]",
        options: &[
            opt("-designs <mods>", "Black-box module definitions"),
            opt("-cells <paths>", "Black-box specific instance paths"),
            opt("-clear", "Clear existing black boxes"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `elaborate -bbox_m / -bbox_i`"),
        example: "set_blackbox -designs {sram_1rw}",
        notes: "Takes effect at the next `elaborate` or `read_file`.",
    },
    CmdDoc {
        name: "set_fml_appmode",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Select VC Formal application mode (`FPV`)",
        usage: "set_fml_appmode [FPV]",
        options: &[],
        ignored_options: &[],
        equivalent: None,
        example: "set_fml_appmode FPV",
        notes: "Currently accepts `FPV`; for Formal Apps (`check_cov`, `check_csr`, etc.) call the `check_*` commands directly.",
    },
    CmdDoc {
        name: "set_fml_var",
        flavor: DocFlavor::Vcf,
        category: Category::ProvingAndEngines,
        summary: "Set VC Formal session variable (`fml_max_time`, `fml_effort`, `fml_max_depth`)",
        usage: "set_fml_var <name> <value>",
        options: &[
            opt("fml_max_time", "Per-property time limit (e.g. `1H`, `300s`)"),
            opt("fml_effort", "Engine proof effort (`low`, `standard`, `high`, `max`)"),
            opt("fml_max_depth", "Persistent BMC trace bound (`N`, or `0`/`off` to clear)"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `set_prove_time_limit` / `set_prove_effort` / `set_max_trace_length`"),
        example: "set_fml_var fml_max_time 10m\nset_fml_var fml_effort high",
        notes: "Unknown vendor tuning variables are accepted and ignored with a note.",
    },
    CmdDoc {
        name: "read_file",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Analyze and elaborate SystemVerilog sources in one command",
        usage: "read_file -top <M> [-format sverilog] [-sva] [-vcs {args}] [-define {K=V}] [-parameters {P=V}]",
        options: &[
            opt("-top <module>", "Top module to elaborate"),
            opt("-format <fmt>", "Source language (`sverilog` or `verilog`)"),
            opt("-sva", "Enable SystemVerilog Assertions"),
            opt("-vcs <words>", "VCS-style filelist, files, `+define+`, and `+incdir+` arguments"),
            opt("-define <defs>", "Macro definitions"),
            opt("-parameters <params>", "Top-level parameter overrides"),
        ],
        ignored_options: &["-cov", "-inject_fault", "-work"],
        equivalent: Some("JG: `analyze` + `elaborate`"),
        example: "read_file -top counter -format sverilog -sva -vcs {counter.sv}",
        notes: "",
    },
    CmdDoc {
        name: "analyze",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Analyze SystemVerilog source files in VC Formal style",
        usage: "analyze [-format sverilog] [-vcs {args}] [-define {K=V}] [file ...]",
        options: &[
            opt("-format <fmt>", "Source language (`sverilog` or `verilog`)"),
            opt("-vcs <words>", "VCS-style compile flags and files"),
            opt("-define <defs>", "Preprocessor definitions"),
        ],
        ignored_options: &["-library", "-work"],
        equivalent: Some("JG: `analyze -sv12`"),
        example: "analyze -format sverilog -vcs {+incdir+rtl rtl/counter.sv}",
        notes: "",
    },
    CmdDoc {
        name: "elaborate",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Elaborate the analyzed top module in VC Formal style",
        usage: "elaborate [<top>] [-sva] [-parameters {P=V}] [-vcs {args}]",
        options: &[
            opt("-sva", "Enable SystemVerilog Assertions"),
            opt("-parameters <params>", "Parameter overrides"),
            opt("-vcs <words>", "Additional VCS flags (including `-pvalue+P=V`)"),
        ],
        ignored_options: &["-cov", "-library", "-work"],
        equivalent: Some("JG: `elaborate -top`"),
        example: "elaborate counter -sva",
        notes: "",
    },
    CmdDoc {
        name: "create_clock",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Declare a design clock signal in VC Formal style",
        usage: "create_clock <sig> [-period N] [-name N] [-initial V]",
        options: &[
            opt("-period <N>", "Clock period (used for relative clock ratios)"),
            opt("-name <N>", "Optional clock label"),
        ],
        ignored_options: &["-initial"],
        equivalent: Some("JG: `clock`"),
        example: "create_clock clk -period 100",
        notes: "",
    },
    CmdDoc {
        name: "snip_driver",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Cut signals from their RTL drivers (VC Formal equivalent of `stopat`)",
        usage: "snip_driver <sig ...>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("JG: `stopat`"),
        example: "snip_driver u_core.stall",
        notes: "",
    },
    CmdDoc {
        name: "create_reset",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Declare the active-high or active-low formal reset signal",
        usage: "create_reset <sig> (-sense high|low | -high | -low) [-name N]",
        options: &[
            opt("-sense high|low", "Reset polarity (`high` -> `sig`, `low` -> `!sig`)"),
            opt("-high / -low", "Shorthand polarity flags"),
            opt("-name <N>", "Optional reset label"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `reset -expression`"),
        example: "create_reset rst_n -sense low",
        notes: "",
    },
    CmdDoc {
        name: "sim_run",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Accept VC Formal reset simulation directive (reset is modeled natively)",
        usage: "sim_run [-stable]",
        options: &[],
        ignored_options: &["-stable"],
        equivalent: Some("Handled automatically by `reset`"),
        example: "sim_run -stable",
        notes: "Accepted so VC Formal scripts run unmodified.",
    },
    CmdDoc {
        name: "sim_save_reset",
        flavor: DocFlavor::Vcf,
        category: Category::DesignSetup,
        summary: "Accept VC Formal save-reset directive (reset is modeled natively)",
        usage: "sim_save_reset",
        options: &[],
        ignored_options: &[],
        equivalent: Some("Handled automatically by `reset`"),
        example: "sim_save_reset",
        notes: "Accepted so VC Formal scripts run unmodified.",
    },
    CmdDoc {
        name: "fvassert",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Add an SVA assertion in VC Formal style",
        usage: "fvassert <name> -expr {<sva_expr>}",
        options: &[opt("-expr <sva>", "SystemVerilog assertion expression")],
        ignored_options: &["-type", "-usage"],
        equivalent: Some("JG: `assert -name`"),
        example: "fvassert a_max -expr {q <= 4'd9}",
        notes: "",
    },
    CmdDoc {
        name: "fvassume",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Add an SVA assumption or promote a helper assertion in VC Formal style",
        usage: "fvassume <name> -expr {<sva_expr>} | fvassume -from_assert <pat> [-name N]",
        options: &[
            opt("-expr <sva>", "SystemVerilog assumption expression"),
            opt("-from_assert <pat>", "Promote helper assertion(s) into assumptions"),
            opt("-name <N>", "Optional name override when promoting"),
        ],
        ignored_options: &["-type", "-usage"],
        equivalent: Some("JG: `assume`"),
        example: "fvassume m_en -expr {en == 1'b1}",
        notes: "",
    },
    CmdDoc {
        name: "fvcover",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Add an SVA cover property in VC Formal style",
        usage: "fvcover <name> -expr {<sva_expr>}",
        options: &[opt("-expr <sva>", "SystemVerilog cover sequence/expression")],
        ignored_options: &["-type", "-usage"],
        equivalent: Some("JG: `cover -name`"),
        example: "fvcover c_done -expr {done}",
        notes: "",
    },
    CmdDoc {
        name: "fvdisable",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Disable properties matching a glob pattern",
        usage: "fvdisable <pattern ...>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("JG: `assert -disable`"),
        example: "fvdisable a_slow_*",
        notes: "",
    },
    CmdDoc {
        name: "fvenable",
        flavor: DocFlavor::Vcf,
        category: Category::PropertiesAndTasks,
        summary: "Re-enable previously disabled properties matching a glob pattern",
        usage: "fvenable <pattern ...>",
        options: &[],
        ignored_options: &[],
        equivalent: Some("JG: `assert -enable`"),
        example: "fvenable a_slow_*",
        notes: "",
    },
    CmdDoc {
        name: "check_fv",
        flavor: DocFlavor::Vcf,
        category: Category::ProvingAndEngines,
        summary: "Run the engine portfolio on VC Formal properties",
        usage: "check_fv [-property {P ...}] [-effort low|standard|high|max] [-time_limit D]",
        options: &[
            opt("-property <list>", "Prove only the specified property names"),
            opt("-effort <level>", "Override proof effort for this run (`low`, `standard`, `high`, `max`)"),
            opt("-time_limit <D>", "Override per-property timeout for this run"),
        ],
        ignored_options: &["-block", "-break", "-run_finish"],
        equivalent: Some("JG: `prove`"),
        example: "check_fv -effort high -time_limit 60s",
        notes: "",
    },
    CmdDoc {
        name: "report_fv",
        flavor: DocFlavor::Vcf,
        category: Category::ProvingAndEngines,
        summary: "Print verification results table in VC Formal style",
        usage: "report_fv [-list] [-verbose] [-property P]",
        options: &[
            opt("-list", "Print property table"),
            opt("-verbose", "Print detailed engine and vacuity statistics"),
            opt("-property <P>", "Restrict verbose output to one property"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `report`"),
        example: "report_fv -list",
        notes: "",
    },
    CmdDoc {
        name: "get_props",
        flavor: DocFlavor::Vcf,
        category: Category::ProvingAndEngines,
        summary: "Return Tcl list of properties filtered by type and status",
        usage: "get_props [-type assert|assume|cover] [-status S]",
        options: &[
            opt("-type <kind>", "Filter by `assert`, `assume`, or `cover`"),
            opt("-status <status>", "Filter by proof status (e.g. `proven`, `cex`, `covered`)"),
        ],
        ignored_options: &[],
        equivalent: Some("JG: `get_property_list`"),
        example: "get_props -type assert -status proven",
        notes: "",
    },
];
