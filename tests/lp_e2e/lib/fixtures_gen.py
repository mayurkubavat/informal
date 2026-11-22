"""Deterministic UPF, SystemVerilog, Liberty, and Gate/PG fixture generators."""

from __future__ import annotations

from typing import NamedTuple


class NDomainFixture(NamedTuple):
    """Paired multi-domain UPF and SystemVerilog RTL fixture."""

    upf: str
    sv: str


def generate_n_domain_upf(
    n_domains: int = 32,
    include_contradiction: bool = False,
    upf_version: str = "2.1",
) -> NDomainFixture:
    """Generate a deterministic N-domain SoC RTL + UPF fixture (`soc_top`).

    `upf_version` supports `"1.0"`, `"2.1"`, and `"3.1"`.
    When `include_contradiction=True`, seeds an explicit PST/power-state
    contradiction (`SS_D0` in 2.1/3.1 or inconsistent PST in 1.0).
    """
    if n_domains < 1:
        raise ValueError(f"n_domains must be >= 1, got {n_domains}")
    if upf_version not in {"1.0", "2.1", "3.1"}:
        raise ValueError(
            f"Unsupported upf_version {upf_version!r}; expected '1.0', '2.1', or '3.1'"
        )

    sv_lines: list[str] = [
        "module domain_block #(",
        "    parameter int ID = 0",
        ") (",
        "    input  logic       clk,",
        "    input  logic       rst_n,",
        "    input  logic [7:0] din,",
        "    output logic [7:0] dout",
        ");",
        "    always_ff @(posedge clk or negedge rst_n) begin",
        "        if (!rst_n) begin",
        "            dout <= 8'h00;",
        "        end else begin",
        "            dout <= din ^ ID[7:0];",
        "        end",
        "    end",
        "endmodule",
        "",
        "module soc_top (",
        "    input  logic                 clk,",
        "    input  logic                 rst_n,",
        "    input  logic                 VDD_AON,",
        "    input  logic                 VSS,",
        f"    input  logic [{n_domains - 1}:0] pwr_ctrl,",
        f"    output logic [{n_domains - 1}:0] pwr_ack,",
        f"    input  logic [{n_domains - 1}:0] iso_en,",
        f"    input  logic [{n_domains - 1}:0] ret_save,",
        f"    input  logic [{n_domains - 1}:0] ret_restore,",
        "    input  logic [7:0]           din,",
        "    output logic [7:0]           dout",
        ");",
        "    assign pwr_ack = pwr_ctrl;",
        f"    logic [7:0] stage [0:{n_domains}];",
        "    assign stage[0] = din;",
        "",
    ]
    for i in range(n_domains):
        sv_lines.extend(
            [
                f"    domain_block #(.ID({i})) u_dom_{i} (",
                "        .clk(clk),",
                "        .rst_n(rst_n),",
                f"        .din(stage[{i}]),",
                f"        .dout(stage[{i + 1}])",
                "    );",
            ]
        )
    sv_lines.extend(
        [
            "",
            f"    assign dout = stage[{n_domains}];",
            "endmodule",
            "",
        ]
    )

    upf_lines: list[str] = [
        f"upf_version {upf_version}",
        "set_design_top soc_top",
        "",
        "create_supply_port VDD_AON -direction in",
        "create_supply_port VSS     -direction in",
        "create_supply_net  VDD_AON",
        "create_supply_net  VSS",
        "connect_supply_net VDD_AON -ports VDD_AON",
        "connect_supply_net VSS     -ports VSS",
        "",
    ]

    if upf_version == "1.0":
        upf_lines.extend(
            [
                "create_power_domain PD_TOP -include_scope",
                "set_domain_supply_net PD_TOP -primary_power_net VDD_AON -primary_ground_net VSS",
                "add_port_state VDD_AON -state {AON_ON 0.90}",
                "add_port_state VSS     -state {VSS_ON 0.00}",
                "",
            ]
        )
        pst_supplies = ["VDD_AON", "VSS"]
        for i in range(n_domains):
            voltage = "0.90" if i % 2 == 0 else "0.75"
            pst_supplies.append(f"VDD_D{i}")
            upf_lines.extend(
                [
                    f"create_supply_net VDD_D{i}",
                    f"create_power_domain PD_D{i} -elements {{u_dom_{i}}}",
                    f"set_domain_supply_net PD_D{i} -primary_power_net VDD_D{i} -primary_ground_net VSS",
                    (
                        f"create_power_switch PSW_D{i} -domain PD_D{i} "
                        f"-input_supply_port {{in VDD_AON}} -output_supply_port {{out VDD_D{i}}} "
                        f"-control_port {{ctrl pwr_ctrl[{i}]}} -ack_port {{ack pwr_ack[{i}]}} "
                        "-on_state {ON_ST in {ctrl}} -off_state {OFF_ST {!ctrl}}"
                    ),
                    f"add_port_state VDD_D{i} -state {{D{i}_ON {voltage}}} -state {{D{i}_OFF off}}",
                    (
                        f"set_isolation iso_d{i} -domain PD_D{i} -applies_to outputs -clamp_value 0 "
                        "-isolation_power_net VDD_AON -isolation_ground_net VSS"
                    ),
                    (
                        f"set_isolation_control iso_d{i} -domain PD_D{i} "
                        f"-isolation_signal iso_en[{i}] -isolation_sense high -location parent"
                    ),
                ]
            )
            if i % 2 == 1:
                upf_lines.extend(
                    [
                        f"set_level_shifter ls_out_d{i} -domain PD_D{i} -applies_to outputs -rule low_to_high -location parent",
                        f"set_level_shifter ls_in_d{i}  -domain PD_D{i} -applies_to inputs  -rule high_to_low -location self",
                    ]
                )
            else:
                upf_lines.extend(
                    [
                        (
                            f"set_retention ret_d{i} -domain PD_D{i} "
                            "-retention_power_net VDD_AON -retention_ground_net VSS"
                        ),
                        (
                            f"set_retention_control ret_d{i} -domain PD_D{i} "
                            f"-save_signal {{ret_save[{i}] high}} -restore_signal {{ret_restore[{i}] high}}"
                        ),
                    ]
                )
            upf_lines.append("")

        upf_lines.append(f"create_pst pst_soc -supplies {{{' '.join(pst_supplies)}}}")
        if include_contradiction:
            upf_lines.append(
                "add_port_state VDD_D0 -state {D0_CONFLICT 0.75}"
            )
            upf_lines.append(
                "add_pst_state PST_BAD -pst pst_soc -state "
                + "{"
                + " ".join(
                    ["AON_ON", "VSS_ON", "D0_UNDEF"]
                    + [f"D{i}_ON" for i in range(1, n_domains)]
                )
                + "}"
            )
        else:
            all_on = ["AON_ON", "VSS_ON"] + [f"D{i}_ON" for i in range(n_domains)]
            upf_lines.append(
                f"add_pst_state PST_ALL_ON -pst pst_soc -state {{{' '.join(all_on)}}}"
            )
            all_off = ["AON_ON", "VSS_ON"] + [f"D{i}_OFF" for i in range(n_domains)]
            upf_lines.append(
                f"add_pst_state PST_SLEEP  -pst pst_soc -state {{{' '.join(all_off)}}}"
            )
    else:
        upf_lines.extend(
            [
                "create_supply_set SS_AON -function {power VDD_AON} -function {ground VSS}",
                "create_power_domain PD_TOP -include_scope -supply {primary SS_AON}",
                (
                    "add_power_state SS_AON -state {AON_ON "
                    "-supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} "
                    "-simstate NORMAL}"
                ),
                "",
            ]
        )
        for i in range(n_domains):
            voltage = "0.90" if i % 2 == 0 else "0.75"
            upf_lines.extend(
                [
                    f"create_supply_net VDD_D{i}",
                    f"create_supply_set SS_D{i} -function {{power VDD_D{i}}} -function {{ground VSS}}",
                    f"create_power_domain PD_D{i} -elements {{u_dom_{i}}} -supply {{primary SS_D{i}}}",
                    (
                        f"create_power_switch PSW_D{i} -domain PD_D{i} "
                        f"-input_supply_port {{in VDD_AON}} -output_supply_port {{out VDD_D{i}}} "
                        f"-control_port {{ctrl pwr_ctrl[{i}]}} -ack_port {{ack pwr_ack[{i}]}} "
                        "-on_state {ON_ST in {ctrl}} -off_state {OFF_ST {!ctrl}}"
                    ),
                    (
                        f"add_power_state SS_D{i} "
                        f"-state {{ON -supply_expr {{power == {{FULL_ON {voltage}}} && ground == {{FULL_ON 0.0}}}} "
                        f"-logic_expr {{pwr_ctrl[{i}]}} -simstate NORMAL}} "
                        f"-state {{OFF -supply_expr {{power == OFF && ground == {{FULL_ON 0.0}}}} "
                        f"-logic_expr {{!pwr_ctrl[{i}]}} -simstate CORRUPT}}"
                    ),
                    (
                        f"set_isolation iso_d{i} -domain PD_D{i} -applies_to outputs -clamp_value 0 "
                        f"-isolation_supply_set SS_AON -isolation_signal iso_en[{i}] "
                        "-isolation_sense high -location parent"
                    ),
                ]
            )
            if i % 2 == 1:
                upf_lines.extend(
                    [
                        f"set_level_shifter ls_out_d{i} -domain PD_D{i} -applies_to outputs -rule low_to_high -location parent",
                        f"set_level_shifter ls_in_d{i}  -domain PD_D{i} -applies_to inputs  -rule high_to_low -location self",
                    ]
                )
            else:
                upf_lines.extend(
                    [
                        (
                            f"set_retention ret_d{i} -domain PD_D{i} -retention_supply_set SS_AON "
                            f"-save_signal {{ret_save[{i}] high}} -restore_signal {{ret_restore[{i}] high}}"
                        ),
                        f"set_retention_elements ret_elems_d{i} -domain PD_D{i} -elements {{u_dom_{i}/dout}}",
                    ]
                )
            upf_lines.append("")

        if include_contradiction:
            upf_lines.extend(
                [
                    "# Contradictory illegal states covering both polarities of pwr_ctrl[0]",
                    "add_power_state SS_D0 -state {ILL_D0_ON  -logic_expr {pwr_ctrl[0]}  -illegal}",
                    "add_power_state SS_D0 -state {ILL_D0_OFF -logic_expr {!pwr_ctrl[0]} -illegal}",
                ]
            )

    return NDomainFixture(upf="\n".join(upf_lines) + "\n", sv="\n".join(sv_lines))


def _nldm_7x7_values(base: float, step_i: float, step_j: float) -> str:
    rows: list[str] = []
    for i in range(7):
        vals = [f"{base + i * step_i + j * step_j:.4f}" for j in range(7)]
        suffix = ", \\" if i < 6 else ""
        rows.append(f'                "{", ".join(vals)}"{suffix}')
    return "\n".join(rows)


def _nldm_timing_block(related_pin: str, base_delay: float = 0.030) -> str:
    rise_tbl = _nldm_7x7_values(base_delay, 0.004, 0.007)
    fall_tbl = _nldm_7x7_values(base_delay * 0.95, 0.004, 0.006)
    rtran_tbl = _nldm_7x7_values(0.015, 0.002, 0.005)
    ftran_tbl = _nldm_7x7_values(0.014, 0.002, 0.004)
    pwr_tbl = _nldm_7x7_values(0.0020, 0.0003, 0.0005)
    return f"""        timing () {{
            related_pin : "{related_pin}";
            cell_rise (delay_template_7x7) {{
                index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
                index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
                values ( \\
{rise_tbl}
                );
            }}
            cell_fall (delay_template_7x7) {{
                index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
                index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
                values ( \\
{fall_tbl}
                );
            }}
            rise_transition (delay_template_7x7) {{
                index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
                index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
                values ( \\
{rtran_tbl}
                );
            }}
            fall_transition (delay_template_7x7) {{
                index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
                index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
                values ( \\
{ftran_tbl}
                );
            }}
        }}
        internal_power () {{
            related_pin : "{related_pin}";
            rise_power (delay_template_7x7) {{
                index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
                index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
                values ( \\
{pwr_tbl}
                );
            }}
        }}"""


def generate_mini_liberty() -> str:
    """Generate a realistic Liberty `.lib` with 7x7 NLDM tables and LP/PG attributes."""
    t_a = _nldm_timing_block("A", 0.028)
    t_iso = _nldm_timing_block("ISO", 0.034)
    t_ck = _nldm_timing_block("CK", 0.045)
    t_slp = _nldm_timing_block("SLEEP", 0.050)

    return f"""library (mini_lp_lib) {{
    delay_model : table_lookup;
    nom_process : 1.0;
    nom_temperature : 25.0;
    nom_voltage : 0.90;
    voltage_map (VDD, 0.90);
    voltage_map (VDDH, 1.10);
    voltage_map (VDDL, 0.90);
    voltage_map (VDD_BK, 0.90);
    voltage_map (VDD_SW, 0.90);
    voltage_map (VDD_IN, 0.90);
    voltage_map (VDD_PERI, 0.90);
    voltage_map (VDD_ARRAY, 0.90);
    voltage_map (VSS, 0.00);

    lu_table_template (delay_template_7x7) {{
        variable_1 : input_net_transition;
        variable_2 : total_output_net_capacitance;
        index_1 ("0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64");
        index_2 ("0.001, 0.002, 0.005, 0.010, 0.020, 0.040, 0.080");
    }}

    type (bus4) {{
        base_type : array;
        data_type : bit;
        bit_width : 4;
        bit_from : 3;
        bit_to : 0;
        downto : true;
    }}

    cell (BUF_X1) {{
        pg_pin (VDD) {{
            pg_type : primary_power;
            voltage_name : VDD;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (A) {{
            direction : input;
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
        }}
        pin (Y) {{
            direction : output;
            function : "A";
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
            power_down_function : "!VDD + VSS";
{t_a}
        }}
    }}

    cell (INV_X1) {{
        pg_pin (VDD) {{
            pg_type : primary_power;
            voltage_name : VDD;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (A) {{
            direction : input;
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
        }}
        pin (Y) {{
            direction : output;
            function : "!A";
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
            power_down_function : "!VDD + VSS";
{t_a}
        }}
    }}

    cell (AON_BUF_X1) {{
        always_on : true;
        pg_pin (VDD_BK) {{
            pg_type : backup_power;
            voltage_name : VDD_BK;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (A) {{
            direction : input;
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
        }}
        pin (Y) {{
            direction : output;
            always_on : true;
            function : "A";
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
            power_down_function : "!VDD_BK + VSS";
{t_a}
        }}
    }}

    cell (ISO_LO_X1) {{
        is_isolation_cell : true;
        isolation_enable_condition : "ISO";
        pg_pin (VDD_BK) {{
            pg_type : backup_power;
            voltage_name : VDD_BK;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (A) {{
            direction : input;
            isolation_cell_data_pin : true;
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
        }}
        pin (ISO) {{
            direction : input;
            isolation_cell_enable_pin : true;
            isolation_enable_pin : true;
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
        }}
        pin (Y) {{
            direction : output;
            function : "A & !ISO";
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
            power_down_function : "!VDD_BK + VSS";
{t_a}
{t_iso}
        }}
    }}

    cell (LS_LH_X1) {{
        is_level_shifter : true;
        level_shifter_type : LH;
        valid_location : "both";
        input_voltage_range (0.65, 1.00);
        output_voltage_range (1.00, 1.25);
        pg_pin (VDDL) {{
            pg_type : primary_power;
            voltage_name : VDDL;
        }}
        pg_pin (VDDH) {{
            pg_type : backup_power;
            voltage_name : VDDH;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (A) {{
            direction : input;
            level_shifter_data_pin : true;
            related_power_pin : "VDDL";
            related_ground_pin : "VSS";
        }}
        pin (Y) {{
            direction : output;
            function : "A";
            related_power_pin : "VDDH";
            related_ground_pin : "VSS";
            power_down_function : "!VDDL + !VDDH + VSS";
{t_a}
        }}
    }}

    cell (RET_DFF_X1) {{
        retention_cell : "RETAIN";
        pg_pin (VDD) {{
            pg_type : primary_power;
            voltage_name : VDD;
        }}
        pg_pin (VDD_BK) {{
            pg_type : backup_power;
            voltage_name : VDD_BK;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        ff (IQ, IQN) {{
            clocked_on : "CK";
            next_state : "D";
        }}
        pin (CK) {{
            direction : input;
            clock : true;
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
        }}
        pin (D) {{
            direction : input;
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
        }}
        pin (SAVE) {{
            direction : input;
            retention_pin (save, "high");
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
        }}
        pin (RESTORE) {{
            direction : input;
            retention_pin (restore, "high");
            related_power_pin : "VDD_BK";
            related_ground_pin : "VSS";
        }}
        pin (Q) {{
            direction : output;
            function : "IQ";
            related_power_pin : "VDD";
            related_ground_pin : "VSS";
            power_down_function : "!VDD + VSS";
{t_ck}
        }}
    }}

    cell (PSW_HDR_X1) {{
        switch_cell_type : coarse_grain;
        pg_pin (VDD_IN) {{
            pg_type : primary_power;
            voltage_name : VDD_IN;
        }}
        pg_pin (VDD_OUT) {{
            pg_type : internal_power;
            voltage_name : VDD_SW;
            switch_function : "!SLEEP";
            pg_function : "VDD_IN";
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (SLEEP) {{
            direction : input;
            switch_pin : true;
            related_power_pin : "VDD_IN";
            related_ground_pin : "VSS";
        }}
        pin (SLEEPOUT) {{
            direction : output;
            function : "SLEEP";
            related_power_pin : "VDD_IN";
            related_ground_pin : "VSS";
            power_down_function : "!VDD_IN + VSS";
{t_slp}
        }}
    }}

    cell (SRAM_MACRO_2R) {{
        is_macro_cell : true;
        pg_pin (VDD_PERI) {{
            pg_type : primary_power;
            voltage_name : VDD_PERI;
        }}
        pg_pin (VDD_ARRAY) {{
            pg_type : backup_power;
            voltage_name : VDD_ARRAY;
        }}
        pg_pin (VSS) {{
            pg_type : primary_ground;
            voltage_name : VSS;
        }}
        pin (CK) {{
            direction : input;
            clock : true;
            related_power_pin : "VDD_PERI";
            related_ground_pin : "VSS";
        }}
        pin (CE) {{
            direction : input;
            related_power_pin : "VDD_PERI";
            related_ground_pin : "VSS";
        }}
        bus (DIN) {{
            bus_type : bus4;
            direction : input;
            related_power_pin : "VDD_PERI";
            related_ground_pin : "VSS";
        }}
        bus (DOUT) {{
            bus_type : bus4;
            direction : output;
            related_power_pin : "VDD_ARRAY";
            related_ground_pin : "VSS";
            power_down_function : "!VDD_ARRAY + VSS";
{t_ck}
        }}
    }}
}}
"""


def generate_stdcell_verilog() -> str:
    """Generate behavioral Verilog stdcell models compatible with `informal lower`."""
    return """module BUF_X1 (
    input  wire A,
    output wire Y,
    input  wire VDD,
    input  wire VSS
);
    assign Y = A;
endmodule

module INV_X1 (
    input  wire A,
    output wire Y,
    input  wire VDD,
    input  wire VSS
);
    assign Y = ~A;
endmodule

module AON_BUF_X1 (
    input  wire A,
    output wire Y,
    input  wire VDD_BK,
    input  wire VSS
);
    assign Y = A;
endmodule

module ISO_LO_X1 (
    input  wire A,
    input  wire ISO,
    output wire Y,
    input  wire VDD_BK,
    input  wire VSS
);
    assign Y = ISO ? 1'b0 : A;
endmodule

module LS_LH_X1 (
    input  wire A,
    output wire Y,
    input  wire VDDL,
    input  wire VDDH,
    input  wire VSS
);
    assign Y = A;
endmodule

module RET_DFF_X1 (
    input  wire CK,
    input  wire D,
    input  wire SAVE,
    input  wire RESTORE,
    output reg  Q,
    input  wire VDD,
    input  wire VDD_BK,
    input  wire VSS
);
    reg q_ret;
    initial begin
        Q = 1'b0;
        q_ret = 1'b0;
    end
    always @(posedge CK) begin
        if (SAVE) begin
            q_ret <= Q;
        end
        Q <= RESTORE ? q_ret : D;
    end
endmodule

module PSW_HDR_X1 (
    input  wire SLEEP,
    output wire SLEEPOUT,
    input  wire VDD_IN,
    output wire VDD_OUT,
    input  wire VSS
);
    assign SLEEPOUT = SLEEP;
    assign VDD_OUT  = SLEEP ? 1'b0 : VDD_IN;
endmodule

module SRAM_MACRO_2R (
    input  wire       CK,
    input  wire       CE,
    input  wire [3:0] DIN,
    output reg  [3:0] DOUT,
    input  wire       VDD_PERI,
    input  wire       VDD_ARRAY,
    input  wire       VSS
);
    initial begin
        DOUT = 4'b0000;
    end
    always @(posedge CK) begin
        if (CE) begin
            DOUT <= DIN;
        end
    end
endmodule
"""


def generate_gate_pg_upf() -> str:
    """Generate companion UPF 2.1 matching `soc_pg_top` from `generate_gate_pg_netlist`."""
    return """upf_version 2.1
set_design_top soc_pg_top

create_supply_port VDD_AON  -direction in
create_supply_port VDD_HIGH -direction in
create_supply_port VDD_BK   -direction in
create_supply_port VSS      -direction in

create_supply_net VDD_AON
create_supply_net VDD_HIGH
create_supply_net VDD_BK
create_supply_net VDD_CORE_SW
create_supply_net VSS

connect_supply_net VDD_AON  -ports VDD_AON
connect_supply_net VDD_HIGH -ports VDD_HIGH
connect_supply_net VDD_BK   -ports VDD_BK
connect_supply_net VSS      -ports VSS

create_supply_set SS_AON  -function {power VDD_AON}     -function {ground VSS}
create_supply_set SS_BK   -function {power VDD_BK}      -function {ground VSS}
create_supply_set SS_CORE -function {power VDD_CORE_SW} -function {ground VSS}
create_supply_set SS_HIGH -function {power VDD_HIGH}    -function {ground VSS}

create_power_domain PD_TOP  -include_scope -supply {primary SS_AON}
create_power_domain PD_CORE -elements {u_core} -supply {primary SS_CORE}
create_power_domain PD_HIGH -elements {u_high} -supply {primary SS_HIGH}

create_power_switch PSW_CORE -domain PD_CORE \\
    -input_supply_port {VDD_IN VDD_AON} \\
    -output_supply_port {VDD_OUT VDD_CORE_SW} \\
    -control_port {SLEEP sleep_core} \\
    -ack_port {SLEEPOUT sleep_ack} \\
    -on_state {CORE_ON VDD_IN {!SLEEP}} \\
    -off_state {CORE_OFF {SLEEP}}

add_power_state SS_AON -state {AON_ON -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_BK  -state {BK_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -simstate NORMAL}
add_power_state SS_CORE \\
    -state {CORE_ON  -supply_expr {power == {FULL_ON 0.90} && ground == {FULL_ON 0.0}} -logic_expr {!sleep_core} -simstate NORMAL} \\
    -state {CORE_OFF -supply_expr {power == OFF && ground == {FULL_ON 0.0}} -logic_expr {sleep_core} -simstate CORRUPT}
add_power_state SS_HIGH -state {HIGH_ON -supply_expr {power == {FULL_ON 1.10} && ground == {FULL_ON 0.0}} -simstate NORMAL}

set_isolation ISO_CORE -domain PD_CORE -applies_to outputs -clamp_value 0 \\
    -isolation_supply_set SS_BK -isolation_signal iso_en -isolation_sense high -location parent
set_retention RET_CORE -domain PD_CORE -retention_supply_set SS_BK \\
    -save_signal {ret_save high} -restore_signal {ret_restore high}
set_level_shifter LS_CORE -domain PD_CORE -applies_to outputs -rule low_to_high -location parent
"""


def generate_gate_pg_netlist(
    num_cells: int = 100000,
    include_bugs: bool = False,
) -> str:
    """Generate a deterministic structural gate/PG netlist (`soc_pg_top`).

    Builds `num_cells` total leaf stdcell/macro instances cleanly and quickly
    (<0.1s for 100,000 cells) without `inout` ports.
    When `include_bugs=True`, seeds PG mismatch, unconnected PG pin, missing ISO,
    stray logic before ISO, non-AON buffer on ISO enable, and backup-PG mismatch.
    """
    if num_cells < 1:
        raise ValueError(f"num_cells must be >= 1, got {num_cells}")

    # Fixed leaf cells across hierarchy:
    # - core_pg: u_ret, u_sram (2)
    # - high_pg: \u_high/buf_out (1)
    # - soc_pg_top: u_psw_core, u_aon_iso, u_iso, u_ls (4)
    # Plus 7 bug cells when include_bugs=True (2 in core_pg, 5 in soc_pg_top).
    fixed_leaf_cells = 7 + (7 if include_bugs else 0)
    filler_count = max(1, num_cells - fixed_leaf_cells)

    lines: list[str] = []

    # 1. core_pg module
    core_ports = [
        "    input  wire       clk,",
        "    input  wire       rst_n,",
        "    input  wire       VDD_CORE_SW,",
        "    input  wire       VDD_BK,",
        "    input  wire       VSS,",
        "    input  wire       din,",
        "    input  wire       ret_save,",
        "    input  wire       ret_restore,",
        "    output wire       core_o,",
        "    output wire [3:0] macro_dout",
    ]
    if include_bugs:
        core_ports[-1] = "    output wire [3:0] macro_dout,"
        core_ports.extend(
            [
                "    input  wire       VDD_AON,",
                "    output wire       core_uniso_o,",
                "    output wire       core_raw_o,",
                "    output wire       bug_pg_mismatch_o,",
                "    output wire       bug_pg_float_o",
            ]
        )

    lines.append("module core_pg (")
    lines.extend(core_ports)
    lines.append(");")
    lines.append("    wire d_int;")
    lines.append("    assign d_int = din & rst_n;")
    lines.append("    wire n_0;")
    lines.append(
        "    RET_DFF_X1 u_ret (.CK(clk), .D(d_int), .SAVE(ret_save), .RESTORE(ret_restore), "
        ".Q(n_0), .VDD(VDD_CORE_SW), .VDD_BK(VDD_BK), .VSS(VSS));"
    )
    lines.append(
        "    SRAM_MACRO_2R u_sram (.CK(clk), .CE(ret_save), .DIN({4{din}}), .DOUT(macro_dout), "
        ".VDD_PERI(VDD_CORE_SW), .VDD_ARRAY(VDD_BK), .VSS(VSS));"
    )

    # Declare filler chain wires in groups of 16 for compact output & fast parsing
    for start in range(1, filler_count + 1, 16):
        end = min(start + 16, filler_count + 1)
        lines.append("    wire " + ", ".join(f"n_{k}" for k in range(start, end)) + ";")

    for i in range(filler_count):
        cell = "BUF_X1" if i % 2 == 0 else "INV_X1"
        lines.append(
            f"    {cell} u_cell_{i} (.A(n_{i}), .Y(n_{i + 1}), .VDD(VDD_CORE_SW), .VSS(VSS));"
        )

    lines.append(f"    assign core_o = n_{filler_count};")
    if include_bugs:
        lines.append(f"    assign core_uniso_o = n_{filler_count};")
        lines.append("    assign core_raw_o = n_0;")
        lines.append(
            "    BUF_X1 u_pg_mismatch (.A(n_0), .Y(bug_pg_mismatch_o), .VDD(VDD_AON), .VSS(VSS));"
        )
        lines.append(
            "    BUF_X1 u_pg_unconn (.A(n_0), .Y(bug_pg_float_o), .VSS(VSS));"
        )
    lines.append("endmodule")
    lines.append("")

    # 2. high_pg module (includes escaped identifier)
    lines.extend(
        [
            "module high_pg (",
            "    input  wire VDD_HIGH,",
            "    input  wire VSS,",
            "    input  wire a,",
            "    output wire y",
            ");",
            "    BUF_X1 \\u_high/buf_out (.A(a), .Y(y), .VDD(VDD_HIGH), .VSS(VSS));",
            "endmodule",
            "",
        ]
    )

    # 3. soc_pg_top module
    top_ports = [
        "    input  wire       clk,",
        "    input  wire       rst_n,",
        "    input  wire       VDD_AON,",
        "    input  wire       VDD_HIGH,",
        "    input  wire       VDD_BK,",
        "    input  wire       VSS,",
        "    input  wire       sleep_core,",
        "    input  wire       iso_en,",
        "    input  wire       ret_save,",
        "    input  wire       ret_restore,",
        "    input  wire       din,",
        "    output wire       dout,",
        "    output wire [3:0] macro_dout",
    ]
    if include_bugs:
        top_ports[-1] = "    output wire [3:0] macro_dout,"
        top_ports.extend(
            [
                "    output wire       bug_uniso_o,",
                "    output wire       bug_stray_o,",
                "    output wire       bug_bad_iso_en_o,",
                "    output wire       bug_bk_o",
            ]
        )

    lines.append("module soc_pg_top (")
    lines.extend(top_ports)
    lines.append(");")
    lines.extend(
        [
            "    wire VDD_CORE_SW;",
            "    wire sleep_ack;",
            "    wire aon_iso;",
            "    wire core_sig;",
            "    wire iso_sig;",
            "    wire ls_sig;",
            "",
            "    PSW_HDR_X1 u_psw_core (.SLEEP(sleep_core), .SLEEPOUT(sleep_ack), "
            ".VDD_IN(VDD_AON), .VDD_OUT(VDD_CORE_SW), .VSS(VSS));",
            "    AON_BUF_X1 u_aon_iso (.A(iso_en), .Y(aon_iso), .VDD_BK(VDD_BK), .VSS(VSS));",
        ]
    )
    if include_bugs:
        lines.extend(
            [
                "    wire core_uniso_o;",
                "    wire core_raw_o;",
                "    wire bug_pg_mismatch_o;",
                "    wire bug_pg_float_o;",
                "    wire stray_n;",
                "    core_pg u_core (",
                "        .clk(clk), .rst_n(rst_n),",
                "        .VDD_CORE_SW(VDD_CORE_SW), .VDD_BK(VDD_BK), .VSS(VSS),",
                "        .din(din), .ret_save(ret_save), .ret_restore(ret_restore),",
                "        .core_o(core_sig), .macro_dout(macro_dout),",
                "        .VDD_AON(VDD_AON), .core_uniso_o(core_uniso_o), .core_raw_o(core_raw_o),",
                "        .bug_pg_mismatch_o(bug_pg_mismatch_o), .bug_pg_float_o(bug_pg_float_o)",
                "    );",
            ]
        )
    else:
        lines.extend(
            [
                "    core_pg u_core (",
                "        .clk(clk), .rst_n(rst_n),",
                "        .VDD_CORE_SW(VDD_CORE_SW), .VDD_BK(VDD_BK), .VSS(VSS),",
                "        .din(din), .ret_save(ret_save), .ret_restore(ret_restore),",
                "        .core_o(core_sig), .macro_dout(macro_dout)",
                "    );",
            ]
        )

    lines.extend(
        [
            "    ISO_LO_X1 u_iso (.A(core_sig), .ISO(aon_iso), .Y(iso_sig), .VDD_BK(VDD_BK), .VSS(VSS));",
            "    LS_LH_X1  u_ls  (.A(iso_sig), .Y(ls_sig), .VDDL(VDD_AON), .VDDH(VDD_HIGH), .VSS(VSS));",
            "    high_pg   u_high (.VDD_HIGH(VDD_HIGH), .VSS(VSS), .a(ls_sig), .y(dout));",
        ]
    )

    if include_bugs:
        lines.extend(
            [
                "    BUF_X1    u_sink_uniso (.A(core_uniso_o), .Y(bug_uniso_o), .VDD(VDD_AON), .VSS(VSS));",
                "    INV_X1    u_stray_before_iso (.A(core_raw_o), .Y(stray_n), .VDD(VDD_AON), .VSS(VSS));",
                "    ISO_LO_X1 u_iso_after_stray (.A(stray_n), .ISO(aon_iso), .Y(bug_stray_o), .VDD_BK(VDD_BK), .VSS(VSS));",
                "    BUF_X1    u_non_aon_buf (.A(iso_en), .Y(bug_bad_iso_en_o), .VDD(VDD_CORE_SW), .VSS(VSS));",
                "    ISO_LO_X1 u_iso_bad_pg (.A(core_raw_o), .ISO(aon_iso), .Y(bug_bk_o), .VDD_BK(VDD_CORE_SW), .VSS(VSS));",
            ]
        )

    lines.append("endmodule")
    lines.append("")
    return "\n".join(lines)
