module f6_inv_cell (
    input  logic a,
    output logic y
);
    assign y = ~a;
endmodule

module f6_pol_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 8'h00;
        else        dout <= din;
    end
endmodule

module f6_iso_pol_inv3 (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       pwr_en,
    input  logic       aon_iso_en,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic inv1, inv2, iso_ctrl_inv;

    // 3-inverter odd chain -> negative polarity relative to aon_iso_en
    f6_inv_cell u_inv1 (.a(aon_iso_en), .y(inv1));
    f6_inv_cell u_inv2 (.a(inv1),       .y(inv2));
    f6_inv_cell u_inv3 (.a(inv2),       .y(iso_ctrl_inv));

    f6_pol_core u_core (.clk(clk), .rst_n(rst_n), .din(din), .dout(dout));
endmodule
