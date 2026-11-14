// elaborate -bbox_i: one instance by path; the other instance of the same
// module is still modelled.
//% elaborate -top black_box_instance -bbox_i u1
//% clock clk
//% assert -name a_boxed {y1 == ~d}
//% assert -name a_kept {y2 == ~d}
//= a_boxed cex
//= a_kept proven
module bbi_leaf(input logic [3:0] i, output logic [3:0] o);
  assign o = ~i;
endmodule
module black_box_instance(input logic clk, input logic [3:0] d);
  logic [3:0] y1, y2;
  bbi_leaf u1(.i(d), .o(y1));
  bbi_leaf u2(.i(d), .o(y2));
endmodule
