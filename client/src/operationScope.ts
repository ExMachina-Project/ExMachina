import { sameTarget, type OperationContextState, type OperationTarget } from "./operationContext";

export type ScopeSource = "bound" | "override" | "default" | "instance";
export type ScopeRequirement = "target" | "agent" | "group" | "instance";
export type ScopeIssue = "none" | "directory-unavailable" | "target-unavailable" | "member-required" | "group-required";
export interface OperationScope {
  target: OperationTarget | null;
  source: ScopeSource;
  available: boolean;
  issue: ScopeIssue | null;
}

/** Resolve defaults without mutating a resource, selecting a group primary, or granting access. */
export function resolveOperationScope(input: {
  requirement: ScopeRequirement;
  state: Pick<OperationContextState, "target" | "directory" | "status">;
  /** undefined = no resource binding; null = an unresolved binding (never use a default). */
  bound?: OperationTarget | null;
  /** undefined = follow default; null = explicitly cleared/all (never use a default). */
  override?: OperationTarget | null;
  allowNone?: boolean;
}): OperationScope {
  if (input.requirement === "instance") return { target: null, source: "instance", available: true, issue: null };
  const source: ScopeSource = input.bound !== undefined ? "bound" : input.override !== undefined ? "override" : "default";
  const target = source === "bound" ? input.bound! : source === "override" ? input.override! : input.state.target;
  const result = (issue: ScopeIssue | null): OperationScope => ({ target, source, available: issue === null, issue });
  if (!target) return result(input.allowNone && source !== "bound" ? null : "none");
  // Existing resources are inspected in their own scope, even if their directory
  // owner was deleted. Execution/writes still require server-side validation.
  if (source !== "bound") {
    if (input.state.status !== "ready") return result("directory-unavailable");
    if (!input.state.directory.some((entry) => sameTarget(entry, target))) return result("target-unavailable");
  }
  if (input.requirement === "agent" && target.kind === "group") return result("member-required");
  if (input.requirement === "group" && target.kind === "single") return result("group-required");
  return result(null);
}
