import { useMemo, useState } from "react";
import type { LpViolationView, SessionView } from "../../backend";
import { Glyph } from "../Glyph";

type StatusFilter = "all" | "open" | "cex" | "formally_waived" | "waived";

function glyphForLpStatus(st: string): string {
  if (st === "formally_waived") return "proven";
  if (st === "cex") return "cex";
  if (st === "waived") return "covered";
  return "undetermined";
}

function crossingLabel(v: LpViolationView): string {
  if (v.domain_src && v.domain_dst) return `${v.domain_src} → ${v.domain_dst}`;
  return v.domain_src ?? v.domain_dst ?? "—";
}

export function LpTab({
  session,
  idle,
  onRunTcl,
  onSelectTask,
}: {
  session: SessionView | null;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
  onSelectTask: (task: string) => void;
}) {
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const lp = session?.lp;
  const upfFiles = lp?.upf_files ?? [];
  const violations = useMemo(() => lp?.violations ?? [], [lp]);
  const stagesRun = lp?.stages_run ?? [];
  const lpProps = (session?.props ?? []).filter((p) => p.task === "LowPower");

  const counts = useMemo(() => {
    let open = 0;
    let cex = 0;
    let formallyWaived = 0;
    let waived = 0;
    for (const v of violations) {
      if (v.status === "open") open++;
      else if (v.status === "cex") cex++;
      else if (v.status === "formally_waived") formallyWaived++;
      else if (v.status === "waived") waived++;
    }
    return { open, cex, formallyWaived, waived };
  }, [violations]);

  const filtered = useMemo(
    () => (statusFilter === "all" ? violations : violations.filter((v) => v.status === statusFilter)),
    [violations, statusFilter],
  );

  const hasUpf = upfFiles.length > 0;

  return (
    <div>
      <div className="apps-bar">
        <span>
          Low Power (IEEE 1801 UPF / LPV):{" "}
          {hasUpf ? (
            <>
              <strong className="mono">{upfFiles.map((u) => u.path.split("/").pop()).join(", ")}</strong>
              {" · "}
              {lp?.pst_consistent === true
                ? "PST consistent"
                : lp?.pst_consistent === false
                  ? "PST conflict"
                  : "PST pending"}
              {stagesRun.length > 0 ? ` · stages: ${stagesRun.join(",")}` : ""}
            </>
          ) : (
            "No UPF/CPF loaded"
          )}
        </span>
        <span className="spacer" />
        <button
          className="btn small"
          disabled={!idle || !hasUpf}
          onClick={() => onRunTcl("check_lp -stage rtl")}
        >
          Check Static LP
        </button>
        <button
          className="btn small"
          disabled={!idle || !hasUpf || !session?.elaborated}
          onClick={() => onRunTcl("check_lp -stage all -prove_corruption")}
        >
          Prove Formal LPV
        </button>
        <button
          className="btn small"
          disabled={!idle || !hasUpf || !session?.elaborated}
          onClick={() => onRunTcl("check_lp -stage all -prove_waivers")}
        >
          Certify Waivers
        </button>
        {lpProps.length > 0 && (
          <button className="btn small" onClick={() => onSelectTask("LowPower")}>
            LowPower Task ({lpProps.length})
          </button>
        )}
      </div>

      {violations.length > 0 && (
        <div className="apps-bar lp-filter-bar">
          {(
            [
              ["all", `All (${violations.length})`],
              ["open", `Open (${counts.open})`],
              ["cex", `CEX (${counts.cex})`],
              ["formally_waived", `Certified Waived (${counts.formallyWaived})`],
              ["waived", `Manual Waived (${counts.waived})`],
            ] as [StatusFilter, string][]
          ).map(([key, label]) => (
            <button
              key={key}
              className={`btn small ${statusFilter === key ? "active" : ""}`}
              onClick={() => setStatusFilter(key)}
            >
              {label}
            </button>
          ))}
        </div>
      )}

      {!hasUpf ? (
        <p className="empty small">
          Load a UPF/CPF file via `load_upf design.upf` in the Tcl console (or MCP `load_upf`), then run `check_lp`.
        </p>
      ) : filtered.length === 0 ? (
        <p className="empty small">
          {violations.length === 0
            ? "No low-power violations recorded. Click Check Static LP or Prove Formal LPV to audit power intent."
            : `No violations matching status filter "${statusFilter}".`}
        </p>
      ) : (
        <table className="apps-table">
          <thead>
            <tr>
              <th>ID</th>
              <th>Rule</th>
              <th>Stage</th>
              <th>Crossing</th>
              <th>Signal</th>
              <th>Status</th>
              <th>Detail</th>
            </tr>
          </thead>
          <tbody>
            {filtered.slice(0, 150).map((v) => (
              <tr key={v.id}>
                <td className="mono muted">#{v.id}</td>
                <td className="mono">{v.rule_id}</td>
                <td>{v.stage}</td>
                <td className="mono">{crossingLabel(v)}</td>
                <td className="mono">
                  {v.signal}
                  {v.width_bits > 1 ? ` [${v.width_bits}b]` : ""}
                </td>
                <td>
                  <span
                    className={`status lp-status-${v.status}`}
                    title={
                      v.auto_formal_meta
                        ? `Certified by ${v.auto_formal_meta.engine} in ${v.auto_formal_meta.ms}ms`
                        : undefined
                    }
                  >
                    <Glyph k={glyphForLpStatus(v.status)} />
                    {v.status}
                  </span>
                </td>
                <td className="small muted">{v.message}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
