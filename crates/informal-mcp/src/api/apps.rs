use super::{err, opt_bool, opt_str, opt_u64, prop_view, req_str, strings, wrong, Api, Reply};
use serde_json::{json, Value};
use std::path::Path;

impl Api {
    // -------------------------------------------------------- formal apps

    pub(super) fn prove_cache(&self, args: &Value) -> Reply {
        self.changing()?;
        let enabled = opt_bool(args, "enabled")?;
        let mode = opt_str(args, "mode")?;
        let path_opt = match opt_str(args, "path")? {
            Some(raw) => {
                if Path::new(&raw)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    return Err(err("path", format!("{raw} must not contain '..'")));
                }
                let joined = self.opts.root.join(&raw);
                if !joined.starts_with(&self.opts.root) {
                    return Err(err(
                        "path",
                        format!("{raw} is outside {}", self.opts.root.display()),
                    ));
                }
                Some(joined)
            }
            None => None,
        };
        if let Some(ref m) = mode {
            if !matches!(m.as_str(), "rw" | "ro" | "wo") {
                return Err(wrong("mode", "one of rw, ro, wo"));
            }
        }
        self.on_shell(false, move |sh| {
            let mut s = sh.session.borrow_mut();
            if let Some(en) = enabled {
                s.prove_cache = en;
            }
            if let Some(p) = path_opt {
                s.prove_cache_dir = Some(p);
            }
            if let Some(m) = mode {
                s.prove_cache_mode = informal_fpv::cache::CacheMode::parse(&m)
                    .ok_or_else(|| wrong("mode", "one of rw, ro, wo"))?;
            }
            let mode_str = match s.prove_cache_mode {
                informal_fpv::cache::CacheMode::ReadWrite => "rw",
                informal_fpv::cache::CacheMode::ReadOnly => "ro",
                informal_fpv::cache::CacheMode::WriteOnly => "wo",
            };
            let mut reply = serde_json::Map::new();
            reply.insert("schema".into(), json!("informal.prove_cache/1"));
            reply.insert("enabled".into(), json!(s.prove_cache));
            reply.insert("mode".into(), json!(mode_str));
            if let Some(ref dir) = s.prove_cache_dir {
                reply.insert("path".into(), json!(dir.display().to_string()));
            }
            Ok(Value::Object(reply))
        })?
    }

    pub(super) fn check_cov(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let models = strings(args, "models")?;
        let proof_core = opt_bool(args, "proof_core")?.unwrap_or(false);
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "init" => {
                    let mut cmd = String::from("check_cov -init");
                    if !models.is_empty() {
                        cmd.push_str(&format!(" -model {{{}}}", models.join(" ")));
                    }
                    if proof_core {
                        cmd.push_str(" -enable_prove_based_proof_core");
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "measure" => {
                    let mut cmd = String::from("check_cov -measure");
                    if !models.is_empty() {
                        cmd.push_str(&format!(" -type {{{}}}", models.join(" ")));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.session.borrow_mut().cov = Default::default();
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("init, measure, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let active_models: Vec<String> = s
                .cov
                .active_models()
                .into_iter()
                .map(|m| m.as_str().to_string())
                .collect();
            let items: Vec<Value> = s
                .cov
                .items
                .iter()
                .map(|it| {
                    json!({
                        "id": it.id,
                        "task": it.task,
                        "model": it.model.as_str(),
                        "name": it.name,
                        "status": it.status.word(),
                        "detail": it.detail,
                    })
                })
                .collect();
            let report = informal_fpv::cov::format_report(&s.cov, None, None, &[], false);
            Ok(json!({
                "schema": "informal.cov/1",
                "initialized": s.cov.initialized,
                "measured": s.cov.measured,
                "proof_core": s.cov.prove_based,
                "models": active_models,
                "report": report,
                "items": items,
            }))
        })?
    }

    pub(super) fn check_conn(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let csv_path = match opt_str(args, "file")? {
            Some(f) => Some(self.below_root(&f)?),
            None => None,
        };
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "load" => {
                    let Some(p) = csv_path else {
                        return Err(err("arguments", "`file` is required when action is `load`"));
                    };
                    let cmd = format!("check_conn -load {{{}}}", p.display());
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_conn -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_conn -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("load, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::conn::DEFAULT_CONN_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.conn/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
    }

    pub(super) fn check_xprop(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let targets = strings(args, "targets")?;
        let x_sources = strings(args, "x_sources")?;
        let to = strings(args, "to")?;
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "init" => {
                    let mut cmd = String::from("check_xprop -init");
                    if !targets.is_empty() {
                        cmd.push_str(&format!(" -targets {{{}}}", targets.join(" ")));
                    }
                    for src in &x_sources {
                        cmd.push_str(&format!(" -x_source {{{src}}}"));
                    }
                    for dst in &to {
                        cmd.push_str(&format!(" -to {{{dst}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_xprop -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_xprop -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("init, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let items: Vec<Value> = s
                .xprop
                .items
                .iter()
                .map(|it| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(it.name));
                    o.insert("category".into(), json!(it.category));
                    o.insert("status".into(), json!(it.status.word()));
                    if let informal_fpv::xprop::XpropStatus::Cex { depth: Some(d) } = it.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = it.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    if let Some(ref tr) = it.trace {
                        o.insert("trace".into(), json!(tr));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::xprop::format_report(&s.xprop);
            Ok(json!({
                "schema": "informal.xprop/1",
                "initialized": s.xprop.initialized,
                "report": report,
                "items": items,
            }))
        })?
    }

    pub(super) fn check_spv(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let name = opt_str(args, "name")?;
        let from = strings(args, "from")?;
        let to = strings(args, "to")?;
        let cond = opt_str(args, "cond")?;
        self.on_shell(long, move |sh| {
            match action.as_str() {
                "create" => {
                    if from.is_empty() || to.is_empty() {
                        return Err(err(
                            "arguments",
                            "`from` and `to` are required when action is `create`",
                        ));
                    }
                    let mut cmd = String::from("check_spv -create");
                    if let Some(ref n) = name {
                        cmd.push_str(&format!(" -name {{{n}}}"));
                    }
                    cmd.push_str(&format!(" -from {{{}}}", from.join(" ")));
                    cmd.push_str(&format!(" -to {{{}}}", to.join(" ")));
                    if let Some(ref c) = cond {
                        cmd.push_str(&format!(" -cond {{{c}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_spv -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_spv -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("create, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let rules: Vec<Value> = s
                .spv
                .iter()
                .map(|r| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(r.name));
                    o.insert("task".into(), json!(r.task));
                    o.insert("from".into(), json!(r.from));
                    o.insert("to".into(), json!(r.to));
                    if let Some(ref c) = r.cond {
                        o.insert("cond".into(), json!(c));
                    }
                    o.insert("status".into(), json!(r.status.word()));
                    if let informal_fpv::spv::SpvStatus::Cex { depth: Some(d) } = r.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = r.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    if let Some(ref tr) = r.trace {
                        o.insert("trace".into(), json!(tr));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::spv::format_report(&s.spv);
            Ok(json!({
                "schema": "informal.spv/1",
                "report": report,
                "rules": rules,
            }))
        })?
    }

    pub(super) fn check_sec(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let spec_raw = strings(args, "spec_files")?;
        let imp_raw = strings(args, "imp_files")?;
        let mut spec_files = Vec::with_capacity(spec_raw.len());
        for f in &spec_raw {
            spec_files.push(self.below_root(f)?);
        }
        let mut imp_files = Vec::with_capacity(imp_raw.len());
        for f in &imp_raw {
            imp_files.push(self.below_root(f)?);
        }
        let spec_top = opt_str(args, "spec_top")?;
        let imp_top = opt_str(args, "imp_top")?;
        let clock = opt_str(args, "clock")?;
        let reset = opt_str(args, "reset")?;
        let lec = opt_bool(args, "lec")?.unwrap_or(false);
        let latency = opt_u64(args, "latency")?.unwrap_or(0) as u32;
        let assumptions = strings(args, "assumptions")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "setup" => {
                    if spec_files.is_empty() || imp_files.is_empty() {
                        return Err(err(
                            "arguments",
                            "`spec_files` and `imp_files` are required when action is `setup`",
                        ));
                    }
                    let setup = informal_fpv::sec::SecSetup {
                        spec_files,
                        spec_top,
                        imp_files,
                        imp_top,
                        clock,
                        reset,
                        lec,
                        latency,
                        assumptions,
                        ..Default::default()
                    };
                    let mut s = sh.session.borrow_mut();
                    informal_fpv::sec::setup_sec(&mut s, setup).map_err(|e| err("rejected", e))?;
                }
                "prove" => {
                    sh.eval("check_sec -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_sec -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("setup, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let configured = s.sec.setup.is_some();
            let cur_lec = s.sec.setup.as_ref().is_some_and(|st| st.lec);
            let cur_lat = s.sec.setup.as_ref().map_or(0, |st| st.latency);
            let checks: Vec<Value> = s
                .sec
                .checks
                .iter()
                .map(|c| {
                    let mut o = serde_json::Map::new();
                    o.insert("name".into(), json!(c.name));
                    o.insert("kind".into(), json!(c.kind));
                    o.insert("spec_sig".into(), json!(c.spec_sig));
                    o.insert("imp_sig".into(), json!(c.imp_sig));
                    o.insert("width".into(), json!(c.width));
                    o.insert("latency".into(), json!(c.latency));
                    o.insert("status".into(), json!(c.status.word()));
                    if let informal_fpv::sec::SecStatus::Cex { depth: Some(d) } = c.status {
                        o.insert("depth".into(), json!(d));
                    }
                    if let Some(ref eng) = c.engine {
                        o.insert("engine".into(), json!(eng));
                    }
                    if let Some(ref tr) = c.trace {
                        o.insert("trace".into(), json!(tr));
                    }
                    Value::Object(o)
                })
                .collect();
            let report = informal_fpv::sec::format_report(&s.sec);
            Ok(json!({
                "schema": "informal.sec/1",
                "configured": configured,
                "lec": cur_lec,
                "latency": cur_lat,
                "report": report,
                "checks": checks,
            }))
        })?
    }

    pub(super) fn check_csr(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let spec_path = match opt_str(args, "file")? {
            Some(f) => Some(self.below_root(&f)?),
            None => None,
        };
        let prefix = opt_str(args, "prefix")?;
        let addr = opt_str(args, "addr")?;
        let wdata = opt_str(args, "wdata")?;
        let rdata = opt_str(args, "rdata")?;
        let we = opt_str(args, "we")?;
        let re = opt_str(args, "re")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "load" => {
                    let Some(p) = spec_path else {
                        return Err(err("arguments", "`file` is required when action is `load`"));
                    };
                    let mut cmd = format!("check_csr -load {{{}}}", p.display());
                    if let Some(ref v) = prefix {
                        cmd.push_str(&format!(" -prefix {{{v}}}"));
                    }
                    if let Some(ref v) = addr {
                        cmd.push_str(&format!(" -addr {{{v}}}"));
                    }
                    if let Some(ref v) = wdata {
                        cmd.push_str(&format!(" -wdata {{{v}}}"));
                    }
                    if let Some(ref v) = rdata {
                        cmd.push_str(&format!(" -rdata {{{v}}}"));
                    }
                    if let Some(ref v) = we {
                        cmd.push_str(&format!(" -we {{{v}}}"));
                    }
                    if let Some(ref v) = re {
                        cmd.push_str(&format!(" -re {{{v}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_csr -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_csr -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("load, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::csr::DEFAULT_CSR_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.csr/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
    }

    pub(super) fn check_protocol(&self, args: &Value, long: bool) -> Reply {
        self.changing()?;
        let action = req_str(args, "action")?;
        let protocol = opt_str(args, "protocol")?;
        let role = opt_str(args, "role")?;
        let name = opt_str(args, "name")?;
        let prefix = opt_str(args, "prefix")?;

        self.on_shell(long, move |sh| {
            match action.as_str() {
                "create" => {
                    let Some(ref proto) = protocol else {
                        return Err(err(
                            "arguments",
                            "`protocol` is required when action is `create`",
                        ));
                    };
                    let mut cmd = format!("check_protocol -create -protocol {{{proto}}}");
                    if let Some(ref r) = role {
                        cmd.push_str(&format!(" -role {{{r}}}"));
                    }
                    if let Some(ref n) = name {
                        cmd.push_str(&format!(" -name {{{n}}}"));
                    }
                    if let Some(ref p) = prefix {
                        cmd.push_str(&format!(" -prefix {{{p}}}"));
                    }
                    sh.eval(&cmd).map_err(|e| err("rejected", e.message))?;
                }
                "prove" => {
                    sh.eval("check_protocol -prove")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "clear" => {
                    sh.eval("check_protocol -clear")
                        .map_err(|e| err("rejected", e.message))?;
                }
                "list" => {}
                other => {
                    return Err(wrong(
                        "action",
                        &format!("create, prove, list, or clear (got {other})"),
                    ))
                }
            }
            let s = sh.session.borrow();
            let props: Vec<Value> = s
                .props
                .iter()
                .filter(|p| p.task == informal_fpv::protocol::DEFAULT_PROTOCOL_TASK)
                .map(|p| prop_view(p, false))
                .collect();
            Ok(json!({
                "schema": "informal.protocol/1",
                "count": props.len(),
                "properties": props,
            }))
        })?
    }
}
