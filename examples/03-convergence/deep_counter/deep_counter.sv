// Lab 03 — Deep Watchdog Timer & Heavy Multiplier Datapath
// Demonstrates complexity profiling, blackbox_assistant, sound counter jump
// abstraction (`abstract -counter`), `stopat`, `assume -from_assert`, and proof cache.
module heavy_mul (
    input  logic        clk,
    input  logic [15:0] x,
    input  logic [15:0] y,
    output logic [31:0] prod_q
);
    always_ff @(posedge clk) begin
        prod_q <= x * y;
    end
endmodule

module deep_counter (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        kick,
    input  logic [15:0] mx,
    input  logic [15:0] my,
    output logic        wdog_expired,
    output logic [31:0] mul_prod
);
    logic [23:0] wdog_cnt;

    always_ff @(posedge clk) begin
        if (!rst_n)
            wdog_cnt <= 24'd0;
        else if (kick)
            wdog_cnt <= 24'd0;
        else if (wdog_cnt < 24'd1000000)
            wdog_cnt <= wdog_cnt + 24'd1;
    end

    assign wdog_expired = (wdog_cnt == 24'd1000000);

    heavy_mul u_mul (
        .clk    (clk),
        .x      (mx),
        .y      (my),
        .prod_q (mul_prod)
    );
endmodule
