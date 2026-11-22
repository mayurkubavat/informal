module core_macro_wrap (
    input  wire       clk,
    input  wire       ce,
    input  wire [3:0] din,
    input  wire       VDD_CORE_SW,
    input  wire       VDD_BK,
    input  wire       VSS,
    output wire [3:0] dout
);
    // DOUT pin has related_power_pin = VDD_PERI (VDD_CORE_SW, switchable)
    // while DIN has related_power_pin = VDD_ARRAY (VDD_BK, always-on) -> MACRO_PIN_SUPPLY_CROSSING
    SRAM_MACRO_BAD_PIN u_sram (
        .CK        (clk),
        .CE        (ce),
        .DIN       (din),
        .DOUT      (dout),
        .VDD_PERI  (VDD_CORE_SW),
        .VDD_ARRAY (VDD_BK),
        .VSS       (VSS)
    );
endmodule

module macro_pin_crossing (
    input  wire       clk,
    input  wire       VDD_AON,
    input  wire       VDD_BK,
    input  wire       VSS,
    input  wire       sleep_core,
    input  wire       ce,
    input  wire [3:0] din,
    output wire [3:0] dout
);
    wire       VDD_CORE_SW;
    wire       sleep_ack;
    wire [3:0] sram_q;

    PSW_HDR_X1 u_psw_core (
        .SLEEP    (sleep_core),
        .SLEEPOUT (sleep_ack),
        .VDD_IN   (VDD_AON),
        .VDD_OUT  (VDD_CORE_SW),
        .VSS      (VSS)
    );

    core_macro_wrap u_core (
        .clk         (clk),
        .ce          (ce),
        .din         (din),
        .VDD_CORE_SW (VDD_CORE_SW),
        .VDD_BK      (VDD_BK),
        .VSS         (VSS),
        .dout        (sram_q)
    );

    // Always-on sink receiving unisolated switchable DOUT[3:0] from macro
    BUF_X1 u_sink_0 (.A(sram_q[0]), .Y(dout[0]), .VDD(VDD_AON), .VSS(VSS));
    BUF_X1 u_sink_1 (.A(sram_q[1]), .Y(dout[1]), .VDD(VDD_AON), .VSS(VSS));
    BUF_X1 u_sink_2 (.A(sram_q[2]), .Y(dout[2]), .VDD(VDD_AON), .VSS(VSS));
    BUF_X1 u_sink_3 (.A(sram_q[3]), .Y(dout[3]), .VDD(VDD_AON), .VSS(VSS));
endmodule
