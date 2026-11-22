use super::types::*;
use super::{check_operators, glob, qualify, target_name};

impl Session {
    /// Add a property given by command to the current task.
    pub fn add_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        let task = self.current_task.clone();
        self.add_property_in(&task, kind, name, expr)
    }

    /// Add a property given by command to `task`. Outside the embedded
    /// task its name is qualified (`<task>::<name>`); a name given already
    /// qualified with that task is taken as it is.
    pub fn add_property_in(
        &mut self,
        task: &str,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        // Reject up front what the front end's property compiler is known
        // not to take, and names the design does not have. The front end
        // binds the property at prove and has the last word on both.
        check_operators(expr)?;
        self.check_names(expr)?;
        let task = self.task_name(task)?;
        let short = match name {
            Some(n) => match n.split_once("::") {
                Some((t, rest)) if t == task => rest.to_string(),
                Some((t, _)) => {
                    return Err(format!(
                        "property `{n}` names task {t}, but it is being added to task {task}"
                    ))
                }
                None => n,
            },
            None => {
                let n = self
                    .props
                    .iter()
                    .filter(|p| p.kind == kind && p.task == task)
                    .count();
                format!("{}_{}", kind.label(), n)
            }
        };
        let name = qualify(&task, &short);
        if self.props.iter().any(|p| p.name == name) {
            return Err(format!("property `{name}` already exists"));
        }
        self.props.push(Property {
            name: name.clone(),
            kind,
            expr: expr.to_string(),
            enabled: true,
            status: Status::Unprocessed,
            trace: None,
            source: None,
            rtl: false,
            engines: Vec::new(),
            ai: false,
            under_ai: Vec::new(),
            vacuity: None,
            witness_trace: None,
            task,
            copy_of: None,
            bound: None,
            proof_core: None,
            from_assert: None,
            rtl_from_assert: false,
            stats: None,
        });
        Ok(name)
    }

    /// Promotes selected `Assert` properties into helper `Assume` properties in `task`
    /// (`assume -from_assert <prop_or_glob> [-name N]`), recording provenance and
    /// preventing any promoted assertion from circularly assuming itself during `prove`.
    pub fn assume_from_assert(
        &mut self,
        task: &str,
        indices: &[usize],
        custom_name: Option<String>,
    ) -> Result<Vec<String>, String> {
        let task = self.task_name(task)?;
        let assert_idxs: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|&i| self.props[i].kind == Kind::Assert)
            .collect();
        if assert_idxs.is_empty() {
            return Err("assume -from_assert: no matching assertions selected".into());
        }
        if custom_name.is_some() && assert_idxs.len() > 1 {
            return Err(
                "assume -from_assert: -name can only be used when promoting a single assertion"
                    .into(),
            );
        }
        let mut created = Vec::with_capacity(assert_idxs.len());
        for &idx in &assert_idxs {
            let src = self.props[idx].clone();
            let orig_target = match &src.copy_of {
                Some(orig) => target_name(orig),
                None => target_name(&src.name),
            };
            let is_rtl_src = src.rtl || src.copy_of.is_some();
            let base_short = match &custom_name {
                Some(n) => n
                    .strip_prefix(&format!("{task}::"))
                    .unwrap_or(n.as_str())
                    .to_string(),
                None => format!("{}_assume", src.short_name()),
            };
            let full_name = qualify(&task, &base_short);
            if self.props.iter().any(|p| p.name == full_name) {
                return Err(format!("assume -from_assert: `{full_name}` already exists"));
            }
            self.props.push(Property {
                name: full_name.clone(),
                kind: Kind::Assume,
                expr: src.expr.clone(),
                enabled: true,
                status: Status::Unprocessed,
                trace: None,
                source: src.source.clone(),
                rtl: false,
                engines: Vec::new(),
                ai: false,
                under_ai: Vec::new(),
                vacuity: None,
                witness_trace: None,
                task: task.clone(),
                copy_of: None,
                bound: None,
                proof_core: None,
                from_assert: Some(orig_target),
                rtl_from_assert: is_rtl_src,
                stats: None,
            });
            let prov = format!("{full_name} <- {}", src.name);
            if !self.assumed_from_assert.contains(&prov) {
                self.assumed_from_assert.push(prov);
            }
            created.push(full_name);
        }
        Ok(created)
    }

    // ------------------------------------------------------------ tasks

    /// The task called `name`, as the session spells it; `Err` when there
    /// is none.
    pub fn task_name(&self, name: &str) -> Result<String, String> {
        self.tasks
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.name.clone())
            .ok_or_else(|| {
                format!(
                    "no task `{name}` (tasks: {})",
                    self.tasks
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
    }

    fn task_mut(&mut self, name: &str) -> Result<&mut Task, String> {
        let name = self.task_name(name)?;
        Ok(self.tasks.iter_mut().find(|t| t.name == name).unwrap())
    }

    /// The cut points of `task`.
    pub fn stopats_of(&self, task: &str) -> &[String] {
        self.tasks
            .iter()
            .find(|t| t.name == task)
            .map_or(&[][..], |t| &t.stopats)
    }

    /// The cut points of `task`, to change.
    pub fn stopats_mut(&mut self, task: &str) -> Result<&mut Vec<String>, String> {
        Ok(&mut self.task_mut(task)?.stopats)
    }

    /// `task -create`: a new, empty task. The assumptions written in the
    /// RTL are in every task's model (they are part of the design: the
    /// front end cannot leave one out yet), so they are listed in the new
    /// task from the start, as copies.
    pub fn create_task(&mut self, name: &str) -> Result<(), String> {
        if name.is_empty() || name.contains("::") || name.chars().any(char::is_whitespace) {
            return Err(format!(
                "task name `{name}`: a name has no white space and no `::`"
            ));
        }
        if self.tasks.iter().any(|t| t.name == name) {
            return Err(format!("task `{name}` already exists"));
        }
        self.tasks.push(Task::new(name));
        let design_assumes: Vec<usize> = (0..self.props.len())
            .filter(|&i| self.props[i].rtl && self.props[i].kind == Kind::Assume)
            .collect();
        self.copy_into(name, &design_assumes)?;
        Ok(())
    }

    /// Copy properties `which` into `task` (`task -create/-edit -copy`):
    /// each copy is named `<task>::<name without its task>`, starts
    /// unprocessed, and is enabled. One that is there already is left as it
    /// is. Returns the names of the copies made.
    pub fn copy_into(&mut self, task: &str, which: &[usize]) -> Result<Vec<String>, String> {
        let task = self.task_name(task)?;
        let mut made = Vec::new();
        for &i in which {
            let src = &self.props[i];
            let name = qualify(&task, src.short_name());
            if src.task == task || self.props.iter().any(|p| p.name == name) {
                continue;
            }
            let mut p = src.clone();
            p.forget_result();
            p.copy_of = match (src.rtl, &src.copy_of) {
                (true, _) => Some(src.name.clone()),
                (false, c) => c.clone(),
            };
            p.rtl = false;
            p.ai = false;
            p.enabled = true;
            p.name = name.clone();
            p.task = task.clone();
            self.props.push(p);
            made.push(name);
        }
        Ok(made)
    }

    /// Copy `from`'s cut points into `to`.
    pub fn copy_stopats(&mut self, from: &str, to: &str) -> Result<(), String> {
        let cuts = self.task_mut(from)?.stopats.clone();
        let dst = self.stopats_mut(to)?;
        for c in cuts {
            if !dst.contains(&c) {
                dst.push(c);
            }
        }
        Ok(())
    }

    /// `task -set`: the task that commands without `-task` apply to.
    pub fn set_task(&mut self, name: &str) -> Result<(), String> {
        self.current_task = self.task_name(name)?;
        Ok(())
    }

    /// `task -delete`: a task and its properties. The embedded task stays.
    pub fn delete_task(&mut self, name: &str) -> Result<(), String> {
        let name = self.task_name(name)?;
        if name == EMBEDDED {
            return Err(format!("the {EMBEDDED} task cannot be deleted"));
        }
        self.tasks.retain(|t| t.name != name);
        self.props.retain(|p| p.task != name);
        if self.current_task == name {
            self.current_task = EMBEDDED.to_string();
        }
        Ok(())
    }

    /// Indices of the properties of `task`.
    pub fn props_of(&self, task: &str) -> Vec<usize> {
        (0..self.props.len())
            .filter(|&i| self.props[i].task == task)
            .collect()
    }

    /// What [`Self::add_property`] would reject, without adding anything.
    pub fn check_property(&self, name: Option<&str>, expr: &str) -> Result<(), String> {
        check_operators(expr)?;
        self.check_names(expr)?;
        match name.map(|n| qualify(&self.current_task, n)) {
            Some(n) if self.props.iter().any(|p| p.name == n) => {
                Err(format!("property `{n}` already exists"))
            }
            _ => Ok(()),
        }
    }

    /// Add a property through the AI interface: as [`Self::add_property`],
    /// tagged AI-added and written to the log.
    pub fn add_ai_property(
        &mut self,
        kind: Kind,
        name: Option<String>,
        expr: &str,
    ) -> Result<String, String> {
        let name = self.add_property(kind, name, expr)?;
        if let Some(p) = self.props.iter_mut().find(|p| p.name == name) {
            p.ai = true;
        }
        self.log(&format!("AI-added {} {name}: {expr}", kind.label()));
        Ok(name)
    }

    /// Remove an AI-added property. Returns the properties whose status
    /// was found with it in the model (an assumption) and is now reset.
    pub fn remove_ai_property(&mut self, name: &str) -> Result<Vec<String>, String> {
        let at = self
            .props
            .iter()
            .position(|p| p.name == name && p.ai)
            .ok_or_else(|| format!("no AI-added property `{name}`"))?;
        self.props.remove(at);
        self.log(&format!("AI-added property {name} removed"));
        Ok(self.reset_under(name))
    }

    /// Cut a signal from its drivers through the AI interface: as
    /// `stopat`, tagged AI-added and written to the log. Returns the cut
    /// point as the session lists it.
    pub fn add_ai_cut_point(&mut self, signal: &str) -> Result<String, String> {
        let cut = self.cut_point(signal)?;
        let task = self.current_task.clone();
        let t = self.task_mut(&task)?;
        if t.stopats.contains(&cut) {
            return Err(format!("`{cut}` is already a cut point"));
        }
        t.stopats.push(cut.clone());
        t.ai_stopats.push(cut.clone());
        self.log(&format!("AI-added cut point (stopat) {cut}"));
        Ok(cut)
    }

    /// Remove an AI-added cut point. Returns the properties whose status
    /// was found with it in the model and is now reset.
    pub fn remove_ai_cut_point(&mut self, cut: &str) -> Result<Vec<String>, String> {
        if !self
            .tasks
            .iter()
            .any(|t| t.ai_stopats.iter().any(|c| c == cut))
        {
            return Err(format!("no AI-added cut point `{cut}`"));
        }
        for t in &mut self.tasks {
            if t.ai_stopats.iter().any(|c| c == cut) {
                t.ai_stopats.retain(|c| c != cut);
                t.stopats.retain(|c| c != cut);
            }
        }
        self.log(&format!("AI-added cut point {cut} removed"));
        Ok(self.reset_under(&format!("stopat {cut}")))
    }

    /// Back to unprocessed: every property whose last prove had the
    /// AI-added change `item` in its model.
    fn reset_under(&mut self, item: &str) -> Vec<String> {
        let mut reset = Vec::new();
        for p in &mut self.props {
            if p.under_ai.iter().any(|u| u == item) {
                p.forget_result();
                reset.push(p.name.clone());
            }
        }
        reset
    }

    /// The AI-added changes a prove of the current task started now would
    /// have in its model (see [`Property::under_ai`]).
    pub fn ai_in_force(&self) -> Vec<String> {
        self.ai_in_force_for(&self.current_task)
    }

    /// [`Self::ai_in_force`] for `task`.
    pub fn ai_in_force_for(&self, task: &str) -> Vec<String> {
        let ai_cuts = self
            .tasks
            .iter()
            .filter(|t| t.name == task)
            .flat_map(|t| t.ai_stopats.iter());
        self.props
            .iter()
            .filter(|p| p.ai && p.enabled && p.kind == Kind::Assume && p.task == task)
            .map(|p| p.name.clone())
            .chain(ai_cuts.map(|c| format!("stopat {c}")))
            .collect()
    }

    /// The name of property `i`'s check in its task's model: its own name,
    /// or for a copy of a property in the RTL, the original's.
    pub fn check_name(&self, i: usize) -> String {
        let p = &self.props[i];
        target_name(p.copy_of.as_deref().unwrap_or(&p.name))
    }

    /// Names matching a Tcl-style glob (`*`, `?`). A pattern that matches
    /// no name as it stands is tried as `<embedded>::<name>` (JasperGold's
    /// full name of an embedded property) and then in the current task.
    pub fn matching(&self, pattern: &str) -> Vec<usize> {
        let by = |pat: &str, task: Option<&str>| -> Vec<usize> {
            self.props
                .iter()
                .enumerate()
                .filter(|(_, p)| task.is_none_or(|t| p.task == t) && glob(pat, &p.name))
                .map(|(i, _)| i)
                .collect()
        };
        let direct = by(pattern, None);
        if !direct.is_empty() {
            return direct;
        }
        if let Some(rest) = pattern.strip_prefix(&format!("{EMBEDDED}::")) {
            return by(rest, Some(EMBEDDED));
        }
        if self.current_task != EMBEDDED {
            return by(&qualify(&self.current_task, pattern), None);
        }
        Vec::new()
    }
}
