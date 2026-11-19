// IEEE 1800 `bind` directive attaching an SVA checker module to an internal
// submodule instance, binding internal signals of the target scope.
//% clock clk
//% reset -expression {rst}
//= sva_bind.u_core.u_mon.a_bound_ok proven
//= sva_bind.u_core.u_mon.a_bound_goto proven
//= sva_bind.u_core.u_mon.a_bound_bad cex
//= sva_bind.u_core.u_mon.c_bound_full covered
module sva_bind_core(
  input  logic       clk,
  input  logic       rst,
  input  logic       inc,
  output logic [1:0] level
);
  logic [1:0] count_q;
  assign level = count_q;

  always_ff @(posedge clk) begin
    if (rst) count_q <= 2'd0;
    else if (inc && count_q < 2'd3) count_q <= count_q + 2'd1;
  end
endmodule

module sva_bind_mon(
  input logic       clk,
  input logic       rst,
  input logic       inc,
  input logic [1:0] count_q
);
  default clocking @(posedge clk); endclocking
  default disable iff (rst);

  a_bound_ok:   assert property (count_q == 2'd3 |=> count_q == 2'd3);
  a_bound_goto: assert property ((count_q == 2'd0 && inc) ##1 inc [->2] |=> count_q == 2'd3);
  a_bound_bad:  assert property (count_q <= 2'd2);
  c_bound_full: cover  property (inc [->3] ##1 count_q == 2'd3);
endmodule

bind sva_bind_core sva_bind_mon u_mon(
  .clk(clk),
  .rst(rst),
  .inc(inc),
  .count_q(count_q)
);

module sva_bind(
  input  logic       clk,
  input  logic       rst,
  input  logic       inc,
  output logic [1:0] level
);
  sva_bind_core u_core(.clk(clk), .rst(rst), .inc(inc), .level(level));
endmodule
