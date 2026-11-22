module f6_psw_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       ctrl_in,
    input  logic [7:0] din,
    output logic       self_ctrl_out,
    output logic [7:0] dout
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            self_ctrl_out <= 1'b1;
            dout          <= 8'h00;
        end else begin
            self_ctrl_out <= ctrl_in;
            dout          <= din;
        end
    end
endmodule

module f6_psw_port_bugs (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       ctrl_in,
    input  logic       iso_en,
    input  logic [7:0] din,
    output logic [7:0] io_unattr_out
);
    logic       self_pwr_ctrl;
    logic [7:0] core_q;

    f6_psw_core u_core (
        .clk(clk),
        .rst_n(rst_n),
        .ctrl_in(ctrl_in),
        .din(din),
        .self_ctrl_out(self_pwr_ctrl),
        .dout(core_q)
    );

    assign io_unattr_out = core_q;
endmodule
