// A bind whose parameter override names nothing in the target scope gives
// the bound module no usable parameter: the design has no model, and the
// bound module's assertions must not vanish without a word.
//% clock clk
//% prove -all
//! nothing was proved: the design cannot be modelled
module bind_typo_props #(parameter int unsigned WIDTH = 4)
    (input logic clk, input logic [WIDTH-1:0] q);
  a_stable: assert property (@(posedge clk) q == q);
endmodule
module bind_typo #(parameter int unsigned Width = 4)
    (input logic clk, input logic [Width-1:0] d, output logic [Width-1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
bind bind_typo bind_typo_props #(.WIDTH(WIDTH)) i_props (.clk(clk), .q(q));
