use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use informal_svlang::struct_graph::split_crossing_group_by_cuts;
use informal_svlang::{compile_cached, BitRange, FeedthroughKind, Polarity, Request};

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn compile_sv(tag: &str, sv: &str) -> (TempDir, std::sync::Arc<informal_svlang::CompiledDesign>) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("informal_sg_ana_{}_{n}_{tag}", std::process::id()));
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
fn feedthrough_classification_pure_buffered_comb_and_seq() {
    let sv = r#"
module blk_a(input logic clk, input logic in_ret, output logic [3:0] out_sig);
  always_ff @(posedge clk) out_sig <= {4{in_ret}};
endmodule
module blk_b(
  input  logic clk,
  input  logic in_wire,
  input  logic in_buf,
  input  logic in_comb,
  input  logic in_seq,
  output logic out_wire,
  output logic out_buf,
  output logic out_comb,
  output logic out_seq
);
  assign out_wire = in_wire;
  buf u_buf(out_buf, in_buf);
  assign out_comb = ~in_comb;
  always_ff @(posedge clk) out_seq <= in_seq;
endmodule
module top(input logic clk, input logic in_ret, output logic y_w, output logic y_b, output logic y_c, output logic y_s);
  logic [3:0] bus;
  blk_a u_a(.clk(clk), .in_ret(in_ret), .out_sig(bus));
  blk_b u_b(
    .clk(clk),
    .in_wire(bus[0]),
    .in_buf(bus[1]),
    .in_comb(bus[2]),
    .in_seq(bus[3]),
    .out_wire(y_w),
    .out_buf(y_b),
    .out_comb(y_c),
    .out_seq(y_s)
  );
endmodule
"#;
    let (_dir, compiled) = compile_sv("ft", sv);
    let g = compiled.struct_graph();
    let ua = g.lookup_cell_by_upf("u_a").unwrap();
    let ub = g.lookup_cell_by_upf("u_b").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(ua, 1u32);
    dom.insert(ub, 2u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let ft_by_dst: HashMap<String, FeedthroughKind> = edges
        .iter()
        .filter(|e| e.src_cell == ua && e.dst_cell == ub)
        .map(|e| (g.net(e.dst_net).name.clone(), e.feedthrough))
        .collect();

    assert_eq!(ft_by_dst.get("in_wire"), Some(&FeedthroughKind::PureWire));
    assert_eq!(ft_by_dst.get("in_buf"), Some(&FeedthroughKind::Buffered));
    assert_eq!(
        ft_by_dst.get("in_comb"),
        Some(&FeedthroughKind::Combinational)
    );
    assert_eq!(ft_by_dst.get("in_seq"), Some(&FeedthroughKind::Sequential));
}

#[test]
fn ctrl_cone_4valued_parity_lattice_and_inline_clamps() {
    let sv = r#"
module pmu(
  input logic clk,
  input logic trig,
  input logic sel,
  input logic mask,
  output logic en_pos,
  output logic en_neg,
  output logic en_mix,
  output logic en_unk
);
  logic root_q;
  always_ff @(posedge clk) root_q <= trig;
  wire w_buf;
  buf u_b(w_buf, root_q);
  assign en_pos = w_buf ? 1'b1 : 1'b0;
  assign en_neg = ~(w_buf == 1'b1);
  assign en_mix = sel ? root_q : ~root_q;
  assign en_unk = root_q ^ mask;
endmodule
module top(input logic clk, input logic trig, input logic sel, input logic mask, input logic d_in, output logic y0, output logic y1);
  logic en_pos, en_neg, en_mix, en_unk;
  pmu u_pmu(.*);
  assign y0 = (~en_pos) & d_in;
  assign y1 = en_neg ? 1'b1 : d_in;
endmodule
"#;
    let (_dir, compiled) = compile_sv("ctrl", sv);
    let g = compiled.struct_graph();
    let root_q = g.lookup_net_by_upf("u_pmu/root_q").expect("root_q");

    let cone_pos = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_pos").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(cone_pos.polarity_from(root_q), Some(Polarity::Positive));

    let cone_neg = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_neg").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(cone_neg.polarity_from(root_q), Some(Polarity::Negative));

    let cone_mix = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_mix").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(cone_mix.polarity_from(root_q), Some(Polarity::Mixed));
    assert_eq!(cone_mix.overall_polarity, Polarity::Mixed);

    let cone_unk = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_unk").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(cone_unk.polarity_from(root_q), Some(Polarity::Unknown));

    let y0 = g.net(g.lookup_net_by_upf("y0").unwrap());
    let ic0 = y0
        .drivers
        .iter()
        .find_map(|d| d.inline_clamp)
        .expect("inline clamp y0");
    assert!(ic0.active_high);
    assert!(!ic0.clamp_one);

    let y1 = g.net(g.lookup_net_by_upf("y1").unwrap());
    let ic1 = y1
        .drivers
        .iter()
        .find_map(|d| d.inline_clamp)
        .expect("inline clamp y1");
    assert!(ic1.active_high);
    assert!(ic1.clamp_one);
}

#[test]
fn bus_group_coalesces_64b_slices_into_one_group() {
    let sv = r#"
module src(output logic [63:0] data, output logic valid);
  assign data = 64'h0;
  assign valid = 1'b1;
endmodule
module dst(input logic [63:0] data, input logic valid, output logic out_b);
  assign out_b = valid ^ data[0] ^ data[63];
endmodule
module top(output logic out_b);
  logic [63:0] bus;
  logic valid;
  src u_src(.data({bus[63:32], bus[31:0]}), .valid(valid));
  dst u_dst(.data(bus), .valid(valid), .out_b(out_b));
endmodule
"#;
    let (_dir, compiled) = compile_sv("busgrp", sv);
    let g = compiled.struct_graph();
    let us = g.lookup_cell_by_upf("u_src").unwrap();
    let ud = g.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(us, 1u32);
    dom.insert(ud, 2u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let groups = g.group_crossings(&edges);
    let data_groups: Vec<_> = groups
        .iter()
        .filter(|gr| gr.src_cell == us && gr.dst_cell == ud && gr.src_display.contains("data"))
        .collect();
    assert_eq!(data_groups.len(), 1);
    assert_eq!(data_groups[0].width_bits, 64);
    assert_eq!(data_groups[0].raw_crossings, 2);
}

#[test]
fn ctrl_cone_per_root_parity_distinct_roots_and_minor_c_width_check() {
    let sv = r#"
module top(
  input  logic en_a,
  input  logic en_b,
  input  logic [7:0] data_bus,
  output logic en_diff,
  output logic en_same_mix,
  output logic en_nand_inv,
  output logic [7:0] bad_1b_mask,
  output logic [7:0] good_rep_mask
);
  assign en_diff = en_a & (~en_b);
  assign en_same_mix = en_a & (~en_a);
  assign en_nand_inv = ~(en_a & (~en_b));
  assign bad_1b_mask = (~en_a) & data_bus;
  assign good_rep_mask = {8{~en_a}} & data_bus;
endmodule
"#;
    let (_dir, compiled) = compile_sv("per_root", sv);
    let g = compiled.struct_graph();
    let en_a = g.lookup_net_by_upf("en_a").unwrap();
    let en_b = g.lookup_net_by_upf("en_b").unwrap();

    let c_diff = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_diff").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(c_diff.polarity_from(en_a), Some(Polarity::Positive));
    assert_eq!(c_diff.polarity_from(en_b), Some(Polarity::Negative));
    assert_ne!(c_diff.overall_polarity, Polarity::Mixed);

    let c_mix = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_same_mix").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(c_mix.polarity_from(en_a), Some(Polarity::Mixed));
    assert_eq!(c_mix.overall_polarity, Polarity::Mixed);

    let c_nand = g.trace_ctrl_cone(
        g.lookup_net_by_upf("en_nand_inv").unwrap(),
        BitRange::full(1),
        32,
    );
    assert_eq!(c_nand.polarity_from(en_a), Some(Polarity::Negative));
    assert_eq!(c_nand.polarity_from(en_b), Some(Polarity::Positive));
    assert_ne!(c_nand.overall_polarity, Polarity::Mixed);

    let bad_net = g.net(g.lookup_net_by_upf("bad_1b_mask").unwrap());
    assert!(bad_net.drivers.iter().all(|d| d.inline_clamp.is_none()));
    let good_net = g.net(g.lookup_net_by_upf("good_rep_mask").unwrap());
    assert!(good_net.drivers.iter().any(|d| d.inline_clamp.is_some()));
}

#[test]
fn partial_bus_slice_and_boundary_internal_inversion_splitting() {
    let sv = r#"
module src(output logic [7:0] out_bus, output logic [127:0] wide_out);
  assign out_bus = 8'h5a;
  assign wide_out = 128'h0;
endmodule
module dst_slice(input logic [3:0] d, output logic y);
  assign y = ^d;
endmodule
module dst_bnd(input logic [127:0] d, output logic [127:0] y);
  assign y = d;
endmodule
module dst_split(input logic [127:0] d, output logic [63:0] out_a, output logic [63:0] out_b);
  assign out_a = d[127:64];
  assign out_b = ~d[63:0];
endmodule
module top(output logic y, output logic [127:0] yb, output logic [63:0] oa, output logic [63:0] ob);
  logic [7:0] out_bus;
  logic [127:0] wide_a, wide_b;
  src u_src(.out_bus(out_bus), .wide_out(wide_a));
  src u_src2(.out_bus(), .wide_out(wide_b));
  dst_slice u_dst_slice(.d(out_bus[3:0]), .y(y));
  dst_bnd   u_dst_bnd(.d({wide_a[127:64], ~wide_a[63:0]}), .y(yb));
  dst_split u_dst_int(.d(wide_b), .out_a(oa), .out_b(ob));
endmodule
"#;
    let (_dir, compiled) = compile_sv("slice_inv", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_src2 = g.lookup_cell_by_upf("u_src2").unwrap();
    let u_slice = g.lookup_cell_by_upf("u_dst_slice").unwrap();
    let u_bnd = g.lookup_cell_by_upf("u_dst_bnd").unwrap();
    let u_int = g.lookup_cell_by_upf("u_dst_int").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_src2, 1u32);
    dom.insert(u_slice, 2u32);
    dom.insert(u_bnd, 3u32);
    dom.insert(u_int, 4u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let groups = g.group_crossings(&edges);

    let slice_g: Vec<_> = groups
        .iter()
        .filter(|gr| gr.src_cell == u_src && gr.dst_cell == u_slice)
        .collect();
    assert_eq!(slice_g.len(), 1);
    assert_eq!(slice_g[0].width_bits, 4);

    let bnd_g: Vec<_> = groups
        .iter()
        .filter(|gr| gr.src_cell == u_src && gr.dst_cell == u_bnd)
        .collect();
    assert_eq!(bnd_g.len(), 2, "{bnd_g:#?}");
    assert!(bnd_g.iter().any(|gr| !gr.inverted && gr.width_bits == 64));
    assert!(bnd_g.iter().any(|gr| gr.inverted && gr.width_bits == 64));

    let int_g: Vec<_> = groups
        .iter()
        .filter(|gr| gr.src_cell == u_src2 && gr.dst_cell == u_int)
        .collect();
    assert_eq!(int_g.len(), 2, "{int_g:#?}");
    assert!(int_g.iter().any(|gr| !gr.inverted
        && gr.feedthrough == FeedthroughKind::PureWire
        && gr.width_bits == 64));
    assert!(int_g.iter().any(|gr| gr.inverted
        && gr.feedthrough == FeedthroughKind::Combinational
        && gr.width_bits == 64));
}

#[test]
fn split_crossing_group_by_cuts_splits_discontiguous_src_ranges() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'haa;
endmodule
module dst(input logic [3:0] d, output logic y);
  assign y = ^d;
endmodule
module top(output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[7:6], bus[1:0]}), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("discontig_cuts", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_dst, 2u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let groups: Vec<_> = g
        .group_crossings(&edges)
        .into_iter()
        .filter(|gr| gr.src_cell == u_src && gr.dst_cell == u_dst)
        .collect();
    assert_eq!(groups.len(), 1, "{groups:#?}");
    let grp = &groups[0];
    assert_eq!(
        grp.src_ranges,
        vec![BitRange { lo: 0, hi: 1 }, BitRange { lo: 6, hi: 7 }]
    );
    assert_eq!(grp.width_bits, 4);

    // Cut `[1:0]` -> `[1:0]` and `[7:6]` sub-groups of width 2 each.
    let sub_lo = split_crossing_group_by_cuts(g, grp, &[BitRange { lo: 0, hi: 1 }]);
    assert_eq!(sub_lo.len(), 2, "{sub_lo:#?}");
    assert_eq!(sub_lo[0].src_ranges, vec![BitRange { lo: 0, hi: 1 }]);
    assert_eq!(sub_lo[0].dst_ranges, vec![BitRange { lo: 0, hi: 1 }]);
    assert_eq!(sub_lo[0].width_bits, 2);
    assert_eq!(sub_lo[1].src_ranges, vec![BitRange { lo: 6, hi: 7 }]);
    assert_eq!(sub_lo[1].dst_ranges, vec![BitRange { lo: 2, hi: 3 }]);
    assert_eq!(sub_lo[1].width_bits, 2);

    // Cut `[7:6]` -> `[1:0]` and `[7:6]` sub-groups of width 2 each.
    let sub_hi = split_crossing_group_by_cuts(g, grp, &[BitRange { lo: 6, hi: 7 }]);
    assert_eq!(sub_hi.len(), 2, "{sub_hi:#?}");
    assert_eq!(sub_hi[0].width_bits, 2);
    assert_eq!(sub_hi[1].width_bits, 2);

    // Uniform superset `[7:0]` keeps the single 4-bit discontiguous group intact.
    let sub_all = split_crossing_group_by_cuts(g, grp, &[BitRange { lo: 0, hi: 7 }]);
    assert_eq!(sub_all.len(), 1);
    assert_eq!(sub_all[0].width_bits, 4);
}

#[test]
fn swapped_concat_preserves_lane_pairing_and_parent_clamp_slice() {
    let sv = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [7:0] bus;
  logic [3:0] bus_hi_iso;
  src u_src(.out_bus(bus));
  assign bus_hi_iso = iso_en ? 4'b1111 : bus[7:4];
  dst u_dst(.d({bus[3:0], bus_hi_iso}), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("swap_clamp", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_dst, 2u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    // `u_src/out_bus[3:0] -> u_dst/d[7:4]` must NOT pick up `bus_hi_iso`'s clamp on `bus[7:4]`.
    let dst_edge = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_dst)
        .expect("u_src -> u_dst edge");
    assert_eq!(dst_edge.src_range, BitRange { lo: 0, hi: 3 });
    assert_eq!(dst_edge.dst_range, BitRange { lo: 4, hi: 7 });
    assert!(
        dst_edge.inline_clamps.is_empty(),
        "disjoint slice bus[3:0] must not inherit bus[7:4] clamp: {:?}",
        dst_edge.inline_clamps
    );

    // Verify swapped direct concatenation `{bus[3:0], bus[7:4]}` keeps separate paired lanes.
    let sv_swap = r#"
module src(output logic [7:0] out_bus);
  assign out_bus = 8'h5a;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(output logic y);
  logic [7:0] bus;
  src u_src(.out_bus(bus));
  dst u_dst(.d({bus[3:0], bus[7:4]}), .y(y));
endmodule
"#;
    let (_dir2, compiled2) = compile_sv("swap_lanes", sv_swap);
    let g2 = compiled2.struct_graph();
    let u_src2 = g2.lookup_cell_by_upf("u_src").unwrap();
    let u_dst2 = g2.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom2 = HashMap::new();
    dom2.insert(g2.tops[0], 0u32);
    dom2.insert(u_src2, 1u32);
    dom2.insert(u_dst2, 2u32);
    let edges2 = g2.extract_crossings(&|c| dom2.get(&c).copied());
    let grp2 = g2
        .group_crossings(&edges2)
        .into_iter()
        .find(|gr| gr.src_cell == u_src2 && gr.dst_cell == u_dst2)
        .expect("swap group");
    assert_eq!(
        grp2.lanes,
        vec![
            (BitRange { lo: 0, hi: 3 }, BitRange { lo: 4, hi: 7 }),
            (BitRange { lo: 4, hi: 7 }, BitRange { lo: 0, hi: 3 }),
        ]
    );
    let split = split_crossing_group_by_cuts(g2, &grp2, &[BitRange { lo: 4, hi: 7 }]);
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].src_ranges, vec![BitRange { lo: 0, hi: 3 }]);
    assert_eq!(split[0].dst_ranges, vec![BitRange { lo: 4, hi: 7 }]);
    assert_eq!(split[1].src_ranges, vec![BitRange { lo: 4, hi: 7 }]);
    assert_eq!(split[1].dst_ranges, vec![BitRange { lo: 0, hi: 3 }]);
}

#[test]
fn replicated_fanout_splits_by_dst_lane_and_retains_pnet_clamp_scope() {
    let sv = r#"
module src(output logic [3:0] out_bus);
  assign out_bus = 4'ha;
endmodule
module dst(input logic [7:0] d, output logic y);
  assign y = ^d;
endmodule
module top(input logic iso_en, output logic y);
  logic [3:0] bus;
  logic [3:0] unused_sib_clamp;
  src u_src(.out_bus(bus));
  assign unused_sib_clamp = iso_en ? 4'b1111 : bus;
  dst u_dst(.d({bus[3:0], bus[3:0]}), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("rep_fanout", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_dst, 2u32);
    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let grp = g
        .group_crossings(&edges)
        .into_iter()
        .find(|gr| gr.src_cell == u_src && gr.dst_cell == u_dst)
        .expect("replicated fanout group");
    assert_eq!(
        grp.lanes,
        vec![
            (BitRange { lo: 0, hi: 3 }, BitRange { lo: 0, hi: 3 }),
            (BitRange { lo: 0, hi: 3 }, BitRange { lo: 4, hi: 7 }),
        ]
    );
    // Even though `cuts` covers all of `src_ranges` (`[0..3]`), overlapping `src` lanes must
    // still split into the two distinct `(src, dst)` lanes.
    let split = split_crossing_group_by_cuts(g, &grp, &[BitRange { lo: 0, hi: 3 }]);
    assert_eq!(split.len(), 2);
    assert_eq!(split[0].dst_ranges, vec![BitRange { lo: 0, hi: 3 }]);
    assert_eq!(split[1].dst_ranges, vec![BitRange { lo: 4, hi: 7 }]);

    // Coalesced intermediate `pnet` clamp on `[0..3]` must not stick to `[4..7]` when split.
    let mut synth = grp.clone();
    let bus_net = g.lookup_net_by_upf("bus").unwrap();
    let sib_clamp = g
        .net(g.lookup_net_by_upf("unused_sib_clamp").unwrap())
        .drivers[0]
        .inline_clamp
        .expect("sib_clamp");
    assert_eq!(sib_clamp.data_net, bus_net);
    synth.src_ranges = vec![BitRange { lo: 0, hi: 7 }];
    synth.dst_ranges = vec![BitRange { lo: 0, hi: 7 }];
    synth.lanes = vec![(BitRange { lo: 0, hi: 7 }, BitRange { lo: 0, hi: 7 })];
    synth.inline_clamps = vec![sib_clamp];
    synth.inline_clamp_scopes = vec![(
        sib_clamp,
        BitRange { lo: 0, hi: 3 },
        BitRange { lo: 0, hi: 3 },
    )];
    let split_clamps = split_crossing_group_by_cuts(g, &synth, &[BitRange { lo: 0, hi: 3 }]);
    assert_eq!(split_clamps.len(), 2);
    assert_eq!(split_clamps[0].inline_clamps.len(), 1);
    assert!(split_clamps[1].inline_clamps.is_empty());
}

#[test]
fn two_hop_assign_and_buf_classifies_buffered_and_tracks_feedthrough_nets() {
    let sv = r#"
module src_blk(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid_blk(input logic in_s, output logic out_s);
  wire ft_buf;
  wire dead_sink;
  assign ft_buf = in_s;
  buf u_b(out_s, ft_buf);
  assign dead_sink = in_s & 1'b0;
endmodule
module dst_blk(input logic a, output logic y);
  assign y = a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, ft;
  src_blk u_src(.clk(clk), .d(d), .q(q));
  mid_blk u_mid(.in_s(q), .out_s(ft));
  dst_blk u_dst(.a(ft), .y(y));
endmodule
"#;
    let (_dir, compiled) = compile_sv("two_hop_buf", sv);
    let g = compiled.struct_graph();
    let u_src = g.lookup_cell_by_upf("u_src").unwrap();
    let u_mid = g.lookup_cell_by_upf("u_mid").unwrap();
    let u_dst = g.lookup_cell_by_upf("u_dst").unwrap();
    let ft_buf_net = g.lookup_net_by_upf("u_mid/ft_buf").unwrap();
    let dead_net = g.lookup_net_by_upf("u_mid/dead_sink").unwrap();

    let mut dom = HashMap::new();
    dom.insert(g.tops[0], 0u32);
    dom.insert(u_src, 1u32);
    dom.insert(u_mid, 2u32);
    dom.insert(u_dst, 3u32);

    let edges = g.extract_crossings(&|c| dom.get(&c).copied());
    let edge = edges
        .iter()
        .find(|e| e.src_cell == u_src && e.dst_cell == u_mid)
        .expect("u_src -> u_mid edge");
    assert_eq!(edge.feedthrough, FeedthroughKind::Buffered);
    assert!(edge.feedthrough_nets.contains(&ft_buf_net));
    assert!(!edge.feedthrough_nets.contains(&dead_net));
}

#[test]
fn parent_alias_multibit_slicing_wrapper_and_per_dc_reaching_routes() {
    // 1. Parent pass-through alias `assign m_alias = m;` reaches `u_dst` without marking `top`.
    let sv_alias = r#"
module src(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid(input logic in_s, output logic out_s);
  assign out_s = in_s;
endmodule
module dst(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y);
  logic q, m, m_alias;
  src u_src(.clk(clk), .d(d), .q(q));
  mid u_mid(.in_s(q), .out_s(m));
  assign m_alias = m;
  dst u_dst(.clk(clk), .a(m_alias), .y(y));
endmodule
"#;
    let (_d1, c1) = compile_sv("parent_alias", sv_alias);
    let g1 = c1.struct_graph();
    let top1 = g1.tops[0];
    let us1 = g1.lookup_cell_by_upf("u_src").unwrap();
    let um1 = g1.lookup_cell_by_upf("u_mid").unwrap();
    let ud1 = g1.lookup_cell_by_upf("u_dst").unwrap();
    let mut dom1 = HashMap::new();
    dom1.insert(top1, 0u32);
    dom1.insert(us1, 1u32);
    dom1.insert(um1, 2u32);
    dom1.insert(ud1, 3u32);
    let edges1 = g1.extract_crossings(&|c| dom1.get(&c).copied());
    let e_mid1 = edges1
        .iter()
        .find(|e| e.src_cell == us1 && e.dst_cell == um1)
        .expect("u_src -> u_mid");
    assert_eq!(e_mid1.feedthrough, FeedthroughKind::PureWire);
    assert_eq!(e_mid1.feedthrough_downstream_cells, vec![ud1]);
    assert!(!e_mid1.feedthrough_downstream_cells.contains(&top1));
    let m_alias_net = g1.lookup_net_by_upf("m_alias").unwrap();
    assert!(
        !edges1
            .iter()
            .any(|e| e.src_cell == top1 && e.dst_cell == ud1 && e.src_net == m_alias_net),
        "pass-through alias in top must not create spurious top -> u_dst edge on m_alias"
    );

    // 2. Multi-bit exit pin `out_s[1:0]` keeps `u_dst1` on bit 0 and `u_dst2` on bit 1 separate.
    let sv_2b = r#"
module src(input logic clk, input logic [1:0] d, output logic [1:0] q);
  always_ff @(posedge clk) q <= d;
endmodule
module mid(input logic [1:0] in_s, output logic [1:0] out_s);
  assign out_s[0] = in_s[0];
  assign out_s[1] = in_s[1];
endmodule
module dst(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic [1:0] d, output logic y1, output logic y2);
  logic [1:0] q, m;
  src u_src(.clk(clk), .d(d), .q(q));
  mid u_mid(.in_s(q), .out_s(m));
  dst u_dst1(.clk(clk), .a(m[0]), .y(y1));
  dst u_dst2(.clk(clk), .a(m[1]), .y(y2));
endmodule
"#;
    let (_d2, c2) = compile_sv("split_2b", sv_2b);
    let g2 = c2.struct_graph();
    let us2 = g2.lookup_cell_by_upf("u_src").unwrap();
    let um2 = g2.lookup_cell_by_upf("u_mid").unwrap();
    let ud2_1 = g2.lookup_cell_by_upf("u_dst1").unwrap();
    let ud2_2 = g2.lookup_cell_by_upf("u_dst2").unwrap();
    let mut dom2 = HashMap::new();
    dom2.insert(g2.tops[0], 0u32);
    dom2.insert(us2, 1u32);
    dom2.insert(um2, 2u32);
    dom2.insert(ud2_1, 3u32);
    dom2.insert(ud2_2, 4u32);
    let edges2 = g2.extract_crossings(&|c| dom2.get(&c).copied());
    let bit0 = edges2
        .iter()
        .find(|e| {
            e.src_cell == us2 && e.dst_cell == um2 && e.dst_range == BitRange { lo: 0, hi: 0 }
        })
        .expect("bit 0 edge");
    let bit1 = edges2
        .iter()
        .find(|e| {
            e.src_cell == us2 && e.dst_cell == um2 && e.dst_range == BitRange { lo: 1, hi: 1 }
        })
        .expect("bit 1 edge");
    assert_eq!(bit0.feedthrough_downstream_cells, vec![ud2_1]);
    assert_eq!(bit1.feedthrough_downstream_cells, vec![ud2_2]);

    // 3. Same-domain wrapper `u_wrap -> u_mid` (buffered) + multi-exit per-dc route tracking.
    let sv_wrap = r#"
module src(input logic clk, input logic d, output logic q);
  always_ff @(posedge clk) q <= d;
endmodule
module inner_mid(input logic in_s, output logic out1, output logic out2);
  wire w1, w2;
  assign w1 = in_s;
  assign w2 = in_s;
  buf u_b1(out1, w1);
  buf u_b2(out2, w2);
endmodule
module wrap(input logic in_w, output logic out_w1, output logic out_w2);
  inner_mid u_mid(.in_s(in_w), .out1(out_w1), .out2(out_w2));
endmodule
module dst(input logic clk, input logic a, output logic y);
  always_ff @(posedge clk) y <= a;
endmodule
module top(input logic clk, input logic d, output logic y1, output logic y2);
  logic q, ow1, ow2;
  src u_src(.clk(clk), .d(d), .q(q));
  wrap u_wrap(.in_w(q), .out_w1(ow1), .out_w2(ow2));
  dst u_dst1(.clk(clk), .a(ow1), .y(y1));
  dst u_dst2(.clk(clk), .a(ow2), .y(y2));
endmodule
"#;
    let (_d3, c3) = compile_sv("wrap_per_dc", sv_wrap);
    let g3 = c3.struct_graph();
    let us3 = g3.lookup_cell_by_upf("u_src").unwrap();
    let uw3 = g3.lookup_cell_by_upf("u_wrap").unwrap();
    let um3 = g3.lookup_cell_by_upf("u_wrap/u_mid").unwrap();
    let ud3_1 = g3.lookup_cell_by_upf("u_dst1").unwrap();
    let ud3_2 = g3.lookup_cell_by_upf("u_dst2").unwrap();
    let w1_net = g3.lookup_net_by_upf("u_wrap/u_mid/w1").unwrap();
    let w2_net = g3.lookup_net_by_upf("u_wrap/u_mid/w2").unwrap();
    let out_w1_pin = g3.lookup_pin_by_upf("u_wrap/out_w1").unwrap();
    let out_w2_pin = g3.lookup_pin_by_upf("u_wrap/out_w2").unwrap();
    let mut dom3 = HashMap::new();
    dom3.insert(g3.tops[0], 0u32);
    dom3.insert(us3, 1u32);
    dom3.insert(uw3, 2u32);
    dom3.insert(um3, 2u32);
    dom3.insert(ud3_1, 3u32);
    dom3.insert(ud3_2, 4u32);
    let edges3 = g3.extract_crossings(&|c| dom3.get(&c).copied());
    let e_wrap = edges3
        .iter()
        .find(|e| e.src_cell == us3 && e.dst_cell == uw3)
        .expect("u_src -> u_wrap");
    assert_eq!(e_wrap.feedthrough, FeedthroughKind::Buffered);
    let r1 = e_wrap
        .feedthrough_routes
        .iter()
        .find(|r| r.downstream_cell == ud3_1)
        .expect("route 1");
    let r2 = e_wrap
        .feedthrough_routes
        .iter()
        .find(|r| r.downstream_cell == ud3_2)
        .expect("route 2");
    assert!(r1.reaching_pins.iter().any(|&(p, _)| p == out_w1_pin));
    assert!(!r1.reaching_pins.iter().any(|&(p, _)| p == out_w2_pin));
    assert!(r1.reaching_nets.iter().any(|&(n, _)| n == w1_net));
    assert!(!r1.reaching_nets.iter().any(|&(n, _)| n == w2_net));
    assert!(r2.reaching_pins.iter().any(|&(p, _)| p == out_w2_pin));
    assert!(!r2.reaching_pins.iter().any(|&(p, _)| p == out_w1_pin));
    assert!(r2.reaching_nets.iter().any(|&(n, _)| n == w2_net));
    assert!(!r2.reaching_nets.iter().any(|&(n, _)| n == w1_net));
}
