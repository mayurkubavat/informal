// A counter with one assertion and no assumptions of its own: the
// environment comes from the script (crates/informal-fpv/tests/vacuity.rs).
module counter(input logic clk, rst, en, output logic [3:0] cnt);
  always_ff @(posedge clk) if (rst) cnt <= 0; else if (en) cnt <= cnt + 1;
  a_le: assert property (@(posedge clk) disable iff (rst) cnt <= 5);
endmodule
