import { useState } from "react";
import type { SessionView } from "../../backend";
import { CovTab } from "./CovTab";
import { CsrConnTab } from "./CsrConnTab";
import { ProtocolTab } from "./ProtocolTab";
import { SecTab } from "./SecTab";
import { SpvTab } from "./SpvTab";
import { XpropTab } from "./XpropTab";

export type AppMode = "cov" | "xprop" | "spv" | "sec" | "csr" | "protocol";

export function normalizeStatus(
  st: string | { cex?: { depth?: number | null } } | undefined,
): string {
  if (!st) return "unprocessed";
  if (typeof st === "string") return st;
  if ("cex" in st) return "cex";
  return "unprocessed";
}

export function FormalApps({
  session,
  mode,
  onMode,
  idle,
  onRunTcl,
  onSelectTask,
}: {
  session: SessionView | null;
  mode: AppMode;
  onMode: (m: AppMode) => void;
  idle: boolean;
  onRunTcl: (cmd: string) => void;
  onSelectTask: (task: string) => void;
}) {
  const [spvFrom, setSpvFrom] = useState("");
  const [spvTo, setSpvTo] = useState("");
  const [protoKind, setProtoKind] = useState<"tlul" | "axi4lite" | "apb">("apb");
  const [protoRole, setProtoRole] = useState<"slave" | "master" | "monitor">("slave");

  const covItems = session?.cov?.items ?? [];
  const xpropItems = session?.xprop?.items ?? [];
  const spvRules = session?.spv ?? [];
  const secChecks = session?.sec?.checks ?? [];
  const connProps = (session?.props ?? []).filter((p) => p.task === "Connectivity");
  const csrProps = (session?.props ?? []).filter((p) => p.task === "CSR");
  const protoProps = (session?.props ?? []).filter((p) => p.task === "Protocol");

  return (
    <div className="formal-apps" aria-label="Formal verification apps">
      <div className="apps-nav" role="tablist" aria-label="Formal app">
        {(
          [
            ["cov", `Coverage (${covItems.length})`],
            ["xprop", `X-Prop (${xpropItems.length})`],
            ["spv", `SPV (${spvRules.length})`],
            ["sec", `SEC / LEC (${secChecks.length})`],
            ["csr", `CSR / Conn (${csrProps.length + connProps.length})`],
            ["protocol", `Bus VIP (${protoProps.length})`],
          ] as [AppMode, string][]
        ).map(([k, label]) => (
          <button
            key={k}
            role="tab"
            aria-selected={mode === k}
            className={`apps-tab ${mode === k ? "on" : ""}`}
            onClick={() => onMode(k)}
          >
            {label}
          </button>
        ))}
      </div>

      <div className="apps-body">
        {mode === "cov" && <CovTab session={session} idle={idle} onRunTcl={onRunTcl} />}
        {mode === "xprop" && <XpropTab session={session} idle={idle} onRunTcl={onRunTcl} />}
        {mode === "spv" && (
          <SpvTab
            session={session}
            idle={idle}
            spvFrom={spvFrom}
            setSpvFrom={setSpvFrom}
            spvTo={spvTo}
            setSpvTo={setSpvTo}
            onRunTcl={onRunTcl}
          />
        )}
        {mode === "sec" && <SecTab session={session} idle={idle} onRunTcl={onRunTcl} />}
        {mode === "csr" && <CsrConnTab session={session} onSelectTask={onSelectTask} />}
        {mode === "protocol" && (
          <ProtocolTab
            session={session}
            idle={idle}
            protoKind={protoKind}
            setProtoKind={setProtoKind}
            protoRole={protoRole}
            setProtoRole={setProtoRole}
            onRunTcl={onRunTcl}
            onSelectTask={onSelectTask}
          />
        )}
      </div>
    </div>
  );
}
