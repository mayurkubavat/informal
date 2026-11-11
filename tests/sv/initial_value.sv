// A constant initial block gives a register's starting value (no reset here).
//% clock clk
//% assert -name a_parity {q[0] == 1'b1}
//= a_parity proven
module initial_value(input logic clk);
  logic [3:0] q;
  initial q = 4'd1;
  always_ff @(posedge clk) q <= q + 4'd2;
endmodule
