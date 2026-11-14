// A black-box instance path that does not exist is an error.
//% elaborate -top black_box_typo -bbox_i u_missing
//! is not an instance of black_box_typo
module black_box_typo(input logic clk, input logic d);
  logic q;
  always_ff @(posedge clk) q <= d;
endmodule
