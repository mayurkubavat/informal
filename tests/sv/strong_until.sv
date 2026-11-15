// A strong until is two things: what has to hold meanwhile (a safety
// check) and its end having to come (liveness). Here busy holds until
// done, but done need not come (the work can stall for ever), so the
// strong form fails with a loop where the weak one is proven; with the
// stall assumed to end, both hold. Bounded `nexttime`, `eventually` and
// `always` are sequences in other words.
//% clock clk
//% reset -expression {rst}
//% assert -name x_weak {disable iff (rst) go && !busy |=> busy until done}
//% assert -name x_strong {disable iff (rst) go && !busy |=> busy s_until done}
//% assert -name x_next {disable iff (rst) go && !busy |-> nexttime busy}
//% assert -name x_within {disable iff (rst) go && !busy |-> s_eventually [1:2] busy}
//% assert -name x_stay {disable iff (rst) go && !busy |-> nexttime always [0:1] busy}
//= x_weak proven
//= x_strong cex
//= x_next proven
//= x_within proven
//= x_stay cex
//= strong_until.a_strong cex
//= strong_until.a_idle proven
module strong_until(input logic clk, input logic rst, input logic go, input logic fin);
  logic busy, done;
  assign done = busy && fin;
  always_ff @(posedge clk)
    if (rst) busy <= 1'b0;
    else if (!busy) busy <= go;
    else if (fin) busy <= 1'b0;
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  a_strong: assert property (go && !busy |=> busy s_until_with done);
  // Idle ends: a request is assumed to come some time.
  m_go:     assume property (s_eventually go);
  a_idle:   assert property (!busy |-> !busy s_until go);
endmodule
