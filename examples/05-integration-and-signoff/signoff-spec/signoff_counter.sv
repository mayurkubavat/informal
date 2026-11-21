// Lab 05 — Signoff-Ready Counter (zero intentional failures)
module signoff_counter (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       en,
    output logic [3:0] q,
    output logic       done
);
    assign done = (q == 4'd9);

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n)           q <= 4'd0;
        else if (en && !done) q <= q + 4'd1;
    end
endmodule
