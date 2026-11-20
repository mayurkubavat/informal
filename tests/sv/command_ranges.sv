// Bounded sequence operators in properties given by command: the front
// end binds them in the top module and compiles them, with the answers the
// same properties give when written in the RTL (rtl_ranges.sv). An
// operator it cannot compile is an error on that property only.
//% clock clk
//% reset -expression {rst}
//% assume -name m_gap {disable iff (rst) busy |-> !req}
//% assert -name a_window {disable iff (rst) req |-> ##[2:3] gnt}
//% assert -name a_early {disable iff (rst) req |-> ##[1:2] gnt}
//% assert -name a_held {disable iff (rst) req |-> ##[2:3] gnt [*2]}
//% assert -name a_through {disable iff (rst) req |=> busy throughout (##[1:2] gnt)}
//% assert -name a_first {@(posedge clk) disable iff (rst) req ##1 first_match(##[1:2] gnt) |=> gnt}
//% assert -name a_unbounded {disable iff (rst) req |-> ##[1:$] gnt}
//% assert -name a_always {disable iff (rst) req |-> always (n <= 3'd5)}
//% assert -name a_always_seq {disable iff (rst) req |-> always (gnt ##1 gnt)}
//% assert -name a_unsup {disable iff (rst) req |-> (gnt ##1 gnt) [->2]}
//% assert -name a_fixed {disable iff (rst) req |-> ##1 !gnt}
//% cover -name c_late {disable iff (rst) req ##[3:4] gnt ##1 gnt}
//% cover -name c_one {disable iff (rst) req ##[0:1] gnt}
//= a_window proven
//= a_early cex
//= a_held proven
//= a_through proven
//= a_first proven
//= a_unbounded proven
//= a_always proven
//= a_always_seq cex
//= a_unsup error
//= a_fixed proven
//= c_late covered
//= c_one unreachable
module command_ranges(input logic clk, input logic rst, input logic req, input logic slow);
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
endmodule
