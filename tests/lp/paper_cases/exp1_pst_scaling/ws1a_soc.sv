// Parameterized multi-stage SoC pipeline for WS1A CompiledDesign cache benchmarking.

module ws1a_stage #(
    parameter int W = 8,
    parameter int IDX = 0
) (
    input  logic         clk,
    input  logic         rst_n,
    input  logic         in_valid,
    input  logic [W-1:0] in_data,
    input  logic         out_ready,
    output logic         in_ready,
    output logic         out_valid,
    output logic [W-1:0] out_data,
    output logic [7:0]   stage_cnt
);
  logic         valid_q;
  logic [W-1:0] data_q;
  logic [7:0]   cnt_q;

  assign in_ready  = !valid_q || out_ready;
  assign out_valid = valid_q;
  assign out_data  = data_q;
  assign stage_cnt = cnt_q;

  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      valid_q <= 1'b0;
      data_q  <= '0;
      cnt_q   <= 8'd0;
    end else begin
      if (in_ready) begin
        valid_q <= in_valid;
        if (in_valid) begin
          data_q <= in_data ^ W'(IDX + 1);
          cnt_q  <= cnt_q + 8'd1;
        end
      end
    end
  end

  // Safety & cover properties per stage
  a_hold_valid: assert property (@(posedge clk) disable iff (!rst_n)
    (valid_q && !out_ready) |=> valid_q);
  a_hold_data: assert property (@(posedge clk) disable iff (!rst_n)
    (valid_q && !out_ready) |=> (data_q == $past(data_q)));
  a_ready_when_empty: assert property (@(posedge clk) disable iff (!rst_n)
    !valid_q |-> in_ready);
  c_xfer: cover property (@(posedge clk) disable iff (!rst_n)
    valid_q && out_ready);
endmodule

module ws1a_soc #(
    parameter int STAGES = 4,
    parameter int W = 8
) (
    input  logic         clk,
    input  logic         rst_n,
    input  logic         req_valid,
    input  logic [W-1:0] req_data,
    input  logic         rsp_ready,
    output logic         req_ready,
    output logic         rsp_valid,
    output logic [W-1:0] rsp_data,
    output logic [7:0]   last_stage_cnt
);
  logic         v_link   [0:STAGES];
  logic [W-1:0] d_link   [0:STAGES];
  logic         r_link   [0:STAGES];
  logic [7:0]   cnt_link [0:STAGES-1];

  assign v_link[0] = req_valid;
  assign d_link[0] = req_data;
  assign req_ready = r_link[0];

  assign rsp_valid = v_link[STAGES];
  assign rsp_data  = d_link[STAGES];
  assign r_link[STAGES] = rsp_ready;
  assign last_stage_cnt = cnt_link[STAGES-1];

  genvar g;
  generate
    for (g = 0; g < STAGES; g++) begin : gen_stage
      ws1a_stage #(.W(W), .IDX(g)) u_stage (
          .clk       (clk),
          .rst_n     (rst_n),
          .in_valid  (v_link[g]),
          .in_data   (d_link[g]),
          .out_ready (r_link[g + 1]),
          .in_ready  (r_link[g]),
          .out_valid (v_link[g + 1]),
          .out_data  (d_link[g + 1]),
          .stage_cnt (cnt_link[g])
      );
    end
  endgenerate

  // Top-level handshake invariants & covers
  a_rsp_hold_valid: assert property (@(posedge clk) disable iff (!rst_n)
    (rsp_valid && !rsp_ready) |=> rsp_valid);
  a_rsp_hold_data: assert property (@(posedge clk) disable iff (!rst_n)
    (rsp_valid && !rsp_ready) |=> (rsp_data == $past(rsp_data)));
  c_rsp_xfer: cover property (@(posedge clk) disable iff (!rst_n)
    rsp_valid && rsp_ready);
endmodule
