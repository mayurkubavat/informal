// A child instance's clock port is the same net as the parent's clock:
// one clock, not two.
//% clock clk
//% reset -expression {rst}
//% assert -name a_same {a == b}
//= a_same proven
module child_ff(input logic clk, input logic rst, input logic d, output logic q);
  always_ff @(posedge clk) q <= rst ? 1'b0 : d;
endmodule
module child_clock(input logic clk, input logic rst, input logic d);
  logic a, b;
  always_ff @(posedge clk) a <= rst ? 1'b0 : d;
  child_ff u(.clk(clk), .rst(rst), .d(d), .q(b));
endmodule
