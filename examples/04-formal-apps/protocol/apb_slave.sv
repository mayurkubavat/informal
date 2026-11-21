// Formal Bus Protocol VIP Lab (`check_protocol` — APB4 Slave)
// Compliant APB4 slave with 1 wait state and gated pslverr.
module apb_slave (
    input  logic        clk,
    input  logic        rst_n,
    input  logic        psel,
    input  logic        penable,
    input  logic        pwrite,
    input  logic [7:0]  paddr,
    input  logic [31:0] pwdata,
    output logic        pready,
    output logic        pslverr,
    output logic [31:0] prdata
);
    logic        wait_q;
    logic [31:0] reg0_q;

    always_ff @(posedge clk) begin
        if (!rst_n) begin
            wait_q <= 1'b0;
            reg0_q <= 32'd0;
        end else begin
            if (psel && !penable)
                wait_q <= 1'b1;
            else if (psel && penable && wait_q)
                wait_q <= 1'b0;

            if (psel && penable && pready && pwrite && (paddr == 8'h00))
                reg0_q <= pwdata;
        end
    end

    assign pready  = psel && penable && !wait_q;
    assign pslverr = pready && (paddr != 8'h00);
    assign prdata  = (paddr == 8'h00) ? reg0_q : 32'd0;
endmodule
