module core_extra (
    input  wire clk,
    input  wire din,
    input  wire VDD_CORE_SW,
    input  wire VSS,
    output wire core_q
);
    // Non-retention DFF_X1 inside a domain with set_retention -> RET_CELL_UNMAPPED
    DFF_X1 u_unmapped_ret (
        .CK  (clk),
        .D   (din),
        .Q   (core_q),
        .VDD (VDD_CORE_SW),
        .VSS (VSS)
    );
endmodule

module high_extra (
    input  wire VDD_HIGH,
    input  wire VSS,
    input  wire a,
    output wire y
);
    BUF_X1 u_high_buf (
        .A   (a),
        .Y   (y),
        .VDD (VDD_HIGH),
        .VSS (VSS)
    );
endmodule

module stage3_bugs_extra (
    input  wire clk,
    input  wire VDD_AON,
    input  wire VDD_HIGH,
    input  wire VDD_BK,
    input  wire VSS,
    input  wire sleep_core,
    input  wire iso_en,
    input  wire din,
    output wire dout
);
    wire VDD_CORE_SW;
    wire sleep_ack;
    wire core_q;
    wire bad_iso_en;
    wire iso_y;
    wire ls_y;

    PSW_HDR_X1 u_psw_core (
        .SLEEP    (sleep_core),
        .SLEEPOUT (sleep_ack),
        .VDD_IN   (VDD_AON),
        .VDD_OUT  (VDD_CORE_SW),
        .VSS      (VSS)
    );

    core_extra u_core (
        .clk         (clk),
        .din         (din),
        .VDD_CORE_SW (VDD_CORE_SW),
        .VSS         (VSS),
        .core_q      (core_q)
    );

    // Non-AON buffer powered by switchable VDD_CORE_SW driving ISO enable -> AON_NON_AON_BUFFER
    BUF_X1 u_bad_ctrl (
        .A   (iso_en),
        .Y   (bad_iso_en),
        .VDD (VDD_CORE_SW),
        .VSS (VSS)
    );

    ISO_LO_X1 u_iso_ctrl_bug (
        .A      (core_q),
        .ISO    (bad_iso_en),
        .Y      (iso_y),
        .VDD_BK (VDD_BK),
        .VSS    (VSS)
    );

    // Wrong-direction HL level shifter on 0.90V -> 1.10V crossing -> LS_DIR_CELL_MISMATCH
    LS_HL_X1 u_bad_ls (
        .A    (iso_y),
        .Y    (ls_y),
        .VDDH (VDD_AON),
        .VDDL (VDD_HIGH),
        .VSS  (VSS)
    );

    high_extra u_high (
        .VDD_HIGH (VDD_HIGH),
        .VSS      (VSS),
        .a        (ls_y),
        .y        (dout)
    );
endmodule
