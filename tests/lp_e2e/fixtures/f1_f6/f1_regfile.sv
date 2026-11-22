module f1_regfile (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       wr_en,
    input  logic       clr,
    input  logic [7:0] din,
    output logic [7:0] q,
    output logic       full
);
    logic [7:0] shadow_q;
    logic [2:0] cnt;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q        <= 8'h00;
            shadow_q <= 8'h00;
            cnt      <= 3'd0;
        end else if (clr) begin
            q        <= 8'h00;
            shadow_q <= 8'h00;
            cnt      <= 3'd0;
        end else if (wr_en) begin
            q        <= din;
            shadow_q <= din;
            if (cnt < 3'd6) begin
                cnt <= cnt + 3'd1;
            end
        end
    end

    assign full = (cnt >= 3'd4);
endmodule
