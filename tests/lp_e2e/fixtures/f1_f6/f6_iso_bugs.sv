module f6_iso_blk (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [15:0] din,
    output logic [15:0] dout,
    output logic        ctrl_out
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            dout     <= 16'h0000;
            ctrl_out <= 1'b0;
        end else begin
            dout     <= din;
            ctrl_out <= din[0];
        end
    end
endmodule

module f6_iso_bugs (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        pwr_a_en,
    input  logic        pwr_b_en,
    input  logic        aon_iso_en,
    input  logic [15:0] din,
    output logic [15:0] out_missing,
    output logic [15:0] out_unpowered_ctrl,
    output logic [15:0] out_bad_loc
);
    logic [15:0] a_bus, b_bus, c_bus;
    logic        sw_ctrl, unused_b_ctrl, unused_c_ctrl;

    // PD_A: unisolated crossing to PD_TOP (ISO_MISSING), also drives sw_ctrl!
    f6_iso_blk u_dom_a (
        .clk(clk), .rst_n(rst_n), .din(din), .dout(a_bus), .ctrl_out(sw_ctrl)
    );
    // PD_B: isolated with control signal sw_ctrl from switchable PD_A (ISO_CTRL_UNPOWERED)
    f6_iso_blk u_dom_b (
        .clk(clk), .rst_n(rst_n), .din(din ^ 16'h00FF), .dout(b_bus), .ctrl_out(unused_b_ctrl)
    );
    // PD_C: isolated with -location self using switchable primary supply (ISO_LOCATION_INVALID)
    f6_iso_blk u_dom_c (
        .clk(clk), .rst_n(rst_n), .din(din ^ 16'hFF00), .dout(c_bus), .ctrl_out(unused_c_ctrl)
    );

    assign out_missing        = a_bus;
    assign out_unpowered_ctrl = b_bus;
    assign out_bad_loc        = c_bus;
endmodule
