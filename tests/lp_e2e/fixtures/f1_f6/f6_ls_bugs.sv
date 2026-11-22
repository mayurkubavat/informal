module f6_ls_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 8'h00;
        else        dout <= din;
    end
endmodule

module f6_ls_bugs (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout_missing,
    output logic [7:0] dout_dir
);
    logic [7:0] lo1, lo2;

    // 0.75 V -> 1.10 V without level shifter -> LS_MISSING
    f6_ls_blk u_lo_1 (.clk(clk), .rst_n(rst_n), .din(din), .dout(lo1));
    f6_ls_blk u_hi_1 (.clk(clk), .rst_n(rst_n), .din(lo1), .dout(dout_missing));

    // 0.75 V -> 1.10 V with wrong -rule high_to_low -> LS_DIR_MISMATCH
    f6_ls_blk u_lo_2 (.clk(clk), .rst_n(rst_n), .din(din ^ 8'h33), .dout(lo2));
    f6_ls_blk u_hi_2 (.clk(clk), .rst_n(rst_n), .din(lo2),          .dout(dout_dir));
endmodule
