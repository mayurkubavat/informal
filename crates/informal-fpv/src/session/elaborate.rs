use super::types::*;
use super::{drop_assumptions, has_target, only_target, target_name};
use anyhow::bail;

impl Session {
    /// The Btor2 model property `i` is proved on: the whole design with
    /// every assumption of its task and this one target. Lowers the design.
    pub fn model_for(&self, i: usize) -> Result<String, String> {
        let (btor, _) = self.lower(&self.props[i].task)?;
        let keep = self.check_name(i);
        if !has_target(&btor, &keep) {
            return Err(format!("no check named {keep} in the model"));
        }
        let (model, _) = informal_svlang::split_trace(&btor);
        Ok(only_target(model, &keep))
    }

    /// With slang's view of the design, reject names the top module does
    /// not declare (typos would otherwise become implicit nets that make a
    /// property vacuous).
    pub(super) fn check_names(&self, expr: &str) -> Result<(), String> {
        if let (Some(d), Some(top)) = (&self.elab, &self.design.top) {
            let unknown = d.unknown_names(top, expr);
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown name(s) in module {top}: {}",
                    unknown.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// With slang's view of the design, reject a reset expression or clock
    /// that names a signal the top module does not declare. (Unchecked, a
    /// typo would leave the real reset free and give a wrong verdict.)
    pub(crate) fn check_reset_and_clocks(&self) -> Result<(), String> {
        if let Some(r) = &self.reset {
            self.check_names(r)
                .map_err(|e| format!("reset {{{r}}}: {e}"))?;
        }
        for c in &self.clocks {
            self.check_names(&c.signal)
                .map_err(|e| format!("clock {}: {e}", c.signal))?;
        }
        Ok(())
    }

    /// `stopat`: the path of a signal to cut, from the top module (a
    /// leading `<top>.` is accepted). After elaboration the name must be a
    /// signal of the design; before it, the check happens at prove.
    pub fn cut_point(&self, name: &str) -> Result<String, String> {
        let top = self.design.top.as_deref().unwrap_or("");
        let rel = name
            .strip_prefix(top)
            .and_then(|r| r.strip_prefix('.'))
            .filter(|_| !top.is_empty())
            .unwrap_or(name);
        if let (Some(d), false) = (&self.elab, top.is_empty()) {
            let full = format!("{top}.{rel}");
            // A whole signal, or a part of one (`pkt.valid`, `mem[3]`).
            let known = d.signals.iter().filter(|s| !s.constant).any(|s| {
                full.strip_prefix(s.path.as_str())
                    .is_some_and(|r| r.is_empty() || r.starts_with(['.', '[']))
            });
            if !known {
                return Err(format!("`{name}` is not a signal of {top}"));
            }
        }
        Ok(rel.to_string())
    }

    /// `task`'s cut points as the front end wants them: full paths.
    fn cut_points(&self, task: &str) -> Vec<String> {
        let top = self.design.top.as_deref().unwrap_or("");
        self.stopats_of(task)
            .iter()
            .map(|s| format!("{top}.{s}"))
            .collect()
    }

    pub fn elaborate(&mut self, top: Option<String>) -> anyhow::Result<()> {
        if let Some(t) = top {
            self.design.top = Some(t);
        }
        if self.dry_run {
            // Check mode: the design is not read (its files may not exist here).
            self.elaborated = true;
            return Ok(());
        }
        if self.design.files.is_empty() {
            bail!("no design files: run analyze / read_file first");
        }
        let prev = self.compiled.take();
        let req = informal_svlang::Request {
            files: self.design.files.clone(),
            defines: self.design.defines.clone(),
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            black_box_modules: self.design.bbox.clone(),
            bbox_mul_width: self.design.bbox_mul,
            bbox_array_words: self.design.bbox_a,
            trace_outputs: false,
            skip_initial_checks: self.design.skip_initial_checks,
            isunknown_is_false: self.design.isunknown_is_false,
            ..Default::default()
        };
        self.design.hierarchy.clear();
        let compiled = informal_svlang::compile_cached(prev.as_ref(), &req)
            .map_err(|e| anyhow::anyhow!("SystemVerilog errors:\nerror: {e}"))?;
        let d = informal_svlang::design_from_compiled(&compiled, &req);
        let errors: Vec<String> = d.errors().take(20).map(|e| e.to_string()).collect();
        if !errors.is_empty() {
            bail!("SystemVerilog errors:\n{}", errors.join("\n"));
        }
        if self.design.top.is_none() {
            match d.tops.as_slice() {
                [t] => self.design.top = Some(t.clone()),
                tops => bail!("cannot tell the top module (candidates: {tops:?}); use -top"),
            }
        }
        self.hierarchy = d.instances.clone();
        self.design.hierarchy = d.hierarchy.clone();
        // A design with no model lists no properties of its own. Say so here,
        // with the reasons: an empty table must not read as "nothing to
        // prove". A property given by command gets the same reasons as its
        // error, and a prove with nothing to run fails with them.
        if !d.not_lowered.is_empty() {
            self.log("NOT MODELLED: the design does not lower, so its own properties are not listed and nothing can be proved until this is resolved:");
            for line in d.not_lowered.iter().take(20) {
                self.log(&format!("  {line}"));
            }
        }
        self.rtl_properties(&d.properties);
        // Every task lists the design's assumptions (see create_task).
        let design_assumes: Vec<usize> = (0..self.props.len())
            .filter(|&i| self.props[i].rtl && self.props[i].kind == Kind::Assume)
            .collect();
        let others: Vec<String> = self.tasks[1..].iter().map(|t| t.name.clone()).collect();
        for t in others {
            let _ = self.copy_into(&t, &design_assumes);
        }
        self.elab = Some(d);
        std::fs::create_dir_all(&self.workdir)?;
        let tree = self.workdir.join("design.hierarchy.json");
        if self.design.hierarchy.is_empty() {
            let _ = std::fs::remove_file(&tree);
        } else {
            std::fs::write(&tree, serde_json::to_string_pretty(&self.design.hierarchy)?)?;
        }
        self.check_black_boxes()?;
        self.mark_boxed_properties();
        self.compiled = Some(compiled);
        self.elaborated = true;
        self.lp.invalidate_design();
        Ok(())
    }

    /// List the concurrent properties written in the RTL beside the ones
    /// commands created, replacing those of an earlier elaboration.
    fn rtl_properties(&mut self, found: &[informal_svlang::RtlProperty]) {
        self.props.retain(|p| !p.rtl);
        for f in found {
            if self.props.iter().any(|p| p.name == f.name) {
                self.log(&format!(
                    "{} in the RTL is not listed: a property of that name exists",
                    f.name
                ));
                continue;
            }
            self.props.push(Property {
                name: f.name.clone(),
                kind: match f.kind.as_str() {
                    "assume" => Kind::Assume,
                    "cover" => Kind::Cover,
                    _ => Kind::Assert,
                },
                // As written in the design, for the reader: the model has
                // its own compiled check.
                expr: f.text.clone().unwrap_or_default(),
                enabled: true,
                status: Status::Unprocessed,
                trace: None,
                source: f.file.as_ref().map(|file| SourceLoc {
                    file: file.clone(),
                    line: f.line.unwrap_or(0),
                }),
                rtl: true,
                engines: Vec::new(),
                ai: false,
                under_ai: Vec::new(),
                vacuity: None,
                witness_trace: None,
                task: EMBEDDED.to_string(),
                copy_of: None,
                bound: None,
                proof_core: None,
                from_assert: None,
                rtl_from_assert: false,
                stats: None,
            });
        }
    }

    /// Match the requested black boxes against the elaborated design. A
    /// module with no instance here is dropped with a note (flow scripts
    /// list boxes for every configuration); an instance path that does not
    /// exist, or the top module, is an error.
    fn check_black_boxes(&mut self) -> anyhow::Result<()> {
        let (Some(d), Some(top)) = (&self.elab, self.design.top.clone()) else {
            return Ok(());
        };
        if self.design.bbox.contains(&top) {
            bail!("the top module {top} cannot be a black box");
        }
        let (used, unused): (Vec<String>, Vec<String>) = std::mem::take(&mut self.design.bbox)
            .into_iter()
            .partition(|m| d.instances.iter().any(|i| &i.module == m));
        self.design.bbox = used;
        let mut paths = Vec::new();
        for p in std::mem::take(&mut self.design.bbox_instances) {
            let rel = p
                .strip_prefix(top.as_str())
                .and_then(|r| r.strip_prefix('.'))
                .unwrap_or(&p)
                .to_string();
            if !d.instances.iter().any(|i| i.path == format!("{top}.{rel}")) {
                bail!("black box `{p}` is not an instance of {top}");
            }
            paths.push(rel);
        }
        self.design.bbox_instances = paths;
        for m in unused {
            self.log(&format!(
                "black box {m}: the design has no instance of that module (ignored)"
            ));
        }
        Ok(())
    }

    /// Mark the properties written inside a black box at elaboration: the
    /// box has no contents, so they are not in the model. They stay in the
    /// list (no property is dropped without a word), with the reason as
    /// their status, and the log says which they are.
    fn mark_boxed_properties(&mut self) {
        let (Some(d), Some(top)) = (&self.elab, self.design.top.as_deref()) else {
            return;
        };
        let boxed: Vec<String> = d
            .instances
            .iter()
            .filter(|i| self.design.bbox.contains(&i.module))
            .map(|i| format!("{}.", i.path))
            .chain(
                self.design
                    .bbox_instances
                    .iter()
                    .map(|r| format!("{top}.{r}.")),
            )
            .collect();
        if boxed.is_empty() {
            return;
        }
        let mut inside = Vec::new();
        for p in self.props.iter_mut() {
            if p.rtl && boxed.iter().any(|b| p.name.starts_with(b.as_str())) {
                p.status = Status::Error {
                    message: "not in the model (inside a black box)".to_string(),
                };
                inside.push(p.name.clone());
            }
        }
        if !inside.is_empty() {
            self.log(&format!(
                "{} propert{} inside black boxes cannot be proved (not in the model): {}",
                inside.len(),
                if inside.len() == 1 { "y" } else { "ies" },
                inside.join(", ")
            ));
        }
    }

    /// How long the reset lasts in a model that drives the clocks (a design
    /// on both clock edges, several clocks or computed clocks), where one
    /// model step is half a period of a `-factor 1` clock: until every
    /// declared clock has had a whole period, and then on to the first
    /// clock's next rising edge, so that registers on any clock and edge
    /// see the reset and the properties start with a clock cycle. With it,
    /// for each declared clock and edge, how many of its edges fall in the
    /// reset: a property on that clock and edge skips those attempts.
    pub(super) fn tick_reset(&self) -> informal_svlang::TickReset {
        if self.reset.is_none() {
            return informal_svlang::TickReset::default();
        }
        // With no clock declared (a property names its own), the one clock
        // in use toggles on every step.
        let default = Clock {
            signal: String::new(),
            edge: "posedge",
            factor: 1,
            phase: 0,
        };
        let c0 = self.clocks.first().unwrap_or(&default);
        // Clock c first completes a period at step phase + 2 * factor, and
        // the first clock's n-th rising edge ends step phase0 + factor0 - 1
        // + (n - 1) * period0.
        let whole = |c: &Clock| c.phase + 2 * c.factor;
        let window = self.clocks.iter().map(whole).max().unwrap_or(whole(c0));
        let rest = window.saturating_sub(c0.phase + c0.factor);
        let cycles = rest.div_ceil(2 * c0.factor) + 1;
        let distinct = self.clocks.iter().any(|c| c.signal != c0.signal);
        // One clock: the reset ends with its first whole period, as soon as
        // its falling edge has passed.
        let steps = match distinct {
            true => c0.phase + c0.factor + (cycles - 1) * 2 * c0.factor,
            false => window,
        };
        let mut by_clock = Vec::new();
        for c in &self.clocks {
            let period = 2 * c.factor;
            let shift = (period - c.phase % period) % period;
            for (posedge, last) in [(true, c.factor - 1), (false, period - 1)] {
                let edges = (0..steps).filter(|t| (t + shift) % period == last).count();
                by_clock.push((c.signal.clone(), posedge, edges as u32));
            }
        }
        informal_svlang::TickReset {
            steps,
            cycles: if distinct { cycles } else { 1 },
            by_clock,
        }
    }

    /// The enabled properties given by command, each as the name of its
    /// check and a whole directive on its clock: its own clocking event, or
    /// the first declared clock.
    fn bound_properties(&self, task: &str) -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        let given = |p: &&Property| {
            !p.rtl && !p.rtl_from_assert && p.copy_of.is_none() && p.enabled && p.task == task
        };
        for p in self.props.iter().filter(given) {
            let expr = p.expr.trim();
            let clocked = if expr.starts_with('@') {
                expr.to_string()
            } else {
                let c = self.clocks.first().ok_or_else(|| {
                    format!(
                        "{}: no clock: declare one with `clock` / `create_clock`",
                        p.name
                    )
                })?;
                format!("@({} {}) {expr}", c.edge, c.signal)
            };
            out.push((
                target_name(&p.name),
                format!("{} property ({clocked})", p.kind.label()),
            ));
        }
        Ok(out)
    }

    /// One Btor2 model with every enabled property, and the properties as
    /// the front end compiled them: those written in the RTL, then those
    /// given by command. `Err` rejects the design: a compile error, a reset
    /// that does not bind, or a construct the front end does not model.
    /// Nothing is guessed: a guess would turn a typo into a free signal
    /// (D13).
    pub(super) fn lower(
        &self,
        task: &str,
    ) -> Result<(String, Vec<informal_svlang::RtlProperty>), String> {
        self.lower_opts(task, false, false)
    }

    pub(crate) fn lower_for_coverage(
        &self,
        task: &str,
        branch_covers: bool,
        toggle_covers: bool,
    ) -> Result<(String, Vec<informal_svlang::RtlProperty>), String> {
        self.lower_opts(task, branch_covers, toggle_covers)
    }

    fn lower_opts(
        &self,
        task: &str,
        branch_covers: bool,
        toggle_covers: bool,
    ) -> Result<(String, Vec<informal_svlang::RtlProperty>), String> {
        let req = informal_svlang::Request {
            files: self.design.files.clone(),
            defines: self.design.defines.clone(),
            incdirs: self.design.incdirs.clone(),
            top: self.design.top.clone(),
            params: self.design.params.clone(),
            // Appends named outputs for combinational nets after
            // informal_svlang::TRACE_MARKER: engines get the text before it
            // (unchanged), the trace replay all of it.
            trace_outputs: true,
            cut_points: self.cut_points(task),
            black_box_modules: self.design.bbox.clone(),
            black_box_instances: self
                .design
                .bbox_instances
                .iter()
                .map(|i| format!("{}.{i}", self.design.top.as_deref().unwrap_or("")))
                .collect(),
            properties: self.bound_properties(task)?,
            clocks: self
                .clocks
                .iter()
                .map(|c| (c.signal.clone(), c.factor, c.phase))
                .collect(),
            cover_targets: true,
            precondition_covers: self.preconditions && !branch_covers && !toggle_covers,
            witness_covers: (self.witnesses || self.effort == Effort::Max)
                && !branch_covers
                && !toggle_covers,
            bbox_mul_width: self.design.bbox_mul,
            bbox_array_words: self.design.bbox_a,
            branch_covers,
            toggle_covers,
            reset: self.reset.clone(),
            tick: self.tick_reset(),
            skip_initial_checks: self.design.skip_initial_checks,
            isunknown_is_false: self.design.isunknown_is_false,
        };
        if let Some(w) = req.bbox_mul_width {
            self.log(&format!(
                "elaborate -bbox_mul {w}: variable-by-variable multipliers/dividers of width >= {w} are free cuts in this model"
            ));
        }
        if let Some(n) = req.bbox_array_words {
            self.log(&format!(
                "elaborate -bbox_a {n}: memories of >= {n} words have free read ports in this model"
            ));
        }
        // What the user chose to leave out is said with every model.
        if req.skip_initial_checks {
            self.log("set_lowering skip_initial_checks is on: assertions in initial blocks that check a signal at time zero are left out of this model");
        }
        if req.isunknown_is_false {
            self.log("set_lowering isunknown_is_false is on: $isunknown is 0 in this model, so a property that only says a signal is known holds without saying anything about the design");
        }
        if !(req.black_box_modules.is_empty() && req.black_box_instances.is_empty()) {
            self.log(&format!(
                "black boxes in this model (outputs free, contents not modelled): {}. A proof holds for the design; a counterexample may use output values the boxed logic cannot produce",
                self.design
                    .bbox
                    .iter()
                    .chain(&self.design.bbox_instances)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !req.cut_points.is_empty() {
            let cuts = self.stopats_of(task);
            let of = match task {
                EMBEDDED => String::new(),
                t => format!(" of task {t}"),
            };
            self.log(&format!(
                "{} cut point(s) (stopat){of} are free in this model: {}. A proof holds for the design; a counterexample may use values the design cannot produce there",
                cuts.len(),
                cuts.join(", ")
            ));
        }
        let l = match self.compiled.as_deref() {
            Some(c) if c.matches_request(&req) && c.is_fresh() => {
                informal_svlang::lower_from_compiled(c, &req)
            }
            _ => informal_svlang::lower(&req),
        };
        for n in &l.notes {
            self.log(&format!("svlang: {n}"));
        }
        if l.ok {
            let disabled = self.disabled_rtl_assumptions(task);
            let mut btor2 = if disabled.is_empty() {
                l.btor2
            } else {
                let of = match task {
                    EMBEDDED => String::new(),
                    t => format!(" in task {t}"),
                };
                self.log(&format!(
                    "{} RTL assumption(s) disabled{of} and left out of this model: {}",
                    disabled.len(),
                    disabled.join(", ")
                ));
                drop_assumptions(&l.btor2, &disabled)
            };
            let rtl_helpers: Vec<(String, String)> = self
                .props
                .iter()
                .filter(|p| {
                    p.enabled && p.kind == Kind::Assume && p.task == task && p.rtl_from_assert
                })
                .filter_map(|p| {
                    p.from_assert
                        .as_ref()
                        .map(|orig| (orig.clone(), target_name(&p.name)))
                })
                .collect();
            if !rtl_helpers.is_empty() {
                btor2 = inject_rtl_assert_assumptions(&btor2, &rtl_helpers);
            }
            if !self.counter_abstractions.is_empty() {
                self.log(&format!(
                    "counter abstractions active in this model: {}. A proof holds for the design; a counterexample may use an abstract counter jump",
                    crate::complexity::format_abstractions(&self.counter_abstractions)
                ));
                btor2 = crate::complexity::apply_counter_abstractions(
                    &btor2,
                    &self.counter_abstractions,
                    self.design.top.as_deref(),
                )?;
            }
            Ok((btor2, l.properties))
        } else {
            Err(format!("svlang: {}", l.errors.join("; ")))
        }
    }

    /// RTL assumption target symbols that have been disabled (`assume -disable`)
    /// or removed (`assume -remove`) in `task`.
    fn disabled_rtl_assumptions(&self, task: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        // Explicitly disabled RTL assumptions (or copies of them) in `task`.
        for p in &self.props {
            if p.task == task
                && p.kind == Kind::Assume
                && !p.enabled
                && (p.rtl || p.copy_of.is_some())
            {
                let sym = match &p.copy_of {
                    Some(orig) => target_name(orig),
                    None => target_name(&p.name),
                };
                if !out.contains(&sym) {
                    out.push(sym);
                }
            }
        }
        // If `elaborate` populated RTL assumptions in `<embedded>`, any RTL
        // assumption whose copy was removed from `task` (`assume -remove`) is
        // also excluded from `task`'s model.
        if task != EMBEDDED && self.props.iter().any(|p| p.rtl) {
            for orig in self
                .props
                .iter()
                .filter(|p| p.rtl && p.kind == Kind::Assume)
            {
                let present = self.props.iter().any(|p| {
                    p.task == task
                        && p.kind == Kind::Assume
                        && p.copy_of.as_deref() == Some(orig.name.as_str())
                });
                if !present {
                    let sym = target_name(&orig.name);
                    if !out.contains(&sym) {
                        out.push(sym);
                    }
                }
            }
        }
        out
    }

    /// The Btor2 model of `task` as the engines get it: the design, the
    /// task's assumptions and cut points, and every target of the task.
    pub fn task_model(&self, task: &str) -> Result<String, String> {
        let (btor, _) = self.lower(task)?;
        Ok(informal_svlang::split_trace(&btor).0.to_string())
    }
}

/// Injects `constraint <-bad_arg> <assume_sym>` lines for RTL assertions promoted via
/// `assume -from_assert`, bumping trailing `; informal-trace-outputs` IDs to stay monotonic.
fn inject_rtl_assert_assumptions(btor: &str, helpers: &[(String, String)]) -> String {
    let (core, trace_sec) = informal_svlang::split_trace(btor);
    let mut bad_args: std::collections::HashMap<&str, i64> = std::collections::HashMap::new();
    let mut max_core_id: u32 = 0;
    for l in core.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if let Some(Ok(id)) = w.first().map(|s| s.parse::<u32>()) {
            max_core_id = max_core_id.max(id);
        }
        if w.get(1) == Some(&"bad") {
            if let (Some(Ok(arg)), Some(&sym)) = (w.get(2).map(|s| s.parse::<i64>()), w.get(3)) {
                bad_args.insert(sym, arg);
            }
        }
    }
    let mut extra = String::new();
    let mut added: u32 = 0;
    for (orig_sym, assume_sym) in helpers {
        if let Some(&bad_arg) = bad_args.get(orig_sym.as_str()) {
            added += 1;
            let cid = max_core_id + added;
            let hold_arg = -bad_arg;
            extra.push_str(&format!("{cid} constraint {hold_arg} {assume_sym}\n"));
        }
    }
    if added == 0 {
        return btor.to_string();
    }
    let mut out = String::with_capacity(btor.len() + extra.len() + 64);
    out.push_str(core);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&extra);
    if !trace_sec.is_empty() {
        for l in trace_sec.lines() {
            let w: Vec<&str> = l.split_whitespace().collect();
            if w.get(1) == Some(&"output") {
                if let Some(Ok(oid)) = w.first().map(|s| s.parse::<u32>()) {
                    let new_id = oid + added;
                    out.push_str(&format!("{new_id} {}\n", w[1..].join(" ")));
                    continue;
                }
            }
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}
