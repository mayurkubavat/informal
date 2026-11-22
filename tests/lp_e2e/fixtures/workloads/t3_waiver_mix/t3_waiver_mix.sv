module sw_island (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] sw_masked,
    output logic [7:0] sw_manual,
    output logic [7:0] sw_leak
);
    logic [7:0] r_masked;
    logic [7:0] r_manual;
    logic [7:0] r_leak;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            r_masked <= 8'h00;
            r_manual <= 8'h00;
            r_leak   <= 8'h00;
        end else begin
            r_masked <= din + 8'h01;
            r_manual <= din + 8'h02;
            r_leak   <= din ^ 8'hA5;
        end
    end

    assign sw_masked = r_masked;
    assign sw_manual = r_manual;
    assign sw_leak   = r_leak;
endmodule

module t3_waiver_mix (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       pwr_on,
    output logic       pwr_ack,
    input  logic       iso_en,
    input  logic [7:0] din,
    output logic [7:0] out_masked,
    output logic [7:0] out_manual,
    output logic [7:0] out_leak
);
    logic [7:0] sw_masked_w;
    logic [7:0] sw_manual_w;
    logic [7:0] sw_leak_w;

    assign pwr_ack = pwr_on;

    sw_island u_sw (
        .clk       (clk),
        .rst_n     (rst_n),
        .din       (din),
        .sw_masked (sw_masked_w),
        .sw_manual (sw_manual_w),
        .sw_leak   (sw_leak_w)
    );

    assign out_masked = pwr_on ? sw_masked_w : 8'h00;
    assign out_manual = sw_manual_w;
    assign out_leak   = sw_leak_w;

    a_masked_clamp_ok: assert property (@(posedge clk) disable iff (!rst_n) (!pwr_on |-> (out_masked == 8'h00)));
    c_pwr_off_seen:    cover  property (@(posedge clk) disable iff (!rst_n) (!pwr_on));
endmodule
