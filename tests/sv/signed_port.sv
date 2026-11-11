// A narrow signed output port driving a wider net sign-extends.
//% clock clk
//% assert -name a_port {w == 8'hfe}
//= a_port proven
module signed_src(output logic signed [3:0] o);
  assign o = -4'sd2;
endmodule
module signed_port(input logic clk);
  logic signed [7:0] w;
  signed_src u(.o(w));
endmodule
