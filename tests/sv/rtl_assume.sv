// An assumption written in the RTL constrains the model: without m_low the
// counter could pass 3. A property in a child instance is named by its
// instance path.
//% clock clk
//% reset -expression {rst}
//% assert -name x_low {q <= 4'd3}
//= x_low proven
//= rtl_assume.u.a_in proven
//= rtl_assume.u.a_free cex
module rtl_assume_child(input logic clk, input logic rst, input logic [3:0] v, input logic f);
  a_in:   assert property (@(posedge clk) disable iff (rst) v <= 4'd3);
  a_free: assert property (@(posedge clk) !f);
endmodule
module rtl_assume(input logic clk, input logic rst, input logic en, input logic f, output logic [3:0] q);
  always_ff @(posedge clk)
    if (rst) q <= 4'd0;
    else if (en) q <= q + 4'd1;
  m_low: assume property (@(posedge clk) disable iff (rst) q == 4'd3 |-> !en);
  rtl_assume_child u(.clk(clk), .rst(rst), .v(q), .f(f));
endmodule
