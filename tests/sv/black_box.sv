// elaborate -bbox_m: instances of the module keep their ports, nothing
// inside is modelled and the outputs are free. What relied on the box's
// logic can fail; the rest still holds. (The memory inside the box is not
// part of the model at all.)
//% elaborate -top black_box -bbox_m bb_leaf
//% clock clk
//% reset -expression {rst}
//% assert -name a_through_box {y == ~d}
//% assert -name a_beside {z == (d ^ 4'h5)}
//% assert -name a_other_inst {w == d + 4'd1}
//= a_through_box cex
//= a_beside proven
//= a_other_inst proven
module bb_leaf(input logic clk, input logic [3:0] i, output logic [3:0] o);
  logic [3:0] mem [0:15];
  always_ff @(posedge clk) mem[i] <= i;
  assign o = ~i;
endmodule
module bb_plain(input logic [3:0] i, output logic [3:0] o);
  assign o = i + 4'd1;
endmodule
module black_box(input logic clk, input logic rst, input logic [3:0] d);
  logic [3:0] y, z, w;
  bb_leaf u(.clk(clk), .i(d), .o(y));
  bb_plain v(.i(d), .o(w));
  assign z = d ^ 4'h5;
endmodule
