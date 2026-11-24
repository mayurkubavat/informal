//! Architectural CPF defaults applied during finalization: virtual primary
//! supply nets when physical nets are omitted, default switch supply wiring,
//! and default always-on domain backup/clamp supply fallback.

use crate::ir::{PowerDomain, PowerIntent, SupplyNet, SupplyNetId, VoltageSpec};

use super::{cpf_ss_name, CpfState};

pub(crate) fn apply_cpf_arch_defaults(intent: &mut PowerIntent, cs: &CpfState) {
    if intent.supply_nets.is_empty() && !cs.cpf_ss_names.is_empty() {
        let vss_id = SupplyNetId(intent.supply_nets.len() as u32);
        intent.supply_nets.push(SupplyNet {
            id: vss_id,
            name: "VSS".to_string(),
            short_name: "VSS".to_string(),
            scope: String::new(),
            domain: None,
            resolve: None,
            reuse: false,
            connected_ports: Vec::new(),
            connected_pins: Vec::new(),
            port_states: Vec::new(),
            nominal_voltage: Some(VoltageSpec::Nominal(0)),
            is_ground: true,
            is_internal: false,
            loc: None,
        });
        for pd in &mut intent.domains {
            let ss_name = cpf_ss_name(&pd.name);
            let norm_ss = ss_name.trim_start_matches('/').to_string();
            if !cs.cpf_ss_names.contains(&ss_name) && !cs.cpf_ss_names.contains(&norm_ss) {
                continue;
            }
            let Some(ss) = intent.supply_sets.iter_mut().find(|s| s.name == ss_name) else {
                continue;
            };
            if ss.power.is_none() {
                let leaf = pd.name.rsplit('/').next().unwrap_or(&pd.name);
                let vdd_name = if pd.scope.is_empty() {
                    format!("VDD_{leaf}")
                } else {
                    format!("{}/VDD_{leaf}", pd.scope)
                };
                let nom_v = intent
                    .power_states
                    .iter()
                    .filter(|ps| {
                        !ps.illegal && !ps.update && ps.target.trim_start_matches('/') == norm_ss
                    })
                    .find_map(|ps| {
                        ps.rail_states
                            .iter()
                            .find(|r| r.function == "power" && !r.is_off && r.voltage.is_on_known())
                            .map(|r| r.voltage)
                    });
                let nid = SupplyNetId(intent.supply_nets.len() as u32);
                intent.supply_nets.push(SupplyNet {
                    id: nid,
                    name: vdd_name.clone(),
                    short_name: format!("VDD_{leaf}"),
                    scope: pd.scope.clone(),
                    domain: Some(pd.name.clone()),
                    resolve: None,
                    reuse: false,
                    connected_ports: Vec::new(),
                    connected_pins: Vec::new(),
                    port_states: Vec::new(),
                    nominal_voltage: nom_v,
                    is_ground: false,
                    is_internal: false,
                    loc: pd.loc.clone(),
                });
                ss.power = Some(vdd_name.clone());
                if ss.ground.is_none() {
                    ss.ground = Some("VSS".to_string());
                }
                if pd.legacy_primary_power_net.is_none() {
                    pd.legacy_primary_power_net = Some(vdd_name);
                }
                if pd.legacy_primary_ground_net.is_none() {
                    pd.legacy_primary_ground_net = Some("VSS".to_string());
                }
            }
        }
    }

    let default_domain_for_scope = |domains: &[PowerDomain], scope: &str| {
        domains
            .iter()
            .find(|d| d.scope == scope && d.include_scope)
            .or_else(|| domains.iter().find(|d| d.is_top || d.include_scope))
            .map(|d| (d.name.clone(), d.legacy_primary_power_net.clone()))
    };

    for sw in &mut intent.power_switches {
        let uses_vdd_in = sw
            .on_states
            .iter()
            .any(|s| s.supply_port.as_deref() == Some("vdd_in"));
        if sw.input_supply_ports.is_empty() && uses_vdd_in {
            if let Some((def_name, Some(def_pwr))) =
                default_domain_for_scope(&intent.domains, &sw.scope)
            {
                if def_name != sw.domain.trim_start_matches('/') {
                    sw.input_supply_ports.push(("vdd_in".to_string(), def_pwr));
                }
            }
        }
        if sw.output_supply_ports.is_empty() && uses_vdd_in {
            let dom_key = sw.domain.trim_start_matches('/');
            if let Some(pd) = intent.domains.iter().find(|d| d.name == dom_key) {
                if let Some(ref pwr) = pd.legacy_primary_power_net {
                    sw.output_supply_ports
                        .push(("vdd_out".to_string(), pwr.clone()));
                }
            }
        }
    }

    let is_cpf_dom = |dom: &str| {
        let ss = cpf_ss_name(dom);
        cs.cpf_ss_names.contains(&ss) || cs.cpf_ss_names.contains(ss.trim_start_matches('/'))
    };
    for iso in &mut intent.isolation_strategies {
        if iso.isolation_supply_set.is_none()
            && iso.isolation_power_net.is_none()
            && is_cpf_dom(&iso.domain)
        {
            if let Some((def_name, _)) = default_domain_for_scope(&intent.domains, &iso.scope) {
                if def_name != iso.domain.trim_start_matches('/') {
                    iso.isolation_supply_set = Some(format!("{def_name}.primary"));
                }
            }
        }
    }
    for ret in &mut intent.retention_strategies {
        if ret.retention_supply_set.is_none()
            && ret.retention_power_net.is_none()
            && is_cpf_dom(&ret.domain)
        {
            if let Some((def_name, _)) = default_domain_for_scope(&intent.domains, &ret.scope) {
                if def_name != ret.domain.trim_start_matches('/') {
                    ret.retention_supply_set = Some(format!("{def_name}.primary"));
                }
            }
        }
    }
}
