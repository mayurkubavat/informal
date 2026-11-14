// Bounded sequences in properties written in the RTL. A request is granted
// two or three cycles later (never one), and held for two cycles.
//% clock clk
//% reset -expression {rst}
//= rtl_ranges.a_window proven
//= rtl_ranges.a_early cex
//= rtl_ranges.a_exact cex
//= rtl_ranges.a_held proven
//= rtl_ranges.a_through proven
//= rtl_ranges.a_through_wrong cex
//= rtl_ranges.a_first proven
//= rtl_ranges.c_late covered
//= rtl_ranges.c_one unreachable
//= rtl_ranges.m_gap_used proven
module rtl_ranges(input logic clk, input logic rst, input logic req, input logic slow);
  logic [2:0] n;      // 0 idle, 1..2 or 1..3 waiting, 4..5 granting
  logic       busy, gnt, late;
  assign busy = n != 3'd0;
  assign gnt  = n >= 3'd4;
  always_ff @(posedge clk)
    if (rst) begin n <= 3'd0; late <= 1'b0; end
    else case (n)
      3'd0: if (req) begin n <= 3'd1; late <= slow; end
      3'd1: n <= late ? 3'd2 : 3'd4;
      3'd2: n <= 3'd4;
      3'd4: n <= 3'd5;
      default: n <= 3'd0;
    endcase
  default clocking @(posedge clk); endclocking
  default disable iff (rst);
  // No new request while one is in flight.
  m_gap: assume property (busy |-> !req);
  m_gap_used: assert property (req |-> !busy);
  a_window: assert property (req |-> ##[2:3] gnt);
  a_early:  assert property (req |-> ##[1:2] gnt);
  a_exact:  assert property (req |-> ##2 gnt);
  a_held:   assert property (req |-> ##[2:3] gnt [*2]);
  a_through: assert property (req |=> busy throughout (##[1:2] gnt));
  a_through_wrong: assert property (req |=> !gnt throughout (##[1:2] gnt));
  a_first:  assert property (req ##1 first_match(##[1:2] gnt) |=> gnt);
  c_late:   cover property (req ##3 gnt);
  c_one:    cover property (req ##1 gnt);
endmodule
