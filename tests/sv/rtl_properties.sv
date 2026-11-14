// Concurrent properties written in the RTL are checked under their full
// hierarchical names, with the answers the same properties give through
// the `assert` / `cover` commands (the x_ ones).
//% clock clk
//% reset -expression {rst}
//% assert -name x_wrong {q < 4'd5}
//% assert -name x_step {en && q < 4'd9 |=> q == $past(q) + 4'd1}
//% assert -name x_hold {!en |=> $stable(q)}
//% cover -name x_seq {q == 4'd3 ##1 q == 4'd4 ##1 q == 4'd4}
//= x_wrong cex
//= x_step proven
//= x_hold proven
//= x_seq covered
//= rtl_properties.a_wrong cex
//= rtl_properties.a_max proven
//= rtl_properties.a_step proven
//= rtl_properties.a_hold proven
//= rtl_properties.a_two proven
//= rtl_properties.a_two_wrong cex
//= rtl_properties.a_rep proven
//= rtl_properties.c_nine covered
//= rtl_properties.c_never unreachable
//= rtl_properties.c_seq covered
//= rtl_properties._assert_1 proven
module rtl_properties(input logic clk, input logic rst, input logic en, output logic [3:0] q);
  always_ff @(posedge clk)
    if (rst) q <= 4'd0;
    else if (en && q != 4'd9) q <= q + 4'd1;
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  a_wrong:     assert property (q < 4'd5);
  a_max:       assert property (q <= 4'd9);
  a_step:      assert property (en && q < 4'd9 |=> q == $past(q) + 4'd1);
  a_hold:      assert property (!en |=> $stable(q));
  a_two:       assert property (en ##1 en |-> ##1 q >= 4'd2);
  a_two_wrong: assert property (en ##1 en |-> ##1 q >= 4'd3);
  a_rep:       assert property (en [*3] |=> q >= 4'd3);
  c_nine:      cover property (q == 4'd9);
  c_never:     cover property (q == 4'd12);
  c_seq:       cover property (q == 4'd3 ##1 q == 4'd4 ##1 q == 4'd4);
  assert property (q != 4'd10);
endmodule
