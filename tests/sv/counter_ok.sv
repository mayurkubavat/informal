// Sanity: a counter that wraps at 9.
//% clock clk
//% reset -expression {!rst_n}
//% assert -name a_le9 {q <= 4'd9}
//% assert -name a_never9 {q != 4'd9}
//= a_le9 proven
//= a_never9 cex
module counter_ok(input logic clk, input logic rst_n, output logic [3:0] q);
  always_ff @(posedge clk) if (!rst_n) q <= 4'd0; else q <= (q == 4'd9) ? 4'd0 : q + 4'd1;
endmodule
