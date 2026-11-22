module axi_pmu(
  input  logic clk,
  input  logic rst_n,
  input  logic sleep_req,
  output logic pmu_iso_en,
  output logic pmu_pwr_en,
  output logic pmu_pwr_ack
);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      pmu_iso_en  <= 1'b0;
      pmu_pwr_en  <= 1'b1;
      pmu_pwr_ack <= 1'b1;
    end else begin
      pmu_iso_en  <= sleep_req;
      pmu_pwr_en  <= !(sleep_req && pmu_iso_en);
      pmu_pwr_ack <= pmu_pwr_en;
    end
  end
endmodule

module axi_src_producer #(parameter int DW = 64) (
  input  logic          clk,
  input  logic          rst_n,
  input  logic          m_ready,
  input  logic [DW-1:0] host_data,
  input  logic          host_valid,
  output logic          m_valid,
  output logic [DW-1:0] m_data
);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      m_valid <= 1'b0;
      m_data  <= '0;
    end else if (!m_valid || m_ready) begin
      m_valid <= host_valid;
      m_data  <= host_data;
    end
  end
endmodule

module axi_dst_consumer #(parameter int DW = 64) (
  input  logic          clk,
  input  logic          rst_n,
  input  logic          m_valid,
  input  logic          m_ready,
  input  logic [DW-1:0] m_data,
  output logic [DW-1:0] acc_q,
  output logic          reconv_masked_q
);
  always_ff @(posedge clk or negedge rst_n) begin
    if (!rst_n) begin
      acc_q           <= '0;
      reconv_masked_q <= 1'b0;
    end else begin
      if (m_valid && m_ready) begin
        acc_q <= acc_q ^ m_data;
      end
      // Reconvergent masking helper: (x & ~x) is identically 0 under 2-copy exactness.
      reconv_masked_q <= (m_data[0] & ~m_data[0]) ^ acc_q[0];
    end
  end
endmodule

module axi_valid_data #(
  parameter int DW = 64,
  parameter bit BAD_VALID_CLAMP = 1'b0
) (
  input  logic          clk,
  input  logic          rst_n,
  input  logic          sleep_req,
  input  logic          m_ready,
  input  logic          host_valid,
  input  logic [DW-1:0] host_data,
  output logic [DW-1:0] acc_q,
  output logic          reconv_masked_q
);
  logic          pmu_iso_en, pmu_pwr_en, pmu_pwr_ack;
  logic          m_valid, m_valid_iso;
  logic [DW-1:0] m_data;

  axi_pmu u_pmu(
    .clk(clk), .rst_n(rst_n), .sleep_req(sleep_req),
    .pmu_iso_en(pmu_iso_en), .pmu_pwr_en(pmu_pwr_en), .pmu_pwr_ack(pmu_pwr_ack)
  );

  axi_src_producer #(.DW(DW)) u_src(
    .clk(clk), .rst_n(rst_n), .m_ready(m_ready),
    .host_data(host_data), .host_valid(host_valid),
    .m_valid(m_valid), .m_data(m_data)
  );

  // Valid is clamped to 0 when pmu_iso_en == 1 (unless BAD_VALID_CLAMP == 1);
  // m_data[63:0] is intentionally left unisolated!
  assign m_valid_iso = BAD_VALID_CLAMP ? (pmu_iso_en | m_valid) : (!pmu_iso_en & m_valid);

  axi_dst_consumer #(.DW(DW)) u_dst(
    .clk(clk), .rst_n(rst_n),
    .m_valid(m_valid_iso), .m_ready(m_ready), .m_data(m_data),
    .acc_q(acc_q), .reconv_masked_q(reconv_masked_q)
  );
endmodule
