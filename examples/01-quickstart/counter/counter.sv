// Lab 01 — Saturating Modulo-9 Counter
// Counts 0..9 when `en` is asserted and saturates at 9 without wrapping.
module counter (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [3:0] q,
    output logic       done
);
    logic [3:0] nxt;
    logic       wrap;

    assign nxt  = q + 4'd1;
    assign done = (q == 4'd9);
    assign wrap = 1'b0;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)           q <= 4'd0;
        else if (en && !done) q <= nxt;
    end
endmodule
