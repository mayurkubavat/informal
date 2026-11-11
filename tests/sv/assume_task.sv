// Tasks are not modelled: an assumption scoped to a task would constrain
// every property, so it is an error.
//% clock clk
//% assume -task t1 -name m1 {d == 1'b0}
//! tasks are not modelled
module assume_task(input logic clk, input logic d);
  // (Yosys, which still elaborates the plain design, treats an empty module as a black box.)
  wire unused = clk;
endmodule
