//! IEEE 1801 UPF (1.0–4.0) Tcl 8.6 front end using `informal-tcl` with
//! Decision D13 fail-closed option checking and re-entrancy-safe `load_upf`.

pub mod cmds_domain_pst;
pub mod cmds_iso_ls;
pub mod cmds_ret_psw_port;
pub mod cmds_supply;
pub mod eval;
pub mod finder;
pub mod finder_api;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use informal_tcl::{Interp, Value};

use crate::cpf::{finalize_cpf, finalize_cpf_ex, register_cpf_commands};
use crate::ir::{normalize_scope, PowerIntent, UpfLoc, UpfVersion};

pub use eval::UpfError;
pub(crate) use eval::{
    loc_err, record_d13_error, resolve_scope_arg, ScopeFrame, UpfArgs, UpfEvalState,
};
pub use finder_api::{NullObjectFinder, ObjectFinder, ObjectType};

/// Specification of one UPF or CPF file to evaluate sequentially inside a shared session.
#[derive(Debug, Clone)]
pub struct UpfFileSpec {
    pub path: PathBuf,
    pub scope: Option<String>,
    pub version: Option<UpfVersion>,
    pub is_cpf: bool,
}

/// Evaluates one or more UPF/CPF files sequentially in a shared Tcl session.
pub fn load_upf_files(
    specs: &[UpfFileSpec],
    finder: Arc<dyn ObjectFinder>,
) -> Result<PowerIntent, UpfError> {
    if specs.is_empty() {
        return Err(UpfError::Semantic(
            "load_upf: no UPF/CPF files provided".to_string(),
        ));
    }
    let interp = Interp::new();
    let state = Rc::new(RefCell::new(UpfEvalState::new(finder)));
    register_upf_commands(&interp, &state)?;
    register_cpf_commands(&interp, &state)?;

    let mut last_was_cpf = false;
    for (idx, spec) in specs.iter().enumerate() {
        let later_files = idx + 1 < specs.len();
        let canon = std::fs::canonicalize(&spec.path).map_err(|source| UpfError::Io {
            path: spec.path.display().to_string(),
            source,
        })?;
        if idx == 0 {
            {
                let mut st = state.borrow_mut();
                if let Some(ver) = spec.version {
                    st.intent.upf_version = Some(ver);
                }
                if let Some(sc) = spec.scope.as_deref() {
                    if !sc.trim().is_empty() {
                        let norm = normalize_scope(sc, st.intent.hierarchy_separator);
                        if st.finder.has_design()
                            && !norm.is_empty()
                            && !st.finder.scope_exists(&norm)
                        {
                            return Err(UpfError::Semantic(format!(
                                "load_upf: unresolved scope `{norm}` in design"
                            )));
                        }
                        st.current_scope = norm;
                    }
                }
                st.is_cpf_mode = spec.is_cpf;
                st.defer_root_cpf_fallback = spec.is_cpf && later_files;
                st.intent
                    .loaded_files
                    .push(canon.to_string_lossy().into_owned());
            }
            let import_cmd = if spec.is_cpf {
                "namespace eval :: { namespace import -force ::cpf::* }"
            } else {
                "namespace eval :: { namespace import -force ::upf::* }"
            };
            interp.eval(import_cmd).map_err(|e| UpfError::Tcl {
                message: e.message,
                info: e.info,
            })?;
            interp.eval_file(&canon).map_err(|e| UpfError::Tcl {
                message: e.message,
                info: e.info,
            })?;
            let mut st = state.borrow_mut();
            st.defer_root_cpf_fallback = false;
            if st.is_cpf_mode || st.cpf_state.has_pending() {
                let UpfEvalState {
                    intent, cpf_state, ..
                } = &mut *st;
                finalize_cpf_ex(intent, cpf_state, !later_files, None);
            }
            last_was_cpf = st.is_cpf_mode || spec.is_cpf || !st.cpf_state.is_empty();
            st.is_cpf_mode = false;
        } else {
            {
                let mut st = state.borrow_mut();
                let new_scope = match spec.scope.as_deref() {
                    Some(sc) if !sc.trim().is_empty() => {
                        let norm = normalize_scope(sc, st.intent.hierarchy_separator);
                        if st.finder.has_design()
                            && !norm.is_empty()
                            && !st.finder.scope_exists(&norm)
                        {
                            return Err(UpfError::Semantic(format!(
                                "load_upf: unresolved scope `{norm}` in design"
                            )));
                        }
                        norm
                    }
                    _ => String::new(),
                };
                let prev_is_cpf = st.is_cpf_mode;
                let prev_was_cpf = prev_is_cpf || last_was_cpf;
                let prev_cpf_state = std::mem::take(&mut st.cpf_state);
                let scoped_push = !new_scope.is_empty();
                let prev_design_top = st.intent.design_top.clone();
                let design_reset =
                    scoped_push || (prev_design_top.is_some() && new_scope != st.current_scope);
                if prev_was_cpf {
                    st.cpf_state.inherit_from(&prev_cpf_state);
                    if design_reset {
                        st.cpf_state.active_design = None;
                        st.cpf_state.design_stack.clear();
                    }
                }
                if design_reset {
                    st.intent.design_top = None;
                }
                let frame = ScopeFrame {
                    prev_scope: st.current_scope.clone(),
                    prev_version: st.intent.upf_version,
                    prev_is_cpf: prev_was_cpf,
                    prev_cpf_state,
                    prev_hierarchy_separator: st.intent.hierarchy_separator,
                    prev_design_top,
                    scoped_push,
                    explicit_scope: false,
                    defer_cpf_fallback: later_files,
                    is_sequential_sibling: true,
                };
                st.scope_stack.push(frame);
                st.current_scope = new_scope;
                st.is_cpf_mode = spec.is_cpf;
                if let Some(ver) = spec.version {
                    st.intent.upf_version = Some(ver);
                } else if spec.is_cpf != prev_was_cpf
                    || st
                        .intent
                        .upf_version
                        .is_some_and(|v| v.is_cpf() != spec.is_cpf)
                {
                    st.intent.upf_version = None;
                }
                st.intent
                    .loaded_files
                    .push(canon.to_string_lossy().into_owned());
            }
            let import_cmd = if spec.is_cpf {
                "namespace eval :: { namespace import -force ::cpf::* }"
            } else {
                "namespace eval :: { namespace import -force ::upf::* }"
            };
            interp.eval(import_cmd).map_err(|e| UpfError::Tcl {
                message: e.message,
                info: e.info,
            })?;
            interp.eval_file(&canon).map_err(|e| UpfError::Tcl {
                message: e.message,
                info: e.info,
            })?;
            interp
                .eval("::upf::_pop_scope")
                .map_err(|e| UpfError::Tcl {
                    message: e.message,
                    info: e.info,
                })?;
            last_was_cpf = spec.is_cpf || !state.borrow().cpf_state.is_empty();
        }
    }

    let intent = state.borrow().intent.clone();
    Ok(intent)
}

/// Evaluates a UPF file on disk with an [`ObjectFinder`] and optional initial scope/version.
pub fn load_upf_file(
    path: &Path,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<PowerIntent, UpfError> {
    let canon = std::fs::canonicalize(path).map_err(|source| UpfError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let interp = Interp::new();
    let state = Rc::new(RefCell::new(UpfEvalState::new(finder)));
    register_upf_commands(&interp, &state)?;
    register_cpf_commands(&interp, &state)?;
    interp
        .eval("namespace eval :: { namespace import -force ::upf::* }")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    {
        let mut st = state.borrow_mut();
        if let Some(ver) = version {
            st.intent.upf_version = Some(ver);
        }
        if let Some(sc) = scope {
            let norm = normalize_scope(sc, st.intent.hierarchy_separator);
            if st.finder.has_design() && !norm.is_empty() && !st.finder.scope_exists(&norm) {
                return Err(UpfError::Semantic(format!(
                    "load_upf: unresolved scope `{norm}` in design"
                )));
            }
            st.current_scope = norm;
        }
        st.intent
            .loaded_files
            .push(canon.to_string_lossy().into_owned());
    }

    interp.eval_file(&canon).map_err(|e| UpfError::Tcl {
        message: e.message,
        info: e.info,
    })?;

    {
        let mut st = state.borrow_mut();
        if st.is_cpf_mode || st.cpf_state.has_pending() {
            let UpfEvalState {
                intent, cpf_state, ..
            } = &mut *st;
            finalize_cpf(intent, cpf_state);
        }
    }
    let intent = state.borrow().intent.clone();
    Ok(intent)
}

/// Evaluates an inline UPF Tcl script string (useful for unit tests and embedded scripts).
pub fn parse_upf_str(
    script: &str,
    scope: Option<&str>,
    version: Option<UpfVersion>,
    finder: Arc<dyn ObjectFinder>,
) -> Result<PowerIntent, UpfError> {
    let interp = Interp::new();
    let state = Rc::new(RefCell::new(UpfEvalState::new(finder)));
    register_upf_commands(&interp, &state)?;
    register_cpf_commands(&interp, &state)?;
    interp
        .eval("namespace eval :: { namespace import -force ::upf::* }")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    {
        let mut st = state.borrow_mut();
        if let Some(ver) = version {
            st.intent.upf_version = Some(ver);
        }
        if let Some(sc) = scope {
            let norm = normalize_scope(sc, st.intent.hierarchy_separator);
            if st.finder.has_design() && !norm.is_empty() && !st.finder.scope_exists(&norm) {
                return Err(UpfError::Semantic(format!(
                    "load_upf: unresolved scope `{norm}` in design"
                )));
            }
            st.current_scope = norm;
        }
    }

    interp.eval(script).map_err(|e| UpfError::Tcl {
        message: e.message,
        info: e.info,
    })?;

    {
        let mut st = state.borrow_mut();
        if st.is_cpf_mode || st.cpf_state.has_pending() {
            let UpfEvalState {
                intent, cpf_state, ..
            } = &mut *st;
            finalize_cpf(intent, cpf_state);
        }
    }
    let intent = state.borrow().intent.clone();
    Ok(intent)
}

pub(crate) fn register_upf_commands(
    interp: &Interp,
    state: &Rc<RefCell<UpfEvalState>>,
) -> Result<(), UpfError> {
    interp
        .eval("namespace eval ::upf {}")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;
    // Allow unbraced HDL bit/part-selects (e.g. `iso_en[0]`, `bus[7:0]`) in UPF/CPF
    // commands while preserving standard Tcl error reporting for unknown commands.
    interp
        .eval(
            r#"
            proc ::unknown {args} {
                if {[llength $args] == 1} {
                    set cmd [lindex $args 0]
                    if {[regexp {^[0-9]+(:[0-9]+)?$} $cmd]} {
                        return "\[$cmd\]"
                    }
                }
                return -code error "invalid command name \"[lindex $args 0]\""
            }
            "#,
        )
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    {
        let st = state.clone();
        interp.register(
            "::upf::_push_scope",
            Box::new(move |a| {
                if a.len() != 5 {
                    return Err("internal error: _push_scope expects 4 args".to_string());
                }
                let scope_arg = &a[1];
                let version_arg = &a[2];
                let format_arg = &a[3];
                let file_arg = &a[4];
                let mut s = st.borrow_mut();

                let new_cpf = match format_arg.to_ascii_lowercase().as_str() {
                    "" | "upf" => false,
                    "cpf" => true,
                    other => return Err(format!("load_upf: unsupported -format `{other}`")),
                };

                let new_ver = if version_arg.is_empty() {
                    if new_cpf != s.is_cpf_mode
                        || s.intent.upf_version.is_some_and(|v| v.is_cpf() != new_cpf)
                    {
                        None
                    } else {
                        s.intent.upf_version
                    }
                } else if new_cpf {
                    Some(UpfVersion::parse_cpf(version_arg)?)
                } else {
                    Some(UpfVersion::parse_upf(version_arg)?)
                };

                let new_scope = if scope_arg.is_empty() {
                    s.current_scope.clone()
                } else {
                    resolve_scope_arg(&s.current_scope, scope_arg, s.intent.hierarchy_separator)
                };
                if s.finder.has_design()
                    && !new_scope.is_empty()
                    && !s.finder.scope_exists(&new_scope)
                {
                    return Err(format!(
                        "load_upf: unresolved scope `{new_scope}` in design"
                    ));
                }

                let prev_is_cpf = s.is_cpf_mode;
                let prev_cpf_state = std::mem::take(&mut s.cpf_state);
                let scoped_push = new_scope != s.current_scope;
                let explicit_scope = !scope_arg.is_empty();
                let prev_design_top = s.intent.design_top.clone();
                let design_reset = scoped_push;
                if prev_is_cpf {
                    s.cpf_state
                        .inherit_nested_child_cpf(&prev_cpf_state, !design_reset);
                } else {
                    s.cpf_state.inherit_from(&prev_cpf_state);
                    if design_reset {
                        s.cpf_state.active_design = None;
                        s.cpf_state.design_stack.clear();
                    }
                }
                s.cpf_state.ancestor_direct_nominal_conditions = prev_cpf_state
                    .ancestor_direct_nominal_conditions
                    .union(&prev_cpf_state.direct_nominal_conditions)
                    .cloned()
                    .collect();
                if design_reset {
                    s.intent.design_top = None;
                }
                let frame = ScopeFrame {
                    prev_scope: s.current_scope.clone(),
                    prev_version: s.intent.upf_version,
                    prev_is_cpf,
                    prev_cpf_state,
                    prev_hierarchy_separator: s.intent.hierarchy_separator,
                    prev_design_top,
                    scoped_push,
                    explicit_scope,
                    defer_cpf_fallback: prev_is_cpf,
                    is_sequential_sibling: false,
                };
                s.scope_stack.push(frame);
                s.current_scope = new_scope;
                s.intent.upf_version = new_ver;
                s.is_cpf_mode = new_cpf;
                s.intent.loaded_files.push(file_arg.clone());
                Ok(Value::Str(if new_cpf { "cpf" } else { "upf" }.to_string()))
            }),
        );
    }

    {
        let st = state.clone();
        interp.register(
            "::upf::_pop_scope",
            Box::new(move |_| {
                let mut s = st.borrow_mut();
                let defer_fallback = s.scope_stack.last().is_some_and(|f| f.defer_cpf_fallback);
                let needs_deferred_fallback =
                    !defer_fallback && s.cpf_state.finalized && !s.cpf_state.fallback_applied;
                if s.is_cpf_mode || s.cpf_state.has_pending() || needs_deferred_fallback {
                    let defer_other_scopes = s
                        .scope_stack
                        .last()
                        .filter(|f| !f.is_sequential_sibling)
                        .map(|_| s.current_scope.clone());
                    let UpfEvalState {
                        intent, cpf_state, ..
                    } = &mut *s;
                    finalize_cpf_ex(
                        intent,
                        cpf_state,
                        !defer_fallback,
                        defer_other_scopes.as_deref(),
                    );
                }
                if let Some(mut frame) = s.scope_stack.pop() {
                    let child_no_active_design = s.cpf_state.active_design.is_none();
                    let merge_cpf_state = if frame.is_sequential_sibling {
                        s.is_cpf_mode
                            || frame.prev_is_cpf
                            || frame.defer_cpf_fallback
                            || !s.cpf_state.is_empty()
                    } else {
                        true
                    };
                    if merge_cpf_state {
                        let overwrite_mode = frame.is_sequential_sibling || !frame.prev_is_cpf;
                        let propagate_finalized = frame.is_sequential_sibling || !frame.prev_is_cpf;
                        s.cpf_state
                            .merge_domain_conditions_into(&mut frame.prev_cpf_state);
                        if overwrite_mode || frame.defer_cpf_fallback {
                            s.cpf_state.merge_nominal_conditions_into(
                                &mut frame.prev_cpf_state,
                                frame.is_sequential_sibling,
                                frame.explicit_scope,
                            );
                        }
                        s.cpf_state
                            .merge_power_modes_into(&mut frame.prev_cpf_state);
                        frame.prev_cpf_state.emitted_fallback_states =
                            s.cpf_state.emitted_fallback_states.clone();
                        frame.prev_cpf_state.emitted_mode_states =
                            s.cpf_state.emitted_mode_states.clone();
                        if (s.is_cpf_mode
                            || !frame.is_sequential_sibling
                            || !s.cpf_state.is_empty())
                            && !frame.scoped_push
                        {
                            frame.prev_cpf_state.active_design = s.cpf_state.active_design.clone();
                            frame.prev_cpf_state.design_stack = s.cpf_state.design_stack.clone();
                        }
                        if propagate_finalized {
                            if s.is_cpf_mode
                                || frame.is_sequential_sibling
                                || !s.cpf_state.is_empty()
                            {
                                frame.prev_cpf_state.finalized = s.cpf_state.finalized;
                                frame.prev_cpf_state.fallback_applied =
                                    s.cpf_state.fallback_applied;
                            }
                        } else if s.is_cpf_mode || !s.cpf_state.is_empty() {
                            frame.prev_cpf_state.fallback_applied &= s.cpf_state.fallback_applied;
                            if !s.cpf_state.finalized {
                                frame.prev_cpf_state.finalized = false;
                            }
                        }
                    }
                    s.current_scope = frame.prev_scope;
                    s.intent.upf_version = frame.prev_version;
                    s.is_cpf_mode = frame.prev_is_cpf;
                    s.cpf_state = frame.prev_cpf_state;
                    s.intent.hierarchy_separator = frame.prev_hierarchy_separator;
                    if frame.scoped_push
                        || frame.prev_design_top.is_some()
                        || (!frame.is_sequential_sibling
                            && frame.prev_is_cpf
                            && child_no_active_design)
                    {
                        s.intent.design_top = frame.prev_design_top;
                    }
                }
                Ok(Value::Str(
                    if s.is_cpf_mode { "cpf" } else { "upf" }.to_string(),
                ))
            }),
        );
    }

    {
        let st = state.clone();
        interp.register(
            "::upf::_d13_error",
            Box::new(move |a| {
                let loc = UpfLoc::from_tcl();
                let cmd = a.get(1).map(String::as_str).unwrap_or("load_upf");
                let detail = a.get(2).map(String::as_str).unwrap_or("error");
                let msg = format!("{cmd}: {detail}");
                record_d13_error(&mut st.borrow_mut().intent, &msg, loc.clone());
                Err(loc_err(loc.as_ref(), &msg))
            }),
        );
    }

    // Tcl wrapper `proc ::upf::load_upf`: avoids `Slot::busy` re-entrancy while
    // preserving `[info script]` and `caller_location()` `file:line`.
    interp
        .eval(
            r#"
            proc ::upf::load_upf {args} {
                set file ""
                set scope ""
                set version ""
                set format ""
                set i 0
                while {$i < [llength $args]} {
                    set w [lindex $args $i]
                    switch -- $w {
                        -scope   { incr i; if {$i >= [llength $args]} { ::upf::_d13_error "load_upf" "option -scope needs 1 value(s)" }; set scope [lindex $args $i] }
                        -version { incr i; if {$i >= [llength $args]} { ::upf::_d13_error "load_upf" "option -version needs 1 value(s)" }; set version [lindex $args $i] }
                        -format  { incr i; if {$i >= [llength $args]} { ::upf::_d13_error "load_upf" "option -format needs 1 value(s)" }; set format [lindex $args $i] }
                        default  {
                            if {[string match "-*" $w]} {
                                ::upf::_d13_error "load_upf" "unsupported option $w"
                            }
                            if {$file ne ""} {
                                ::upf::_d13_error "load_upf" "unexpected positional argument $w"
                            }
                            set file $w
                        }
                    }
                    incr i
                }
                if {$file eq ""} { ::upf::_d13_error "load_upf" "missing file path" }
                set resolved $file
                if {[file pathtype $file] eq "relative"} {
                    set cur [info script]
                    if {$cur ne ""} {
                        set base [file dirname $cur]
                        set cand [file join $base $file]
                        if {[file exists $cand]} {
                            set resolved $cand
                        }
                    }
                }
                if {![file exists $resolved]} { ::upf::_d13_error "load_upf" "file not found: $file" }
                set resolved [file normalize $resolved]
                set mode [::upf::_push_scope $scope $version $format $resolved]
                if {$mode eq "cpf"} {
                    namespace eval :: { namespace import -force ::cpf::* }
                } else {
                    namespace eval :: { namespace import -force ::upf::* }
                }
                set rc [catch {uplevel #0 [list source $resolved]} err opts]
                set prev_mode [::upf::_pop_scope]
                if {$prev_mode eq "upf"} {
                    namespace eval :: { namespace import -force ::upf::* }
                } else {
                    namespace eval :: { namespace import -force ::cpf::* }
                }
                if {$rc != 0} {
                    return -options $opts $err
                }
                return ""
            }
            "#,
        )
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;

    register_cmd(
        interp,
        state,
        "::upf::upf_version",
        cmds_ret_psw_port::cmd_upf_version,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_design_top",
        cmds_ret_psw_port::cmd_set_design_top,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_scope",
        cmds_ret_psw_port::cmd_set_scope,
    );
    register_cmd(
        interp,
        state,
        "::upf::upf_create_implicit_supply_sets",
        cmds_supply::cmd_implicit_supply_sets,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_supply_port",
        cmds_supply::cmd_create_supply_port,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_supply_net",
        cmds_supply::cmd_create_supply_net,
    );
    register_cmd(
        interp,
        state,
        "::upf::connect_supply_net",
        cmds_supply::cmd_connect_supply_net,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_supply_set",
        cmds_supply::cmd_create_supply_set,
    );
    register_cmd(
        interp,
        state,
        "::upf::associate_supply_set",
        cmds_supply::cmd_associate_supply_set,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_domain_supply_net",
        cmds_supply::cmd_set_domain_supply_net,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_power_domain",
        cmds_domain_pst::cmd_create_power_domain,
    );
    register_cmd(
        interp,
        state,
        "::upf::add_power_state",
        cmds_domain_pst::cmd_add_power_state,
    );
    register_cmd(
        interp,
        state,
        "::upf::add_port_state",
        cmds_domain_pst::cmd_add_port_state,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_pst",
        cmds_domain_pst::cmd_create_pst,
    );
    register_cmd(
        interp,
        state,
        "::upf::add_pst_state",
        cmds_domain_pst::cmd_add_pst_state,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_isolation",
        cmds_iso_ls::cmd_set_isolation,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_isolation_control",
        cmds_iso_ls::cmd_set_isolation_control,
    );
    register_cmd(
        interp,
        state,
        "::upf::map_isolation_cell",
        cmds_iso_ls::cmd_map_isolation_cell,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_level_shifter",
        cmds_iso_ls::cmd_set_level_shifter,
    );
    register_cmd(
        interp,
        state,
        "::upf::map_level_shifter_cell",
        cmds_iso_ls::cmd_map_level_shifter_cell,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_retention",
        cmds_ret_psw_port::cmd_set_retention,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_retention_control",
        cmds_ret_psw_port::cmd_set_retention_control,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_retention_elements",
        cmds_ret_psw_port::cmd_set_retention_elements,
    );
    register_cmd(
        interp,
        state,
        "::upf::map_retention_cell",
        cmds_ret_psw_port::cmd_map_retention_cell,
    );
    register_cmd(
        interp,
        state,
        "::upf::create_power_switch",
        cmds_supply::cmd_create_power_switch,
    );
    register_cmd(
        interp,
        state,
        "::upf::map_power_switch",
        cmds_supply::cmd_map_power_switch,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_port_attributes",
        cmds_ret_psw_port::cmd_set_port_attributes,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_repeater",
        cmds_ret_psw_port::cmd_set_repeater,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_equivalent",
        cmds_supply::cmd_set_equivalent,
    );
    register_cmd(
        interp,
        state,
        "::upf::set_design_attributes",
        cmds_ret_psw_port::cmd_set_design_attributes,
    );
    register_cmd(
        interp,
        state,
        "::upf::use_interface_cell",
        cmds_ret_psw_port::cmd_use_interface_cell,
    );
    register_cmd(
        interp,
        state,
        "::upf::find_objects",
        finder::cmd_find_objects,
    );

    interp
        .eval("namespace eval ::upf { namespace export * }")
        .map_err(|e| UpfError::Tcl {
            message: e.message,
            info: e.info,
        })?;
    Ok(())
}

fn register_cmd<F>(interp: &Interp, state: &Rc<RefCell<UpfEvalState>>, name: &str, f: F)
where
    F: Fn(&mut UpfEvalState, &[String], Option<UpfLoc>) -> Result<Value, String> + 'static,
{
    let st = state.clone();
    interp.register(
        name,
        Box::new(move |args| {
            let loc = UpfLoc::from_tcl();
            let mut s = st
                .try_borrow_mut()
                .map_err(|_| "UPF interpreter state busy".to_string())?;
            f(&mut s, args, loc)
        }),
    );
}
