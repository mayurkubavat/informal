//! UPF boolean/power-control expression parsing and compilation to SVA strings
//! and Btor2 word-level nodes for Formal Low-Power Verification.

use std::collections::BTreeMap;

use informal_upf::{
    strip_outer_braces, PowerDomainId, PowerSwitchDef, ResolvedPowerIntent, Sense, SimState,
    SupplySetId,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoolExpr {
    ConstBool(bool),
    Lit { val: u64, width: u32 },
    Sig(String),
    Not(Box<BoolExpr>),
    And(Box<BoolExpr>, Box<BoolExpr>),
    Or(Box<BoolExpr>, Box<BoolExpr>),
    Xor(Box<BoolExpr>, Box<BoolExpr>),
    Eq(Box<BoolExpr>, Box<BoolExpr>),
    Neq(Box<BoolExpr>, Box<BoolExpr>),
}

impl BoolExpr {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let cleaned = strip_outer_braces(raw.trim());
        let toks = tokenize(cleaned)?;
        if toks.is_empty() {
            return Err("empty UPF boolean expression".to_string());
        }
        let mut p = Parser {
            toks: &toks,
            pos: 0,
        };
        let expr = p.parse_or()?;
        if p.pos < p.toks.len() {
            return Err(format!(
                "unexpected token `{:?}` in UPF expression `{cleaned}`",
                p.toks[p.pos]
            ));
        }
        Ok(expr)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        match self {
            Self::ConstBool(b) => Self::ConstBool(!b),
            Self::Not(inner) => *inner,
            other => Self::Not(Box::new(other)),
        }
    }

    pub fn and(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::ConstBool(false), _) | (_, Self::ConstBool(false)) => Self::ConstBool(false),
            (Self::ConstBool(true), x) | (x, Self::ConstBool(true)) => x,
            (x, y) if x == y => x,
            (x, y) => Self::And(Box::new(x), Box::new(y)),
        }
    }

    pub fn or(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::ConstBool(true), _) | (_, Self::ConstBool(true)) => Self::ConstBool(true),
            (Self::ConstBool(false), x) | (x, Self::ConstBool(false)) => x,
            (x, y) if x == y => x,
            (x, y) => Self::Or(Box::new(x), Box::new(y)),
        }
    }

    pub fn any_of(exprs: Vec<Self>) -> Option<Self> {
        let mut it = exprs.into_iter();
        let first = it.next()?;
        Some(it.fold(first, Self::or))
    }

    pub fn all_of(exprs: Vec<Self>) -> Option<Self> {
        let mut it = exprs.into_iter();
        let first = it.next()?;
        Some(it.fold(first, Self::and))
    }

    pub fn sig_with_sense(sig: &str, sense: Option<Sense>) -> Self {
        let s = Self::Sig(sig.trim().to_string());
        match sense.unwrap_or(Sense::High) {
            Sense::Low | Sense::Negedge => s.not(),
            Sense::High | Sense::Posedge => s,
        }
    }

    pub fn substitute(&self, map: &BTreeMap<String, String>) -> Self {
        match self {
            Self::ConstBool(b) => Self::ConstBool(*b),
            Self::Lit { val, width } => Self::Lit {
                val: *val,
                width: *width,
            },
            Self::Sig(s) => {
                if let Some(rep) = map.get(s) {
                    Self::Sig(rep.clone())
                } else {
                    Self::Sig(s.clone())
                }
            }
            Self::Not(x) => Self::Not(Box::new(x.substitute(map))),
            Self::And(a, b) => Self::And(Box::new(a.substitute(map)), Box::new(b.substitute(map))),
            Self::Or(a, b) => Self::Or(Box::new(a.substitute(map)), Box::new(b.substitute(map))),
            Self::Xor(a, b) => Self::Xor(Box::new(a.substitute(map)), Box::new(b.substitute(map))),
            Self::Eq(a, b) => Self::Eq(Box::new(a.substitute(map)), Box::new(b.substitute(map))),
            Self::Neq(a, b) => Self::Neq(Box::new(a.substitute(map)), Box::new(b.substitute(map))),
        }
    }

    /// Formats into a valid SystemVerilog boolean expression in `top`'s scope.
    pub fn to_sva(&self, scope: &str, top: Option<&str>) -> String {
        match self {
            Self::ConstBool(true) => "1'b1".to_string(),
            Self::ConstBool(false) => "1'b0".to_string(),
            Self::Lit { val, width } => format!("{width}'d{val}"),
            Self::Sig(s) => normalize_sva_signal(s, scope, top),
            Self::Not(x) => format!("(!({}))", x.to_sva(scope, top)),
            Self::And(a, b) => {
                format!("(({}) && ({}))", a.to_sva(scope, top), b.to_sva(scope, top))
            }
            Self::Or(a, b) => format!("(({}) || ({}))", a.to_sva(scope, top), b.to_sva(scope, top)),
            Self::Xor(a, b) => format!("(({}) ^ ({}))", a.to_sva(scope, top), b.to_sva(scope, top)),
            Self::Eq(a, b) => format!("(({}) == ({}))", a.to_sva(scope, top), b.to_sva(scope, top)),
            Self::Neq(a, b) => {
                format!("(({}) != ({}))", a.to_sva(scope, top), b.to_sva(scope, top))
            }
        }
    }
}

pub fn normalize_sva_signal(raw: &str, scope: &str, top: Option<&str>) -> String {
    let mut s = raw.trim().trim_start_matches('/').replace('/', ".");
    if let Some(t) = top.filter(|t| !t.is_empty()) {
        if let Some(rest) = s.strip_prefix(t).and_then(|r| r.strip_prefix('.')) {
            s = rest.to_string();
        }
    }
    let norm_scope = scope.trim_matches('/').replace('/', ".");
    if !norm_scope.is_empty() && !s.contains('.') {
        format!("{norm_scope}.{s}")
    } else {
        s
    }
}

/// Extracted formal power-control predicates for one UPF power domain.
#[derive(Debug, Clone)]
pub struct DomainPowerControls {
    pub pd_id: PowerDomainId,
    pub name: String,
    pub scope: String,
    pub primary_ss: Option<SupplySetId>,
    /// `None` if domain is always-on (all legal states are NORMAL, no switch).
    pub on_expr: Option<BoolExpr>,
    /// Unconditional corruption predicate (`CORRUPT` / `OFF` / `NOT_NORMAL`).
    pub corrupt_expr: Option<BoolExpr>,
    /// `CORRUPT_ON_ACTIVITY` state entry predicate.
    pub coa_expr: Option<BoolExpr>,
    /// `CORRUPT_STATE_ON_ACTIVITY` / `CORRUPT_STATE_ON_CHANGE` state predicate.
    pub csoa_expr: Option<BoolExpr>,
    /// Power-switch acknowledge signal (`-ack_port`), if declared.
    pub ack_sig: Option<String>,
    /// Combined isolation-active expression across all active isolation strategies.
    pub iso_active_expr: Option<BoolExpr>,
    /// Retention save-active expression, if domain has an active retention strategy.
    pub ret_save_expr: Option<BoolExpr>,
    /// Retention restore-active expression, if domain has an active retention strategy.
    pub ret_restore_expr: Option<BoolExpr>,
    /// Retention backup supply ON predicate (`None` = always-on).
    pub ret_backup_on_expr: Option<BoolExpr>,
    /// Isolation supply ON predicate (`None` = always-on).
    pub iso_supply_on_expr: Option<BoolExpr>,
}

impl DomainPowerControls {
    pub fn is_corruptible(&self) -> bool {
        self.corrupt_expr.is_some() || self.coa_expr.is_some() || self.csoa_expr.is_some()
    }
}

/// Extracts power-control predicates for every domain in `resolved`.
pub fn extract_domain_controls(resolved: &ResolvedPowerIntent) -> Vec<DomainPowerControls> {
    resolved
        .intent
        .domains
        .iter()
        .map(|pd| extract_one_domain(resolved, pd.id))
        .collect()
}

fn extract_one_domain(resolved: &ResolvedPowerIntent, pd_id: PowerDomainId) -> DomainPowerControls {
    let pd = &resolved.intent.domains[pd_id.0 as usize];
    let primary_ss = resolved.primary_ss(pd_id);

    // 1. Find any power switch controlling this domain's primary power net.
    let psw = find_domain_power_switch(resolved, pd_id, primary_ss);
    let psw_on = psw.and_then(switch_on_expr);
    let psw_off = psw.and_then(switch_off_expr);
    let ack_sig = psw.and_then(|sw| {
        sw.ack_ports
            .first()
            .map(|(_, net)| qualify_in_scope(&sw.scope, net))
    });

    // 2. Collect power state predicates on `primary_ss`.
    let (mut ss_on, mut ss_corrupt, ss_coa, ss_csoa) = primary_ss
        .map(|ss| supply_set_state_exprs(resolved, ss))
        .unwrap_or((None, None, None, None));

    // Prefer physical power-switch control for ON/OFF when present, so illegal
    // PST states with `!pwr_ctrl && !iso_en` still register as power-off (`!on`).
    if let Some(sw_on) = psw_on {
        ss_on = Some(sw_on.clone());
        ss_corrupt = Some(psw_off.unwrap_or_else(|| sw_on.not()));
    } else if ss_corrupt.is_none() && ss_coa.is_none() && ss_csoa.is_none() {
        // Always-on domain: clear `ss_on` so callers know it is unconditionally ON.
        ss_on = None;
    }

    // 3. Isolation active predicate across all active strategies on this domain.
    let mut iso_exprs = Vec::new();
    let mut iso_supply_on_expr = None;
    for iso in &resolved.intent.isolation_strategies {
        if iso.no_isolation {
            continue;
        }
        if resolved.resolve_domain(&iso.domain, &iso.scope) != Some(pd_id) {
            continue;
        }
        if let Some(ref sig) = iso.isolation_signal {
            let scope = iso.ctrl_scope.as_deref().unwrap_or(&iso.scope);
            let qsig = qualify_in_scope(scope, sig);
            iso_exprs.push(BoolExpr::sig_with_sense(&qsig, iso.isolation_sense));
        }
        if iso_supply_on_expr.is_none() {
            if let Some(iso_ss) = resolved.effective_iso_supply(iso) {
                iso_supply_on_expr = supply_set_on_expr(resolved, iso_ss);
            }
        }
    }
    let iso_active_expr = BoolExpr::any_of(iso_exprs);

    // 4. Retention save/restore/backup predicates on this domain.
    let mut save_exprs = Vec::new();
    let mut restore_exprs = Vec::new();
    let mut ret_backup_on_expr = None;
    for ret in &resolved.intent.retention_strategies {
        if ret.no_retention {
            continue;
        }
        if resolved.resolve_domain(&ret.domain, &ret.scope) != Some(pd_id) {
            continue;
        }
        if let Some(ref sig) = ret.save_signal {
            let scope = ret.save_scope.as_deref().unwrap_or(&ret.scope);
            let qsig = qualify_in_scope(scope, sig);
            save_exprs.push(BoolExpr::sig_with_sense(&qsig, ret.save_sense));
        } else if let Some(ref cond) = ret.save_condition {
            if let Ok(e) = BoolExpr::parse(cond) {
                save_exprs.push(e);
            }
        }
        if let Some(ref sig) = ret.restore_signal {
            let scope = ret.restore_scope.as_deref().unwrap_or(&ret.scope);
            let qsig = qualify_in_scope(scope, sig);
            restore_exprs.push(BoolExpr::sig_with_sense(&qsig, ret.restore_sense));
        } else if let Some(ref cond) = ret.restore_condition {
            if let Ok(e) = BoolExpr::parse(cond) {
                restore_exprs.push(e);
            }
        }
        if ret_backup_on_expr.is_none() {
            if let Some(ret_ss) = resolved.effective_ret_supply(ret) {
                ret_backup_on_expr = supply_set_on_expr(resolved, ret_ss);
            }
        }
    }

    DomainPowerControls {
        pd_id,
        name: pd.name.clone(),
        scope: pd.scope.clone(),
        primary_ss,
        on_expr: ss_on,
        corrupt_expr: ss_corrupt,
        coa_expr: ss_coa,
        csoa_expr: ss_csoa,
        ack_sig,
        iso_active_expr,
        ret_save_expr: BoolExpr::any_of(save_exprs),
        ret_restore_expr: BoolExpr::any_of(restore_exprs),
        ret_backup_on_expr,
        iso_supply_on_expr,
    }
}

/// Returns `Some(on_expr)` if `ss_id` can turn off, or `None` if `ss_id` is always-on.
pub fn supply_set_on_expr(resolved: &ResolvedPowerIntent, ss_id: SupplySetId) -> Option<BoolExpr> {
    if let Some(pnet) = resolved
        .ss_power_net
        .get(ss_id.0 as usize)
        .copied()
        .flatten()
    {
        let net_name = &resolved.intent.supply_nets[pnet.0 as usize].name;
        for sw in &resolved.intent.power_switches {
            if sw
                .output_supply_ports
                .iter()
                .any(|(_, n)| n == net_name || qualify_in_scope(&sw.scope, n) == *net_name)
            {
                if let Some(on) = switch_on_expr(sw) {
                    return Some(on);
                }
            }
        }
    }
    let (on, corrupt, coa, csoa) = supply_set_state_exprs(resolved, ss_id);
    if corrupt.is_none() && coa.is_none() && csoa.is_none() {
        None
    } else {
        on
    }
}

fn find_domain_power_switch(
    resolved: &ResolvedPowerIntent,
    pd_id: PowerDomainId,
    primary_ss: Option<SupplySetId>,
) -> Option<&PowerSwitchDef> {
    if let Some(ss) = primary_ss {
        if let Some(pnet) = resolved.ss_power_net.get(ss.0 as usize).copied().flatten() {
            let net_name = &resolved.intent.supply_nets[pnet.0 as usize].name;
            for sw in &resolved.intent.power_switches {
                if sw
                    .output_supply_ports
                    .iter()
                    .any(|(_, n)| n == net_name || qualify_in_scope(&sw.scope, n) == *net_name)
                {
                    return Some(sw);
                }
            }
        }
    }
    resolved
        .intent
        .power_switches
        .iter()
        .find(|sw| resolved.resolve_domain(&sw.domain, &sw.scope) == Some(pd_id))
}

fn switch_port_map(sw: &PowerSwitchDef) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (port, sig) in &sw.control_ports {
        let scope = sw
            .control_port_scopes
            .get(port)
            .map(String::as_str)
            .unwrap_or(&sw.scope);
        map.insert(port.clone(), qualify_in_scope(scope, sig));
    }
    for (port, sig) in &sw.ack_ports {
        map.insert(port.clone(), qualify_in_scope(&sw.scope, sig));
    }
    map
}

fn switch_on_expr(sw: &PowerSwitchDef) -> Option<BoolExpr> {
    let map = switch_port_map(sw);
    let mut exprs = Vec::new();
    for st in &sw.on_states {
        if let Ok(parsed) = BoolExpr::parse(&st.expr) {
            exprs.push(parsed.substitute(&map));
        }
    }
    if exprs.is_empty() {
        if let Some((_, sig)) = sw.control_ports.first() {
            exprs.push(BoolExpr::Sig(qualify_in_scope(&sw.scope, sig)));
        }
    }
    BoolExpr::any_of(exprs)
}

fn switch_off_expr(sw: &PowerSwitchDef) -> Option<BoolExpr> {
    let map = switch_port_map(sw);
    let mut exprs = Vec::new();
    for st in &sw.off_states {
        if let Ok(parsed) = BoolExpr::parse(&st.expr) {
            exprs.push(parsed.substitute(&map));
        }
    }
    BoolExpr::any_of(exprs)
}

fn supply_set_state_exprs(
    resolved: &ResolvedPowerIntent,
    ss_id: SupplySetId,
) -> (
    Option<BoolExpr>,
    Option<BoolExpr>,
    Option<BoolExpr>,
    Option<BoolExpr>,
) {
    let Some(states) = resolved.ss_states.get(ss_id.0 as usize) else {
        return (None, None, None, None);
    };
    let mut on_list = Vec::new();
    let mut corrupt_list = Vec::new();
    let mut coa_list = Vec::new();
    let mut csoa_list = Vec::new();

    for st in states {
        if st.illegal {
            continue;
        }
        let Some(ref raw_expr) = st.logic_expr else {
            continue;
        };
        let Ok(expr) = BoolExpr::parse(raw_expr) else {
            continue;
        };
        let sim = st.simstate.unwrap_or_else(|| {
            if st.rail_states.iter().any(|r| r.is_off) {
                SimState::Corrupt
            } else {
                SimState::Normal
            }
        });
        match sim {
            SimState::Normal => on_list.push(expr),
            SimState::Corrupt | SimState::Off | SimState::NotNormal => corrupt_list.push(expr),
            SimState::CorruptOnActivity => coa_list.push(expr),
            SimState::CorruptStateOnActivity | SimState::CorruptStateOnChange => {
                csoa_list.push(expr)
            }
        }
    }

    (
        BoolExpr::any_of(on_list),
        BoolExpr::any_of(corrupt_list),
        BoolExpr::any_of(coa_list),
        BoolExpr::any_of(csoa_list),
    )
}

fn qualify_in_scope(scope: &str, sig: &str) -> String {
    let s = sig.trim();
    let sc = scope.trim_matches('/');
    if sc.is_empty() || s.starts_with('/') || s.contains('/') || s.contains('.') {
        s.trim_start_matches('/').to_string()
    } else {
        format!("{sc}/{s}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Lit(u64, u32),
    LParen,
    RParen,
    Bang,
    Tilde,
    AndAnd,
    OrOr,
    Amp,
    Pipe,
    Caret,
    EqEq,
    BangEq,
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let b = s.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '~' => {
                out.push(Tok::Tilde);
                i += 1;
            }
            '^' => {
                out.push(Tok::Caret);
                i += 1;
            }
            '!' => {
                if i + 1 < b.len() && b[i + 1] == b'=' {
                    out.push(Tok::BangEq);
                    i += 2;
                } else {
                    out.push(Tok::Bang);
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < b.len() && b[i + 1] == b'&' {
                    out.push(Tok::AndAnd);
                    i += 2;
                } else {
                    out.push(Tok::Amp);
                    i += 1;
                }
            }
            '|' => {
                if i + 1 < b.len() && b[i + 1] == b'|' {
                    out.push(Tok::OrOr);
                    i += 2;
                } else {
                    out.push(Tok::Pipe);
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < b.len() && b[i + 1] == b'=' {
                    out.push(Tok::EqEq);
                    i += 2;
                } else {
                    return Err(format!("unexpected `=` in `{s}`"));
                }
            }
            '0'..='9' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'\'' || b[i] == b'_')
                {
                    i += 1;
                }
                let raw = &s[start..i];
                let (val, width) = parse_sv_literal(raw)?;
                out.push(Tok::Lit(val, width));
            }
            _ if is_ident_start(c) => {
                let start = i;
                while i < b.len() && is_ident_cont(b[i] as char) {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].to_string()));
            }
            _ => return Err(format!("unexpected character `{c}` in `{s}`")),
        }
    }
    Ok(out)
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || matches!(c, '_' | '/' | '$' | '\\')
}

fn is_ident_cont(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '.' | '[' | ']' | ':' | '$')
}

fn parse_sv_literal(raw: &str) -> Result<(u64, u32), String> {
    let clean = raw.replace('_', "");
    if let Some((w_str, rest)) = clean.split_once('\'') {
        let w: u32 = if w_str.is_empty() {
            1
        } else {
            w_str
                .parse()
                .map_err(|_| format!("invalid literal width `{raw}`"))?
        };
        let rest = rest.trim_start_matches(['s', 'S']);
        let (radix, digits) = match rest.as_bytes().first().copied() {
            Some(b'b' | b'B') => (2, &rest[1..]),
            Some(b'o' | b'O') => (8, &rest[1..]),
            Some(b'd' | b'D') => (10, &rest[1..]),
            Some(b'h' | b'H') => (16, &rest[1..]),
            _ => return Err(format!("invalid SV literal `{raw}`")),
        };
        let val = u64::from_str_radix(digits, radix)
            .map_err(|_| format!("invalid SV literal digits `{raw}`"))?;
        Ok((val, w.max(1)))
    } else {
        let val: u64 = clean
            .parse()
            .map_err(|_| format!("invalid integer literal `{raw}`"))?;
        let width = if val <= 1 {
            1
        } else {
            (64 - val.leading_zeros()).max(1)
        };
        Ok((val, width))
    }
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&'a Tok> {
        self.toks.get(self.pos)
    }

    fn parse_or(&mut self) -> Result<BoolExpr, String> {
        let mut lhs = self.parse_xor()?;
        while matches!(self.peek(), Some(Tok::OrOr | Tok::Pipe)) {
            self.pos += 1;
            let rhs = self.parse_xor()?;
            lhs = BoolExpr::or(lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_xor(&mut self) -> Result<BoolExpr, String> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Caret)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            lhs = BoolExpr::Xor(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<BoolExpr, String> {
        let mut lhs = self.parse_eq()?;
        while matches!(self.peek(), Some(Tok::AndAnd | Tok::Amp)) {
            self.pos += 1;
            let rhs = self.parse_eq()?;
            lhs = BoolExpr::and(lhs, rhs);
        }
        Ok(lhs)
    }

    fn parse_eq(&mut self) -> Result<BoolExpr, String> {
        let mut lhs = self.parse_unary()?;
        while let Some(op @ (Tok::EqEq | Tok::BangEq)) = self.peek() {
            let is_eq = matches!(op, Tok::EqEq);
            self.pos += 1;
            let rhs = self.parse_unary()?;
            lhs = if is_eq {
                BoolExpr::Eq(Box::new(lhs), Box::new(rhs))
            } else {
                BoolExpr::Neq(Box::new(lhs), Box::new(rhs))
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<BoolExpr, String> {
        match self.peek() {
            Some(Tok::Bang | Tok::Tilde) => {
                self.pos += 1;
                Ok(self.parse_unary()?.not())
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<BoolExpr, String> {
        match self.peek().cloned() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let e = self.parse_or()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    return Err("unclosed `(` in UPF expression".to_string());
                }
                self.pos += 1;
                Ok(e)
            }
            Some(Tok::Ident(s)) => {
                self.pos += 1;
                if s.eq_ignore_ascii_case("true") {
                    Ok(BoolExpr::ConstBool(true))
                } else if s.eq_ignore_ascii_case("false") {
                    Ok(BoolExpr::ConstBool(false))
                } else {
                    Ok(BoolExpr::Sig(s))
                }
            }
            Some(Tok::Lit(v, w)) => {
                self.pos += 1;
                if w == 1 && (v == 0 || v == 1) {
                    Ok(BoolExpr::ConstBool(v == 1))
                } else {
                    Ok(BoolExpr::Lit { val: v, width: w })
                }
            }
            other => Err(format!("unexpected token `{other:?}` in UPF expression")),
        }
    }
}
