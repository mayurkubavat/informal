// The properties of tests/fpv/fifo/fifo.jg.tcl written in SystemVerilog
// and bound to the design, so that every flow of the comparison reads the
// same text. One line per command of the script.
module fifo_props (
    input logic       clk,
    input logic       rst_n,
    input logic       push,
    input logic       pop,
    input logic [7:0] din,
    input logic [7:0] dout,
    input logic       full,
    input logic       empty,
    input logic [1:0] status,
    input logic [1:0] wp,
    input logic [1:0] rp,
    input logic [2:0] count,
    input logic [1:0] sub_p
);
    default clocking @(posedge clk); endclocking
    default disable iff (!rst_n);
    a_count_max:   assert property (count <= 3'd4);
    a_full_empty:  assert property (!(full && empty));
    a_ptr_diff:    assert property ((wp - rp) == count[1:0]);
    a_no_overflow: assert property (full && push && !pop |=> full);
    a_status:      assert property (status == 2'd2 |-> full);
    a_data:        assert property (push && empty && !pop ##1 !pop |-> dout == $past(din));
    a_wrong:       assert property (count != 3'd3);
    a_sub_ptr:     assert property (sub_p == wp);
    c_full:        cover property (full);
    c_status3:     cover property (status == 2'd3);
    c_count5:      cover property (count == 3'd5);
endmodule

bind fifo fifo_props fifo_props_i (.clk(clk), .rst_n(rst_n), .push(push), .pop(pop), .din(din), .dout(dout), .full(full), .empty(empty), .status(status), .wp(wp), .rp(rp), .count(count), .sub_p(u_wp.p));
