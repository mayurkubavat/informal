// Layer 5 — Per-App & Complex Operator Debug Real-Backend E2E Suite
// Drives the real React Workbench in Playwright connected via `?bridge=` to the
// REAL compiled Rust `informal` binary (`svlang` + CaDiCaL / IC3 / k-induction / Bitwuzla)
// with ZERO `mock.ts`, exercising pass/fail debug analysis across:
//   1. FPV CEX Debug: Trace -> Why CEX -> RTL Live Values & Go-to-Driver -> COI Schematic -> DesignInfo -> Disable / Assume / Stopat Re-Prove
//   2. Formal-Readiness Lint (`SV-FPV-003`) & 1-Click Quick-Fix on real RTL
//   3. Convergence Engineering: `get_fv_complexity` + `assume -from_assert` + `abstract -counter` (1,000,000-cycle cover)
//   4. COV / UNR App (`check_cov`): real reachable + dead-code UNR table, CodeMirror coverage gutter, and waiver
//   5. X-Prop App (`check_xprop`): real 2-copy miter proving reconvergent mux X-clean + catching unreset flop X-leak + waiver
//   6. SPV Security Path App (`check_spv`): UI rule builder proving non-interference (`pub_status_o`) & catching leak (`debug_port_o`)
//   7. SEC / LEC Equivalence App (`check_sec`): buggy RTL mismatch CEX -> fixed RTL equivalence Proven
//   8. CSR / RAL & Connectivity Apps (`check_csr` + `check_conn`): multi-task synthesis, proving, and wiring CEX waveform inspection
//   9. Low-Power UPF App (`load_upf` + `check_lp` + `report_lp`): multi-domain UPF structural & missing-isolation detection

import fs from "node:fs";
import path from "node:path";
import { expect, test, type Page } from "@playwright/test";
import { findInformalBinary, REPO_ROOT, startRealBackendBridge } from "./real-backend-server.mjs";

const HAS_REAL_BIN = Boolean(findInformalBinary());
const gridRows = (page: Page) => page.locator(".table-wrap table.grid tbody tr:not(.empty-row)");
const gridRow = (page: Page, name: string) => gridRows(page).filter({ hasText: name });

async function waitIdle(page: Page, timeout = 30_000) {
  await expect(page.locator("footer.statusbar")).toContainText("Idle", { timeout });
}

async function runTcl(page: Page, cmd: string, timeout = 30_000) {
  const prompt = page.getByLabel("Tcl command");
  await prompt.fill(cmd);
  await prompt.press("Enter");
  await waitIdle(page, timeout);
}

async function openAppsDock(page: Page) {
  await page.getByRole("menuitem", { name: "Tools", exact: true }).click();
  await page.getByRole("menuitem", { name: "Formal apps" }).click();
  const apps = page.locator(".formal-apps");
  await expect(apps).toBeVisible();
  return apps;
}

test.describe("Layer 5 — Per-App & Complex Debug Real-Backend E2E", () => {
  test.skip(!HAS_REAL_BIN, "requires compiled target/{release,debug}/informal binary");

  let bridge: { url: string; workdir: string; stop: () => Promise<void> };

  test.beforeEach(async () => {
    bridge = await startRealBackendBridge();
  });

  test.afterEach(async () => {
    await bridge?.stop();
  });

  test("1. FPV Pass/Fail Debug Loop: CEX Trace -> real Why CEX -> RTL live values & Go-to-Driver -> COI Schematic -> DesignInfo -> Disable / Assume / Stopat re-prove", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const pathInput = page.getByRole("textbox", { name: "Script path" });
    await pathInput.fill("examples/01-quickstart/counter/run.jg.tcl");
    await pathInput.press("Enter");
    await expect(page.locator(".strip-summary")).toContainText("4 proven", { timeout: 30_000 });
    await expect(page.locator(".strip-summary")).toContainText("1 cex");
    await waitIdle(page);

    // Open real CEX VCD on `a_never9` and compute real backward causal slice (`why_cex`)
    await gridRow(page, "a_never9").dblclick();
    const ctxBar = page.getByRole("region", { name: "Active property context" });
    await expect(ctxBar).toContainText("a_never9");
    await expect(page.locator(".trace-bar .strip-summary")).toContainText("CEX");
    await page.getByRole("button", { name: "Why CEX" }).click();
    const sliceRegion = page.getByRole("region", { name: "Causal root-cause slice" });
    await expect(sliceRegion).toBeVisible({ timeout: 15_000 });
    await expect(sliceRegion).toContainText(/COI/);

    // Pivot to RTL Source -> verify live trace value badge + real `signal_nav` Go-to-Driver on `q`
    const pivots = ctxBar.getByRole("group", { name: "Switch property view" });
    await pivots.getByRole("button", { name: "Source" }).click();
    const fileSwitcher = page.getByRole("group", { name: "Source files" });
    if (await fileSwitcher.count()) {
      await fileSwitcher.getByRole("button", { name: "counter.sv" }).click();
    }
    await expect(page.locator(".source-live-badge")).toContainText("Live values @C");
    const sigInput = page.getByRole("textbox", { name: "Go to signal driver or load" });
    await sigInput.fill("q");
    await sigInput.press("Enter");
    await expect(page.locator(".nav-popover")).toContainText("Drivers", { timeout: 10_000 });
    await expect(page.locator(".nav-popover")).toContainText(/counter\.sv/);

    // Pivot to COI Schematic -> verify real `schematic_get` graph loads
    await pivots.getByRole("button", { name: "Cone" }).click();
    await expect(page.locator(".schematic-pane")).toBeVisible();
    await expect(page.locator(".schematic-pane")).toContainText("a_never9", { timeout: 10_000 });

    // Open Design Info dock tab -> verify real `get_design_info -json` model counts
    await page.getByRole("tab", { name: "Design Info" }).click();
    await expect(page.locator(".design-info")).toContainText("counter", { timeout: 15_000 });
    await page.getByRole("tab", { name: "Console" }).click();

    // Return to Table and execute Pass/Fail triage loop:
    await pivots.getByRole("button", { name: "Table" }).click();

    // (a) Disable failing `a_never9` via JG context menu -> row turns Disabled
    await gridRow(page, "a_never9").click({ button: "right" });
    await page.getByRole("menu", { name: "Actions for a_never9" }).getByRole("menuitem", { name: /Disable/ }).click();
    await waitIdle(page);
    await expect(gridRow(page, "a_never9")).toContainText("Disabled");

    // (b) Re-enable `a_never9`, constrain `q == 4'd8 |-> !en`, re-prove -> `a_never9` flips from CEX to Proven!
    await gridRow(page, "a_never9").click({ button: "right" });
    await page.getByRole("menu", { name: "Actions for a_never9" }).getByRole("menuitem", { name: /Enable/ }).click();
    await waitIdle(page);
    await runTcl(page, "assume -name m_cap8 {q == 4'd8 |-> !en}");
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(gridRow(page, "a_never9")).toContainText("Proven");

    // (c) Cut driver on `q` via Design sidebar right-click (`stopat q`) -> `a_max` flips to CEX, then restore (`stopat -clear q`) -> Proven!
    await page.locator("aside.left").getByRole("tab", { name: "Design" }).click();
    const qSig = page.locator(".sig-row", { hasText: /^q/ }).first();
    await qSig.click({ button: "right" });
    await page.getByRole("menu", { name: "Signal actions for counter.q" }).getByRole("menuitem", { name: /Cut driver/ }).click();
    await waitIdle(page);
    await expect(qSig.locator(".tag-cut")).toHaveText("cut");
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(gridRow(page, "a_max")).toContainText("CEX");

    await qSig.click({ button: "right" });
    await page.getByRole("menu", { name: "Signal actions for counter.q" }).getByRole("menuitem", { name: /Restore driver/ }).click();
    await waitIdle(page);
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(gridRow(page, "a_max")).toContainText("Proven");
  });

  test("2. Formal-Readiness Lint (`SV-FPV-003`) & 1-Click Quick-Fix rewrites isolated RTL file and clears diagnostic", async ({
    page,
  }) => {
    await bridge.stop();
    const sandboxDir = fs.mkdtempSync(path.join(path.dirname(bridge.workdir), "informal-lint-root-"));
    try {
      const svFile = path.join(sandboxDir, "lint_dut.sv");
      fs.writeFileSync(
        svFile,
        `module lint_dut(input logic clk, input logic rst_n, output logic [3:0] q);
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) q <= 4'd0;
    else if (q < 4'd9) q <= q + 4'd1;
  a_bound: assert property (@(posedge clk) q <= 4'd9);
endmodule
`,
      );
      bridge = await startRealBackendBridge({ root: sandboxDir });
      await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
      await waitIdle(page);

      await runTcl(page, `analyze -sv12 {${svFile}}; elaborate -top lint_dut; clock clk; reset -expression {!rst_n}`);
      await page.keyboard.press("F3");
      const lintStrip = page.getByRole("region", { name: "Formal-readiness lint diagnostics" });
      await expect(lintStrip).toContainText("SV-FPV-003", { timeout: 15_000 });
      await lintStrip.getByRole("button", { name: /Quick-Fix/ }).click();

      await expect(lintStrip).toHaveCount(0, { timeout: 15_000 });
      expect(fs.readFileSync(svFile, "utf8")).toContain("disable iff");
    } finally {
      fs.rmSync(sandboxDir, { recursive: true, force: true });
    }
  });

  test("3. Convergence Engineering: `get_fv_complexity` + `assume -from_assert` + `abstract -counter` reaches 1,000,000-cycle watchdog cover", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const sv = path.join(REPO_ROOT, "examples/03-convergence/deep_counter/deep_counter.sv");
    await runTcl(
      page,
      `analyze -sv12 {${sv}}; elaborate -top deep_counter; clock clk; reset -expression {!rst_n}; assert -name a_wdog_bound {wdog_cnt <= 24'd1000000}; assert -name a_expired_exact {wdog_expired |-> wdog_cnt == 24'd1000000}; cover -name c_wdog_expired {wdog_expired}`,
    );

    await runTcl(page, "get_fv_complexity");
    await expect(page.locator(".console-log")).toContainText("wdog_cnt");

    await runTcl(page, "prove -property a_wdog_bound");
    await expect(gridRow(page, "a_wdog_bound")).toContainText("Proven");

    await runTcl(page, "assume -from_assert a_wdog_bound; abstract -counter wdog_cnt -threshold 1000000");
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);

    await expect(gridRow(page, "a_expired_exact")).toContainText("Proven");
    await expect(gridRow(page, "c_wdog_expired")).toContainText("Covered");
  });

  test("4. COV / UNR Formal App: real reachable + dead-code UNR targets, CodeMirror coverage gutter, and COI waiver", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const sv = path.join(REPO_ROOT, "examples/04-formal-apps/cov/dut.sv");
    await runTcl(
      page,
      `analyze -sv12 {${sv}}; elaborate -top cov_dut; clock clk; reset -expression {!rst_n}; assume -name m_no_mode3 {mode != 2'b11}; assert -name a_chk {checked_q |-> $past(mode == 2'b01) || !$past(rst_n)}; prove -all`,
    );

    const apps = await openAppsDock(page);
    await apps.getByRole("button", { name: "Measure COI & UNR" }).click();
    await waitIdle(page);
    await expect(apps).toContainText("covered");
    await expect(apps).toContainText("unreachable");

    // Open Source view -> verify CodeMirror coverage gutter markers render on real RTL lines
    await page.keyboard.press("F3");
    await expect(page.locator(".cm-cov-gutter")).toBeVisible({ timeout: 10_000 });

    // Waive unobserved shadow register via Console and verify report reflects waiver
    await page.getByRole("tab", { name: "Console" }).click();
    await runTcl(page, "check_cov -waiver -add -type coi -instance shadow_unobs_q -comment {debug shadow}; check_cov -report");
    await expect(page.locator(".console-log")).toContainText("shadow_unobs_q");
  });

  test("5. X-Prop Formal App: real 2-copy miter proves reconvergent mux X-clean, catches unreset flop X-leak, and applies waiver", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const sv = path.join(REPO_ROOT, "examples/04-formal-apps/xprop/xprop_dut.sv");
    await runTcl(page, `analyze -sv12 {${sv}}; elaborate -top xprop_dut; clock clk; reset -expression {!rst_n}`);

    const apps = await openAppsDock(page);
    await apps.locator(".apps-tab", { hasText: "X-Prop" }).click();
    await apps.getByRole("button", { name: "Prove X-Prop" }).click();
    await waitIdle(page);

    await expect(apps.locator("tr", { hasText: "clean_out" })).toContainText("proven");
    await expect(apps.locator("tr", { hasText: "reconv_out" })).toContainText("proven");
    await expect(apps.locator("tr", { hasText: "leaky_out" })).toContainText("cex");

    await page.getByRole("tab", { name: "Console" }).click();
    await runTcl(page, "check_xprop -waiver -add -instance leaky_out -comment {qualified by valid}; check_xprop -report");
    await expect(page.locator(".console-log")).toContainText("waived");
  });

  test("6. SPV Security Path App: UI rule builder proves public status isolated (`proven`) and catches unlocked debug leak (`cex`)", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const sv = path.join(REPO_ROOT, "examples/04-formal-apps/spv/crypto_wrap.sv");
    await runTcl(page, `analyze -sv12 {${sv}}; elaborate -top crypto_wrap; clock clk; reset -expression {!rst_n}`);

    const apps = await openAppsDock(page);
    await apps.locator(".apps-tab", { hasText: "SPV" }).click();

    await apps.getByLabel("SPV secret source").fill("secret_key");
    await apps.getByLabel("SPV observable sink").fill("pub_status_o");
    await apps.getByRole("button", { name: "Add SPV Rule" }).click();
    await waitIdle(page);

    await apps.getByLabel("SPV secret source").fill("secret_key");
    await apps.getByLabel("SPV observable sink").fill("debug_port_o");
    await apps.getByRole("button", { name: "Add SPV Rule" }).click();
    await waitIdle(page);

    await apps.getByRole("button", { name: "Prove SPV" }).click();
    await waitIdle(page);

    await expect(apps.locator("tr", { hasText: "pub_status_o" })).toContainText("proven");
    await expect(apps.locator("tr", { hasText: "debug_port_o" })).toContainText("cex");
  });

  test("7. SEC / LEC Equivalence App: catches `imp_bug.sv` sequential mismatch (`cex`), then proves `imp_ok.sv` equivalent (`proven`)", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const secDir = path.join(REPO_ROOT, "examples/04-formal-apps/sec");
    const spec = path.join(secDir, "spec.sv");
    const impBug = path.join(secDir, "imp_bug.sv");
    const impOk = path.join(secDir, "imp_ok.sv");

    await runTcl(page, `check_sec -setup -spec {${spec}} -imp {${impBug}} -clock clk -reset {!rst_n}`);
    const apps = await openAppsDock(page);
    await apps.locator(".apps-tab", { hasText: "SEC / LEC" }).click();
    await apps.getByRole("button", { name: "Prove SEC / LEC" }).click();
    await waitIdle(page);
    await expect(apps.locator("tr", { hasText: "acc_out" })).toContainText("cex");

    await page.getByRole("tab", { name: "Console" }).click();
    await runTcl(page, `check_sec -setup -spec {${spec}} -imp {${impOk}} -clock clk -reset {!rst_n}`);
    await page.getByRole("tab", { name: "Formal Apps" }).click();
    await apps.getByRole("button", { name: "Prove SEC / LEC" }).click();
    await waitIdle(page);
    await expect(apps.locator("tr", { hasText: "acc_out" })).toContainText("proven");
  });

  test("8. CSR / RAL & Connectivity Apps: synthesizes `CSR` and `Connectivity` tasks, proves valid checks, and opens `bad_conn` CEX waveform", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    // Part A: CSR / RAL (`check_csr`)
    const csrDir = path.join(REPO_ROOT, "examples/04-formal-apps/csr");
    await runTcl(
      page,
      `analyze -sv12 {${path.join(csrDir, "csr_block.sv")}}; elaborate -top csr_block; clock clk; reset -expression {!rst_n}; check_csr -load {${path.join(csrDir, "regs.hjson")}}`,
    );
    const apps = await openAppsDock(page);
    await apps.locator(".apps-tab", { hasText: "CSR / Conn" }).click();
    await expect(apps).toContainText(/CSR assertions:\s*[1-9]/);
    await apps.getByRole("button", { name: "Show CSR Task" }).click();
    await expect(page.getByRole("button", { name: "all tasks" })).toContainText("Task: CSR");
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(page.locator(".strip-summary")).toContainText("proven");

    // Part B: Connectivity (`check_conn`) with passing mux/pipeline links + failing `bad_conn`
    const connDir = path.join(REPO_ROOT, "examples/04-formal-apps/conn");
    await page.getByRole("tab", { name: "Console" }).click();
    await runTcl(
      page,
      `clear -all; analyze -sv12 {${path.join(connDir, "soc_top.sv")}}; elaborate -top soc_top; clock clk; reset -expression {!rst_n}; check_conn -load {${path.join(connDir, "conn.csv")}}`,
    );
    await page.getByRole("tab", { name: "Formal Apps" }).click();
    await apps.getByRole("button", { name: "Show Connectivity Task" }).click();
    await expect(page.getByRole("button", { name: "all tasks" })).toContainText("Task: Connectivity");
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);

    await expect(gridRow(page, "mux_conn")).toContainText("Proven");
    await expect(gridRow(page, "pipe_conn")).toContainText("Proven");
    await expect(gridRow(page, "bad_conn")).toContainText("CEX");

    // Double-click `bad_conn` CEX row -> opens real connectivity violation VCD in TraceView!
    await gridRow(page, "bad_conn").dblclick();
    await expect(page.locator(".trace-bar .strip-summary")).toContainText("CEX");
  });

  test("9. Low-Power UPF App: `load_upf` + `check_lp` + `report_lp` flags missing isolation on cross-domain path and passes after adding UPF isolation", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    const svPath = path.join(bridge.workdir, "soc_lp.sv");
    const upfBad = path.join(bridge.workdir, "soc_lp_missing_iso.upf");
    const upfGood = path.join(bridge.workdir, "soc_lp_fixed.upf");

    fs.writeFileSync(
      svPath,
      `module lp_core(input logic clk, input logic rst_n, input logic [3:0] din, output logic [3:0] dout);
  always_ff @(posedge clk or negedge rst_n)
    if (!rst_n) dout <= 4'd0;
    else dout <= din;
endmodule

module soc_lp(
  input  logic       clk,
  input  logic       rst_n,
  input  logic       pwr_en,
  input  logic       iso_en,
  output logic       pwr_ack,
  input  logic [3:0] din,
  output logic [3:0] dout
);
  assign pwr_ack = pwr_en;
  lp_core u_core(.clk(clk), .rst_n(rst_n), .din(din), .dout(dout));
endmodule
`,
    );

    const baseUpf = `upf_version 2.1
set_design_top soc_lp
create_supply_port VDD_AON -direction in
create_supply_port VSS     -direction in
create_supply_net  VDD_AON
create_supply_net  VSS
create_supply_net  VDD_CORE
connect_supply_net VDD_AON -ports VDD_AON
connect_supply_net VSS     -ports VSS
create_power_domain PD_TOP  -include_scope
create_power_domain PD_CORE -elements {u_core}
set_domain_supply_net PD_TOP  -primary_power_net VDD_AON  -primary_ground_net VSS
set_domain_supply_net PD_CORE -primary_power_net VDD_CORE -primary_ground_net VSS
create_power_switch PSW_CORE -domain PD_CORE \\
  -input_supply_port {in VDD_AON} -output_supply_port {out VDD_CORE} \\
  -control_port {ctrl pwr_en} -ack_port {ack pwr_ack} \\
  -on_state {ON_ST in {ctrl}} -off_state {OFF_ST {!ctrl}}
add_port_state VDD_AON  -state {AON_ON 0.90}
add_port_state VSS      -state {VSS_ON 0.00}
add_port_state VDD_CORE -state {CORE_ON 0.90} -state {CORE_OFF off}
create_pst pst_soc -supplies {VDD_AON VSS VDD_CORE}
add_pst_state ALL_ON -pst pst_soc -state {AON_ON VSS_ON CORE_ON}
add_pst_state SLEEP  -pst pst_soc -state {AON_ON VSS_ON CORE_OFF}
`;
    fs.writeFileSync(upfBad, baseUpf);
    fs.writeFileSync(
      upfGood,
      `${baseUpf}
set_isolation iso_core -domain PD_CORE -applies_to outputs -clamp_value 0 \\
  -isolation_power_net VDD_AON -isolation_ground_net VSS
set_isolation_control iso_core -domain PD_CORE \\
  -isolation_signal iso_en -isolation_sense high -location parent
`,
    );

    // Run without isolation -> `check_lp` flags missing isolation violation on switchable PD_CORE -> PD_TOP crossing
    await runTcl(page, `analyze -sv12 {${svPath}}; elaborate -top soc_lp; clock clk; reset -expression {!rst_n}; load_upf {${upfBad}}; check_lp`);
    await expect(page.locator(".console-log")).toContainText("ISO_MISSING");

    // Open Formal Apps -> Low Power tab -> verify live `ISO_MISSING` violation renders for PD_CORE -> PD_TOP
    const apps = await openAppsDock(page);
    await apps.locator(".apps-tab", { hasText: "Low Power (1)" }).click();
    await expect(apps).toContainText("ISO_MISSING");
    await expect(apps).toContainText("PD_CORE");

    // Reload with complete UPF isolation strategy -> violation clears cleanly to 0!
    await page.getByRole("tab", { name: "Console" }).click();
    await runTcl(
      page,
      `clear -all; analyze -sv12 {${svPath}}; elaborate -top soc_lp; clock clk; reset -expression {!rst_n}; load_upf {${upfGood}}; check_lp -json`,
    );
    await expect(page.locator(".console-log")).toContainText('"schema": "informal.lp/1"');
    await expect(page.locator(".console-log")).toContainText('"total": 0');
    await expect(page.locator(".console-log")).toContainText('"violations": []');
    await page.getByRole("tab", { name: "Formal Apps" }).click();
    await expect(apps.locator(".apps-tab", { hasText: "Low Power (0)" })).toBeVisible();
  });

  test("10. VC Formal (`vcf`) Flavor Real-Backend Flow: `read_file`, `fvassert`, `check_fv`, `fvdisable`, `snip_driver`, and real Tab completion", async ({
    page,
  }) => {
    await page.goto(`/?bridge=${encodeURIComponent(bridge.url)}`);
    await waitIdle(page);

    // Switch Flavor dropdown to VC Formal -> imports ::vcf::* on the real Rust Tcl interpreter
    await page.getByLabel("Flavor").selectOption("vcf");
    await waitIdle(page);
    await expect(page.locator(".prompt")).toHaveText("informal[vcf]>");

    const sv = path.join(REPO_ROOT, "examples/01-quickstart/counter/counter.sv");
    await runTcl(
      page,
      `read_file -top counter -format sverilog -vcs {${sv}}; create_clock clk -period 10; create_reset rst_n -sense low; fvassert p_max -expr {q <= 4'd9}; fvassert p_bad -expr {q != 4'd9}`,
    );

    // Verify real-backend Tab completion on property prefix `p_`
    const prompt = page.getByLabel("Tcl command");
    await prompt.fill("check_fv -property p_m");
    await prompt.press("Tab");
    await expect(prompt).toHaveValue("check_fv -property p_max ");
    await prompt.fill("");

    // Prove via toolbar `Prove all` (`check_fv`) -> `p_max` Proven, `p_bad` CEX
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(gridRow(page, "p_max")).toContainText("Proven");
    await expect(gridRow(page, "p_bad")).toContainText("CEX");

    // Disable `p_bad` via VCF context menu (`fvdisable {p_bad}`)
    await gridRow(page, "p_bad").click({ button: "right" });
    await page.getByRole("menu", { name: "Actions for p_bad" }).getByRole("menuitem", { name: /Disable/ }).click();
    await waitIdle(page);
    await expect(gridRow(page, "p_bad")).toContainText("Disabled");

    // Cut `q` driver under VCF (`snip_driver q`) via Design sidebar right-click -> `p_max` flips to CEX!
    await page.locator("aside.left").getByRole("tab", { name: "Design" }).click();
    const qSig = page.locator(".sig-row", { hasText: /^q/ }).first();
    await qSig.click({ button: "right" });
    const sigMenu = page.getByRole("menu", { name: "Signal actions for counter.q" });
    await expect(sigMenu.getByRole("menuitem", { name: /Cut driver/ })).toContainText("snip_driver q");
    await sigMenu.getByRole("menuitem", { name: /Cut driver/ }).click();
    await waitIdle(page);
    await page.getByRole("button", { name: "Prove all" }).click();
    await waitIdle(page);
    await expect(gridRow(page, "p_max")).toContainText("CEX");
  });
});
