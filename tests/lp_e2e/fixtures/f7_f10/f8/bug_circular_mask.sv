module dom_a_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] w1
);
    logic [7:0] q_a;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_a <= 8'h00;
        end else begin
            q_a <= din;
        end
    end

    assign w1 = q_a;
endmodule

module dom_b_blk (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       pwr_ctrl_a,
    input  logic [7:0] din,
    output logic [7:0] w2,
    output logic       w2_mask
);
    logic [7:0] q_b;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_b <= 8'h00;
        end else begin
            q_b <= din ^ 8'h33;
        end
    end

    assign w2      = q_b;
    assign w2_mask = !pwr_ctrl_a;
endmodule

module bug_circular_mask (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_a,
    input  logic       sleep_b,
    input  logic [7:0] din,
    output logic [7:0] out_xor,
    output logic [7:0] out_cross
);
    logic       pwr_ctrl_a;
    logic       pwr_ack_a;
    logic       pwr_ctrl_b;
    logic       pwr_ack_b;
    logic [7:0] w1;
    logic [7:0] w2;
    logic       w2_mask;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pwr_ctrl_a <= 1'b1;
            pwr_ack_a  <= 1'b1;
            pwr_ctrl_b <= 1'b1;
            pwr_ack_b  <= 1'b1;
        end else begin
            pwr_ctrl_a <= !sleep_a;
            pwr_ack_a  <= pwr_ctrl_a;
            pwr_ctrl_b <= !sleep_b;
            pwr_ack_b  <= pwr_ctrl_b;
        end
    end

    dom_a_blk u_a (
        .clk   (clk),
        .rst_n (rst_n),
        .din   (din),
        .w1    (w1)
    );

    dom_b_blk u_b (
        .clk        (clk),
        .rst_n      (rst_n),
        .pwr_ctrl_a (pwr_ctrl_a),
        .din        (din),
        .w2         (w2),
        .w2_mask    (w2_mask)
    );

    // Circular mutual-masking + cross-domain switchable mask (neither is isolated!)
    assign out_xor   = w1 ^ w2;
    assign out_cross = w2_mask ? 8'h00 : w1;
endmodule
