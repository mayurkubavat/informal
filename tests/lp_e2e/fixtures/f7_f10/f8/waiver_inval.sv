module waiver_src (
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

module waiver_inval (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic       bypass_uniso,
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

    waiver_src u_src (
        .clk      (clk),
        .rst_n    (rst_n),
        .we       (we && pwr_ctrl),
        .din      (din),
        .src_data (src_data)
    );

    // Mask holds ONLY under assumption {!bypass_uniso}!
    assign top_out = (pwr_ctrl || bypass_uniso) ? src_data : 8'h00;
endmodule
