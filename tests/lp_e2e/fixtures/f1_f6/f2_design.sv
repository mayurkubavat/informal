`include "f2_design_defs.svh"

module sub_blk #(
    parameter int W = `F2_W
) (
    input  logic [W-1:0] a,
    output logic [W-1:0] y
);
    assign y = a ^ {{(W-1){1'b0}}, 1'b1};
endmodule

module f2_design (
    input  logic               clk,
    input  logic               rst_n,
    input  logic               en,
    input  logic [`F2_W-1:0]   din,
    output logic [`F2_W-1:0]   dout,
    output logic               busy
);
    logic [`F2_W-1:0] sub_y;
    logic [1:0]       cnt;

    sub_blk #(.W(`F2_W)) u_sub (
        .a(din),
        .y(sub_y)
    );

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            dout <= `F2_ZERO;
            cnt  <= 2'd0;
        end else if (en) begin
            dout <= sub_y;
            cnt  <= cnt + 2'd1;
        end
    end

    assign busy = (cnt != 2'd0);
endmodule
