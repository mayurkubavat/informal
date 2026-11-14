// `until` in properties, written in the RTL and given by command: once a
// request is taken, busy holds until done, and done comes with busy still
// up (until_with) but one cycle after go at the earliest.
//% clock clk
//% reset -expression {rst}
//% assert -name x_until {disable iff (rst) go && !busy |=> busy until done}
//% assert -name x_with {disable iff (rst) go && !busy |=> busy until_with done}
//% assert -name x_wrong {disable iff (rst) go && !busy |=> !done until !busy}
//= x_until proven
//= x_with proven
//= x_wrong cex
//= rtl_until.a_until proven
//= rtl_until.a_with proven
//= rtl_until.a_idle proven
//= rtl_until.a_wrong cex
//= rtl_until.a_early cex
module rtl_until(input logic clk, input logic rst, input logic go, input logic fin);
  logic busy, done;
  // done is a one-cycle pulse in the last busy cycle; fin ends the work.
  assign done = busy && fin;
  always_ff @(posedge clk)
    if (rst) busy <= 1'b0;
    else if (!busy) busy <= go;
    else if (fin) busy <= 1'b0;
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  a_until: assert property (go && !busy |=> busy until done);
  a_with:  assert property (go && !busy |=> busy until_with done);
  // Idle stays idle until a request.
  a_idle:  assert property (!busy |-> !busy until go);
  // busy does not outlast done.
  a_wrong: assert property (go && !busy |=> busy until !go);
  // Nor does the work always take two cycles.
  a_early: assert property (go && !busy |=> !done until_with busy);
endmodule
