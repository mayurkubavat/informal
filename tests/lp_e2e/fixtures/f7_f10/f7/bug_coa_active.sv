module coa_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] coa_out
);
    logic [7:0] data_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            data_q <= 8'h00;
        end else begin
            data_q <= din;
        end
    end

    assign coa_out = data_q ^ din ^ 8'h55;
endmodule

module bug_coa_active (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       coa_mode,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] coa_out;

    // BUG: din is NOT gated while PD_COA enters CORRUPT_ON_ACTIVITY!
    coa_blk u_coa (
        .clk     (clk),
        .rst_n   (rst_n),
        .din     (din),
        .coa_out (coa_out)
    );

    assign dout = coa_out;
endmodule
