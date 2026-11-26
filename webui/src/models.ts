/** English display label for built-in agent identifiers. */
import { tr } from "./i18n/core";

/** 个体 identifier 的英文标注（纯展示映射）：指挥体去 EXMACHINA- 冗余前缀 */
export function agentIdEn(identifier: string): string {
  if (identifier === "orchestrator") return tr("enAgent.orchestrator");
  return identifier.toUpperCase();
}
