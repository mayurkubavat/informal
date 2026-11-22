//! `find_objects` command implementation, pattern normalization, RTL bracket matching, and `-hsc` aliasing.

use std::collections::BTreeSet;

use informal_tcl::Value;
use regex::RegexBuilder;

use crate::ir::{
    closed_bracket_kind, escape_glob, escape_rtl_brackets, escape_sv_escaped_backslashes_for_glob,
    glob, has_hier_sep, has_unescaped_glob_wildcards, is_glob_escaped_bracket_at,
    last_hier_segment, scan_sv_escaped_ident_end, split_hier_segments, split_tcl_list,
    strip_sv_escapes_in_glob, strip_sv_escapes_in_path, unescape_rtl_brackets, SupplyDirection,
    UpfLoc,
};

use super::eval::{loc_err, resolve_scope_arg, UpfArgs, UpfEvalState};
use super::finder_api::{ObjectFinder, ObjectType};

fn format_found_path(raw: &str, current_scope: &str, include_hier: bool) -> String {
    let norm = raw.trim_start_matches('/');
    if include_hier {
        if norm.is_empty() {
            "/".to_string()
        } else {
            format!("/{norm}")
        }
    } else if current_scope.is_empty() {
        norm.to_string()
    } else if norm == current_scope {
        ".".to_string()
    } else if let Some(rest) = norm
        .strip_prefix(current_scope)
        .and_then(|s| s.strip_prefix('/'))
    {
        rest.to_string()
    } else {
        format!("/{norm}")
    }
}

pub(crate) fn cmd_find_objects(
    st: &mut UpfEvalState,
    a: &[String],
    loc: Option<UpfLoc>,
) -> Result<Value, String> {
    let args = UpfArgs::parse(
        a,
        &[
            ("-pattern", 1),
            ("-object_type", 1),
            ("-direction", 1),
            ("-hsc", 1),
        ],
        &["-transitive", "-regexp", "-ignore_case", "-include_hier"],
        &[],
        &mut st.intent,
        loc.clone(),
    )?;
    args.check_max_pos(1)?;
    let scope_arg = args.pos.first().map(String::as_str).unwrap_or(".");
    let scope_path = resolve_scope_arg(&st.current_scope, scope_arg, st.intent.hierarchy_separator);

    let obj_type = match args.val("-object_type") {
        Some(t) => Some(ObjectType::parse(t).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let dir_filter = match args.val("-direction") {
        Some(d) => Some(SupplyDirection::parse(d).map_err(|e| loc_err(loc.as_ref(), &e))?),
        None => None,
    };
    let transitive = args.bool_val("-transitive")?.unwrap_or(true);
    let use_regexp = args.bool_val("-regexp")?.unwrap_or(false);
    let ignore_case = args.bool_val("-ignore_case")?.unwrap_or(false);
    let include_hier = args.bool_val("-include_hier")?.unwrap_or(false);
    let patterns: Vec<String> = match args.val("-pattern") {
        Some(p) if use_regexp => vec![p.to_string()],
        Some(p) => split_tcl_list(p)
            .into_iter()
            .map(|pat| translate_hier_sep_in_pattern(&pat, st.intent.hierarchy_separator))
            .collect(),
        None => vec!["*".to_string()],
    };

    let raw_results = if use_regexp {
        match obj_type {
            Some(ObjectType::SupplyPort) => collect_upf_names(
                st.intent
                    .supply_ports
                    .iter()
                    .filter(|p| dir_filter.is_none_or(|d| p.direction == d))
                    .map(|p| p.name.as_str()),
                &scope_path,
                &patterns,
                true,
                transitive,
            ),
            Some(ObjectType::SupplyNet) => collect_upf_names(
                st.intent.supply_nets.iter().map(|n| n.name.as_str()),
                &scope_path,
                &patterns,
                true,
                transitive,
            ),
            Some(ObjectType::SupplySet) => collect_upf_names(
                st.intent.supply_sets.iter().map(|s| s.name.as_str()),
                &scope_path,
                &patterns,
                true,
                transitive,
            ),
            Some(ObjectType::PowerDomain) => collect_upf_names(
                st.intent.domains.iter().map(|d| d.name.as_str()),
                &scope_path,
                &patterns,
                true,
                transitive,
            ),
            _ => st.finder.find_objects_with_direction(
                &scope_path,
                &["*".to_string()],
                obj_type,
                transitive,
                dir_filter,
            ),
        }
    } else {
        let mut combined = Vec::new();
        let mut seen = BTreeSet::new();
        for pat in &patterns {
            let (eff_scope, eff_pat) = resolve_pattern_scope(&scope_path, pat);
            let single = [eff_pat];
            let hits = match obj_type {
                Some(ObjectType::SupplyPort) => collect_upf_names(
                    st.intent
                        .supply_ports
                        .iter()
                        .filter(|p| dir_filter.is_none_or(|d| p.direction == d))
                        .map(|p| p.name.as_str()),
                    &eff_scope,
                    &single,
                    false,
                    transitive,
                ),
                Some(ObjectType::SupplyNet) => collect_upf_names(
                    st.intent.supply_nets.iter().map(|n| n.name.as_str()),
                    &eff_scope,
                    &single,
                    false,
                    transitive,
                ),
                Some(ObjectType::SupplySet) => collect_upf_names(
                    st.intent.supply_sets.iter().map(|s| s.name.as_str()),
                    &eff_scope,
                    &single,
                    false,
                    transitive,
                ),
                Some(ObjectType::PowerDomain) => collect_upf_names(
                    st.intent.domains.iter().map(|d| d.name.as_str()),
                    &eff_scope,
                    &single,
                    false,
                    transitive,
                ),
                _ => find_design_objects_with_rtl_brackets(
                    st.finder.as_ref(),
                    &eff_scope,
                    &single,
                    obj_type,
                    transitive,
                    dir_filter,
                ),
            };
            for h in hits {
                if seen.insert(h.clone()) {
                    combined.push(h);
                }
            }
        }
        combined
    };

    let mut results: Vec<String> = raw_results
        .into_iter()
        .map(|r| format_found_path(&r, &st.current_scope, include_hier))
        .collect();
    if use_regexp {
        results = filter_regex(&results, &patterns, ignore_case)?;
    }

    if let Some(hsc) = args.val("-hsc") {
        if hsc != "/" {
            for r in &mut results {
                if r != "/" {
                    let is_root = r.starts_with('/');
                    let canon = if is_root {
                        r.trim_start_matches('/').to_string()
                    } else if st.current_scope.is_empty() {
                        r.clone()
                    } else if r == "." {
                        st.current_scope.clone()
                    } else {
                        format!("{}/{}", st.current_scope, r)
                    };
                    let rewritten = if r == "." {
                        ".".to_string()
                    } else {
                        split_hier_segments(r.trim_start_matches('/')).join(hsc)
                    };
                    if is_root && rewritten == canon {
                        *r = format!("/{rewritten}");
                    } else {
                        if rewritten != *r && rewritten != canon {
                            if is_root || st.current_scope.is_empty() {
                                st.found_root_aliases.insert(rewritten.clone(), canon);
                            } else {
                                st.found_rel_aliases
                                    .insert((st.current_scope.clone(), rewritten.clone()), canon);
                            }
                        }
                        *r = rewritten;
                    }
                }
            }
        }
    }

    Ok(Value::List(results))
}

pub(crate) fn split_hier_segments_keep_dots(pat: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = pat.as_bytes();
    let mut i = 0usize;
    let mut seg_start = 0usize;
    let mut at_seg_start = true;
    while i < bytes.len() {
        if at_seg_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(pat, i) {
                i = end;
                at_seg_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(pat, i) {
            i = end;
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = pat[i + 1..].chars().next().map_or(1, char::len_utf8);
            i += 1 + ch_len;
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(pat, i) {
                i = end;
                at_seg_start = is_char_class;
                continue;
            }
        }
        if bytes[i] == b'/' {
            out.push(&pat[seg_start..i]);
            i += 1;
            seg_start = i;
            at_seg_start = true;
            continue;
        }
        let ch = pat[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
        at_seg_start = matches!(ch, '*' | '?');
    }
    if seg_start <= bytes.len() {
        out.push(&pat[seg_start..]);
    }
    out
}

fn collapse_find_pattern_dots(pat: &str) -> String {
    let is_abs = pat.starts_with('/');
    let mut segs: Vec<&str> = Vec::new();
    for seg in split_hier_segments_keep_dots(pat) {
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
    if segs.is_empty() {
        if is_abs {
            "/".to_string()
        } else if !pat.is_empty() {
            ".".to_string()
        } else {
            String::new()
        }
    } else if is_abs {
        format!("/{}", segs.join("/"))
    } else {
        segs.join("/")
    }
}

fn resolve_pattern_scope(scope_path: &str, pat: &str) -> (String, String) {
    let collapsed = collapse_find_pattern_dots(pat);
    if collapsed.starts_with('/') {
        return (scope_path.to_string(), collapsed);
    }
    let segs = split_hier_segments_keep_dots(&collapsed);
    let mut lead_up = 0usize;
    while lead_up < segs.len() && segs[lead_up] == ".." {
        lead_up += 1;
    }
    if lead_up == 0 {
        return (scope_path.to_string(), collapsed);
    }
    let mut scope_segs = split_hier_segments(scope_path);
    let keep = scope_segs.len().saturating_sub(lead_up);
    scope_segs.truncate(keep);
    let eff_scope = scope_segs.join("/");
    let rem = &segs[lead_up..];
    let eff_pat = if rem.is_empty() {
        "*".to_string()
    } else {
        rem.join("/")
    };
    (eff_scope, eff_pat)
}

pub(crate) fn translate_hier_sep_in_pattern(pat: &str, sep: char) -> String {
    if sep == '/' || pat == "." || pat == ".." {
        return pat.to_string();
    }
    let mut out = String::with_capacity(pat.len());
    let bytes = pat.as_bytes();
    let mut i = 0;
    let mut at_seg_start = true;
    let mut at_sv_start = true;
    while i < bytes.len() {
        if at_seg_start {
            if pat[i..].starts_with("./") {
                out.push_str("./");
                i += 2;
                at_seg_start = true;
                at_sv_start = true;
                continue;
            }
            if pat[i..].starts_with("../") {
                out.push_str("../");
                i += 3;
                at_seg_start = true;
                at_sv_start = true;
                continue;
            }
            if sep != '/' && sep != '.' {
                let dot_sep = format!(".{sep}");
                if pat[i..].starts_with(&dot_sep) {
                    out.push_str("./");
                    i += dot_sep.len();
                    at_seg_start = true;
                    at_sv_start = true;
                    continue;
                }
                let dotdot_sep = format!("..{sep}");
                if pat[i..].starts_with(&dotdot_sep) {
                    out.push_str("../");
                    i += dotdot_sep.len();
                    at_seg_start = true;
                    at_sv_start = true;
                    continue;
                }
            }
            if &pat[i..] == "." || &pat[i..] == ".." {
                out.push_str(&pat[i..]);
                break;
            }
            if i == 0 && sep != '/' && sep != '.' && pat[i..].starts_with(sep) {
                out.push('/');
                i += sep.len_utf8();
                at_seg_start = true;
                at_sv_start = true;
                continue;
            }
        }
        if at_sv_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(pat, i) {
                out.push_str(&pat[i..end]);
                i = end;
                at_seg_start = false;
                at_sv_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(pat, i) {
            out.push_str(&pat[i..end]);
            i = end;
            at_seg_start = false;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = pat[i + 1..].chars().next().map_or(1, char::len_utf8);
            out.push_str(&pat[i..i + 1 + ch_len]);
            i += 1 + ch_len;
            at_seg_start = false;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(pat, i) {
                out.push_str(&pat[i..end]);
                i = end;
                at_seg_start = false;
                at_sv_start = is_char_class;
                continue;
            }
        }
        let ch = pat[i..].chars().next().unwrap_or('\0');
        let ch_len = ch.len_utf8();
        if ch == '/' || ch == sep {
            out.push('/');
            at_seg_start = true;
            at_sv_start = true;
        } else {
            out.push(ch);
            at_seg_start = false;
            at_sv_start = matches!(ch, '*' | '?');
        }
        i += ch_len;
    }
    out
}

fn normalize_find_pattern(pat: &str) -> (String, String) {
    let collapsed = collapse_find_pattern_dots(pat);
    let lit = unescape_rtl_brackets(&collapsed);
    let esc = if !has_unescaped_glob_wildcards(&collapsed) {
        escape_glob(&lit)
    } else {
        escape_sv_escaped_backslashes_for_glob(&escape_rtl_brackets(&collapsed))
    };
    (lit, esc)
}

fn find_design_objects_with_rtl_brackets(
    finder: &dyn ObjectFinder,
    scope_path: &str,
    patterns: &[String],
    obj_type: Option<ObjectType>,
    transitive: bool,
    dir_filter: Option<SupplyDirection>,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut all_cache: Option<Vec<String>> = None;
    for pat in patterns {
        let is_root_anchored = pat.starts_with('/');
        let exact_only = !has_unescaped_glob_wildcards(pat);
        let (lit, esc) = normalize_find_pattern(pat);
        let lit_rel = lit.trim_start_matches('/');
        let esc_rel = esc.trim_start_matches('/');
        let mut hits = finder.find_objects_with_direction(
            scope_path,
            std::slice::from_ref(&esc),
            obj_type,
            transitive,
            dir_filter,
        );
        if hits.is_empty() && is_root_anchored {
            hits = finder.find_objects_with_direction(
                scope_path,
                std::slice::from_ref(&esc_rel.to_string()),
                obj_type,
                transitive,
                dir_filter,
            );
        }
        if hits.is_empty() && (is_root_anchored || esc != lit || esc.contains('\\')) {
            let lit_rel_unesc = strip_sv_escapes_in_path(lit_rel);
            let esc_unesc =
                strip_sv_escapes_in_glob(&escape_rtl_brackets(&collapse_find_pattern_dots(pat)));
            let esc_rel_unesc = esc_unesc.trim_start_matches('/');
            let all = all_cache.get_or_insert_with(|| {
                finder.find_objects_with_direction(
                    scope_path,
                    &["*".to_string()],
                    obj_type,
                    transitive,
                    dir_filter,
                )
            });
            for obj in all.iter() {
                let obj_norm = obj.trim_start_matches('/');
                let check_unesc = !obj_norm.contains('\\');
                let matched = if is_root_anchored {
                    if exact_only {
                        obj_norm == lit_rel || (check_unesc && obj_norm == lit_rel_unesc)
                    } else {
                        glob(esc_rel, obj_norm) || (check_unesc && glob(esc_rel_unesc, obj_norm))
                    }
                } else {
                    let rel = if scope_path.is_empty() {
                        obj_norm
                    } else {
                        obj_norm
                            .strip_prefix(scope_path)
                            .and_then(|s| s.strip_prefix('/'))
                            .unwrap_or(obj_norm)
                    };
                    let short = last_hier_segment(obj_norm);
                    if exact_only {
                        obj_norm == lit_rel
                            || rel == lit_rel
                            || short == lit_rel
                            || (check_unesc
                                && (obj_norm == lit_rel_unesc
                                    || rel == lit_rel_unesc
                                    || short == lit_rel_unesc))
                    } else {
                        glob(&esc, obj_norm)
                            || glob(esc_rel, obj_norm)
                            || glob(&esc, rel)
                            || glob(&esc, short)
                            || (check_unesc
                                && (glob(&esc_unesc, obj_norm)
                                    || glob(esc_rel_unesc, obj_norm)
                                    || glob(&esc_unesc, rel)
                                    || glob(&esc_unesc, short)))
                    }
                };
                if matched {
                    hits.push(obj.clone());
                }
            }
        } else if is_root_anchored {
            hits.retain(|obj| {
                let obj_norm = obj.trim_start_matches('/');
                if exact_only {
                    obj_norm == lit_rel
                } else {
                    glob(esc_rel, obj_norm)
                }
            });
        }
        for h in hits {
            if seen.insert(h.clone()) {
                out.push(h);
            }
        }
    }
    out
}

fn collect_upf_names<'a, I>(
    names: I,
    scope: &str,
    patterns: &[String],
    use_regexp: bool,
    transitive: bool,
) -> Vec<String>
where
    I: Iterator<Item = &'a str>,
{
    let scoped: Vec<String> = names
        .filter(|name| {
            if scope.is_empty() {
                transitive || !has_hier_sep(name)
            } else if let Some(rest) = name.strip_prefix(scope).and_then(|s| s.strip_prefix('/')) {
                transitive || !has_hier_sep(rest)
            } else {
                false
            }
        })
        .map(str::to_string)
        .collect();
    if use_regexp {
        scoped
    } else {
        let norm_pats: Vec<(String, String, bool, bool)> = patterns
            .iter()
            .map(|p| {
                let (lit, esc) = normalize_find_pattern(p);
                (
                    lit,
                    esc,
                    !has_unescaped_glob_wildcards(p),
                    p.starts_with('/'),
                )
            })
            .collect();
        scoped
            .into_iter()
            .filter(|n| {
                let rel = if scope.is_empty() {
                    n.as_str()
                } else {
                    n.strip_prefix(scope)
                        .and_then(|s| s.strip_prefix('/'))
                        .unwrap_or(n.as_str())
                };
                let short = last_hier_segment(n);
                norm_pats
                    .iter()
                    .any(|(lit, esc, exact_only, is_root_anchored)| {
                        let lit_rel = lit.trim_start_matches('/');
                        let esc_rel = esc.trim_start_matches('/');
                        if *is_root_anchored {
                            if *exact_only {
                                n == lit_rel
                            } else {
                                glob(esc_rel, n)
                            }
                        } else if *exact_only {
                            n == lit_rel || rel == lit_rel || short == lit_rel
                        } else {
                            glob(esc, n) || glob(esc_rel, n) || glob(esc, rel) || glob(esc, short)
                        }
                    })
            })
            .collect()
    }
}

fn filter_regex(
    items: &[String],
    patterns: &[String],
    ignore_case: bool,
) -> Result<Vec<String>, String> {
    let mut compiled = Vec::with_capacity(patterns.len());
    for p in patterns {
        let re = RegexBuilder::new(p)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|e| format!("find_objects: invalid regexp `{p}`: {e}"))?;
        compiled.push(re);
    }
    Ok(items
        .iter()
        .filter(|item| compiled.iter().any(|re| re.is_match(item)))
        .cloned()
        .collect())
}
