module f6_bus64_src (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 64'h0;
        else        dout <= din;
    end
endmodule

module f6_bus64_dst (
    input  logic        clk,
    input  logic        rst_n,
    input  logic [63:0] din,
    output logic [63:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 64'h0;
        else        dout <= din ^ 64'hFFFF_FFFF_FFFF_FFFF;
    end
endmodule

module f6_bus64_uniso (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        pwr_en,
    input  logic [63:0] din,
    output logic [63:0] dout
);
    logic [63:0] bus_q;

    f6_bus64_src u_src (.clk(clk), .rst_n(rst_n), .din(din),   .dout(bus_q));
    f6_bus64_dst u_dst (.clk(clk), .rst_n(rst_n), .din(bus_q), .dout(dout));
endmodule
