module f3_xprop_clean (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    input  logic [3:0] din,
    output logic [3:0] clean_a,
    output logic [3:0] clean_b
);
    logic [3:0] r;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            r       <= 4'h0;
            clean_a <= 4'h0;
        end else if (en) begin
            r       <= din;
            clean_a <= din ^ 4'hF;
        end
    end

    assign clean_b = r[0] ? 4'h5 : 4'h5;
endmodule
