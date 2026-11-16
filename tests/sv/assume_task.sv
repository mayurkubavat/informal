// An assumption for a task that was never made is an error: a typo in a
// task name must not drop the assumption (tasks that exist are tested in
// crates/informal-fpv/tests/tasks.rs).
//% clock clk
//% assume -task t1 -name m1 {d == 1'b0}
//! no task `t1`
module assume_task(input logic clk, input logic d);
  wire unused = clk;
endmodule
