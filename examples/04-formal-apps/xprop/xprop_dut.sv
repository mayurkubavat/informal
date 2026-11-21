// Formal X-Propagation Lab (`check_xprop`)
module xprop_dut (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       load_en,
    input  logic [7:0] din,
    output logic [7:0] clean_out,
    output logic [7:0] reconv_out,
    output logic [7:0] leaky_out
);
    logic [7:0] uninit_pipe_q;

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) clean_out <= 8'd0;
        else        clean_out <= din;
    end

    // Reset-less register: holds power-on X until `load_en` fires
    always_ff @(posedge clk) begin
        if (load_en) uninit_pipe_q <= din;
    end

    // Reconvergent mux: both arms equal 8'hA5 -> exact 2-copy miter proves X-clean!
    assign reconv_out = uninit_pipe_q[0] ? 8'hA5 : 8'hA5;
    assign leaky_out  = uninit_pipe_q;
endmodule
