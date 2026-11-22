module mig_cpu (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] cpu_out
);
    logic [7:0] q_cpu;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_cpu <= 8'h00;
        end else begin
            q_cpu <= din + 8'h01;
        end
    end
    assign cpu_out = q_cpu;
endmodule

module mig_dsp (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dsp_out
);
    logic [7:0] q_dsp;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_dsp <= 8'h00;
        end else begin
            q_dsp <= din ^ 8'h55;
        end
    end
    assign dsp_out = q_dsp;
endmodule

module mig_mem (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] mem_out
);
    logic [7:0] q_mem;
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            q_mem <= 8'h00;
        end else begin
            q_mem <= din;
        end
    end
    assign mem_out = q_mem;
endmodule

module mig_soc (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       VDD_AON,
    input  logic       VDD_HIGH,
    input  logic       VSS,
    input  logic       pwr_cpu,
    output logic       ack_cpu,
    input  logic       pwr_dsp,
    output logic       ack_dsp,
    input  logic       iso_cpu,
    input  logic       iso_dsp,
    input  logic       ret_save,
    input  logic       ret_restore,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] cpu_w;
    logic [7:0] dsp_w;
    logic [7:0] mem_w;

    assign ack_cpu = pwr_cpu;
    assign ack_dsp = pwr_dsp;

    mig_cpu u_cpu (.clk(clk), .rst_n(rst_n), .din(din),   .cpu_out(cpu_w));
    mig_dsp u_dsp (.clk(clk), .rst_n(rst_n), .din(cpu_w), .dsp_out(dsp_w));
    mig_mem u_mem (.clk(clk), .rst_n(rst_n), .din(dsp_w), .mem_out(mem_w));

    assign dout = mem_w;
endmodule
