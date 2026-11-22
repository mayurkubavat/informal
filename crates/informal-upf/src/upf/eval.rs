//! Decision D13 argument parser, scope stack, and evaluation state for UPF/CPF Tcl front ends.

use std::collections::BTreeMap;
use std::sync::Arc;

use thiserror::Error;

use crate::cpf::CpfState;
use crate::ir::{
    has_hier_sep, push_norm_segments, qualify_name, qualify_ref, qualify_signal_ref,
    split_handle_dot, split_hier_segments, split_parent_leaf, split_tcl_list, trim_hier_path,
    DiagSeverity, PowerIntent, UpfDiag, UpfLoc, UpfVersion,
};

use super::finder::split_hier_segments_keep_dots;
use super::finder_api::ObjectFinder;

/// Errors produced when loading or evaluating UPF/CPF Tcl scripts.
#[derive(Debug, Error)]
pub enum UpfError {
    #[error("I/O error reading `{path}`: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{message}")]
    Tcl { message: String, info: String },
    #[error("{0}")]
    Semantic(String),
}

/// Decision D13 strict option parser shared by UPF and CPF Tcl commands.
pub(crate) struct UpfArgs {
    pub(crate) cmd: String,
    pub(crate) pos: Vec<String>,
    pub(crate) opts: Vec<(String, Vec<String>)>,
}

impl UpfArgs {
    pub(crate) fn parse(
        a: &[String],
        arity: &[(&str, usize)],
        opt_bool: &[&str],
        ignore: &[(&str, usize)],
        intent: &mut PowerIntent,
        loc: Option<UpfLoc>,
    ) -> Result<Self, String> {
        let cmd = a[0].rsplit("::").next().unwrap_or(&a[0]).to_string();
        Self::parse_named(&cmd, &a[1..], arity, opt_bool, ignore, intent, loc)
    }

    pub(crate) fn parse_named(
        cmd: &str,
        words: &[String],
        arity: &[(&str, usize)],
        opt_bool: &[&str],
        ignore: &[(&str, usize)],
        intent: &mut PowerIntent,
        loc: Option<UpfLoc>,
    ) -> Result<Self, String> {
        let mut out = Self {
            cmd: cmd.to_string(),
            pos: Vec::new(),
            opts: Vec::new(),
        };
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            if w.starts_with('-')
                && w.len() > 1
                && !w[1..].starts_with(|c: char| c.is_ascii_digit() || c == '.')
            {
                if opt_bool.contains(&w.as_str()) {
                    if let Some(next) = words.get(i + 1) {
                        if parse_bool_literal(next).is_some() {
                            out.opts.push((w.clone(), vec![next.clone()]));
                            i += 2;
                            continue;
                        }
                    }
                    out.opts.push((w.clone(), vec!["true".to_string()]));
                    i += 1;
                    continue;
                }
                if let Some(&(_, n)) = arity.iter().find(|(o, _)| o == w) {
                    let vals = words.get(i + 1..i + 1 + n).ok_or_else(|| {
                        let msg = format!("{cmd}: option {w} needs {n} value(s)");
                        record_d13_error(intent, &msg, loc.clone());
                        format!("{}: {msg}", UpfLoc::display_opt(loc.as_ref()))
                    })?;
                    out.opts.push((w.clone(), vals.to_vec()));
                    i += 1 + n;
                    continue;
                }
                if let Some(&(_, n)) = ignore.iter().find(|(o, _)| o == w) {
                    let _ = words.get(i + 1..i + 1 + n).ok_or_else(|| {
                        let msg = format!("{cmd}: option {w} needs {n} value(s)");
                        record_d13_error(intent, &msg, loc.clone());
                        format!("{}: {msg}", UpfLoc::display_opt(loc.as_ref()))
                    })?;
                    intent.diagnostics.push(UpfDiag {
                        rule_id: "UPF_D13_NOTE".to_string(),
                        severity: DiagSeverity::Note,
                        message: format!("{cmd}: ignoring vendor layout/synthesis hint {w}"),
                        loc: loc.clone(),
                    });
                    i += 1 + n;
                    continue;
                }
                let msg = format!("{cmd}: unsupported option {w}");
                record_d13_error(intent, &msg, loc.clone());
                return Err(format!("{}: {msg}", UpfLoc::display_opt(loc.as_ref())));
            }
            out.pos.push(w.clone());
            i += 1;
        }
        Ok(out)
    }

    pub(crate) fn flag(&self, o: &str) -> bool {
        self.opts.iter().any(|(k, _)| k == o)
    }

    pub(crate) fn val(&self, o: &str) -> Option<&str> {
        self.opts
            .iter()
            .rev()
            .find(|(k, _)| k == o)
            .and_then(|(_, v)| v.first())
            .map(String::as_str)
    }

    pub(crate) fn all<'a>(&'a self, o: &'a str) -> impl Iterator<Item = &'a Vec<String>> + 'a {
        self.opts
            .iter()
            .filter(move |(k, _)| k == o)
            .map(|(_, v)| v)
    }

    pub(crate) fn bool_val(&self, o: &str) -> Result<Option<bool>, String> {
        match self.val(o) {
            Some(v) => parse_bool_literal(v)
                .map(Some)
                .ok_or_else(|| format!("{}: invalid boolean `{v}` for {o}", self.cmd)),
            None => Ok(None),
        }
    }

    pub(crate) fn req_pos(&self, idx: usize, what: &str) -> Result<&str, String> {
        self.pos
            .get(idx)
            .map(String::as_str)
            .ok_or_else(|| format!("{}: missing {what}", self.cmd))
    }

    pub(crate) fn check_max_pos(&self, max: usize) -> Result<(), String> {
        if self.pos.len() > max {
            Err(format!(
                "{}: unexpected positional argument `{}`",
                self.cmd, self.pos[max]
            ))
        } else {
            Ok(())
        }
    }
}

pub(crate) fn record_d13_error(intent: &mut PowerIntent, msg: &str, loc: Option<UpfLoc>) {
    intent.diagnostics.push(UpfDiag {
        rule_id: "UPF_D13_UNSUPPORTED".to_string(),
        severity: DiagSeverity::Error,
        message: msg.to_string(),
        loc,
    });
}

pub(crate) fn parse_bool_literal(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

pub(crate) fn loc_err(loc: Option<&UpfLoc>, msg: &str) -> String {
    format!("{}: {msg}", UpfLoc::display_opt(loc))
}

pub(crate) struct ScopeFrame {
    pub(crate) prev_scope: String,
    pub(crate) prev_version: Option<UpfVersion>,
    pub(crate) prev_is_cpf: bool,
    pub(crate) prev_cpf_state: CpfState,
    pub(crate) prev_hierarchy_separator: char,
    pub(crate) prev_design_top: Option<String>,
    pub(crate) scoped_push: bool,
    pub(crate) explicit_scope: bool,
    pub(crate) defer_cpf_fallback: bool,
    pub(crate) is_sequential_sibling: bool,
}

pub(crate) struct UpfEvalState {
    pub(crate) intent: PowerIntent,
    pub(crate) current_scope: String,
    pub(crate) is_cpf_mode: bool,
    pub(crate) defer_root_cpf_fallback: bool,
    pub(crate) cpf_state: CpfState,
    pub(crate) next_precedence: u32,
    pub(crate) scope_stack: Vec<ScopeFrame>,
    pub(crate) finder: Arc<dyn ObjectFinder>,
    pub(crate) found_root_aliases: BTreeMap<String, String>,
    pub(crate) found_rel_aliases: BTreeMap<(String, String), String>,
}

impl UpfEvalState {
    pub(crate) fn new(finder: Arc<dyn ObjectFinder>) -> Self {
        Self {
            intent: PowerIntent::default(),
            current_scope: String::new(),
            is_cpf_mode: false,
            defer_root_cpf_fallback: false,
            cpf_state: CpfState::default(),
            next_precedence: 1,
            scope_stack: Vec::new(),
            finder,
            found_root_aliases: BTreeMap::new(),
            found_rel_aliases: BTreeMap::new(),
        }
    }

    pub(crate) fn should_defer_cpf_fallback(&self) -> bool {
        if let Some(frame) = self.scope_stack.last() {
            frame.defer_cpf_fallback
        } else {
            self.defer_root_cpf_fallback
        }
    }

    pub(crate) fn next_order(&mut self) -> u32 {
        let ord = self.next_precedence;
        self.next_precedence += 1;
        ord
    }

    fn local_exists(&self, local: &str) -> bool {
        let norm = local.trim_start_matches('/');
        let upf_exists = self.intent.domains.iter().any(|d| d.name == norm)
            || self.intent.supply_sets.iter().any(|s| s.name == norm)
            || self.intent.supply_nets.iter().any(|n| n.name == norm)
            || self.intent.supply_ports.iter().any(|p| p.name == norm)
            || self.intent.power_switches.iter().any(|sw| sw.name == norm)
            || self.intent.pst_tables.iter().any(|t| t.name == norm);
        if upf_exists {
            return true;
        }
        if !self.finder.has_design() {
            return false;
        }
        crate::resolve::design_object_exists(&*self.finder, norm)
    }

    fn has_domain_ref(&self, raw_pd: &str) -> bool {
        let sep = self.intent.hierarchy_separator;
        let trimmed = trim_hier_path(raw_pd);
        let is_abs =
            trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
        let norm = qualify_name(&self.current_scope, trimmed, sep);
        if self.intent.domains.iter().any(|d| d.name == norm) {
            return true;
        }
        if !is_abs && !self.current_scope.is_empty() {
            let local_key = norm
                .strip_prefix(&format!("{}/", self.current_scope))
                .unwrap_or(&norm);
            let mut cur = self.current_scope.as_str();
            while let Some((parent, _)) = split_parent_leaf(cur) {
                let cand = format!("{parent}/{local_key}");
                if self.intent.domains.iter().any(|d| d.name == cand) {
                    return true;
                }
                cur = parent;
            }
            if self.intent.domains.iter().any(|d| d.name == local_key) {
                return true;
            }
        }
        false
    }

    pub(crate) fn qualify_decl(&self, raw: &str) -> String {
        qualify_name(&self.current_scope, raw, self.intent.hierarchy_separator)
    }

    fn normalize_alias_key(raw: &str, is_abs: bool, sep: char) -> String {
        let mut s = raw;
        if is_abs {
            loop {
                let before = s;
                s = s.trim_start_matches('/');
                if sep != '/' && sep != '.' {
                    s = s.trim_start_matches(sep);
                    let dot_sep = format!(".{sep}");
                    while let Some(rest) = s.strip_prefix(&dot_sep) {
                        s = rest;
                    }
                }
                while let Some(rest) = s.strip_prefix("./") {
                    s = rest;
                }
                if s == before {
                    break;
                }
            }
        } else {
            loop {
                let before = s;
                while let Some(rest) = s.strip_prefix("./") {
                    s = rest;
                }
                if sep != '/' && sep != '.' {
                    let dot_sep = format!(".{sep}");
                    while let Some(rest) = s.strip_prefix(&dot_sep) {
                        s = rest;
                    }
                }
                if s == before {
                    break;
                }
            }
        }
        if !s.contains('/') {
            return s.to_string();
        }
        let mut segs: Vec<&str> = Vec::new();
        for seg in split_hier_segments_keep_dots(s) {
            match seg {
                "" | "." => {}
                ".." => {
                    if segs.last().is_some_and(|&prev| prev != "..") {
                        segs.pop();
                    } else if !is_abs {
                        segs.push("..");
                    }
                }
                other => segs.push(other),
            }
        }
        segs.join("/")
    }

    fn lookup_found_alias_in_scope(
        &self,
        scope: &str,
        trimmed: &str,
        is_abs: bool,
    ) -> Option<(&str, bool)> {
        let sep = self.intent.hierarchy_separator;
        if is_abs {
            let abs_key = Self::normalize_alias_key(trimmed, true, sep);
            return self
                .found_root_aliases
                .get(&abs_key)
                .map(|c| (c.as_str(), true));
        }
        if let Some(root_canon) = self.found_root_aliases.get(trimmed) {
            return Some((root_canon.as_str(), true));
        }
        if let Some(rel_canon) = self
            .found_rel_aliases
            .get(&(scope.to_string(), trimmed.to_string()))
        {
            return Some((rel_canon.as_str(), false));
        }
        let rel_key = Self::normalize_alias_key(trimmed, false, sep);
        if rel_key != trimmed {
            if let Some(rel_canon) = self
                .found_rel_aliases
                .get(&(scope.to_string(), rel_key.clone()))
            {
                return Some((rel_canon.as_str(), false));
            }
            if scope.is_empty() {
                if let Some(root_canon) = self.found_root_aliases.get(&rel_key) {
                    return Some((root_canon.as_str(), true));
                }
            }
        }
        None
    }

    fn lookup_found_alias(&self, trimmed: &str, is_abs: bool) -> Option<(&str, bool)> {
        self.lookup_found_alias_in_scope(&self.current_scope, trimmed, is_abs)
    }

    pub(crate) fn qualify(&self, raw: &str) -> String {
        self.qualify_in_scope(&self.current_scope, raw)
    }

    pub(crate) fn qualify_in_scope(&self, scope: &str, raw: &str) -> String {
        let trimmed = trim_hier_path(raw);
        let sep = self.intent.hierarchy_separator;
        let is_abs =
            trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
        let local = qualify_name(scope, trimmed, sep);
        if !self.local_exists(&local) {
            if let Some((canon, _)) = self.lookup_found_alias_in_scope(scope, trimmed, is_abs) {
                return canon.to_string();
            }
        }
        local
    }

    pub(crate) fn qualify_ref(&self, raw: &str) -> String {
        let trimmed = trim_hier_path(raw);
        let sep = self.intent.hierarchy_separator;
        let is_abs =
            trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
        let local = qualify_name(&self.current_scope, trimmed, sep);
        if !self.local_exists(&local) {
            if let Some((canon, is_root)) = self.lookup_found_alias(trimmed, is_abs) {
                return if is_root {
                    if canon.is_empty() {
                        String::new()
                    } else {
                        format!("/{canon}")
                    }
                } else {
                    canon.to_string()
                };
            }
        }
        qualify_ref(&self.current_scope, raw, sep)
    }

    pub(crate) fn qualify_signal_ref(&self, raw: &str) -> String {
        let trimmed = trim_hier_path(raw);
        let sep = self.intent.hierarchy_separator;
        let is_abs =
            trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
        let sig_q = qualify_signal_ref(&self.current_scope, raw, sep);
        let is_root_anchored = is_abs || sig_q.starts_with('/');
        let local = qualify_name(&self.current_scope, trimmed, sep);
        if is_root_anchored || !self.local_exists(&local) {
            if let Some((canon, is_root)) = self.lookup_found_alias(trimmed, is_abs) {
                return if is_root || is_root_anchored {
                    if canon.is_empty() {
                        String::new()
                    } else {
                        format!("/{canon}")
                    }
                } else {
                    canon.to_string()
                };
            }
        }
        sig_q
    }

    pub(crate) fn qualify_list(&self, raw: &str) -> Vec<String> {
        self.qualify_list_in_scope(&self.current_scope, raw)
    }

    pub(crate) fn qualify_list_in_scope(&self, scope: &str, raw: &str) -> Vec<String> {
        split_tcl_list(raw)
            .into_iter()
            .filter_map(|item| {
                let trimmed = trim_hier_path(&item);
                if trimmed.is_empty() {
                    return None;
                }
                if trimmed == "." {
                    Some(if scope.is_empty() {
                        ".".to_string()
                    } else {
                        scope.to_string()
                    })
                } else {
                    let q = self.qualify_in_scope(scope, trimmed);
                    Some(if q.is_empty() {
                        if scope.is_empty() {
                            ".".to_string()
                        } else {
                            "/".to_string()
                        }
                    } else {
                        q
                    })
                }
            })
            .collect()
    }

    pub(crate) fn qualify_ref_list(&self, raw: &str) -> Vec<String> {
        split_tcl_list(raw)
            .into_iter()
            .filter(|item| !item.is_empty())
            .map(|item| self.qualify_ref(&item))
            .collect()
    }

    pub(crate) fn qualify_handle(&self, raw: &str) -> String {
        let trimmed = trim_hier_path(raw);
        let sep = self.intent.hierarchy_separator;
        let is_abs =
            trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
        if self.lookup_found_alias(trimmed, is_abs).is_some() {
            return self.qualify_ref(trimmed);
        }
        if let Some((pd, handle)) = split_handle_dot(trimmed) {
            if !pd.is_empty()
                && !pd.ends_with('.')
                && !pd.ends_with('/')
                && !handle.is_empty()
                && !handle.starts_with('/')
                && !has_hier_sep(handle)
            {
                if sep != '.' {
                    return format!("{}.{handle}", self.qualify_ref(pd));
                }
                let full_ref = self.qualify_ref(trimmed);
                let norm_full = full_ref.trim_start_matches('/');
                let is_known_supply = self.intent.supply_sets.iter().any(|s| s.name == norm_full)
                    || self.intent.supply_nets.iter().any(|n| n.name == norm_full)
                    || self.intent.supply_ports.iter().any(|p| p.name == norm_full);
                if !is_known_supply && self.has_domain_ref(pd) {
                    return format!("{}.{handle}", self.qualify_ref(pd));
                }
                return full_ref;
            }
        }
        self.qualify_ref(trimmed)
    }
}

pub(crate) fn resolve_scope_arg(current_scope: &str, scope_arg: &str, sep: char) -> String {
    let raw = trim_hier_path(scope_arg);
    if raw.is_empty() || raw == "." {
        return current_scope.to_string();
    }
    if raw == ".." {
        let mut parts = split_hier_segments(current_scope);
        parts.pop();
        return parts.join("/");
    }
    if raw == "/" {
        return String::new();
    }
    let is_abs = raw.starts_with('/') || (sep != '/' && sep != '.' && raw.starts_with(sep));
    let mut parts: Vec<&str> = if is_abs {
        Vec::new()
    } else {
        split_hier_segments(current_scope)
    };
    push_norm_segments(&mut parts, raw, sep);
    parts.join("/")
}
