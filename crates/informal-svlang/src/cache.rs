//! Two-tier fingerprinted compilation, elaboration, and netlist lowering
//! cache for `informal-svlang`.
//!
//! - **Tier 1 (`CompiledDesign`)**: caches the parsed `svlang::Compilation`,
//!   `svlang::Elaborated`, check diagnostics, and `FileDigest`s for every
//!   source file and `` `include `` header touched by `svlang`. Keyed by
//!   [`CompileFingerprint`] (order-preserved `files`, `incdirs`, `defines`,
//!   `top`, `params`, `black_box_modules`) and verified against on-disk SHA-256
//!   digests via [`CompiledDesign::is_fresh`].
//! - **Tier 2 (`CompiledDesign::netlists`)**: interior-mutable
//!   `Mutex<HashMap<LowerKey, Arc<LowerOutcome>>>` caching unmutated
//!   [`svlang::netlist::LowerOutcome`]s produced by
//!   [`svlang::netlist::lower_design`] without re-elaborating.

use crate::types::{Design, Diagnostic, Instance, Lowered, Request};
use sha2::{Digest, Sha256};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use svlang::netlist::{LowerConfig, LowerOutcome};
use svlang::Severity;

/// Thread-local cache activity counters for deterministic delta assertions in
/// parallel test runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub tier1_compiles: usize,
    pub tier1_hits: usize,
    pub tier2_lowers: usize,
    pub tier2_hits: usize,
}

impl CacheStats {
    const fn new() -> Self {
        Self {
            tier1_compiles: 0,
            tier1_hits: 0,
            tier2_lowers: 0,
            tier2_hits: 0,
        }
    }
}

thread_local! {
    static CACHE_STATS: Cell<CacheStats> = const { Cell::new(CacheStats::new()) };
}

/// Snapshot the calling thread's cumulative cache statistics.
pub fn cache_stats() -> CacheStats {
    CACHE_STATS.with(Cell::get)
}

fn bump_tier1_compile() {
    CACHE_STATS.with(|c| {
        let mut s = c.get();
        s.tier1_compiles += 1;
        c.set(s);
    });
}

fn bump_tier1_hit() {
    CACHE_STATS.with(|c| {
        let mut s = c.get();
        s.tier1_hits += 1;
        c.set(s);
    });
}

fn bump_tier2_lower() {
    CACHE_STATS.with(|c| {
        let mut s = c.get();
        s.tier2_lowers += 1;
        c.set(s);
    });
}

fn bump_tier2_hit() {
    CACHE_STATS.with(|c| {
        let mut s = c.get();
        s.tier2_hits += 1;
        c.set(s);
    });
}

fn canonicalize_path(p: &Path) -> PathBuf {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()))
}

/// Content + metadata fingerprint of a single source or `` `include `` file
/// read during compilation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    pub path: PathBuf,
    pub len: u64,
    pub mtime: Option<SystemTime>,
    pub sha256: [u8; 32],
}

impl FileDigest {
    /// Verify that the file on disk still exists, has matching byte length, and
    /// hashes to the exact same SHA-256 digest (catching same-length edits with
    /// preserved or coarse `mtime`).
    pub fn is_fresh(&self) -> bool {
        let Ok(meta) = std::fs::metadata(&self.path) else {
            return false;
        };
        if meta.len() != self.len {
            return false;
        }
        let Ok(bytes) = std::fs::read(&self.path) else {
            return false;
        };
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        digest == self.sha256
    }
}

fn collect_file_digests(trees: &[svlang::Tree]) -> Vec<FileDigest> {
    let mut unique: BTreeMap<PathBuf, &[u8]> = BTreeMap::new();
    for t in trees {
        for (file, src) in t.files.iter().zip(&t.sources) {
            if file.as_os_str().is_empty() || file.to_string_lossy().contains('\0') {
                continue;
            }
            let canon = canonicalize_path(file);
            unique.entry(canon).or_insert_with(|| src.as_bytes());
        }
    }
    unique
        .into_iter()
        .map(|(path, src_bytes)| {
            let (len, mtime, sha256) = match std::fs::metadata(&path) {
                Ok(meta) => {
                    let sha = match std::fs::read(&path) {
                        Ok(disk_bytes) => Sha256::digest(&disk_bytes).into(),
                        Err(_) => Sha256::digest(src_bytes).into(),
                    };
                    (meta.len(), meta.modified().ok(), sha)
                }
                Err(_) => (
                    src_bytes.len() as u64,
                    None,
                    Sha256::digest(src_bytes).into(),
                ),
            };
            FileDigest {
                path,
                len,
                mtime,
                sha256,
            }
        })
        .collect()
}

/// Tier-1 compilation & elaboration fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileFingerprint {
    pub files: Vec<PathBuf>,
    pub incdirs: Vec<PathBuf>,
    pub defines: Vec<(String, Option<String>)>,
    pub top: Option<String>,
    pub params: Vec<(String, String)>,
    pub black_box_modules: Vec<String>,
}

impl CompileFingerprint {
    pub fn from_request(req: &Request) -> Self {
        Self {
            files: req.files.iter().map(|p| canonicalize_path(p)).collect(),
            incdirs: req.incdirs.iter().map(|p| canonicalize_path(p)).collect(),
            defines: req.defines.clone(),
            top: req.top.clone(),
            params: req.params.clone(),
            black_box_modules: req.black_box_modules.clone(),
        }
    }
}

/// Tier-2 netlist lowering key within a [`CompiledDesign`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LowerKey {
    pub top: Option<String>,
    pub cut_points: Vec<String>,
    pub black_box_modules: Vec<String>,
    pub black_box_instances: Vec<String>,
    pub blast_memories_up_to: Option<usize>,
    pub skip_initial_checks: bool,
    pub isunknown_is_false: bool,
    pub bbox_mul_width: Option<u32>,
    pub bbox_array_words: Option<usize>,
}

/// Cached compilation, elaboration, diagnostics, file digests, and Tier-2
/// lowered netlists.
pub struct CompiledDesign {
    pub fingerprint: CompileFingerprint,
    pub file_digests: Vec<FileDigest>,
    pub compilation: svlang::Compilation,
    pub elaborated: svlang::Elaborated,
    pub diagnostics: Vec<Diagnostic>,
    pub check_ok: bool,
    pub inferred_single_top: Option<String>,
    netlists: Mutex<HashMap<LowerKey, Arc<LowerOutcome>>>,
    tier2_lowers: AtomicUsize,
    tier2_hits: AtomicUsize,
    /// Lazily built hierarchical pin-net structural graph (`OnceLock` preserves `Send + Sync`).
    pub struct_graph: OnceLock<crate::struct_graph::StructuralGraph>,
    struct_graph_build_ns: AtomicU64,
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CompiledDesign>();
};

impl std::fmt::Debug for CompiledDesign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached_netlists = self.netlists.lock().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("CompiledDesign")
            .field("fingerprint", &self.fingerprint)
            .field("file_digests_len", &self.file_digests.len())
            .field("diagnostics_len", &self.diagnostics.len())
            .field("check_ok", &self.check_ok)
            .field("inferred_single_top", &self.inferred_single_top)
            .field("cached_netlists", &cached_netlists)
            .field("tier2_lowers", &self.tier2_lowers())
            .field("tier2_hits", &self.tier2_hits())
            .field("struct_graph_cached", &self.struct_graph.get().is_some())
            .finish()
    }
}

impl CompiledDesign {
    /// Returns `true` if every source and `` `include `` file recorded in
    /// `file_digests` still matches its on-disk length and SHA-256 digest.
    pub fn is_fresh(&self) -> bool {
        self.file_digests.iter().all(FileDigest::is_fresh)
    }

    /// Number of Tier-2 `lower_design` executions performed on this instance.
    pub fn tier2_lowers(&self) -> usize {
        self.tier2_lowers.load(Ordering::Relaxed)
    }

    /// Number of Tier-2 netlist cache hits served by this instance.
    pub fn tier2_hits(&self) -> usize {
        self.tier2_hits.load(Ordering::Relaxed)
    }

    fn instantiated_black_boxes(&self, modules: &[String]) -> Vec<String> {
        let instantiated: HashSet<&str> = self
            .elaborated
            .design
            .instances
            .iter()
            .filter(|i| !i.is_interface || i.parent.is_some())
            .map(|i| i.definition.as_str())
            .collect();
        let mut out: Vec<String> = modules
            .iter()
            .filter(|m| instantiated.contains(m.as_str()))
            .cloned()
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Returns `true` if `req` matches this compiled design's Tier-1 inputs,
    /// accounting for `top: None` -> `inferred_single_top` equivalence and
    /// instantiated-module `black_box_modules` pruning when `self.check_ok`.
    pub fn matches_request(&self, req: &Request) -> bool {
        let req_fp = CompileFingerprint::from_request(req);
        if self.fingerprint.files != req_fp.files
            || self.fingerprint.incdirs != req_fp.incdirs
            || self.fingerprint.defines != req_fp.defines
            || self.fingerprint.params != req_fp.params
        {
            return false;
        }
        let top_ok = self.fingerprint.top == req_fp.top
            || (self.fingerprint.top.is_none()
                && self.check_ok
                && req_fp.top.is_some()
                && req_fp.top == self.inferred_single_top);
        if !top_ok {
            return false;
        }
        if self.check_ok {
            self.instantiated_black_boxes(&self.fingerprint.black_box_modules)
                == self.instantiated_black_boxes(&req_fp.black_box_modules)
        } else {
            self.fingerprint.black_box_modules == req_fp.black_box_modules
        }
    }

    /// Lower or fetch the unmutated [`LowerOutcome`] for `key`.
    pub fn lower_netlist_cached(&self, key: &LowerKey) -> Arc<LowerOutcome> {
        let mut guard = self.netlists.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(hit) = guard.get(key) {
            self.tier2_hits.fetch_add(1, Ordering::Relaxed);
            bump_tier2_hit();
            return Arc::clone(hit);
        }

        let config = LowerConfig {
            top: key.top.clone(),
            blast_memories_up_to: key.blast_memories_up_to,
            cut_points: key.cut_points.clone(),
            black_box_modules: key.black_box_modules.clone(),
            black_box_instances: key.black_box_instances.clone(),
            skip_initial_checks: key.skip_initial_checks,
            isunknown_is_false: key.isunknown_is_false,
            bbox_mul_width: key.bbox_mul_width,
            bbox_array_words: key.bbox_array_words,
            ..LowerConfig::default()
        };
        let outcome = Arc::new(svlang::netlist::lower_design(
            self.compilation.trees(),
            &self.elaborated.design,
            &config,
        ));
        self.tier2_lowers.fetch_add(1, Ordering::Relaxed);
        bump_tier2_lower();
        guard.insert(key.clone(), Arc::clone(&outcome));
        outcome
    }

    /// Lazily build or return the cached hierarchical [`crate::struct_graph::StructuralGraph`].
    pub fn struct_graph(&self) -> &crate::struct_graph::StructuralGraph {
        self.struct_graph.get_or_init(|| {
            let t0 = std::time::Instant::now();
            let top = self
                .fingerprint
                .top
                .as_deref()
                .or(self.inferred_single_top.as_deref());
            let g = crate::struct_graph::StructuralGraph::build(
                self.compilation.trees(),
                &self.elaborated.design,
                top,
            );
            self.struct_graph_build_ns
                .store(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
            g
        })
    }

    /// Time taken to build `struct_graph` on first initialization (ms).
    pub fn struct_graph_build_ms(&self) -> f64 {
        let _ = self.struct_graph();
        self.struct_graph_build_ns.load(Ordering::Relaxed) as f64 / 1_000_000.0
    }
}

/// Compile and elaborate `req`, reusing `prev` if its Tier-1 fingerprint
/// matches `req` and all tracked source/include files are still fresh on disk.
pub fn compile_cached(
    prev: Option<&Arc<CompiledDesign>>,
    req: &Request,
) -> Result<Arc<CompiledDesign>, String> {
    compile_cached_cancel(prev, req, None)
}

/// [`compile_cached`] with cooperative cancellation polled between compile,
/// elaboration, and digest stages. Returns `Err("cancelled")` when tripped.
pub fn compile_cached_cancel(
    prev: Option<&Arc<CompiledDesign>>,
    req: &Request,
    cancel: Option<&AtomicBool>,
) -> Result<Arc<CompiledDesign>, String> {
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::SeqCst));
    if cancelled() {
        return Err("cancelled".into());
    }
    if let Some(cached) = prev {
        if cached.matches_request(req) && cached.is_fresh() {
            bump_tier1_hit();
            return Ok(Arc::clone(cached));
        }
    }
    if cancelled() {
        return Err("cancelled".into());
    }

    let fingerprint = CompileFingerprint::from_request(req);
    let compilation = crate::driver(req)
        .compile()
        .map_err(|e| format!("svlang: {e}"))?;
    if cancelled() {
        return Err("cancelled".into());
    }
    let (diags, elaborated) = compilation.check();
    if cancelled() {
        return Err("cancelled".into());
    }
    let diagnostics: Vec<Diagnostic> = diags
        .iter()
        .map(|d| Diagnostic {
            severity: match d.severity {
                Severity::Error => "error",
                _ => "warning",
            }
            .into(),
            message: d.message.clone(),
            file: (!d.file.is_empty()).then(|| d.file.clone()),
            line: (d.line > 0).then_some(d.line),
            column: (d.column > 0).then_some(d.column),
        })
        .collect();
    let check_ok = !diagnostics.iter().any(|d| d.severity == "error");
    let inferred_single_top = if req.top.is_none() && check_ok && elaborated.design.tops.len() == 1
    {
        Some(
            elaborated
                .design
                .instance(elaborated.design.tops[0])
                .name
                .clone(),
        )
    } else {
        None
    };
    let file_digests = collect_file_digests(compilation.trees());
    bump_tier1_compile();

    Ok(Arc::new(CompiledDesign {
        fingerprint,
        file_digests,
        compilation,
        elaborated,
        diagnostics,
        check_ok,
        inferred_single_top,
        netlists: Mutex::new(HashMap::new()),
        tier2_lowers: AtomicUsize::new(0),
        tier2_hits: AtomicUsize::new(0),
        struct_graph: OnceLock::new(),
        struct_graph_build_ns: AtomicU64::new(0),
    }))
}

/// Build a [`Design`] view from a cached [`CompiledDesign`], lowering its
/// un-boxed/un-blasted netlist via Tier 2 to gather [`DesignStats`] and RTL
/// properties.
pub fn design_from_compiled(compiled: &CompiledDesign, req: &Request) -> Design {
    let ok = compiled.check_ok;
    let (mut properties, mut not_lowered) = (Vec::new(), Vec::new());
    let mut stats = None;
    if ok {
        let mut stub_modules: Vec<String> = compiled
            .elaborated
            .design
            .instances
            .iter()
            .filter(|i| i.is_blackbox_stub)
            .map(|i| i.definition.clone())
            .collect();
        stub_modules.sort();
        stub_modules.dedup();
        let key = LowerKey {
            top: req
                .top
                .clone()
                .or_else(|| compiled.inferred_single_top.clone()),
            cut_points: Vec::new(),
            black_box_modules: stub_modules,
            black_box_instances: Vec::new(),
            blast_memories_up_to: None,
            skip_initial_checks: req.skip_initial_checks,
            isunknown_is_false: req.isunknown_is_false,
            bbox_mul_width: req.bbox_mul_width,
            bbox_array_words: req.bbox_array_words,
        };
        let outcome = compiled.lower_netlist_cached(&key);
        match outcome.as_ref() {
            LowerOutcome::Lowered(nl_ref) => {
                let mut nl = nl_ref.clone();
                stats = Some(crate::design_stats(&nl));
                properties = crate::rtl_properties(&mut nl, 0, &[], false, false);
                crate::immediate_checks(&nl, &mut properties);
            }
            LowerOutcome::Unsupported(ds) => {
                not_lowered = ds
                    .iter()
                    .map(|d| {
                        format!(
                            "not supported yet: {}{}",
                            crate::show(d),
                            crate::hint(&d.message)
                        )
                    })
                    .collect();
            }
            LowerOutcome::Error(ds) => not_lowered = ds.iter().map(crate::show).collect(),
        }
    }
    let e = &compiled.elaborated.design;
    Design {
        ok,
        diagnostics: compiled.diagnostics.clone(),
        tops: e.tops.iter().map(|&t| e.instance(t).name.clone()).collect(),
        instances: e
            .instances
            .iter()
            .filter(|i| !i.is_interface || i.parent.is_some())
            .map(|i| Instance {
                path: i.path.clone(),
                module: i.definition.clone(),
            })
            .collect(),
        signals: crate::signals(e),
        hierarchy: e.scope_tree().iter().map(crate::scope).collect(),
        properties,
        not_lowered,
        stats: stats.map(|mut st| {
            st.instances = e
                .instances
                .iter()
                .filter(|i| !i.is_interface || i.parent.is_some())
                .count();
            let mut modules: Vec<&str> =
                e.instances.iter().map(|i| i.definition.as_str()).collect();
            modules.sort_unstable();
            modules.dedup();
            st.modules = modules.len();
            st
        }),
        scope_names: crate::collect_scope_names(compiled.compilation.trees(), e),
    }
}

/// Lower a cached [`CompiledDesign`] into Btor2 (`Lowered`) without repeating
/// parsing or elaboration. Fails closed when `compiled.check_ok` is `false`.
pub fn lower_from_compiled(compiled: &CompiledDesign, req: &Request) -> Lowered {
    lower_from_compiled_cancel(compiled, req, None)
}

/// [`lower_from_compiled`] with cooperative cancellation polled across netlist
/// lowering, property compilation, and Btor2 emission.
pub fn lower_from_compiled_cancel(
    compiled: &CompiledDesign,
    req: &Request,
    cancel: Option<&AtomicBool>,
) -> Lowered {
    let fail = |errors: Vec<String>| Lowered {
        ok: false,
        errors,
        ..Default::default()
    };
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::SeqCst));
    if cancelled() {
        return fail(vec!["cancelled".into()]);
    }
    if !compiled.check_ok {
        let errors: Vec<String> = compiled
            .diagnostics
            .iter()
            .filter(|d| d.severity == "error")
            .map(|d| match &d.file {
                Some(file) if !file.is_empty() => {
                    format!("{file}:{}: {}", d.line.unwrap_or(0), d.message)
                }
                _ => d.message.clone(),
            })
            .collect();
        return fail(if errors.is_empty() {
            vec!["compilation failed".to_string()]
        } else {
            errors
        });
    }

    let key = LowerKey {
        top: req
            .top
            .clone()
            .or_else(|| compiled.inferred_single_top.clone()),
        cut_points: req.cut_points.clone(),
        black_box_modules: req.black_box_modules.clone(),
        black_box_instances: req.black_box_instances.clone(),
        blast_memories_up_to: Some(crate::BLAST_WORDS),
        skip_initial_checks: req.skip_initial_checks,
        isunknown_is_false: req.isunknown_is_false,
        bbox_mul_width: req.bbox_mul_width,
        bbox_array_words: req.bbox_array_words,
    };
    let outcome = compiled.lower_netlist_cached(&key);
    if cancelled() {
        return fail(vec!["cancelled".into()]);
    }
    let base = match outcome.as_ref() {
        LowerOutcome::Lowered(nl) => nl,
        LowerOutcome::Unsupported(ds) => {
            return fail(
                ds.iter()
                    .map(|d| {
                        format!(
                            "not supported yet: {}{}",
                            crate::show(d),
                            crate::hint(&d.message)
                        )
                    })
                    .collect(),
            )
        }
        LowerOutcome::Error(ds) => return fail(ds.iter().map(crate::show).collect()),
    };

    let reset = u32::from(req.reset.is_some());
    let mut built = crate::model_cancel(
        req,
        &compiled.compilation,
        &compiled.elaborated.design,
        base,
        reset,
        reset,
        &[],
        cancel,
    );
    if matches!(&built, Ok((m, _)) if m.steps_per_cycle > 1) && req.reset.is_some() {
        built = crate::model_cancel(
            req,
            &compiled.compilation,
            &compiled.elaborated.design,
            base,
            req.tick.steps,
            req.tick.cycles,
            &req.tick.by_clock,
            cancel,
        );
    }
    match built {
        Ok((m, properties)) => {
            let mut notes: Vec<String> = m
                .notes
                .into_iter()
                .filter(|n| !n.contains("compile_properties"))
                .collect();
            for p in &properties {
                if let Some(e) = &p.error {
                    notes.push(format!("{} {} is not checked: {e}", p.kind, p.name));
                }
                if let Some(why) = &p.unsupported {
                    notes.push(format!(
                        "{} {} is not checked: {why} is not supported yet",
                        p.kind, p.name
                    ));
                }
            }
            Lowered {
                ok: true,
                errors: Vec::new(),
                notes,
                btor2: m.btor2,
                properties,
                half_cycle: m.steps_per_cycle > 1,
            }
        }
        Err(errors) => fail(errors),
    }
}
