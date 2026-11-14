// Liveness: a request is granted three cycles later unless `stall` holds
// the work at its second step, which it may do for ever. So a grant does
// not always come (a counterexample is a loop of states), while the step
// before the stall point always does.
//% clock clk
//% reset -expression {rst}
//% assert -name x_live {disable iff (rst) req && n == 2'd0 |-> s_eventually gnt}
//% assert -name x_step {disable iff (rst) n == 2'd1 |-> s_eventually n == 2'd2}
//= x_live cex
//= x_step proven
//= liveness.a_live cex
//= liveness.a_step proven
//= liveness.a_idle cex
//= liveness.a_back proven
//= liveness.a_safe proven
module liveness(input logic clk, input logic rst, input logic req, input logic stall);
  logic [1:0] n;      // 0 idle, 1 and 2 working, 3 granting
  logic       gnt;
  assign gnt = n == 2'd3;
  always_ff @(posedge clk)
    if (rst) n <= 2'd0;
    else if (n == 2'd0) begin if (req) n <= 2'd1; end
    else if (!(stall && n == 2'd2)) n <= n + 2'd1;
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  a_live: assert property (req && n == 2'd0 |-> s_eventually gnt);
  a_step: assert property (n == 2'd1 |-> s_eventually n == 2'd2);
  // Nothing makes a request come.
  a_idle: assert property (s_eventually gnt);
  // A grant always leads back to idle, one cycle later.
  a_back: assert property (gnt |=> s_eventually n == 2'd0);
  // A safety property beside them is checked as before.
  a_safe: assert property (gnt |=> !gnt);
endmodule
