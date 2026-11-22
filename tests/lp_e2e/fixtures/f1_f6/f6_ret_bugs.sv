module f6_ret_src (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       gen_rst,
    output logic       sw_rst_n,
    output logic [7:0] data_o
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            sw_rst_n <= 1'b0;
            data_o   <= 8'h00;
        end else begin
            sw_rst_n <= !gen_rst;
            data_o   <= 8'h5A;
        end
    end
endmodule

module f6_ret_island (
    input  logic       clk,
    input  logic       async_rst_n,
    input  logic [7:0] din,
    output logic [7:0] q
);
    always_ff @(posedge clk or negedge async_rst_n) begin
        if (!async_rst_n) q <= 8'h00;
        else              q <= din;
    end
endmodule

module f6_ret_bugs (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       pwr_en,
    input  logic       backup_en,
    input  logic       iso_en,
    input  logic       ret_save,
    input  logic       ret_restore,
    input  logic       gen_rst,
    output logic [7:0] ret_q
);
    logic       sw_rst_n;
    logic [7:0] src_data;

    f6_ret_src u_src (
        .clk(clk),
        .rst_n(rst_n),
        .gen_rst(gen_rst),
        .sw_rst_n(sw_rst_n),
        .data_o(src_data)
    );

    // Retention island receives unisolated async_rst_n from switchable u_src
    f6_ret_island u_ret (
        .clk(clk),
        .async_rst_n(sw_rst_n),
        .din(src_data),
        .q(ret_q)
    );
endmodule
