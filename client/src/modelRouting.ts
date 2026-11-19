import type { GroupCapabilities, LlmProfile } from "./api";
import { tr, type TKey } from "./i18n/core";

export const MODEL_PURPOSES = ["chat", "planning", "coding", "review", "summary", "research", "vision", "speech", "transcribe", "embedding", "imageGen"] as const;
export type ModelPurpose = typeof MODEL_PURPOSES[number];
export type AssignmentMode = "inherit" | "fixed" | "auto" | "disabled";
export type ModelCost = "unknown" | "economy" | "balanced" | "premium";
export interface ModelAssignment { mode: AssignmentMode; model?: string; candidates?: string[] }
export interface ModelRouting { default?: ModelAssignment; purposes?: Partial<Record<ModelPurpose, ModelAssignment>>; instructions?: string }
export interface RoutingPreview { agentId: string; purpose: ModelPurpose; assignment: ModelAssignment; source: string; candidates: { reference: string }[] }
export function sourceLabel(source: string): string { return tr(`routing.source.${source}` as TKey); }
export const purposeLabel = (purpose: ModelPurpose): string => tr(`routing.purpose.${purpose}` as TKey);
export const costLabel = (cost: ModelCost = "unknown"): string => tr(`routing.cost.${cost}` as TKey);
export function initialRouting(routing?: ModelRouting | null, legacyModel?: string | null, legacyCapabilities?: GroupCapabilities | null): ModelRouting {
  if (routing) return structuredClone(routing);
  const purposes: ModelRouting["purposes"] = {};
  if (legacyCapabilities) {
    for (const [key, value] of Object.entries(legacyCapabilities)) {
      if (value) purposes[key === "visionRelay" ? "vision" : key as ModelPurpose] = { mode: "fixed", model: value };
    }
  }
  return { default: legacyModel ? { mode: "fixed", model: legacyModel } : { mode: "inherit" }, purposes, instructions: "" };
}
export function routingLabel(routing: ModelRouting | null | undefined, legacy?: string | null): string {
  const binding = routing?.default;
  if (binding?.mode === "auto") return tr("routing.auto");
  return binding?.mode === "fixed" ? binding.model || tr("routing.chooseModel") : (!routing && legacy) || tr("routing.inherit");
}
export function routingValidation(routing: ModelRouting): string | undefined {
  const entries = [[undefined, routing.default], ...Object.entries(routing.purposes || {})] as [ModelPurpose | undefined, ModelAssignment | undefined][];
  const missing = entries.find(([, assignment]) => assignment?.mode === "fixed" && !assignment.model?.trim());
  return missing ? tr("ux.modelRequired", { purpose: missing[0] ? purposeLabel(missing[0]) : tr("routing.default") }) : undefined;
}
export function routingOptions(profiles: LlmProfile[], purpose?: ModelPurpose) {
  return profiles.flatMap((profile) => profile.models.length ? profile.models.map((model) => ({
    value: `${profile.id}/${model.model}`,
    label: `${profile.name} / ${model.model} · ${costLabel(model.cost)}`,
    notes: model.usageNotes || "",
    eligible: !!model.autoEligible,
    suitable: !purpose || !!model.purposes?.includes(purpose) || (purpose === "vision" && model.vision)
      || (["chat", "planning", "coding", "review", "summary", "research"].includes(purpose) && (!model.purposes?.length || model.purposes.includes("chat"))),
  })) : [{value: profile.id, label: `${profile.name} / ${profile.model}`, notes: "", eligible: false, suitable: true}]);
}
