module f4_reg_cell (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] d,
    output logic [7:0] q
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) q <= 8'h00;
        else        q <= d;
    end
endmodule

module f4_pmu (
    input  logic clk,
    input  logic rst_n,
    input  logic sleep_req,
    output logic pwr_en,
    output logic iso_en,
    output logic ret_save,
    output logic ret_restore
);
    always_ff @(posedge clk or negedge rst_n) begin
        if (!rst_n) begin
            pwr_en      <= 1'b1;
            iso_en      <= 1'b0;
            ret_save    <= 1'b0;
            ret_restore <= 1'b0;
        end else begin
            pwr_en      <= !sleep_req;
            iso_en      <= sleep_req;
            ret_save    <= sleep_req;
            ret_restore <= !sleep_req;
        end
    end
endmodule

module f4_core (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic [7:0] alt_out
);
    logic [7:0] q_a;

    f4_reg_cell u_reg_a (.clk(clk), .rst_n(rst_n), .d(din),       .q(q_a));
    f4_reg_cell u_reg_b (.clk(clk), .rst_n(rst_n), .d(q_a ^ 8'h5), .q(dout));
    f4_reg_cell u_pipe  (.clk(clk), .rst_n(rst_n), .d(din ^ 8'hA), .q(alt_out));
endmodule

module f4_leaf (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    f4_reg_cell u_leaf_reg (.clk(clk), .rst_n(rst_n), .d(din), .q(dout));
endmodule

module f4_subip (
    input  logic       clk,
    input  logic       rst_n,
    input  logic [7:0] din,
    output logic [7:0] dout
);
    logic [7:0] sub_q;

    f4_reg_cell u_sub_reg (.clk(clk), .rst_n(rst_n), .d(din), .q(sub_q));
    f4_leaf     u_leaf    (.clk(clk), .rst_n(rst_n), .din(sub_q), .dout(dout));
endmodule

module soc_basic (
    input  logic       clk,
    input  logic       rst_n,
    input  logic       sleep_req,
    input  logic [7:0] din,
    output logic [7:0] dout,
    output logic [7:0] sub_dout,
    output logic       pwr_ack
);
    logic       pwr_en, iso_en, ret_save, ret_restore;
    logic [7:0] core_dout, core_alt;

    f4_pmu u_pmu (
        .clk(clk),
        .rst_n(rst_n),
        .sleep_req(sleep_req),
        .pwr_en(pwr_en),
        .iso_en(iso_en),
        .ret_save(ret_save),
        .ret_restore(ret_restore)
    );

    f4_core u_core (
        .clk(clk),
        .rst_n(rst_n),
        .din(din),
        .dout(core_dout),
        .alt_out(core_alt)
    );

    f4_subip u_sub (
        .clk(clk),
        .rst_n(rst_n),
        .din(core_dout),
        .dout(sub_dout)
    );

    assign dout    = core_alt;
    assign pwr_ack = pwr_en;
endmodule
