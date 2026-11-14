// A 4-entry FIFO built from a pointer submodule, used to cross-check the
// SystemVerilog front ends: hierarchy, parameters, case, part selects,
// packed arrays and a status encoder.
module ptr #(parameter int W = 2) (
    input  logic         clk,
    input  logic         rst_n,
    input  logic         inc,
    output logic [W-1:0] p
);
    always_ff @(posedge clk or negedge rst_n)
        if (!rst_n)   p <= '0;
        else if (inc) p <= p + 1'b1;
endmodule

module fifo #(parameter int DEPTH_LOG2 = 2, parameter int DW = 8) (
    input  logic          clk,
    input  logic          rst_n,
    input  logic          push,
    input  logic          pop,
    input  logic [DW-1:0] din,
    output logic [DW-1:0] dout,
    output logic          full,
    output logic          empty,
    output logic [1:0]    status
);
    localparam int DEPTH = 1 << DEPTH_LOG2;
    logic [DEPTH_LOG2-1:0] wp, rp;
    logic [DEPTH_LOG2:0]   count;
    logic [DEPTH-1:0][DW-1:0] mem;
    logic do_push, do_pop;

    assign do_push = push && !full;
    assign do_pop  = pop && !empty;

    ptr #(.W(DEPTH_LOG2)) u_wp (.clk, .rst_n, .inc(do_push), .p(wp));
    ptr #(.W(DEPTH_LOG2)) u_rp (.clk, .rst_n, .inc(do_pop),  .p(rp));

    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            count <= '0;
        end else begin
            case ({do_push, do_pop})
                2'b10:   count <= count + 1'b1;
                2'b01:   count <= count - 1'b1;
                default: count <= count;
            endcase
            if (do_push) mem[wp] <= din;
        end
    end

    assign full  = count == DEPTH[DEPTH_LOG2:0];
    assign empty = count == '0;
    assign dout  = mem[rp];

    always_comb begin
        status = 2'd0;
        if (full)       status = 2'd2;
        else if (empty) status = 2'd1;
        else if (count[DEPTH_LOG2-1:0] == 2'd3) status = 2'd3;
    end
endmodule
