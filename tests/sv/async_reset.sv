// The usual async-reset flop: the reset edge is not a second clock.
//% clock clk
//% reset -expression {!rst_n}
//% assert -name a_q {q == 1'b0 || q == 1'b1}
//% assert -name a_hold {d == 1'b1 || q == 1'b0 || $past(d) == 1'b1}
//= a_q proven
//= a_hold proven
module async_reset(input logic clk, input logic rst_n, input logic d, output logic q);
  always_ff @(posedge clk or negedge rst_n) if (!rst_n) q <= 1'b0; else q <= d;
endmodule
