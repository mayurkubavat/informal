import type { DesignInfo as Info } from "../backend";

// The Design Info dock tab: what the design holds (counted on the netlist
// as elaborated), what the script declared, and the size of the current
// task's model after lowering. Every number comes from `get_design_info
// -json`; nothing here is computed. Asked for on demand (it builds the
// model), so the tab has a Refresh button and says when it is stale.
export function DesignInfo({
  info,
  error,
  loading,
  stale,
  onRefresh,
}: {
  info: Info | null;
  error: string | null;
  loading: boolean;
  /** The session changed since these numbers were read. */
  stale: boolean;
  onRefresh?: () => void;
}) {
  const bits = (n: number, b: number) => `${n} (${b} bit${b === 1 ? "" : "s"})`;
  const d = info?.design;
  const m = info?.model;
  const list = (xs: string[]) => (xs.length ? xs.join(", ") : "none");
  return (
    <div className="design-info">
      <div className="info-bar">
        <span className="muted">
          {loading ? "Reading the design…" : info ? `top ${info.top ?? "—"} · ${info.files} file${info.files === 1 ? "" : "s"}` : ""}
          {stale && !loading && info ? " · out of date" : ""}
        </span>
        <span className="spacer" />
        <button className="btn small" onClick={onRefresh} disabled={loading || !onRefresh} title="Count again (builds the current task's model)">
          Refresh
        </button>
      </div>
      {error && <p className="empty err">{error}</p>}
      {!info && !error && !loading && <p className="empty">Elaborate a design to see its counts.</p>}
      {info && (
        <div className="info-cols">
          <section aria-label="Design">
            <h4>Design <span className="muted">as elaborated</span></h4>
            {d ? (
              <dl className="kv">
                <dt>Instances</dt>
                <dd className="mono">{`${d.instances} of ${d.modules} module${d.modules === 1 ? "" : "s"}`}</dd>
                <dt>Inputs</dt>
                <dd className="mono">{bits(d.inputs, d.input_bits)}</dd>
                <dt>Outputs</dt>
                <dd className="mono">{bits(d.outputs, d.output_bits)}</dd>
                <dt>Flops</dt>
                <dd className="mono">{bits(d.flops, d.flop_bits)}</dd>
                <dt>Latches</dt>
                <dd className="mono">{bits(d.latches, d.latch_bits)}</dd>
                <dt>Memories</dt>
                <dd className="mono" title={d.memories.map((x) => `${x.name} ${x.words}x${x.width}`).join("\n")}>
                  {d.memories.length ? d.memories.map((x) => `${x.name} ${x.words}×${x.width}`).join(", ") : "0"}
                </dd>
                <dt>Clocks found</dt>
                <dd className="mono">{d.clocks.length ? d.clocks.map((c) => `${c.signal} (${c.edges.join("/")}, ${c.registers})`).join(", ") : "none"}</dd>
                <dt>Async resets</dt>
                <dd className="mono">{d.resets.length ? d.resets.map((r) => `${r.signal} (${r.active_high ? "high" : "low"}, ${r.registers})`).join(", ") : "none"}</dd>
                <dt>Undriven</dt>
                <dd className="mono">{d.undriven}</dd>
              </dl>
            ) : (
              <p className="muted">{info.elaborated ? "Not counted: the design does not lower." : "Not elaborated."}</p>
            )}
          </section>
          <section aria-label="Setup">
            <h4>Setup <span className="muted mono">{info.task}</span></h4>
            <dl className="kv">
              <dt>Clocks</dt>
              <dd className="mono">{list(info.declared_clocks)}</dd>
              <dt>Reset</dt>
              <dd className="mono">{info.declared_reset ?? "none"}</dd>
              <dt>Black boxes</dt>
              <dd className="mono">{list(info.black_boxes)}</dd>
              <dt>Cut points</dt>
              <dd className="mono">{list(info.cut_points)}</dd>
              <dt>Assertions</dt>
              <dd className="mono">{info.properties.asserts}</dd>
              <dt>Covers</dt>
              <dd className="mono">{info.properties.covers}</dd>
              <dt>Assumptions</dt>
              <dd className="mono" title="Enabled, those written in the RTL included">{info.properties.assumptions}</dd>
              <dt>Disabled</dt>
              <dd className="mono">{info.properties.disabled}</dd>
            </dl>
          </section>
          <section aria-label="Model">
            <h4>Model <span className="muted">after lowering</span></h4>
            {m ? (
              <dl className="kv">
                <dt>Btor2 nodes</dt>
                <dd className="mono">{m.nodes}</dd>
                <dt>States</dt>
                <dd className="mono">{bits(m.states, m.state_bits)}{m.array_states ? `, ${m.array_states} array${m.array_states === 1 ? "" : "s"}` : ""}</dd>
                <dt>Inputs</dt>
                <dd className="mono">{bits(m.inputs, m.input_bits)}</dd>
                <dt>Targets</dt>
                <dd className="mono" title="bad lines: assertions, covers and precondition covers; justice: liveness">{`${m.bad} bad, ${m.justice} justice`}</dd>
                <dt>Constraints</dt>
                <dd className="mono" title="Assumptions and the reset">{m.constraints}</dd>
                <dt>AIG ands</dt>
                <dd className="mono" title={m.aig_error}>{m.aig ? m.aig.ands : "not built"}</dd>
                <dt>AIG latches</dt>
                <dd className="mono">{m.aig ? m.aig.latches : "—"}</dd>
              </dl>
            ) : (
              <p className="muted">{info.model_error ? `Not built: ${info.model_error}` : "Not built."}</p>
            )}
          </section>
        </div>
      )}
    </div>
  );
}
