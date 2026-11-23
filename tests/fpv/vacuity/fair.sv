// A grant with one liveness assertion and no assumptions of its own: the
// fairness assumptions come from the script (crates/informal-fpv/tests/vacuity.rs).
module fair(input logic clk, rst, req, gnt);
  a_live: assert property (@(posedge clk) disable iff (rst) req |-> s_eventually gnt);
endmodule
