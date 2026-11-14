// With the assumption that the stall always ends some time (a fairness
// constraint), every request is granted. Without a request nothing is.
//% clock clk
//% reset -expression {rst}
//% assume -name m_cmd {disable iff (rst) s_eventually !stall}
//% assert -name x_live {disable iff (rst) req && n == 2'd0 |-> s_eventually gnt}
//= x_live proven
//= liveness_fair.a_live proven
//= liveness_fair.a_idle cex
module liveness_fair(input logic clk, input logic rst, input logic req, input logic stall);
  logic [1:0] n;
  logic       gnt;
  assign gnt = n == 2'd3;
  always_ff @(posedge clk)
    if (rst) n <= 2'd0;
    else if (n == 2'd0) begin if (req) n <= 2'd1; end
    else if (!(stall && n == 2'd2)) n <= n + 2'd1;
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  a_live: assert property (req && n == 2'd0 |-> s_eventually gnt);
  a_idle: assert property (s_eventually gnt);
endmodule
