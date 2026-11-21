// Formal Connectivity Lab (`check_conn`)
module sub_src (
    input  logic [3:0] raw_i,
    output logic [3:0] sig_o
);
    assign sig_o = raw_i;
endmodule

module sub_dst (
    input  logic [3:0] sig_i,
    output logic [3:0] obs_o
);
    assign obs_o = sig_i;
endmodule

module soc_top (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       scan_en,
    input  logic [1:0] mode,
    input  logic [3:0] raw_a,
    input  logic [3:0] raw_b
);
    logic [3:0] a_wire, mux_wire, pipe_q, broken_wire;

    sub_src u_src (.raw_i(raw_a), .sig_o(a_wire));

    assign mux_wire    = ((scan_en && mode == 2'b01) || (!scan_en && mode == 2'b10)) ? a_wire : raw_b;
    assign broken_wire = scan_en ? a_wire : (a_wire ^ 4'h1); // intentional LSB inversion bug

    sub_dst u_mux_dst  (.sig_i(mux_wire),    .obs_o());
    sub_dst u_pipe_dst (.sig_i(pipe_q),      .obs_o());
    sub_dst u_bad_dst  (.sig_i(broken_wire), .obs_o());

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) pipe_q <= 4'd0;
        else        pipe_q <= a_wire;
    end
endmodule
