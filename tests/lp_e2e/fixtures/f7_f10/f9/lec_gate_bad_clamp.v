module core_logic (
    input  wire a,
    input  wire b,
    input  wire VDD_CORE_SW,
    input  wire VSS,
    output wire y
);
    wire ab;
    assign ab = a & b;

    INV_X1 u_inv (
        .A   (ab),
        .Y   (y),
        .VDD (VDD_CORE_SW),
        .VSS (VSS)
    );
endmodule

module lec_top (
    input  wire clk,
    input  wire rst_n,
    input  wire VDD_AON,
    input  wire VSS,
    input  wire sleep_core,
    input  wire iso_en,
    input  wire a,
    input  wire b,
    output wire dout
);
    wire VDD_CORE_SW;
    wire sleep_ack;
    wire aon_iso;
    wire core_y;

    PSW_HDR_X1 u_psw_core (
        .SLEEP    (sleep_core),
        .SLEEPOUT (sleep_ack),
        .VDD_IN   (VDD_AON),
        .VDD_OUT  (VDD_CORE_SW),
        .VSS      (VSS)
    );

    core_logic u_core (
        .a           (a),
        .b           (b),
        .VDD_CORE_SW (VDD_CORE_SW),
        .VSS         (VSS),
        .y           (core_y)
    );

    AON_BUF_X1 u_aon_iso (
        .A      (iso_en),
        .Y      (aon_iso),
        .VDD_BK (VDD_AON),
        .VSS    (VSS)
    );

    // BUG: inverted clamp_value (ISO_HI_X1 clamps to 1 instead of UPF clamp_value 0)
    ISO_HI_X1 u_iso (
        .A      (core_y),
        .ISO    (aon_iso),
        .Y      (dout),
        .VDD_BK (VDD_AON),
        .VSS    (VSS)
    );
endmodule
