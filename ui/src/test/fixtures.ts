import type { PropStatus, Property } from "../backend";

export function prop(name: string, over: Partial<Property> = {}, status: PropStatus = { status: "unprocessed" }): Property {
  return { name, kind: "assert", expr: "a |-> b", enabled: true, status, ...over };
}
