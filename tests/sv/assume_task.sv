// Referencing a task that has not been created with `task -create` is an
// error: an assumption is never dropped into `<embedded>` by mistake.
//% clock clk
//% assume -task t1 -name m1 {d == 1'b0}
//! unknown task `t1`
module assume_task(input logic clk, input logic d);
  wire unused = clk;
endmodule
