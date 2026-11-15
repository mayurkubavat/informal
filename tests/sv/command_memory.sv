// A property given by command can read a memory, by a constant index and
// by a signal: the memory's words are registers in the model (memories up
// to 4096 words are), and the property is bound after the design is
// lowered.
//% clock clk
//% reset -expression {rst}
//% assert -name a_written {we && !rst |=> mem[$past(wa)] == $past(wd)}
//% assert -name a_other {!we |=> mem[2'd1] == $past(mem[2'd1])}
//% assert -name a_wrong {mem[2'd0] == mem[2'd1]}
//% cover -name c_word {mem[3] == 4'd9}
//= a_written proven
//= a_other proven
//= a_wrong cex
//= c_word covered
module command_memory(input logic clk, input logic rst, input logic we,
                      input logic [1:0] wa, input logic [3:0] wd, input logic [1:0] ra,
                      output logic [3:0] rd);
  logic [3:0] mem [0:3];
  always_ff @(posedge clk) if (we && !rst) mem[wa] <= wd;
  assign rd = mem[ra];
endmodule
