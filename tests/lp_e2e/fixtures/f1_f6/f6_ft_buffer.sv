module f6_ft_src (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 8'h00;
        else        dout <= din;
    end
endmodule

module f6_ft_mid_buf (
    input  logic [7:0] ft_in,
    output logic [7:0] ft_out
);
    // Combinational buffer/double-invert inside switchable traversed domain
    assign ft_out = ~(~ft_in);
endmodule

module f6_ft_dst (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) dout <= 8'h00;
        else        dout <= din;
    end
endmodule

module f6_ft_buffer (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       pwr_en,
    input  logic       iso_en,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] w1, w2;

    f6_ft_src     u_src (.clk(clk), .rst_n(rst_n), .din(din), .dout(w1));
    f6_ft_mid_buf u_mid (.ft_in(w1), .ft_out(w2));
    f6_ft_dst     u_dst (.clk(clk), .rst_n(rst_n), .din(w2),  .dout(dout));
endmodule
