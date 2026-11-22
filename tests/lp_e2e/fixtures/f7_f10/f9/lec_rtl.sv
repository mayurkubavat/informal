module core_logic (
    input  logic a,
    input  logic b,
    output logic y
);
    assign y = ~(a & b);
endmodule

module lec_top (
    input  logic clk,
    input  logic rst_n,
    input  logic VDD_AON,
    input  logic VSS,
    input  logic sleep_core,
    input  logic iso_en,
    input  logic a,
    input  logic b,
    output logic dout
);
    logic core_y;

    core_logic u_core (
        .a (a),
        .b (b),
        .y (core_y)
    );

    assign dout = core_y;
endmodule
