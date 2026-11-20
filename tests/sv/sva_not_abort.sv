// SVA property negation (`not`), followed-by (`#-#`, `#=#`), and synchronous
// aborts (`sync_accept_on`, `sync_reject_on`) compiled natively in svlang:
// command properties and RTL properties (tested via rtl_form) agree.
//% clock clk
//% reset -expression {rst}
//% assert -name a_not_ok {not (req ##1 bad)}
//% assert -name a_not_cex {not (req ##1 ack)}
//% assert -name a_sync_acc {sync_accept_on (abort) (req |=> ack || abort_d)}
//% assert -name a_sync_rej {sync_reject_on (bad) ((req && !abort) |=> ack)}
//% assert -name a_sync_rej_cex {sync_reject_on (abort) (req |=> ack || abort_d)}
//% cover -name c_fby {(req ##1 ack) #-# done}
//% cover -name c_fby_unreach {(req ##1 bad) #=# done}
//= a_not_ok proven
//= a_not_cex cex
//= a_sync_acc proven
//= a_sync_rej proven
//= a_sync_rej_cex cex
//= c_fby covered
//= c_fby_unreach unreachable
module sva_not_abort(
  input  logic clk,
  input  logic rst,
  input  logic req,
  input  logic abort,
  output logic ack,
  output logic done,
  output logic abort_d,
  output logic bad
);
  assign bad = 1'b0;
  always_ff @(posedge clk) begin
    if (rst) begin
      ack     <= 1'b0;
      done    <= 1'b0;
      abort_d <= 1'b0;
    end else begin
      ack     <= req && !abort;
      done    <= req && !abort;
      abort_d <= abort;
    end
  end
endmodule
