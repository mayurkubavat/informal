import type { SessionView } from "../../backend";

export function CsrConnTab({
  session,
  onSelectTask,
}: {
  session: SessionView | null;
  onSelectTask: (task: string) => void;
}) {
  const connProps = (session?.props ?? []).filter((p) => p.task === "Connectivity");
  const csrProps = (session?.props ?? []).filter((p) => p.task === "CSR");

  return (
    <div>
      <div className="apps-bar">
        <span>
          CSR assertions: <strong>{csrProps.length}</strong> · Connectivity assertions:{" "}
          <strong>{connProps.length}</strong>
        </span>
        <span className="spacer" />
        {csrProps.length > 0 && (
          <button className="btn small" onClick={() => onSelectTask("CSR")}>
            Show CSR Task
          </button>
        )}
        {connProps.length > 0 && (
          <button className="btn small" onClick={() => onSelectTask("Connectivity")}>
            Show Connectivity Task
          </button>
        )}
      </div>
      <p className="empty small">
        Load specs via `check_csr -load regs.hjson -prove` (HJSON, CSV, or UVM `uvm_reg_block` `.sv`)
        or `check_conn -load map.csv -prove`.
      </p>
    </div>
  );
}
