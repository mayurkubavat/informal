//! Fast structural Verilog / PG netlist front-end for 100k+ cell Liberty-backed
//! gate/PG netlists.

pub mod elaborate;
pub mod parser;
mod pg_stitch;
mod scanner;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use informal_liberty::LibertyDb;
use sha2::{Digest, Sha256};

use crate::cache::FileDigest;
use crate::struct_graph::StructuralGraph;
use crate::types::Design;

/// Cached fast-path structural gate/PG elaboration result.
#[derive(Debug)]
pub struct FastGateDesign {
    pub files: Vec<PathBuf>,
    pub file_digests: Vec<FileDigest>,
    pub top: String,
    pub struct_graph: Arc<StructuralGraph>,
    pub design: Design,
    pub build_ms: f64,
}

impl FastGateDesign {
    pub fn is_fresh(&self) -> bool {
        self.file_digests.iter().all(FileDigest::is_fresh)
    }

    pub fn struct_graph(&self) -> &StructuralGraph {
        &self.struct_graph
    }

    pub fn struct_graph_arc(&self) -> Arc<StructuralGraph> {
        Arc::clone(&self.struct_graph)
    }
}

/// Decides whether `files` should take the Liberty-backed structural fast path
/// instead of full `svlang` behavioral compilation.
///
/// Triggers when `liberty` is loaded and EITHER:
/// 1. Any instantiated module in `files` is a Liberty cell with no Verilog
///    module definition in `files` (e.g. `gate_100k.v` analyzed without `stdcells.v`), OR
/// 2. Total source size >= 256 KB and all non-Liberty modules are pure structural
///    (no `always`/`property`/`generate` behavioral RTL).
pub fn should_use_fast_path(files: &[PathBuf], liberty: &LibertyDb) -> bool {
    if files.is_empty() || liberty.cells.is_empty() {
        return false;
    }
    let total_bytes: u64 = files
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    let mut parsed_modules = Vec::new();
    for (idx, p) in files.iter().enumerate() {
        match parser::parse_verilog_file(p, idx as u32) {
            Ok(mods) => parsed_modules.extend(mods),
            Err(_) => return false,
        }
    }
    if parsed_modules.is_empty() {
        return false;
    }
    let defined: HashSet<&str> = parsed_modules.iter().map(|m| m.name.as_str()).collect();
    let mut has_missing_lib_inst = false;
    let mut has_any_lib_inst = false;
    for m in &parsed_modules {
        // Non-Liberty module with behavioral RTL must go through full `svlang`.
        if !liberty.contains_cell(&m.name) && m.has_behavioral {
            return false;
        }
        for inst in &m.insts {
            if liberty.contains_cell(&inst.module_name) {
                has_any_lib_inst = true;
                if !defined.contains(inst.module_name.as_str()) {
                    has_missing_lib_inst = true;
                }
            }
        }
    }
    has_missing_lib_inst || (total_bytes >= 256 * 1024 && has_any_lib_inst)
}

/// Parses and elaborates a structural Verilog / PG netlist directly against
/// `liberty`, reusing `prev` when the canonical file paths, top, and on-disk
/// SHA-256 digests match.
pub fn elaborate_fast_cached(
    prev: Option<&Arc<FastGateDesign>>,
    files: &[PathBuf],
    top: Option<&str>,
    liberty: &LibertyDb,
) -> Result<Arc<FastGateDesign>, String> {
    let canon_files: Vec<PathBuf> = files.iter().map(|p| canonicalize_path(p)).collect();
    if let Some(p) = prev {
        let same_top = top.is_none_or(|t| t == p.top);
        if same_top && p.files == canon_files && p.is_fresh() {
            return Ok(Arc::clone(p));
        }
    }

    let t0 = Instant::now();
    let mut all_modules = Vec::new();
    let mut file_names = Vec::with_capacity(files.len());
    let mut digests = Vec::with_capacity(files.len());

    for (idx, p) in files.iter().enumerate() {
        let canon = canonicalize_path(p);
        let bytes = std::fs::read(&canon)
            .map_err(|e| format!("gate_fast: cannot read `{}`: {e}", p.display()))?;
        let mtime = std::fs::metadata(&canon).ok().and_then(|m| m.modified().ok());
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        digests.push(FileDigest {
            path: canon,
            len: bytes.len() as u64,
            mtime,
            sha256,
        });
        let src = String::from_utf8(bytes)
            .map_err(|e| format!("gate_fast: `{}` is not valid UTF-8: {e}", p.display()))?;
        let fname = p.display().to_string();
        let mods = parser::parse_verilog_str(&src, idx as u32, &fname)?;
        file_names.push(fname);
        all_modules.extend(mods);
    }

    let res = elaborate::elaborate_structural(file_names, &all_modules, top, liberty)?;
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;

    Ok(Arc::new(FastGateDesign {
        files: canon_files,
        file_digests: digests,
        top: res.top,
        struct_graph: Arc::new(res.graph),
        design: res.design,
        build_ms,
    }))
}

/// Single-pass fast-path check + structural elaboration + cache lookup.
///
/// Returns `Ok(Some(fast))` when `files` qualify for the Liberty-backed
/// structural gate/PG fast path (missing Liberty module definitions or
/// large pure-structural gate netlist >= 256 KB), or `Ok(None)` to fall
/// back to full `svlang` elaboration.
pub fn try_elaborate_fast_cached(
    prev: Option<&Arc<FastGateDesign>>,
    files: &[PathBuf],
    top: Option<&str>,
    liberty: &LibertyDb,
) -> Result<Option<Arc<FastGateDesign>>, String> {
    if files.is_empty() || liberty.cells.is_empty() {
        return Ok(None);
    }
    let canon_files: Vec<PathBuf> = files.iter().map(|p| canonicalize_path(p)).collect();
    if let Some(p) = prev {
        let same_top = top.is_none_or(|t| t == p.top);
        if same_top && p.files == canon_files && p.is_fresh() {
            return Ok(Some(Arc::clone(p)));
        }
    }

    let t0 = Instant::now();
    let mut total_bytes = 0u64;
    let mut all_modules = Vec::new();
    let mut file_names = Vec::with_capacity(files.len());
    let mut digests = Vec::with_capacity(files.len());

    for (idx, p) in files.iter().enumerate() {
        let canon = canon_files[idx].clone();
        let Ok(bytes) = std::fs::read(&canon) else {
            return Ok(None);
        };
        total_bytes += bytes.len() as u64;
        let mtime = std::fs::metadata(&canon).ok().and_then(|m| m.modified().ok());
        let sha256: [u8; 32] = Sha256::digest(&bytes).into();
        digests.push(FileDigest {
            path: canon,
            len: bytes.len() as u64,
            mtime,
            sha256,
        });
        let Ok(src) = String::from_utf8(bytes) else {
            return Ok(None);
        };
        let fname = p.display().to_string();
        let Ok(mods) = parser::parse_verilog_str(&src, idx as u32, &fname) else {
            return Ok(None);
        };
        file_names.push(fname);
        all_modules.extend(mods);
    }
    if all_modules.is_empty() {
        return Ok(None);
    }

    let defined: HashSet<&str> = all_modules.iter().map(|m| m.name.as_str()).collect();
    let mut has_missing_lib_inst = false;
    let mut has_any_lib_inst = false;
    for m in &all_modules {
        if !liberty.contains_cell(&m.name) && m.has_behavioral {
            return Ok(None);
        }
        for inst in &m.insts {
            if liberty.contains_cell(&inst.module_name) {
                has_any_lib_inst = true;
                if !defined.contains(inst.module_name.as_str()) {
                    has_missing_lib_inst = true;
                }
            }
        }
    }
    if !(has_missing_lib_inst || (total_bytes >= 256 * 1024 && has_any_lib_inst)) {
        return Ok(None);
    }

    let res = elaborate::elaborate_structural(file_names, &all_modules, top, liberty)?;
    let build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(Some(Arc::new(FastGateDesign {
        files: canon_files,
        file_digests: digests,
        top: res.top,
        struct_graph: Arc::new(res.graph),
        design: res.design,
        build_ms,
    })))
}

fn canonicalize_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}
