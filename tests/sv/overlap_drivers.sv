// The same bits assigned in two blocks: a race, rejected.
//% clock clk
//% assert -name a1 {v == 4'h0}
//= a1 error
module overlap_drivers(input logic clk, input logic d);
  logic [3:0] v;
  always @(posedge clk) v[2:0] <= {3{d}};
  always @(posedge clk) v[3:2] <= {2{d}};
endmodule
