mod common;

use std::time::Instant;

use common::{check_illegal, check_illegal_with_widths, check_logic, check_logic_with_widths};
use informal_upf::expr_bv::qualify_logic_expr_in_scope;
use informal_upf::expr_bv::width_ieee1800::{typecheck_ast, TypedExprKind};
use informal_upf::{
    parse_logic_expr, parse_logic_expr_in_scope, LogicConstraintAst, MapWidthProvider,
    NullWidthProvider,
};

#[test]
fn regression_r2_wide_gt64_sv_literals_fill_bits_and_wide_shifts() {
    assert!(parse_logic_expr("bus == 80'h1_0000_0000_0000_0000_0001").is_ok());
    let bin_80 = format!("bus == 80'b1{}1", "0".repeat(78));
    assert!(parse_logic_expr(&bin_80).is_ok());
    assert!(parse_logic_expr("bus == 80'd604462909807314587353089").is_ok());
    let ast_fill1 = parse_logic_expr("80'1").unwrap();
    assert_eq!(
        ast_fill1,
        LogicConstraintAst::BitConst {
            width: 80,
            bits: vec![true; 80],
        }
    );

    check_logic(
        "bus == 80'h8000_0000_0000_0000_0001 && bus[79] == 1'b1 && bus[0] == 1'b1",
        true,
    );
    check_logic(
        "bus == 80'h8000_0000_0000_0000_0001 && bus[79] == 1'b0",
        false,
    );
    check_logic(
        "bus == 80'h1_0000_0000_0000_0000_0001 && bus[79] == 1'b0 && bus[0] == 1'b1",
        true,
    );
    check_logic(
        "bus == 80'h1_0000_0000_0000_0000_0001 && bus[79] == 1'b1",
        false,
    );

    check_logic(
        &format!("{bin_80} && bus[79] == 1'b1 && bus[0] == 1'b1"),
        true,
    );
    check_logic(&format!("{bin_80} && bus[79] == 1'b0"), false);
    check_logic(
        "bus == 80'd604462909807314587353089 && bus[79] == 1'b1 && bus[0] == 1'b1",
        true,
    );
    check_logic(
        "bus == 80'd604462909807314587353089 && bus[79] == 1'b0",
        false,
    );
    check_logic(
        "bus == 80'o200000000000000000000000001 && bus[79] == 1'b1 && bus[0] == 1'b1",
        true,
    );
    check_logic(
        "bus == 80'o200000000000000000000000001 && bus[79] == 1'b0",
        false,
    );

    check_logic("bus == 80'1 && bus[79] == 1'b1", true);
    check_logic("bus == 80'1 && bus[79] == 1'b0", false);
    check_logic("bus == 80'0 && bus[79] == 1'b0", true);
    check_logic("bus == 80'0 && bus[79] == 1'b1", false);

    check_logic("(80'h1 << 79) == bus && bus[79] == 1'b1", true);
    check_logic("(80'h1 << 79) == bus && bus[79] == 1'b0", false);

    // Wide shift counts >= 2^32 and unsized fill bits:
    check_logic("(bus >> 80'1) == 1'b0", true);
    check_logic("(bus << 33'h1_0000_0000) == 1'b0", true);
    check_logic("(bus >> 33'h1_0000_0000) == 1'b0", true);
    check_logic("(bus[7:0] << 33'h1_0000_0000) == 8'hFF", false);
    check_logic("(bus[7:0] >> 80'1) == 8'h00", true);
    check_logic("bus == 'd18446744073709551617", true);

    check_logic("bus[79:0] == '1 && bus[79] == 1'b1", true);
    check_logic("bus[79:0] == '1 && bus[79] == 1'b0", false);
    check_logic("bus == 80'h1 && bus == '1", false);
    check_logic("bus == 80'hFFFF_FFFF_FFFF_FFFF_FFFF && bus == '1", true);
    check_logic("bus[79:0] == ~'0 && bus[79] == 1'b1", true);
    check_logic("bus[79:0] == ~'0 && bus[79] == 1'b0", false);
    check_logic("bus[79:0] == '0 && bus[79] == 1'b0", true);
    check_logic("bus[79:0] == '0 && bus[79] == 1'b1", false);
    check_logic(
        "bus[79:0] == ('1 & mask[79:0]) && mask[79] == 1'b1 && bus[79] == 1'b0",
        false,
    );
    check_logic(
        "bus[79:0] == (sel ? '1 : '0) && sel == 1'b1 && bus[79] == 1'b1",
        true,
    );
}

#[test]
fn scan_ident_token_strictness_and_escaped_newline() {
    for ok_id in [
        "./sig",
        "../sig",
        "../../sig",
        "../\\u_esc /sig",
        "sig.sub",
        "\\u_esc .sub",
    ] {
        assert!(
            parse_logic_expr(&format!("{ok_id} == 1'b1")).is_ok(),
            "expected valid ident `{ok_id}`"
        );
    }
    for bad_id in [
        "...ident",
        "sig.",
        "1.5",
        "u_a..sig",
        "u_a../sig",
        "u_a.../sig",
        "../.sig",
        "\\u_esc ./sig",
        "\\u_esc ../sig",
    ] {
        assert!(
            parse_logic_expr(&format!("{bad_id} == 1'b1")).is_err(),
            "expected syntax error for invalid ident `{bad_id}`"
        );
    }
    assert!(parse_logic_expr("u_top/\\u_mid\n/sig == 1'b1").is_ok());
    check_logic("u_top/\\u_mid\n/sig == 1'b1", true);
}

#[test]
fn deterministic_ieee1800_standalone_widths_and_pinned_soundness() {
    // Category A standalone IEEE 1800-2023 §11.6 SAT checks:
    for sat_expr in [
        "(bus >> 4) == 4'hF && (~bus) == 8'h0F",
        "((bus >> 4) & 4'hF) == 4'hF && (~bus) == 8'h0F",
        "(bus >> 4) == 8'h0F && (~bus) == 8'h0F",
        "(bus >> 4) == 8'h0F && bus == '1",
        "~(bus >> 4) == 8'hF0",
        "~(bus >> 4) == 8'hF0 && bus == 8'hFF",
        "((bus >> 4) << 4) == 8'hF0 && (~bus) == 8'h0F",
        "(~((bus >> 4) | 8'hF0)) == 8'h00 && bus == 8'hFF",
        "((bus >> 4) ^ 8'hF0) == 8'hFF && (~bus) == 8'h00",
        "(bus >> 4) == (other >> 2) && (~bus) == 8'h0F && other == 8'h3C",
        "(bus >> 4) < 8'h10 && (~bus) == 8'h0F",
        "(sel ? (bus >> 4) : 8'h0F) == 8'h0F && sel && (~bus) == 8'h0F",
        "((bus >> 4) >> 4) == 8'h0F && (~bus) == 12'h0FF",
        "(4'hF << 4) == 8'hF0",
        "(bus[3:0] << 4) == 8'hF0 && bus == 8'h0F",
        "(bus >> 4) == 4'hF && (bus >> 4) == 8'h0F && bus == 8'hFF",
        "(bus << 4) == 8'hF0 && bus == 4'hF",
        "~(4'hF << 4) == 8'h0F",
        "(~4'h0) == 8'hFF",
        "((bus >> 4) | 8'hE0) == 8'hE0 && (~bus) == 8'hF0",
        "!(bus >> 4) && bus == '1",
        "^bus == 1'b1 && bus == 2'b01",
        "~^bus == 1'b1 && bus == 2'b11",
        "(sel ? (bus >> 4) : 4'hF) == 4'hF && !sel && bus == '1 && bus == 4'hF",
        "((bus >> 4) ? 1'b0 : 1'b1) && bus == '1 && bus == 4'hF",
        "((bus >> 4) || other) && bus == '1 && bus == 4'hF",
        "!((bus >> 4) == 4'hF) && bus == '1 && bus == 4'hF",
        "(~^(bus >> 4)) == 1'b1 && bus == '1 && bus == 4'hF",
        "((bus >> 4) ^ (~4'h0)) == 8'hF0",
        "((bus >> 4) ^ (~4'h0)) == 8'hFF && bus == 4'h0",
        "(bus[3:0] ^ (~4'h0)) == 4'h0",
        "((~bus) >> 4) == 4'h0 && bus == 4'h0",
        "((~bus) >> 4) == 4'h1 && bus == 5'h0F",
        "((~(bus >> 4)) >> 1) == 4'h7 && bus == '0 && bus == 4'h0",
        "((((bus >> 4) | 4'b0100) >> 1) != 0) && bus == '1 && bus == 4'hF",
        "((~bus) >> 4) == 4'h0 && bus == 4'hF",
        "(((bus >> 4) ^ 4'hF) >> 4) == 4'h0",
        "(((~bus) ^ 4'hF) >> 4) == 4'h0 && bus == 4'h0",
        "a == 4'hF && ((!(a == '1) && a[4] == 1'b0) || 1'b0)",
        "(((~(bus >> 4)) ^ 8'hF0) == 8'h0E)",
        "bus == 4'hF && (((bus & (~4'h0)) == 5'h0F) || 1'b0)",
        "(&bus) && (bus[7:0] == 8'hFF)",
        "a == 8'hFF && b == a",
        "((bus >> 4) ^ (~4'h0)) == 8'hFF && bus == 4'h0",
        "bus == 4'h0 && ((~4'hF) >> 5) == 10'h01F",
        "bus == 4'h0 && ((~4'hF) >> 6) == 10'h00F",
        "bus == 4'h0 && (4'h8 << 1) == 5'h10",
        "(bus <= 4'h1 && bus != 4'h1) && !(bus == (5'h08 << 1))",
        "a == 4'hF && ((a & b) != ~4'h0)",
    ] {
        check_logic(sat_expr, true);
    }

    check_illegal("1'b1", "(bus >> 4) == 1'b0", false);
    check_illegal_with_widths("1'b1", "(bus >> 4) == 1'b0", &[("bus", 8)], true);
    check_illegal("1'b1", "(bus >> 4) == 4'h0", false);
    check_illegal_with_widths("1'b1", "(bus >> 4) == 4'h0", &[("bus", 8)], true);
    check_illegal(
        "!other && bus == '1 && bus == 4'hF",
        "!(bus >> 4) && other",
        true,
    );

    // Pinned-width & IEEE 1800-2023 UNSAT soundness invariants (must all be `false`):
    for unsat_expr in [
        "(bus >> 4) == 4'hF && bus[7:4] == 4'hE",
        "(bus >> 4) == 4'hF && bus == 4'h5",
        "(bus >> 4) == 4'hF && (bus >> 2) == 6'h38",
        "(bus >> 4) == '1 && bus == 8'hFF",
        "(4'hF << 4) == '1",
        "^bus == 1'b1 && bus == 2'b11",
        "^~bus == 1'b1 && bus == 2'b01",
        "((bus >> 4) ^ (~4'h0)) == 4'h0",
        "(~&bus) && bus == 1'b1",
        "bus == 4'h0 && (((~bus) >> 4) == 4'h1 || 1'b0)",
        "bus == 4'h0 && (((~bus) > 4'hF) || 1'b0)",
        "((~bus) >> 4) == 4'h1 && bus == 4'h0",
        "((~bus) >> 4) == 4'h1 && bus == '1",
        "((~bus) == 4'h0) && (((~bus) >> 4) == 4'h1)",
        "(bus == 4'hF) && ((~bus) >> 4) == 4'h1",
        "(~&((bus >> 4) | 4'hE)) == 1'b0",
        "bus == 4'hF && (((bus != ~4'h0) && (((~bus) >> 4) == 4'h1)) || 1'b0)",
        "bus == 4'h0 && (((bus | (~4'h0)) >> 4) == 4'h1)",
        "bus == 4'h0 && !(bus == (4'h8 << 1))",
        "bus == 4'hF && !(bus == '1)",
        "bus == '1 && !(bus == (~4'h0))",
        "bus_a == 4'h0 && bus_b == 4'h0 && (((bus_a | (~bus_b)) >> 4) == 4'h1)",
        "bus_a == 4'hF && bus_b == 4'h0 && !(bus_a == (~bus_b))",
        "bus == (~4'hF) && ((bus >> 4) == 4'h1)",
        "bus == '1 && ((~bus) != 4'h0)",
        "bus == 4'h0 && !(bus == ((~4'hF) << 1))",
        "bus == 4'h0 && bus_b == 4'hF && !(bus == ((~bus_b) << 4))",
        "(~&a) && a == b && b == c && c == 1'b1",
        "((a >> 1) < (b >> 1)) && ((b >> 1) < (a >> 1))",
        "((~a) >> 1) == y && y == 1'b1 && a == 1'b0",
        "((~a) >> 1) != y && y == 1'b0 && a == 1'b0",
        "((~a) > y) && y == 1'b1 && a == 1'b0",
        "a == b && b == 1'b0 && (((~a) >> 1) == y && y == 1'b1)",
        "a == 1'b0 && !(((~a) << 1) == 1'b0)",
        "!(a == '1) && a == 1'b1",
        "(~&a) && (a == (cond ? 1'b1 : ~1'b0))",
        "a == b && b == c && c == d && ((d == 1'b0 && (~a) != 1'b1) || (d == 1'b1 && (~a) != 1'b0))",
        "(~y) && (y == 4'h8) && ((x >> 4) == (~y))",
        // Line 6217 & 6201 IEEE 1800-2023 Table 11-21: `4'h8 << 1` is self-determined 4-bit (`4'h0`).
        "bus_a < (4'h8 << 1)",
        "(bus <= 4'h1 && bus != 4'h1) && !(bus == (4'h8 << 1))",
        "bus_a == 4'hF && !(bus_a == (bus_b | (~4'h0)))",
    ] {
        check_logic(unsat_expr, false);
    }

    // Companion check to `bus_a < (4'h8 << 1)`: IEEE 1800-2023 §11.6 Table 11-21 makes `<<` LHS
    // context-determined (4 bits against `4'h0` -> `4'h0`, 8 bits against `8'h10` -> `8'h10`).
    check_logic("bus_a < (5'h08 << 1)", true);
    check_logic("(4'h8 << 1) == 8'h10", true);
    check_logic("(4'h8 << 1) == 4'h0", true);
    check_logic("(4'h8 << 1) == 8'h00", false);
}

#[test]
fn witness_width_a_at_8_bits_with_and_without_signal_width_provider() {
    // Standalone derives W(a) = max(8, 9) = 9 because `9'h100` is peer to `~a`;
    // explicit RTL width via `MapWidthProvider(&[("a", 8)])` pins `a` at 8 bits with zero `a[8]`.
    let w_standalone = check_logic("((~a) == 9'h100) && a == 8'hFF", true)
        .expect("expected SAT witness standalone");
    assert_eq!(w_standalone.logic_signals.get("a[0]"), Some(&true));

    let w_rtl = check_logic_with_widths("((~a) == 9'h100) && a == 8'hFF", &[("a", 8)], true)
        .expect("expected SAT witness with MapWidthProvider(a: 8)");
    assert!(
        !w_rtl.logic_signals.contains_key("a[8]"),
        "MapWidthProvider(a: 8) must keep `a` at 8 bits (no a[8]), got {:?}",
        w_rtl.logic_signals
    );

    // With explicit 8-bit width (`a: 8`), `a == 8'hFF` equals `'1`, so `!(a == '1)` is `false` (whereas standalone derives 9 bits from `9'h100`):
    for expr in [
        "(a == 8'hFF) && ((~a) == 9'h100) && !(a == '1)",
        "(a == 8'hFF) ? ((~a) == 9'h100 && !(a == '1)) : 1'b0",
    ] {
        check_logic(expr, true);
        check_logic_with_widths(expr, &[("a", 8)], false);
    }

    // Dual-mode Category B conversions: standalone (`NullWidthProvider`) vs explicit RTL widths (`MapWidthProvider`).
    let cat_b1_cases: &[(&str, &[(&str, u32)])] = &[
        ("(bus >> 4) == 4'hF && bus == '1", &[("bus", 8)]),
        ("(bus >> 4) == 4'hF && (bus >> 2) == 6'h3C", &[("bus", 8)]),
        ("(bus >> 4) > 4'h7", &[("bus", 8)]),
        ("(bus >> 4) != 4'h0", &[("bus", 8)]),
        ("((bus >> 4) ^ 4'hF) == 4'h0", &[("bus", 8)]),
        ("((~bus) >> 4) == 4'h1", &[("bus", 8)]),
        (
            "((bus_a >> 4) != (bus_b >> 4))",
            &[("bus_a", 8), ("bus_b", 8)],
        ),
        ("((~(~bus)) >> 4) == 4'h1 && bus == '1", &[("bus", 5)]),
        (
            "(a >> 4) == 4'hF && (b >> 4) == (a >> 4)",
            &[("a", 8), ("b", 8)],
        ),
        ("a == (~b) && ((a >> 4) == 4'h1)", &[("a", 8), ("b", 8)]),
        (
            "((sel ? (bus >> 4) : 4'h0) == 4'hF) && sel",
            &[("sel", 1), ("bus", 8)],
        ),
        ("((~&a) == 1'b0) && ((a >> 4) == 4'hF)", &[("a", 8)]),
        (
            "((~(bus_a >> 4)) < (~(bus_b >> 4)))",
            &[("bus_a", 8), ("bus_b", 8)],
        ),
        (
            "bus_a == 4'hF && !(bus_a == (bus_b | (~4'h0)))",
            &[("bus_a", 5), ("bus_b", 5)],
        ),
    ];
    for &(expr, widths) in cat_b1_cases {
        check_logic(expr, false);
        check_logic_with_widths(expr, widths, true);
    }

    // Category B2: Reduction XOR parity probes over `| 8'hFF` / `| 32'hFFFFFFFF`:
    check_logic("^(bus | 8'hFF) == 1'b1", false);
    check_logic_with_widths("^(bus | 8'hFF) == 1'b1", &[("bus", 9)], true);
    check_logic("^((bus >> 4) | 8'hFF) == 1'b1", false);
    check_logic_with_widths("^((bus >> 4) | 8'hFF) == 1'b1", &[("bus", 13)], true);
    check_logic("^((bus >> 4) | 32'hFFFFFFFF) == 1'b1", false);
    check_logic_with_widths("^((bus >> 4) | 32'hFFFFFFFF) == 1'b1", &[("bus", 37)], true);

    // Category B3: Non-zero upper bits vs lower bit-slice masks:
    check_logic("((bus | 8'h00) != 8'h00) && (bus[7:0] == 8'h00)", false);
    check_logic_with_widths(
        "((bus | 8'h00) != 8'h00) && (bus[7:0] == 8'h00)",
        &[("bus", 9)],
        true,
    );
    check_logic("((a | 8'hFF) != 8'hFF) && ((a & 9'h100) == 9'h000)", false);
    check_logic_with_widths(
        "((a | 8'hFF) != 8'hFF) && ((a & 9'h100) == 9'h000)",
        &[("a", 10)],
        true,
    );
    check_logic(
        "((a | 8'hFF) != 8'hFF) && (a == b) && ((b & 9'h100) == 9'h000)",
        false,
    );
    check_logic_with_widths(
        "((a | 8'hFF) != 8'hFF) && (a == b) && ((b & 9'h100) == 9'h000)",
        &[("a", 10), ("b", 10)],
        true,
    );

    // 100-hop coupled chain under explicit MapWidthProvider(s0..s100 -> 10):
    let mut coupled_parts = vec!["((s0 | 8'hFF) != 8'hFF)".to_string()];
    let mut widths: Vec<(String, u32)> = vec![("s0".to_string(), 10)];
    for i in 0..100 {
        coupled_parts.push(format!("((s{i} | 8'h00) == s{})", i + 1));
        widths.push((format!("s{}", i + 1), 10));
    }
    coupled_parts.push("((s100 & 9'h100) == 9'h000)".to_string());
    let coupled_chain = coupled_parts.join(" && ");
    let mut provider = MapWidthProvider::new();
    for (n, w) in widths {
        provider.insert(n, w);
    }
    let t_coupled = Instant::now();
    common::check_logic_with_provider(&coupled_chain, &provider, true);
    assert!(
        t_coupled.elapsed().as_millis() < 250,
        "100-hop coupled chain with MapWidthProvider took {:?}",
        t_coupled.elapsed()
    );

    // Category B4: `(bus >> 65535) == 1'b1` standalone is false and <25ms (1-bit standalone width):
    let t_65k = Instant::now();
    check_logic("(bus >> 65535) == 1'b1", false);
    assert!(t_65k.elapsed().as_millis() < 25);
}

#[test]
fn ieee1800_redand_redor_self_determined_and_oob_bit_select_padding() {
    // RedAnd / RedOr self-determined operand boundary check:
    let ast_red = parse_logic_expr("(&a) && (a == 4'hF)").unwrap();
    let typed_red = typecheck_ast(&ast_red, &NullWidthProvider);
    if let TypedExprKind::And(items) = &typed_red.kind {
        assert_eq!(items[0].self_w, 1);
        assert_eq!(items[0].ctx_w, 1);
        if let TypedExprKind::RedAnd(inner) = &items[0].kind {
            assert_eq!(inner.self_w, 4);
            assert_eq!(inner.ctx_w, 4);
        } else {
            panic!("expected RedAnd");
        }
    } else {
        panic!("expected And");
    }

    // Out-of-bounds bit select with MapWidthProvider(sig: 4) zero-pads sig[9] without widening `sig`:
    let mut prov = MapWidthProvider::new();
    prov.insert("sig", 4);
    let ast_oob = parse_logic_expr("sig[9] == 1'b1 && sig == 4'hF").unwrap();
    let typed_oob = typecheck_ast(&ast_oob, &prov);
    if let TypedExprKind::And(items) = &typed_oob.kind {
        if let TypedExprKind::Eq(lhs, _) = &items[0].kind {
            if let TypedExprKind::Signal { sig_width, .. } = lhs.kind {
                assert_eq!(sig_width, 4, "sig_width must stay pinned at 4");
            }
        }
    }
    check_logic_with_widths("sig[9] == 1'b0 && sig == 4'hF", &[("sig", 4)], true);
    check_logic_with_widths("sig[9] == 1'b1 && sig == 4'hF", &[("sig", 4)], false);

    // Witness for `sig[9:0] == 10'h00F && sig == 4'hF` must NOT allocate `sig[4]..sig[9]`:
    let w_oob = check_logic_with_widths("sig[9:0] == 10'h00F && sig == 4'hF", &[("sig", 4)], true)
        .expect("OOB upper bits zero-padded to match 10'h00F");
    for bit in 4..10 {
        assert!(
            !w_oob.logic_signals.contains_key(&format!("sig[{bit}]")),
            "signal_bits must not widen past 4 bits, found sig[{bit}]"
        );
    }
}

#[test]
fn regression_r20_root_anchored_colon_ternary_and_slice_width_cap() {
    assert_eq!(
        qualify_logic_expr_in_scope("sel ? :u_sub:a : :u_sub:b", "u_top", ':'),
        "u_top/sel ? /u_sub/a : /u_sub/b"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("sel ? :a : :b", "u_top", ':'),
        "u_top/sel ? /a : /b"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("c1 ? (c2 ? :a : :b) : :c", "u_top", ':'),
        "u_top/c1 ? (u_top/c2 ? /a : /b) : /c"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("c1 ? c2 ? :a : :b : :c", "u_top", ':'),
        "u_top/c1 ? u_top/c2 ? /a : /b : /c"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("sel ? !:u_sub:a : :u_sub:b", "u_top", ':'),
        "u_top/sel ? !/u_sub/a : /u_sub/b"
    );
    for expr in [
        "u_top/sel ? /u_sub/a : /u_sub/b",
        "u_top/c1 ? (u_top/c2 ? /a : /b) : /c",
        "u_top/c1 ? u_top/c2 ? /a : /b : /c",
        "u_top/sel ? !/u_sub/a : /u_sub/b",
    ] {
        assert!(
            parse_logic_expr_in_scope(expr, "").is_ok(),
            "expected valid parsed AST for `{expr}`"
        );
    }
    check_logic(
        "(u_top/sel ? /u_sub/a : /u_sub/b) && u_top/sel && /u_sub/a && !/u_sub/b",
        true,
    );
    check_logic(
        "(u_top/sel ? /u_sub/a : /u_sub/b) && u_top/sel && !/u_sub/a",
        false,
    );

    assert!(parse_logic_expr("sig[4294967295:0]").is_err());
    assert!(parse_logic_expr("sig[65536:0]").is_err());
    assert!(parse_logic_expr("sig[65535:0]").is_ok());
}

#[test]
fn regression_r23_hier_sv_escaped_and_array_index_paths() {
    use std::sync::Arc;

    use informal_upf::{parse_upf_and_resolve, NullObjectFinder, PstSat, DEFAULT_VOLTAGE_TOL_UV};

    for (expr, expected_sig) in [
        ("u_top/\\bus [0]/en == 1'b1", "u_top/\\bus [0]/en"),
        ("u_top/\\gen [3:0]/en == 1'b1", "u_top/\\gen [3:0]/en"),
        ("u_top/arr[0]/en == 1'b1", "u_top/arr[0]/en"),
        ("u_top/arr[3:0]/en == 1'b1", "u_top/arr[3:0]/en"),
    ] {
        let ast = parse_logic_expr(expr).unwrap_or_else(|e| panic!("failed `{expr}`: {e}"));
        assert_eq!(
            informal_upf::collect_logic_expr_signals(&ast),
            std::collections::BTreeSet::from([expected_sig.to_string()])
        );
    }

    // Trailing leaf bit-selects remain bit-selects on the leaf signal.
    let leaf_bus = parse_logic_expr("u_top/\\bus [0] == 1'b1").unwrap();
    assert_eq!(
        leaf_bus,
        LogicConstraintAst::Eq(
            Box::new(LogicConstraintAst::Signal {
                name: "u_top/bus".to_string(),
                msb: Some(0),
                lsb: Some(0),
            }),
            Box::new(LogicConstraintAst::BitConst {
                width: 1,
                bits: vec![true],
            }),
        )
    );
    let leaf_arr = parse_logic_expr("u_top/arr[3:0] == 4'hF").unwrap();
    assert_eq!(
        leaf_arr,
        LogicConstraintAst::Eq(
            Box::new(LogicConstraintAst::Signal {
                name: "u_top/arr".to_string(),
                msb: Some(3),
                lsb: Some(0),
            }),
            Box::new(LogicConstraintAst::BitConst {
                width: 4,
                bits: vec![true, true, true, true],
            }),
        )
    );

    // Qualify in scope with `\bus [0]` and `\gen [3:0]`: no trailing space after `[...]`, round-trips cleanly.
    let q_bus0 = qualify_logic_expr_in_scope("save_en", "u_top/\\bus [0]", '/');
    assert_eq!(q_bus0, "u_top/\\bus [0]/save_en");
    assert_eq!(
        parse_logic_expr(&q_bus0).unwrap(),
        parse_logic_expr("u_top/\\bus [0]/save_en").unwrap()
    );
    let q_gen30 = qualify_logic_expr_in_scope("save_en", "u_top/\\gen [3:0]", '/');
    assert_eq!(q_gen30, "u_top/\\gen [3:0]/save_en");
    assert_eq!(
        parse_logic_expr(&q_gen30).unwrap(),
        parse_logic_expr("u_top/\\gen [3:0]/save_en").unwrap()
    );

    // Non-slash separators (`:` and `.`) + ternary disambiguation.
    assert_eq!(
        qualify_logic_expr_in_scope("u_top:\\bus [0]:en == 1'b1", "", ':'),
        "u_top/\\bus [0]/en == 1'b1"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("u_top.\\bus [0].en == 1'b1", "", '.'),
        "u_top/\\bus [0]/en == 1'b1"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("u_top.arr[0].en == 1'b1", "", '.'),
        "u_top/arr[0]/en == 1'b1"
    );
    assert_eq!(
        qualify_logic_expr_in_scope("sel ? u_top:\\gen [3:0]:a : u_top:arr[0]:b", "", ':'),
        "sel ? u_top/\\gen [3:0]/a : u_top/arr[0]/b"
    );
    assert!(parse_logic_expr("u_top/\\bus [0] /save_en").is_err());

    // `add_power_state` in `set_scope {u_top/\bus [0]}` preserves `\` and matches root `u_top/\bus [0]/en`.
    let upf_scoped_match = r#"
        upf_version 2.1
        create_supply_net VDD
        create_supply_net VSS
        create_supply_set SS -function {power VDD} -function {ground VSS}
        set_scope {u_top/\bus [0]}
        create_supply_net VDD_L
        create_supply_net VSS_L
        create_supply_set SS_L -function {power VDD_L} -function {ground VSS_L}
        add_power_state SS_L -state {ON -supply_expr {power == {FULL_ON 0.9}} -logic_expr {en == 1'b1} -simstate NORMAL}
        set_scope /
    "#;
    let res =
        parse_upf_and_resolve(upf_scoped_match, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat = PstSat::build(&res, DEFAULT_VOLTAGE_TOL_UV);
    assert!(sat.is_state_space_consistent());
    let witnesses = sat.enumerate_symbolic_states(1);
    assert_eq!(witnesses.len(), 1);
    assert_eq!(
        witnesses[0].logic_signals.get("u_top/\\bus [0]/en"),
        Some(&true),
        "expected `u_top/\\bus [0]/en`, got {:?}",
        witnesses[0].logic_signals
    );

    let upf_conflict = format!(
        "{upf_scoped_match}\nadd_power_state SS -state {{ON -supply_expr {{power == {{FULL_ON 0.9}}}} -logic_expr {{u_top/\\bus [0]/en == 1'b0}} -simstate NORMAL}}\n"
    );
    let res_conflict =
        parse_upf_and_resolve(&upf_conflict, None, None, Arc::new(NullObjectFinder)).unwrap();
    let mut sat_conflict = PstSat::build(&res_conflict, DEFAULT_VOLTAGE_TOL_UV);
    assert!(
        !sat_conflict.is_state_space_consistent(),
        "scoped `en == 1'b1` in `u_top/\\bus [0]` must conflict with root `u_top/\\bus [0]/en == 1'b0`"
    );
}
