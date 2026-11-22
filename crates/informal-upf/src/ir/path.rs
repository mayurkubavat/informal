//! Tcl list splitting and SystemVerilog-escaped hierarchical path helpers (`D1`).

use super::glob::{closed_bracket_kind, escape_glob};

/// Splits a Tcl list into elements, handling `{...}` brace groups, `"..."`
/// quotes, `\` backslash escapes, and SystemVerilog escaped identifiers whose
/// hierarchical body continues across their terminating space.
pub fn split_tcl_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let Some(c) = s[i..].chars().next() else {
            break;
        };
        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }
        let mut word = String::new();
        if c == '{' {
            i += 1;
            let mut depth = 1usize;
            while i < bytes.len() {
                let ch = s[i..].chars().next().unwrap();
                i += ch.len_utf8();
                match ch {
                    '\\' => {
                        if let Some(next) = s[i..].chars().next() {
                            if next == '{' || next == '}' || next == '\\' {
                                word.push(ch);
                                word.push(next);
                                i += next.len_utf8();
                                continue;
                            }
                        }
                        word.push(ch);
                    }
                    '{' => {
                        depth += 1;
                        word.push(ch);
                    }
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        word.push(ch);
                    }
                    _ => word.push(ch),
                }
            }
        } else if c == '"' {
            i += 1;
            while i < bytes.len() {
                let ch = s[i..].chars().next().unwrap();
                i += ch.len_utf8();
                match ch {
                    '"' => break,
                    '\\' => {
                        if let Some(esc) = s[i..].chars().next() {
                            word.push(esc);
                            i += esc.len_utf8();
                        }
                    }
                    _ => word.push(ch),
                }
            }
        } else {
            let mut at_sv_start = true;
            let mut pending_char_class_end: Option<usize> = None;
            while i < bytes.len() {
                let ch = s[i..].chars().next().unwrap();
                if ch.is_whitespace() {
                    break;
                }
                if ch == '\\' {
                    if at_sv_start
                        && i + 1 < bytes.len()
                        && !matches!(
                            bytes[i + 1],
                            b'[' | b']' | b'*' | b'?' | b'\\' | b'{' | b'}' | b'"'
                        )
                    {
                        if let Some(end) = scan_sv_escaped_ident_end(s, i) {
                            let ended_with_ws =
                                s[i..end].chars().last().is_some_and(char::is_whitespace);
                            let sv_continues = ended_with_ws
                                && (end == bytes.len()
                                    || matches!(bytes[end], b'/' | b'.' | b':' | b'[')
                                    || s[end..].chars().next().is_some_and(char::is_whitespace));
                            if sv_continues {
                                word.push_str(&s[i..end]);
                                i = end;
                                at_sv_start = false;
                                continue;
                            }
                        }
                    }
                    i += 1;
                    if let Some(esc) = s[i..].chars().next() {
                        word.push(esc);
                        i += esc.len_utf8();
                    }
                    at_sv_start = false;
                } else if ch == ']' && pending_char_class_end == Some(i) {
                    word.push(ch);
                    i += 1;
                    pending_char_class_end = None;
                    at_sv_start = true;
                } else {
                    if ch == '[' && pending_char_class_end.is_none() {
                        if let Some((end, true)) = closed_bracket_kind(s, i) {
                            if !s[i..end].chars().any(char::is_whitespace) {
                                pending_char_class_end = Some(end - 1);
                            }
                        }
                    }
                    word.push(ch);
                    i += ch.len_utf8();
                    at_sv_start = matches!(ch, '/' | ':' | '.' | '*' | '?');
                }
            }
        }
        out.push(word);
    }
    out
}

pub(crate) fn is_glob_escaped_bracket_at(s: &str, idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(idx) != Some(&b'\\') || bytes.get(idx + 1) != Some(&b'[') {
        return None;
    }
    let mut j = idx + 2;
    while j < bytes.len() {
        let ch = s[j..].chars().next()?;
        if ch.is_whitespace() {
            return None;
        }
        if bytes[j] == b'\\' && bytes.get(j + 1) == Some(&b']') {
            return Some(j + 2);
        }
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            let next_len = s[j + 1..].chars().next().map_or(1, char::len_utf8);
            j += 1 + next_len;
            continue;
        }
        j += ch.len_utf8();
    }
    None
}

pub(crate) fn scan_sv_escaped_ident_end(s: &str, idx: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(idx) != Some(&b'\\') || idx + 1 >= bytes.len() {
        return None;
    }
    if is_glob_escaped_bracket_at(s, idx).is_some()
        || matches!(bytes[idx + 1], b'[' | b']' | b'*' | b'?' | b'\\')
    {
        return None;
    }
    let mut j = idx + 1;
    while j < bytes.len() {
        let ch = s[j..].chars().next()?;
        if ch.is_whitespace() {
            break;
        }
        j += ch.len_utf8();
    }
    if j == idx + 1 {
        return None;
    }
    if j < bytes.len() {
        if let Some(ws) = s[j..].chars().next() {
            if ws.is_whitespace() {
                j += ws.len_utf8();
            }
        }
    }
    Some(j)
}

pub(crate) fn trim_hier_path(raw: &str) -> &str {
    let s = raw.trim_start();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut at_sv_start = true;
    while i < bytes.len() {
        if at_sv_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(s, i) {
                if s[end..].trim().is_empty() {
                    return &s[..end];
                }
                i = end;
                at_sv_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(s, i) {
            i = end;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = s[i + 1..].chars().next().map_or(1, char::len_utf8);
            i += 1 + ch_len;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(s, i) {
                i = end;
                at_sv_start = is_char_class;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
        at_sv_start = matches!(ch, '/' | ':' | '.' | '*' | '?') || ch.is_whitespace();
    }
    s.trim_end()
}

pub(crate) fn push_norm_segments<'a>(parts: &mut Vec<&'a str>, raw: &'a str, sep: char) {
    let _ = push_norm_segments_tracked(parts, raw, sep);
}

pub(crate) fn push_norm_segments_tracked<'a>(
    parts: &mut Vec<&'a str>,
    raw: &'a str,
    sep: char,
) -> bool {
    let initial_len = parts.len();
    let mut escaped_scope = false;
    let s = trim_hier_path(raw);
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut seg_start = 0usize;
    let mut at_seg_start = true;
    let mut at_sv_start = true;
    let mut pushed_esc_seg = false;

    let mut pop_seg = |parts: &mut Vec<&'a str>| {
        if parts.pop().is_none() || parts.len() < initial_len {
            escaped_scope = true;
        }
    };

    while i < bytes.len() {
        if at_seg_start {
            while i < bytes.len() && s[i..].chars().next().is_some_and(char::is_whitespace) {
                i += s[i..].chars().next().map_or(1, char::len_utf8);
            }
            seg_start = i;
            if i >= bytes.len() {
                break;
            }
            if bytes[i] == b'.' {
                let is_sep_after = |idx: usize| -> Option<usize> {
                    if idx == bytes.len() {
                        Some(idx)
                    } else if s[idx..]
                        .chars()
                        .next()
                        .is_some_and(|ch| ch == '/' || (sep != '/' && sep != '.' && ch == sep))
                    {
                        let ch_len = s[idx..].chars().next().map_or(1, char::len_utf8);
                        Some(idx + ch_len)
                    } else {
                        None
                    }
                };
                if s[i..].starts_with("..") {
                    if let Some(next_i) = is_sep_after(i + 2) {
                        pop_seg(parts);
                        i = next_i;
                        seg_start = i;
                        at_seg_start = true;
                        at_sv_start = true;
                        continue;
                    }
                }
                if let Some(next_i) = is_sep_after(i + 1) {
                    i = next_i;
                    seg_start = i;
                    at_seg_start = true;
                    at_sv_start = true;
                    continue;
                }
            }
        }
        if at_sv_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(s, i) {
                let mut after_ws = end;
                while after_ws < bytes.len()
                    && s[after_ws..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace)
                {
                    after_ws += s[after_ws..].chars().next().map_or(1, char::len_utf8);
                }
                let next_is_sep = after_ws == bytes.len()
                    || s[after_ws..]
                        .chars()
                        .next()
                        .is_some_and(|ch| ch == '/' || (sep != '/' && ch == sep));
                if next_is_sep {
                    parts.push(&s[seg_start..end]);
                    pushed_esc_seg = true;
                    i = after_ws;
                    at_seg_start = false;
                    at_sv_start = false;
                    continue;
                }
                i = end;
                at_seg_start = false;
                at_sv_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(s, i) {
            i = end;
            at_seg_start = false;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = s[i + 1..].chars().next().map_or(1, char::len_utf8);
            i += 1 + ch_len;
            at_seg_start = false;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(s, i) {
                i = end;
                at_seg_start = false;
                at_sv_start = is_char_class;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        let ch_len = ch.len_utf8();
        if ch == '/' || (sep != '/' && ch == sep) {
            if pushed_esc_seg {
                pushed_esc_seg = false;
            } else {
                match s[seg_start..i].trim() {
                    "" | "." => {}
                    ".." => pop_seg(parts),
                    tok => parts.push(tok),
                }
            }
            i += ch_len;
            seg_start = i;
            at_seg_start = true;
            at_sv_start = true;
            continue;
        }
        i += ch_len;
        at_seg_start = false;
        at_sv_start = matches!(ch, '*' | '?');
    }
    if !pushed_esc_seg && seg_start < bytes.len() {
        match s[seg_start..].trim() {
            "" | "." => {}
            ".." => pop_seg(parts),
            tok => parts.push(tok),
        }
    }
    escaped_scope
}

pub(crate) fn split_hier_segments(norm_path: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    push_norm_segments(&mut parts, norm_path, '/');
    parts
}

pub(crate) fn has_hier_sep(norm_path: &str) -> bool {
    split_hier_segments(norm_path).len() > 1
}

pub(crate) fn last_hier_segment(norm_path: &str) -> &str {
    split_hier_segments(norm_path)
        .into_iter()
        .last()
        .unwrap_or("")
}

pub(crate) fn split_parent_leaf(norm_path: &str) -> Option<(&str, &str)> {
    let bytes = norm_path.as_bytes();
    let mut i = 0usize;
    let mut last_slash = None;
    let mut at_seg_start = true;
    while i < bytes.len() {
        if at_seg_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(norm_path, i) {
                i = end;
                at_seg_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(norm_path, i) {
            i = end;
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = norm_path[i + 1..].chars().next().map_or(1, char::len_utf8);
            i += 1 + ch_len;
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(norm_path, i) {
                i = end;
                at_seg_start = is_char_class;
                continue;
            }
        }
        if bytes[i] == b'/' {
            last_slash = Some(i);
            i += 1;
            at_seg_start = true;
            continue;
        }
        let ch = norm_path[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
        at_seg_start = matches!(ch, '*' | '?');
    }
    last_slash.map(|idx| (&norm_path[..idx], &norm_path[idx + 1..]))
}

pub(crate) fn split_handle_dot(s: &str) -> Option<(&str, &str)> {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut last_dot = None;
    let mut at_sv_start = true;
    while i < bytes.len() {
        if at_sv_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(s, i) {
                i = end;
                at_sv_start = false;
                continue;
            }
        }
        if let Some(end) = is_glob_escaped_bracket_at(s, i) {
            i = end;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let ch_len = s[i + 1..].chars().next().map_or(1, char::len_utf8);
            i += 1 + ch_len;
            at_sv_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(s, i) {
                i = end;
                at_sv_start = is_char_class;
                continue;
            }
        }
        if bytes[i] == b'.'
            && i > 0
            && bytes[i - 1] != b'.'
            && bytes[i - 1] != b'/'
            && bytes.get(i + 1) != Some(&b'/')
            && bytes.get(i + 1) != Some(&b'.')
        {
            last_dot = Some(i);
        }
        let ch = s[i..].chars().next().unwrap_or('\0');
        i += ch.len_utf8();
        at_sv_start = matches!(ch, '/' | ':' | '.' | '*' | '?') || ch.is_whitespace();
    }
    last_dot.map(|idx| (&s[..idx], &s[idx + 1..]))
}

/// Normalizes a hierarchical scope path (`"."` / `"/"` -> `""`, strips leading/trailing `/`,
/// resolves `.` / `..` segments, and converts `sep` to `/`).
pub fn normalize_scope(scope: &str, sep: char) -> String {
    let mut parts = Vec::new();
    push_norm_segments(&mut parts, scope, sep);
    parts.join("/")
}

/// Qualifies a relative object name with `scope` unless already qualified (`/`-prefixed,
/// or `sep`-prefixed when `sep != '/' && sep != '.'`), resolving `.` and `..` segments
/// without a leading `/`.
pub fn qualify_name(scope: &str, raw: &str, sep: char) -> String {
    let trimmed = trim_hier_path(raw);
    let is_abs = trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
    if is_abs {
        return normalize_scope(trimmed, sep);
    }
    let norm_scope = normalize_scope(scope, sep);
    let mut parts: Vec<&str> = if norm_scope.is_empty() {
        Vec::new()
    } else {
        split_hier_segments(&norm_scope)
    };
    push_norm_segments(&mut parts, trimmed, sep);
    parts.join("/")
}

/// Qualifies a relative reference path with `scope`, preserving a leading `/` on
/// explicitly root-anchored references so `lookup_scoped` performs root-only
/// lookup without being shadowed by same-named local objects in subscopes.
pub fn qualify_ref(scope: &str, raw: &str, sep: char) -> String {
    let trimmed = trim_hier_path(raw);
    let is_abs = trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
    if is_abs {
        let norm = normalize_scope(trimmed, sep);
        return if norm.is_empty() {
            String::new()
        } else {
            format!("/{norm}")
        };
    }
    qualify_name(scope, trimmed, sep)
}

/// Qualifies an RTL signal reference with `scope`, preserving a leading `/` both on
/// explicitly root-anchored references (`/sig`) and on `..`-escaped references that
/// walk out of `scope` (`../sig`), preventing local same-named signals from shadowing
/// parent/root signals.
pub fn qualify_signal_ref(scope: &str, raw: &str, sep: char) -> String {
    let trimmed = trim_hier_path(raw);
    let is_abs = trimmed.starts_with('/') || (sep != '/' && sep != '.' && trimmed.starts_with(sep));
    if is_abs {
        let norm = normalize_scope(trimmed, sep);
        return if norm.is_empty() {
            String::new()
        } else {
            format!("/{norm}")
        };
    }
    let norm_scope = normalize_scope(scope, sep);
    let mut parts: Vec<&str> = if norm_scope.is_empty() {
        Vec::new()
    } else {
        split_hier_segments(&norm_scope)
    };
    let escaped = push_norm_segments_tracked(&mut parts, trimmed, sep);
    let joined = parts.join("/");
    if escaped {
        if joined.is_empty() {
            String::new()
        } else {
            format!("/{joined}")
        }
    } else {
        joined
    }
}

/// Strips SystemVerilog `\ident ` escape syntax from each segment of a normalized
/// hierarchical path (e.g. `\u_blk$1 /y` -> `u_blk$1/y`, `\bus [0]` -> `bus[0]`).
pub(crate) fn strip_sv_escapes_in_path(path: &str) -> String {
    split_hier_segments(path)
        .into_iter()
        .map(|seg| {
            if seg.starts_with('\\') {
                if let Some(end) = scan_sv_escaped_ident_end(seg, 0) {
                    let head = seg[1..end].trim_end();
                    let tail = seg[end..].trim_start();
                    return format!("{head}{tail}");
                }
            }
            seg.to_string()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Strips SystemVerilog `\ident ` escape syntax in a glob pattern while preserving
/// Tcl glob escapes (`\*`, `\?`, `\[`, `\]`, `\\`) and `[...]` character classes.
pub(crate) fn strip_sv_escapes_in_glob(pat: &str) -> String {
    let bytes = pat.as_bytes();
    let mut out = String::with_capacity(pat.len());
    let mut i = 0usize;
    let mut at_seg_start = true;
    while i < bytes.len() {
        if at_seg_start && bytes[i] == b'\\' {
            if let Some(end) = scan_sv_escaped_ident_end(pat, i) {
                out.push_str(&escape_glob(pat[i + 1..end].trim_end()));
                at_seg_start = pat[i..end].chars().last().is_some_and(char::is_whitespace);
                i = end;
                continue;
            }
        }
        if bytes[i] == b'\\' {
            out.push('\\');
            i += 1;
            if i < bytes.len() {
                if let Some(ch) = pat[i..].chars().next() {
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            at_seg_start = false;
            continue;
        }
        if bytes[i] == b'[' {
            if let Some((end, is_char_class)) = closed_bracket_kind(pat, i) {
                out.push_str(&pat[i..end]);
                i = end;
                at_seg_start = is_char_class;
                continue;
            }
        }
        if let Some(ch) = pat[i..].chars().next() {
            out.push(ch);
            i += ch.len_utf8();
            at_seg_start = matches!(ch, '/' | ':' | '.' | '*' | '?') || ch.is_whitespace();
        } else {
            break;
        }
    }
    out
}
