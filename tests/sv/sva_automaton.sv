// Compiler-native SVA NFA/DFA operators: goto (`[->m:n]`), non-consecutive
// (`[=m:n]`), unbounded delay (`##[m:$]`) and repetition (`[*m:$]`). Written
// in the plain command shape so `rtl_form_gives_the_same_answers` checks the
// RTL path too.
//% clock clk
//% reset -expression {rst}
//% assert -name a_goto2 {(req && !busy) ##1 ack [->2] |-> done}
//% assert -name a_goto1_bad {(req && !busy) ##1 ack [->1] |-> done}
//% assert -name a_nonconsec {(req && !busy) ##1 ack [=2] ##1 tag |=> saw_two}
//% assert -name a_unbounded_ant {req ##[1:$] done |-> busy}
//% assert -name a_star_unbounded {(req && !busy) ##1 busy [*1:$] ##1 done |-> ack}
//% cover -name c_goto2 {(req && !busy) ##1 ack [->2] ##1 !busy}
//% cover -name c_unreach {req ##1 done [->1] ##0 !busy}
//= a_goto2 proven
//= a_goto1_bad cex
//= a_nonconsec proven
//= a_unbounded_ant proven
//= a_star_unbounded proven
//= c_goto2 covered
//= c_unreach unreachable
module sva_automaton(
  input  logic clk,
  input  logic rst,
  input  logic req,
  input  logic step,
  input  logic tag,
  output logic busy,
  output logic ack,
  output logic done,
  output logic saw_two
);
  logic [1:0] cnt;
  logic       completed;

  assign busy = cnt != 2'd0;
  assign ack  = busy && step;
  assign done = (cnt == 2'd2) && step;

  always_ff @(posedge clk) begin
    if (rst) begin
      cnt       <= 2'd0;
      completed <= 1'b0;
      saw_two   <= 1'b0;
    end else begin
      saw_two <= completed && tag;
      case (cnt)
        2'd0: if (req) cnt <= 2'd1;
        2'd1: if (step) cnt <= 2'd2;
        2'd2: if (step) begin
          cnt       <= 2'd0;
          completed <= 1'b1;
        end
        default: cnt <= 2'd0;
      endcase
    end
  end
endmodule
