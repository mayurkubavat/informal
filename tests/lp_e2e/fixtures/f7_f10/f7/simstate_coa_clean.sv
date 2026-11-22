module io_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       io_en,
    input  logic [7:0] io_din,
    output logic [7:0] io_out
);
    logic [7:0] io_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            io_q <= 8'h00;
        end else if (io_en) begin
            io_q <= io_din;
        end
    end

    assign io_out = io_q ^ io_din;
endmodule

module sram_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sram_we,
    input  logic [7:0] sram_din,
    output logic [7:0] sram_out
);
    logic [7:0] sram_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            sram_q <= 8'h00;
        end else if (sram_we) begin
            sram_q <= sram_din;
        end
    end

    assign sram_out = sram_q;
endmodule

module simstate_coa_clean (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       coa_mode,
    input  logic       csoa_mode,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] io_dout,
    output logic [7:0] sram_dout
);
    logic       io_en_gated;
    logic [7:0] io_din_gated;
    logic       sram_we_gated;
    logic [7:0] sram_din_gated;

    assign io_en_gated    = coa_mode ? 1'b0 : we;
    assign io_din_gated   = coa_mode ? 8'h00 : din;
    assign sram_we_gated  = csoa_mode ? 1'b0 : we;
    assign sram_din_gated = csoa_mode ? 8'h00 : din;

    io_blk u_io (
        .clk    (clk),
        .rst_n  (rst_n),
        .io_en  (io_en_gated),
        .io_din (io_din_gated),
        .io_out (io_dout)
    );

    sram_blk u_sram (
        .clk      (clk),
        .rst_n    (rst_n),
        .sram_we  (sram_we_gated),
        .sram_din (sram_din_gated),
        .sram_out (sram_dout)
    );
endmodule
