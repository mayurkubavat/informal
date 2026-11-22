module leak_src (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] src_data
);
    logic [7:0] data_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            data_q <= 8'h00;
        end else if (we) begin
            data_q <= din;
        end
    end

    assign src_data = data_q;
endmodule

module unmasked_leak (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       leak_sel,
    input  logic       we,
    input  logic [7:0] din,
    input  logic [7:0] bystander_in,
    output logic [7:0] top_out,
    output logic [7:0] alt_out
);
    logic       pwr_ctrl;
    logic       pwr_ack;
    logic [7:0] src_data;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pwr_ctrl <= 1'b1;
            pwr_ack  <= 1'b1;
        end else begin
            pwr_ctrl <= !sleep_req;
            pwr_ack  <= pwr_ctrl;
        end
    end

    leak_src u_src (
        .clk      (clk),
        .rst_n    (rst_n),
        .we       (we && pwr_ctrl),
        .din      (din),
        .src_data (src_data)
    );

    // BUG: unisolated src_data leaks to top_out whenever leak_sel == 1
    assign top_out = leak_sel ? src_data : 8'h00;
    // Untaken/noise branch for why_cex backward-slicing verification
    assign alt_out = bystander_in & 8'h00;
endmodule
