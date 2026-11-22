import type { SessionView } from "../../backend";

export function ProtocolTab({
  session,
  idle,
  protoKind,
  setProtoKind,
  protoRole,
  setProtoRole,
  onRunTcl,
  onSelectTask,
}: {
  session: SessionView | null;
  idle: boolean;
  protoKind: "tlul" | "axi4lite" | "apb";
  setProtoKind: (v: "tlul" | "axi4lite" | "apb") => void;
  protoRole: "slave" | "master" | "monitor";
  setProtoRole: (v: "slave" | "master" | "monitor") => void;
  onRunTcl: (cmd: string) => void;
  onSelectTask: (task: string) => void;
}) {
  const protoProps = (session?.props ?? []).filter((p) => p.task === "Protocol");

  return (
    <div>
      <div className="apps-bar">
        <select
          aria-label="Bus protocol"
          value={protoKind}
          onChange={(e) => setProtoKind(e.target.value as "tlul" | "axi4lite" | "apb")}
        >
          <option value="apb">AMBA APB</option>
          <option value="axi4lite">AMBA AXI4-Lite</option>
          <option value="tlul">TileLink-UL</option>
        </select>
        <select
          aria-label="Bus role"
          value={protoRole}
          onChange={(e) => setProtoRole(e.target.value as "slave" | "master" | "monitor")}
        >
          <option value="slave">Slave DUT</option>
          <option value="master">Master DUT</option>
          <option value="monitor">Monitor</option>
        </select>
        <button
          className="btn small"
          disabled={!idle || !session?.elaborated}
          onClick={() =>
            onRunTcl(`check_protocol -create -protocol ${protoKind} -role ${protoRole}`)
          }
        >
          Generate VIP
        </button>
        <span className="spacer" />
        {protoProps.length > 0 && (
          <button className="btn small" onClick={() => onSelectTask("Protocol")}>
            Show Protocol Task ({protoProps.length})
          </button>
        )}
      </div>
      <p className="empty small">
        Generates formal handshake, payload-stability, opcode-legality, and transaction cover
        properties in task `Protocol`.
      </p>
    </div>
  );
}
