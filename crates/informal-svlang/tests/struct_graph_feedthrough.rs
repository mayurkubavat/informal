use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_svlang::{compile_cached, BitRange, FeedthroughKind, Request};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_sv(tag: &str, sv: &str) -> (TempDir, std::sync::Arc<informal_svlang::CompiledDesign>) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("informal_sg_ft_{}_{n}_{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{tag}.sv"));
    std::fs::write(&path, sv).expect("write sv");
    let req = Request {
        files: vec![path],
        top: Some("top".to_string()),
        ..Default::default()
    };
    let compiled = compile_cached(None, &req).expect("compile");
    assert!(compiled.check_ok, "{:?}", compiled.diagnostics);
    (TempDir(dir), compiled)
}

#[test]
fn r12_bug2_parent_alias_slice_preserves_cur_pnet_on_parent_loads() {
    let sv = r#"
module src(input logic clk, input logic [3:0] d, output logic [3:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module top(input logic clk, input logic [3:0] d, output logic [1:0] y);
  logic [3:0] m_root;
  logic [1:0] m_hi;
  src u_src(.clk(clk), .d(d), .q(m_root));
  assign m_hi = m_root[3:2];
  always_ff @(posedge clk) y <= m_hi;
endmodule
"#;
    let (_dir, compiled) = compile_sv("alias_pnet", sv);
    let g = compiled.struct_graph();
    let top = g.tops[0];
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let m_hi_net = g.lookup_net_by_upf("m_hi").unwrap();
    let mut dom = HashMap::new();
    dom.insert(top, 0u32);
    dom.insert(u_src, 1u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let edge = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == top)
        .expect("u_src -> top crossing");
    assert_eq!(edge.dst_net, m_hi_net);
    assert_eq!(edge.dst_range, BitRange { lo: 0, hi: 1 });
    assert_eq!(edge.src_range, BitRange { lo: 2, hi: 3 });
}

#[test]
fn r12_bug6_same_dc_multiple_exit_pins_emit_independent_routes() {
    let sv = r#"
module src(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_mix(input logic in_s, output logic out1, output logic out2);
  assign out1 = in_s;
  buf u_b2(out2, in_s);
endmodule
module dst2(input logic clk, input logic a, input logic b, output logic y);
  always_ff @(posedge clk) y <= a ^ b;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, o1, o2;
  src u_src(.clk(clk), .d(d), .q(q));
  mid_mix u_mid(.in_s(q), .out1(o1), .out2(o2));
  dst2 u_dst(.clk(clk), .a(o1), .b(o2), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("same_dc", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_mid = g.lookup_cell_by_upf("u_mid").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let out1_pin = g.lookup_pin_by_upf("u_mid/out1").unwrap();
    let out2_pin = g.lookup_pin_by_upf("u_mid/out2").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_mid, 2u32);
    dom.insert(u_dst, 3u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let e_mid = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_mid)
        .expect("u_src -> u_mid");
    assert_eq!(e_mid.feedthrough_routes.len(), 2);
    let r1 = e_mid
        .feedthrough_routes
        .iter()
        .find(|r| r.reaching_pins.iter().any(|&(p, _)| p == out1_pin))
        .expect("route via out1");
    let r2 = e_mid
        .feedthrough_routes
        .iter()
        .find(|r| r.reaching_pins.iter().any(|&(p, _)| p == out2_pin))
        .expect("route via out2");
    assert_eq!(r1.downstream_cell, u_dst);
    assert_eq!(r2.downstream_cell, u_dst);
    assert_eq!(r1.kind, FeedthroughKind::PureWire);
    assert_eq!(r2.kind, FeedthroughKind::Buffered);
    assert!(!r1.reaching_pins.iter().any(|&(p, _)| p == out2_pin));
    assert!(!r2.reaching_pins.iter().any(|&(p, _)| p == out1_pin));
}

#[test]
fn r12_bug7_same_domain_wrapper_tracks_child_output_pins_per_route() {
    let sv = r#"
module src(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic in_s, output logic out_s);
  buf u_b(out_s, in_s);
endmodule
module sib_dst(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module wrap2(input logic clk, input logic in_w, output logic out_w, output logic y_sib);
  logic s_mid;
  inner_mid u_mid(.in_s(in_w), .out_s(s_mid));
  assign out_w = s_mid;
  sib_dst u_sib(.clk(clk), .a(s_mid), .y(y_sib));
endmodule
module wrap1(input logic clk, input logic in_1, output logic out_1, output logic y_sib);
  wrap2 u_w2(.clk(clk), .in_w(in_1), .out_w(out_1), .y_sib(y_sib));
endmodule
module dst(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y_ext, output logic y_sib);
  logic q, w1;
  src u_src(.clk(clk), .d(d), .q(q));
  wrap1 u_w1(.clk(clk), .in_1(q), .out_1(w1), .y_sib(y_sib));
  dst u_dst(.clk(clk), .a(w1), .y(y_ext));
endmodule
"#;
    let (_dir, compiled) = compile_sv("wrap_pins", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_w1 = g.lookup_cell_by_upf("u_w1").unwrap();
    let u_w2 = g.lookup_cell_by_upf("u_w1/u_w2").unwrap();
    let u_mid = g.lookup_cell_by_upf("u_w1/u_w2/u_mid").unwrap();
    let u_sib = g.lookup_cell_by_upf("u_w1/u_w2/u_sib").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let p_mid = g.lookup_pin_by_upf("u_w1/u_w2/u_mid/out_s").unwrap();
    let p_w2 = g.lookup_pin_by_upf("u_w1/u_w2/out_w").unwrap();
    let p_w1 = g.lookup_pin_by_upf("u_w1/out_1").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_w1, 2u32);
    dom.insert(u_w2, 2u32);
    dom.insert(u_mid, 2u32);
    dom.insert(u_sib, 3u32);
    dom.insert(u_dst, 4u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let e_w1 = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_w1)
        .expect("u_src -> u_w1");
    let r_ext = e_w1
        .feedthrough_routes
        .iter()
        .find(|r| r.downstream_cell == u_dst)
        .expect("route to u_dst");
    assert!(r_ext.reaching_pins.iter().any(|&(p, _)| p == p_w1));
    assert!(r_ext.reaching_pins.iter().any(|&(p, _)| p == p_w2));
    assert!(r_ext.reaching_pins.iter().any(|&(p, _)| p == p_mid));

    let r_sib = e_w1
        .feedthrough_routes
        .iter()
        .find(|r| r.downstream_cell == u_sib)
        .expect("route to u_sib");
    assert!(r_sib.reaching_pins.iter().any(|&(p, _)| p == p_mid));
    assert!(!r_sib.reaching_pins.iter().any(|&(p, _)| p == p_w1));
}

#[test]
fn r12_bug3_partial_bit_fanout_records_exact_route_src_range() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic [1:0] in_s, output logic out0);
  assign out0 = in_s[0];
endmodule
module dst_blk(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic [1:0] d, output logic y1, output logic y2);
  logic [1:0] q;
  logic o0;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out0(o0));
  dst_blk u_dst1(.clk(clk), .a(o0), .y(y1));
  dst_blk u_dst2(.clk(clk), .a(o0), .y(y2));
endmodule
"#;
    let (_dir, compiled) = compile_sv("b3_split", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_mid = g.lookup_cell_by_upf("u_mid").unwrap();
    let u_dst1 = g.lookup_cell_by_upf("u_dst1").unwrap();
    let u_dst2 = g.lookup_cell_by_upf("u_dst2").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_mid, 2u32);
    dom.insert(u_dst1, 3u32);
    dom.insert(u_dst2, 4u32);
    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let mid_edge = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_mid)
        .expect("u_src -> u_mid");
    assert_eq!(mid_edge.feedthrough_routes.len(), 2);
    assert!(mid_edge
        .feedthrough_routes
        .iter()
        .all(|r| r.src_range == BitRange { lo: 0, hi: 0 }));
}

#[test]
fn r14_bug_b_per_bit_buf_projects_downstream_pin_bit_range() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic [1:0] in_s, output logic [1:0] out_s);
  buf u_b0(out_s[0], in_s[0]);
  buf u_b1(out_s[1], in_s[1]);
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] mid_o;
  inner_mid u_mid(.in_s(in_w), .out_s(mid_o));
  inner_dst u_dst(.clk(clk), .in_d(mid_o), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("b_buf_proj", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_wrap = g.lookup_cell_by_upf("u_wrap").unwrap();
    let u_mid = g.lookup_cell_by_upf("u_wrap/u_mid").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_wrap/u_dst").unwrap();
    let in_d = g.lookup_pin_by_upf("u_wrap/u_dst/in_d").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_wrap, 2u32);
    dom.insert(u_mid, 2u32);
    dom.insert(u_dst, 3u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let wrap_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.src_cell == u_src && e.dst_cell == u_wrap)
        .collect();
    assert_eq!(
        wrap_edges.len(),
        2,
        "expected split per-bit edges: {wrap_edges:#?}"
    );
    for bit in 0..=1u32 {
        let e = wrap_edges
            .iter()
            .find(|e| e.src_range == BitRange { lo: bit, hi: bit })
            .expect("edge for bit");
        assert_eq!(e.feedthrough_routes.len(), 1);
        assert!(
            e.feedthrough_routes[0]
                .reaching_pins
                .contains(&(in_d, BitRange { lo: bit, hi: bit })),
            "expected projected (in_d, {bit}..={bit}) in {:?}",
            e.feedthrough_routes[0].reaching_pins
        );
    }
}

#[test]
fn r15_comb_port_expr_does_not_project_single_bit_pin_range_and_case_a_mid_net_scopes() {
    let sv_comb = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic [1:0] in_s, output logic [1:0] out_s);
  buf u_b0(out_s[0], in_s[0]);
  buf u_b1(out_s[1], in_s[1]);
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_blk(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] mid_o;
  inner_mid u_mid(.in_s(in_w), .out_s(mid_o));
  inner_dst u_dst(.clk(clk), .in_d(mid_o + 2'b01), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] q;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  wrap_blk u_wrap(.clk(clk), .in_w(q), .y(y));
endmodule
"#;
    let (_dir1, c1) = compile_sv("comb_dst", sv_comb);
    let g1 = c1.struct_graph();
    let u_src = g1.lookup_cell_by_upf("u_src").unwrap();
    let u_wrap = g1.lookup_cell_by_upf("u_wrap").unwrap();
    let u_mid = g1.lookup_cell_by_upf("u_wrap/u_mid").unwrap();
    let u_dst = g1.lookup_cell_by_upf("u_wrap/u_dst").unwrap();
    let in_d = g1.lookup_pin_by_upf("u_wrap/u_dst/in_d").unwrap();
    let mut dom1 = HashMap::new();
    dom1.insert(g1.tops[0], 0u32);
    dom1.insert(u_src, 1u32);
    dom1.insert(u_wrap, 2u32);
    dom1.insert(u_mid, 2u32);
    dom1.insert(u_dst, 3u32);
    let edges1 = g1.extract_crossings(&|c| dom1.get(&c).copied());
    let wrap_edges: Vec<_> = edges1
        .iter()
        .filter(|e| e.src_cell == u_src && e.dst_cell == u_wrap)
        .collect();
    assert!(!wrap_edges.is_empty());
    for e in wrap_edges {
        for r in &e.feedthrough_routes {
            assert!(
                r.reaching_pins.contains(&(in_d, BitRange { lo: 0, hi: 1 })),
                "Comb port expr must keep full dst_range 0..=1 on in_d, got {:?}",
                r.reaching_pins
            );
        }
    }

    let sv_swap = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module dst_blk(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y);
  logic [1:0] wq, mid_wire;
  src_blk u_src(.clk(clk), .d(d), .q(wq));
  assign mid_wire[0] = wq[1];
  assign mid_wire[1] = wq[0];
  dst_blk u_dst(.clk(clk), .in_d(mid_wire), .y(y));
endmodule
"#;
    let (_dir2, c2) = compile_sv("case_a_swap", sv_swap);
    let g2 = c2.struct_graph();
    let s2 = g2.lookup_cell_by_upf("u_src").unwrap();
    let d2 = g2.lookup_cell_by_upf("u_dst").unwrap();
    let mid_wire = g2.lookup_net_by_upf("mid_wire").unwrap();
    let mut dom2 = HashMap::new();
    dom2.insert(g2.tops[0], 0u32);
    dom2.insert(s2, 1u32);
    dom2.insert(d2, 2u32);
    let edges2 = g2.extract_crossings(&|c| dom2.get(&c).copied());
    let groups = g2.group_crossings(&edges2);
    let grp = groups
        .iter()
        .find(|g| g.src_cell == s2 && g.dst_cell == d2)
        .expect("u_src -> u_dst");
    assert!(grp.mid_net_scopes.contains(&(
        (mid_wire, BitRange { lo: 0, hi: 0 }),
        BitRange { lo: 1, hi: 1 },
        BitRange { lo: 0, hi: 0 }
    )));
    assert!(grp.mid_net_scopes.contains(&(
        (mid_wire, BitRange { lo: 1, hi: 1 }),
        BitRange { lo: 0, hi: 0 },
        BitRange { lo: 1, hi: 1 }
    )));
}

#[test]
fn r16_exit_pin_load_op_folds_comb_mux_and_invert() {
    let sv = r#"
module src_blk(input logic clk, input logic [1:0] d, output logic [1:0] q1, output logic [1:0] q2, output logic [1:0] q3, output logic [1:0] q4);
  always_ff @(posedge clk) begin
    q1 <= d;
    q2 <= d;
    q3 <= d;
    q4 <= d;
  end
endmodule
module inner_mid(input logic [1:0] in_s, output logic [1:0] out_s);
  assign out_s = in_s;
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_direct_comb(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  inner_dst u_dst(.clk(clk), .in_d(in_w + 2'b01), .y(y));
endmodule
module wrap_alias_comb(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] mid_o;
  inner_mid u_mid(.in_s(in_w), .out_s(mid_o));
  inner_dst u_dst(.clk(clk), .in_d(mid_o + 2'b01), .y(y));
endmodule
module wrap_inv_pin(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  inner_dst u_dst(.clk(clk), .in_d(~in_w), .y(y));
endmodule
module wrap_bnd_inv_wire(input logic clk, input logic [1:0] in_w, output logic [1:0] y);
  logic [1:0] tmp;
  assign tmp = in_w;
  inner_dst u_dst(.clk(clk), .in_d(tmp), .y(y));
endmodule
module top(input logic clk, input logic [1:0] d, output logic [1:0] y1, output logic [1:0] y2, output logic [1:0] y3, output logic [1:0] y4);
  logic [1:0] q1, q2, q3, q4;
  src_blk u_src(.clk(clk), .d(d), .q1(q1), .q2(q2), .q3(q3), .q4(q4));
  wrap_direct_comb u_dc(.clk(clk), .in_w(q1), .y(y1));
  wrap_alias_comb u_ac(.clk(clk), .in_w(q2), .y(y2));
  wrap_inv_pin u_ip(.clk(clk), .in_w(q3), .y(y3));
  wrap_bnd_inv_wire u_bw(.clk(clk), .in_w(~q4), .y(y4));
endmodule
"#;
    let (_dir, compiled) = compile_sv("exit_pin_op", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_dc = g.lookup_cell_by_upf("u_dc").unwrap();
    let u_dc_dst = g.lookup_cell_by_upf("u_dc/u_dst").unwrap();
    let u_ac = g.lookup_cell_by_upf("u_ac").unwrap();
    let u_ac_mid = g.lookup_cell_by_upf("u_ac/u_mid").unwrap();
    let u_ac_dst = g.lookup_cell_by_upf("u_ac/u_dst").unwrap();
    let u_ip = g.lookup_cell_by_upf("u_ip").unwrap();
    let u_ip_dst = g.lookup_cell_by_upf("u_ip/u_dst").unwrap();
    let u_bw = g.lookup_cell_by_upf("u_bw").unwrap();
    let u_bw_dst = g.lookup_cell_by_upf("u_bw/u_dst").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_dc, 2u32);
    dom.insert(u_dc_dst, 3u32);
    dom.insert(u_ac, 4u32);
    dom.insert(u_ac_mid, 4u32);
    dom.insert(u_ac_dst, 5u32);
    dom.insert(u_ip, 6u32);
    dom.insert(u_ip_dst, 7u32);
    dom.insert(u_bw, 8u32);
    dom.insert(u_bw_dst, 9u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    for (label, cell) in [("u_dc", u_dc), ("u_ac", u_ac)] {
        let e = edges
            .iter()
            .find(|e| e.src_cell == u_src && e.dst_cell == cell)
            .unwrap_or_else(|| panic!("missing edge for {label}"));
        assert_eq!(
            e.feedthrough,
            FeedthroughKind::Combinational,
            "{label}: {e:#?}"
        );
        assert!(!e.inverted, "{label}");
        assert!(
            e.feedthrough_routes
                .iter()
                .all(|r| r.kind == FeedthroughKind::Combinational),
            "{label} routes: {e:#?}"
        );
    }
    let e_ip = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_ip)
        .expect("u_ip edge");
    assert_eq!(e_ip.feedthrough, FeedthroughKind::Combinational);
    assert!(e_ip.inverted, "expected inverted=true for .in_d(~in_w)");

    let e_bw = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_bw)
        .expect("u_bw edge");
    assert_eq!(e_bw.feedthrough, FeedthroughKind::PureWire);
    assert!(e_bw.inverted, "expected inverted=true for .in_w(~q4)");
}

#[test]
fn r17_mux_bitand_bitor_redand_classify_as_non_pure_wire_and_preserve_ctrl_cone() {
    let sv = r#"
module src_blk(
  input logic clk,
  input logic [1:0] d,
  output logic [1:0] q_mux_swap,
  output logic [1:0] q_mux_const_dir,
  output logic [1:0] q_mux_const_asg,
  output logic [1:0] q_mux_2data,
  output logic [1:0] q_and_wire,
  output logic [1:0] q_and_const,
  output logic [1:0] q_or_const,
  output logic [1:0] q_red_and,
  output logic [1:0] q_log_and
);
  always_ff @(posedge clk) begin
    q_mux_swap      <= d;
    q_mux_const_dir <= d;
    q_mux_const_asg <= d;
    q_mux_2data     <= d;
    q_and_wire      <= d;
    q_and_const     <= d;
    q_or_const      <= d;
    q_red_and       <= d;
    q_log_and       <= d;
  end
endmodule
module inner_dst(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module inner_dst1(input logic clk, input logic in_d, output logic y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_comb(
  input logic clk,
  input logic sel,
  input logic [1:0] mask,
  input logic [1:0] in_b,
  input logic [1:0] in_ms,
  input logic [1:0] in_mcd,
  input logic [1:0] in_mca,
  input logic [1:0] in_m2,
  input logic [1:0] in_aw,
  input logic [1:0] in_ac,
  input logic [1:0] in_oc,
  input logic [1:0] in_ra,
  input logic [1:0] in_la,
  output logic [1:0] y0, y1, y2, y3, y4, y5, y6,
  output logic y7, y8
);
  logic [1:0] tmp_mux, tmp_and, tmp_or;
  logic tmp_red, tmp_log;
  assign tmp_mux = sel ? in_mca : 2'b00;
  assign tmp_and = in_ac & 2'b01;
  assign tmp_or  = in_oc | 2'b10;
  assign tmp_red = &in_ra;
  assign tmp_log = in_la && mask;
  inner_dst  u0(.clk(clk), .in_d(sel ? {in_ms[0], in_ms[1]} : in_ms), .y(y0));
  inner_dst  u1(.clk(clk), .in_d(sel ? in_mcd : 2'b00), .y(y1));
  inner_dst  u2(.clk(clk), .in_d(tmp_mux), .y(y2));
  inner_dst  u3(.clk(clk), .in_d(sel ? in_m2 : in_b), .y(y3));
  inner_dst  u4(.clk(clk), .in_d(in_aw & mask), .y(y4));
  inner_dst  u5(.clk(clk), .in_d(tmp_and), .y(y5));
  inner_dst  u6(.clk(clk), .in_d(tmp_or), .y(y6));
  inner_dst1 u7(.clk(clk), .in_d(tmp_red), .y(y7));
  inner_dst1 u8(.clk(clk), .in_d(tmp_log), .y(y8));
endmodule
module top(
  input logic clk,
  input logic sel,
  input logic [1:0] d, mask, in_b,
  input logic root_q,
  output logic en_neg_and1,
  output logic [1:0] y0, y1, y2, y3, y4, y5, y6,
  output logic y7, y8
);
  logic [1:0] q0, q1, q2, q3, q4, q5, q6, q7, q8;
  assign en_neg_and1 = (~root_q) & 1'b1;
  src_blk u_src(
    .clk(clk), .d(d),
    .q_mux_swap(q0), .q_mux_const_dir(q1), .q_mux_const_asg(q2), .q_mux_2data(q3),
    .q_and_wire(q4), .q_and_const(q5), .q_or_const(q6), .q_red_and(q7), .q_log_and(q8)
  );
  wrap_comb u_wrap(
    .clk(clk), .sel(sel), .mask(mask), .in_b(in_b),
    .in_ms(q0), .in_mcd(q1), .in_mca(q2), .in_m2(q3),
    .in_aw(q4), .in_ac(q5), .in_oc(q6), .in_ra(q7), .in_la(q8),
    .y0(y0), .y1(y1), .y2(y2), .y3(y3), .y4(y4), .y5(y5), .y6(y6), .y7(y7), .y8(y8)
  );
endmodule
"#;
    let (_dir, compiled) = compile_sv("r17_mux_and", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_wrap = g.lookup_cell_by_upf("u_wrap").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_wrap, 2u32);
    for i in 0..=8 {
        let dst = g.lookup_cell_by_upf(&format!("u_wrap/u{i}")).unwrap();
        dom.insert(dst, 3u32);
    }

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let expected = [
        ("u_wrap/in_ms", FeedthroughKind::Combinational),
        ("u_wrap/in_mcd", FeedthroughKind::Combinational),
        ("u_wrap/in_mca", FeedthroughKind::Combinational),
        ("u_wrap/in_m2", FeedthroughKind::Combinational),
        ("u_wrap/in_aw", FeedthroughKind::Buffered),
        ("u_wrap/in_ac", FeedthroughKind::Buffered),
        ("u_wrap/in_oc", FeedthroughKind::Buffered),
        ("u_wrap/in_ra", FeedthroughKind::Combinational),
        ("u_wrap/in_la", FeedthroughKind::Combinational),
    ];
    for (dst_name, exp_kind) in expected {
        let dst_nid = g.lookup_net_by_upf(dst_name).unwrap();
        let matched: Vec<_> = edges
            .iter()
            .filter(|e| e.src_cell == u_src && e.dst_cell == u_wrap && e.dst_net == dst_nid)
            .collect();
        assert!(!matched.is_empty(), "missing crossing for {dst_name}");
        for e in matched {
            assert_eq!(e.feedthrough, exp_kind, "{dst_name}: {e:#?}");
            assert!(
                e.feedthrough_routes.iter().all(|r| r.kind == exp_kind),
                "{dst_name} routes: {e:#?}"
            );
        }
    }

    let root_q = g.lookup_net_by_upf("root_q").unwrap();
    let cone = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_neg_and1").unwrap(),
        BitRange::full(1),
        16,
    );
    assert_eq!(
        cone.polarity_from(root_q),
        Some(informal_svlang::Polarity::Negative)
    );
}

#[test]
fn r18_nested_ops_dyn_idx_eq_xor_dbl_inv_and_assign_mux10() {
    let sv = r#"
module src_blk(
  input logic clk, input logic [1:0] d,
  output logic [1:0] q0, q1, q2, q3, q4, q5, q6, q7, q8, q9, q10,
  output logic q11
);
  always_ff @(posedge clk) begin
    q0<=d; q1<=d; q2<=d; q3<=d; q4<=d; q5<=d;
    q6<=d; q7<=d; q8<=d; q9<=d; q10<=d; q11<=d[0];
  end
endmodule
module dst2(input logic clk, input logic [1:0] in_d, output logic [1:0] y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module dst1(input logic clk, input logic in_d, output logic y);
  always_ff @(posedge clk) y <= in_d;
endmodule
module wrap_r18(
  input logic clk, sel, input logic [1:0] mask,
  input logic [1:0] i0, i1, i2, i3, i4, i5, i6, i7, i8, i9, i10,
  input logic i11,
  output logic [1:0] y4, y5, y6, y7, y8, y9, y10,
  output logic y0, y1, y2, y3, y11,
  output logic ctrl_mux10
);
  logic t0, t1, t2, t3, t11;
  logic [1:0] t4, t5, t6, t7, t8, t9, t10;
  assign t0 = i0[sel];
  assign t1 = (i1 & mask)[0];
  assign t2 = (i2 + 2'b01) == 2'b11;
  assign t3 = i3 == 2'b11;
  assign t4 = (i4 + 2'b01) & mask;
  assign t5 = (i5 + 2'b01) | mask;
  assign t6 = (i6 << 1) & mask;
  assign t7 = (sel ? i7 : 2'b00) & mask;
  assign t8 = (i8 + 2'b01) ^ mask;
  assign t9 = ~(~i9);
  assign t10 = ~(~(i10 + 2'b01));
  assign t11 = i11 ? 1'b1 : 1'b0;
  assign ctrl_mux10 = t11;
  dst1 u0(.clk(clk), .in_d(t0), .y(y0));
  dst1 u1(.clk(clk), .in_d(t1), .y(y1));
  dst1 u2(.clk(clk), .in_d(t2), .y(y2));
  dst1 u3(.clk(clk), .in_d(t3), .y(y3));
  dst2 u4(.clk(clk), .in_d(t4), .y(y4));
  dst2 u5(.clk(clk), .in_d(t5), .y(y5));
  dst2 u6(.clk(clk), .in_d(t6), .y(y6));
  dst2 u7(.clk(clk), .in_d(t7), .y(y7));
  dst2 u8(.clk(clk), .in_d(t8), .y(y8));
  dst2 u9(.clk(clk), .in_d(t9), .y(y9));
  dst2 u10(.clk(clk), .in_d(t10), .y(y10));
  dst1 u11(.clk(clk), .in_d(t11), .y(y11));
endmodule
module top(
  input logic clk, sel, input logic [1:0] d, mask,
  output logic [1:0] y4, y5, y6, y7, y8, y9, y10,
  output logic y0, y1, y2, y3, y11, ctrl_mux10
);
  logic [1:0] q0, q1, q2, q3, q4, q5, q6, q7, q8, q9, q10;
  logic q11;
  src_blk u_src(.*);
  wrap_r18 u_wrap(
    .clk(clk), .sel(sel), .mask(mask),
    .i0(q0), .i1(q1), .i2(q2), .i3(q3), .i4(q4), .i5(q5),
    .i6(q6), .i7(q7), .i8(q8), .i9(q9), .i10(q10), .i11(q11),
    .*
  );
endmodule
"#;
    let (_dir, compiled) = compile_sv("r18_nested", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_wrap = g.lookup_cell_by_upf("u_wrap").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_wrap, 2u32);
    for i in 0..=11 {
        dom.insert(g.lookup_cell_by_upf(&format!("u_wrap/u{i}")).unwrap(), 3u32);
    }
    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    for (i, exp_kind, exp_inv) in [
        (0, FeedthroughKind::Combinational, false),
        (1, FeedthroughKind::Combinational, false),
        (2, FeedthroughKind::Combinational, false),
        (3, FeedthroughKind::Combinational, false),
        (4, FeedthroughKind::Combinational, false),
        (5, FeedthroughKind::Combinational, false),
        (6, FeedthroughKind::Combinational, false),
        (7, FeedthroughKind::Combinational, false),
        (8, FeedthroughKind::Combinational, false),
        (9, FeedthroughKind::PureWire, false),
        (10, FeedthroughKind::Combinational, false),
        (11, FeedthroughKind::Combinational, false),
    ] {
        let dst_nid = g.lookup_net_by_upf(&format!("u_wrap/i{i}")).unwrap();
        let matched: Vec<_> = edges
            .iter()
            .filter(|e| e.src_cell == u_src && e.dst_cell == u_wrap && e.dst_net == dst_nid)
            .collect();
        assert!(!matched.is_empty(), "missing crossing for i{i}");
        for e in matched {
            assert_eq!(e.feedthrough, exp_kind, "i{i}: {e:#?}");
            assert_eq!(e.inverted, exp_inv, "i{i} inverted: {e:#?}");
        }
    }
    let cone = g.trace_ctrl_cone(
        g.lookup_net_by_upf("u_wrap/ctrl_mux10").unwrap(),
        BitRange::full(1),
        16,
    );
    assert_eq!(
        cone.polarity_from(g.lookup_net_by_upf("u_wrap/i11").unwrap()),
        Some(informal_svlang::Polarity::Positive)
    );
}
