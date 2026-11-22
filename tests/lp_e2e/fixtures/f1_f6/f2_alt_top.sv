module f2_alt_top (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [3:0] d,
    output logic [3:0] q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) q <= 4'h0;
        else        q <= (d < 4'd8) ? d : 4'd7;
    end
endmodule
