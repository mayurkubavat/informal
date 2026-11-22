module masked_src (
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

module masked_crossing (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       we,
    input  logic [7:0] din,
    output logic [7:0] top_out
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

    masked_src u_src (
        .clk      (clk),
        .rst_n    (rst_n),
        .we       (we && pwr_ctrl),
        .din      (din),
        .src_data (src_data)
    );

    // Functionally masked in PD_TOP whenever PD_SRC is OFF (pwr_ctrl == 0)
    assign top_out = pwr_ctrl ? src_data : 8'h00;
endmodule
