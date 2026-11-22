mod cov_conn;
mod sec_csr_proto;
mod xprop_spv;

pub(in crate::compat) use cov_conn::{check_conn, check_cov};
pub(in crate::compat) use sec_csr_proto::{check_csr, check_protocol, check_sec};
pub(in crate::compat) use xprop_spv::{check_spv, check_xprop, why_cex};

use super::Args;
use crate::session::Session;
use informal_tcl::Value;
use std::path::Path;

pub(in crate::compat) fn get_fv_complexity(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-property", 1), ("-json", 0)], &[("-task", 1)])?;
    if s.dry_run {
        return Ok(().into());
    }
    let prop = a
        .val("-property")
        .or_else(|| a.pos.first().map(String::as_str));
    let rep = crate::complexity::fv_complexity(s, prop)?;
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = crate::complexity::format_complexity(&rep);
    print!("{text}");
    Ok(text.into())
}

pub(in crate::compat) fn blackbox_assistant(
    s: &mut Session,
    a: &[String],
) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-threshold_bits", 1), ("-apply", 0), ("-json", 0)],
        &[],
    )?;
    if s.dry_run {
        return Ok(().into());
    }
    let threshold_bits: u64 = match a.val("-threshold_bits") {
        Some(v) => v
            .parse()
            .map_err(|_| format!("blackbox_assistant: bad -threshold_bits `{v}`"))?,
        None => 64,
    };
    let adv = crate::complexity::blackbox_assistant(s, threshold_bits, a.flag("-apply"))?;
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&adv).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = crate::complexity::format_bbox_advice(&adv);
    print!("{text}");
    Ok(text.into())
}

pub(in crate::compat) fn check_fv_setup(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[("-json", 0), ("-file", 1), ("-force", 0)],
        &[("-verbose", 0)],
    )?;
    if s.dry_run {
        return Ok(().into());
    }
    let rep = crate::complexity::check_fv_setup(s);
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&rep).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = crate::complexity::format_setup_report(&rep);
    if let Some(f) = a.val("-file") {
        let path = Path::new(f);
        if path.exists() && !a.flag("-force") {
            return Err(format!("{}: {f} exists; add -force to overwrite it", a.cmd));
        }
        std::fs::write(path, &text).map_err(|e| format!("{}: {f}: {e}", a.cmd))?;
    } else {
        print!("{text}");
    }
    Ok(text.into())
}

pub(in crate::compat) fn get_reset_info(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-json", 0)], &[])?;
    if s.dry_run {
        return Ok(().into());
    }
    let rep = crate::complexity::check_fv_setup(s);
    if a.flag("-json") {
        let js = serde_json::to_string_pretty(&rep.reset).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = format!(
        "configured_reset: {} | rtl_resets: {} | resetless_flops: {}",
        rep.reset.configured_reset.as_deref().unwrap_or("none"),
        rep.reset
            .rtl_resets
            .iter()
            .map(|r| format!(
                "{}({}, {} regs)",
                r.signal,
                if r.active_high { "high" } else { "low" },
                r.registers
            ))
            .collect::<Vec<_>>()
            .join(", "),
        rep.reset.resetless_flops
    );
    Ok(text.into())
}

pub(in crate::compat) fn abstract_cmd(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(
        a,
        &[
            ("-counter", 1),
            ("-threshold", 1),
            ("-init_value", 0),
            ("-clear", 0),
            ("-list", 0),
        ],
        &[("-task", 1)],
    )?;
    if a.flag("-clear") {
        s.counter_abstractions.clear();
        return Ok(().into());
    }
    if a.flag("-list") {
        let list: Vec<String> = s
            .counter_abstractions
            .iter()
            .map(|c| c.signal.clone())
            .collect();
        return Ok(Value::List(list));
    }
    let sig = a
        .val("-counter")
        .or_else(|| a.pos.first().map(String::as_str))
        .ok_or("abstract: expected -counter <signal> [-threshold K], -list, or -clear")?;
    let threshold =
        match a.val("-threshold") {
            Some(v) => Some(v.parse::<u64>().ok().filter(|&k| k >= 1).ok_or_else(|| {
                format!("abstract -threshold: positive integer expected, not `{v}`")
            })?),
            None => None,
        };
    let clean = sig.trim().to_string();
    if let Some(existing) = s
        .counter_abstractions
        .iter_mut()
        .find(|c| c.signal == clean)
    {
        existing.threshold = threshold;
    } else {
        s.counter_abstractions
            .push(crate::complexity::CounterAbstraction {
                signal: clean.clone(),
                threshold,
            });
    }
    Ok(clean.into())
}

pub(in crate::compat) fn report_abstractions(
    s: &mut Session,
    a: &[String],
) -> Result<Value, String> {
    let a = Args::parse(a, &[("-json", 0)], &[])?;
    if a.flag("-json") {
        let js =
            serde_json::to_string_pretty(&s.counter_abstractions).map_err(|e| e.to_string())?;
        return Ok(js.into());
    }
    let text = crate::complexity::format_abstractions(&s.counter_abstractions);
    if !s.dry_run && !text.is_empty() {
        println!("{text}");
    }
    Ok(text.into())
}

pub(in crate::compat) fn set_abstractions(s: &mut Session, a: &[String]) -> Result<Value, String> {
    let a = Args::parse(a, &[("-clear", 0), ("-off", 0)], &[])?;
    if a.flag("-clear") || a.flag("-off") || a.pos.first().map(String::as_str) == Some("off") {
        s.counter_abstractions.clear();
        return Ok(().into());
    }
    Ok(crate::complexity::format_abstractions(&s.counter_abstractions).into())
}
