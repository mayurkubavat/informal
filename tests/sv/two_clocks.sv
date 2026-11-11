// Two independent clocks cannot be stepped together: rejected.
//% clock clk
//% assert -name a1 {a == b}
//= a1 error
module two_clocks(input logic clk, input logic clk2, input logic d);
  logic a, b;
  always_ff @(posedge clk) a <= d;
  always_ff @(posedge clk2) b <= d;
endmodule
