module src_island (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] src_bus
);
    logic [7:0] q_bus;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_bus <= 8'h00;
        end else begin
            q_bus <= din + 8'h03;
        end
    end
    assign src_bus = q_bus;
endmodule

module autowaiver_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VSS,
    input  logic       sleep_en,
    output logic       sleep_ack,
    input  logic       active_use,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] src_bus_w;

    assign sleep_ack = !sleep_en;

    src_island u_src (
        .clk     (clk),
        .rst_n   (rst_n),
        .din     (din),
        .src_bus (src_bus_w)
    );

    assign dout = active_use ? src_bus_w : 8'h00;
endmodule
