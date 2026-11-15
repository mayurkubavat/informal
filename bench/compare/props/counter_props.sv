// The properties of tests/fpv/counter/counter.jg.tcl written in
// SystemVerilog and bound to the design, so that every flow of the
// comparison reads the same text. One line per command of the script.
module counter_props (
    input logic       clk,
    input logic       rst_n,
    input logic       en,
    input logic [3:0] q,
    input logic       done,
    input logic       wrap
);
    default clocking @(posedge clk); endclocking
    default disable iff (!rst_n);
    a_max:      assert property (q <= 4'd9);
    a_step:     assert property (en && !done |=> q == $past(q) + 4'd1);
    a_hold:     assert property (!en |=> $stable(q));
    a_never9:   assert property (q != 4'd9);
    a_no_wrap:  assert property (!wrap);
    c_done:     cover property ($rose(done));
    c_10:       cover property (q == 4'd10);
    m_en_burst: assume property (en |=> en);
endmodule

bind counter counter_props counter_props_i (.clk(clk), .rst_n(rst_n), .en(en), .q(q), .done(done), .wrap(wrap));
