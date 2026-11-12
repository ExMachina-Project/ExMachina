// settings 域文件：设置 schema 元数据（分区/字段 i18n/枚举覆盖/读取器；自 SettingsView.tsx 拆出）。
import type React from "react";
import type { GatewayConfig } from "../../api";
import type { TKey } from "../../i18n/core";
import { BgColorsOutlined, CloudServerOutlined, DatabaseOutlined, DeleteOutlined, DesktopOutlined, FieldTimeOutlined, GlobalOutlined, IdcardOutlined, NotificationOutlined, PieChartOutlined, RadarChartOutlined, ReloadOutlined, RobotOutlined, SafetyCertificateOutlined, SafetyOutlined, SettingOutlined, ThunderboltOutlined, ToolOutlined, CloudDownloadOutlined } from "@ant-design/icons";


export const SECTIONS: Record<string, { icon: React.ReactNode; titleKey: TKey; descKey: TKey }> = {
  runtime: { icon: <SettingOutlined />, titleKey: "sec.runtime", descKey: "sec.runtime.desc" },
  memory: { icon: <DatabaseOutlined />, titleKey: "sec.memory", descKey: "sec.memory.desc" },
  security: { icon: <SafetyCertificateOutlined />, titleKey: "sec.security", descKey: "sec.security.desc" },
  automation: { icon: <FieldTimeOutlined />, titleKey: "sec.automation", descKey: "sec.automation.desc" },
  search: { icon: <GlobalOutlined />, titleKey: "sec.search", descKey: "sec.search.desc" },
  sandbox: { icon: <SafetyOutlined />, titleKey: "sec.sandbox", descKey: "sec.sandbox.desc" },
  browser: { icon: <RobotOutlined />, titleKey: "sec.browser", descKey: "sec.browser.desc" },
  hooks: { icon: <ThunderboltOutlined />, titleKey: "sec.hooks", descKey: "sec.hooks.desc" },
  computerUse: { icon: <DesktopOutlined />, titleKey: "sec.computer", descKey: "sec.computer.desc" },
  identity: { icon: <IdcardOutlined />, titleKey: "sec.identity", descKey: "sec.identity.desc" },
  limits: { icon: <PieChartOutlined />, titleKey: "sec.limits", descKey: "sec.limits.desc" },
  triggers: { icon: <RadarChartOutlined />, titleKey: "sec.triggers", descKey: "sec.triggers.desc" },
  mcpServe: { icon: <CloudServerOutlined />, titleKey: "sec.mcpServe", descKey: "sec.mcpServe.desc" },
  notify: { icon: <NotificationOutlined />, titleKey: "sec.notify", descKey: "sec.notify.desc" },
  experiments: { icon: <ThunderboltOutlined />, titleKey: "sec.experiments", descKey: "sec.experiments.desc" },
  tools: { icon: <ToolOutlined />, titleKey: "sec.tools", descKey: "sec.tools.desc" },
  trash: { icon: <DeleteOutlined />, titleKey: "sec.trash", descKey: "sec.trash.desc" },
  ui: { icon: <BgColorsOutlined />, titleKey: "sec.ui", descKey: "sec.ui.desc" },
  version: { icon: <CloudDownloadOutlined />, titleKey: "sec.version", descKey: "sec.version.desc" },
};

/**
 * 后端 config_schema() 下发的 label/help 是中文常量，前端在此按字段 key 覆盖为 i18n 词条；
 * 未收录的字段回退后端原文（新增配置项零 UI 改动的契约不被破坏）。
 */
export const FIELD_I18N: Record<string, { label: TKey; help?: TKey }> = {
  "maxConcurrency": { label: "cfg.maxConcurrency", help: "cfg.maxConcurrency.help" },
  "maxSessionTokens": { label: "cfg.maxSessionTokens", help: "cfg.maxSessionTokens.help" },
  "compaction.triggerRatio": { label: "cfg.compaction.triggerRatio", help: "cfg.compaction.triggerRatio.help" },
  "memory.enabled": { label: "cfg.memoryEnabled", help: "cfg.memoryEnabled.help" },
  "memory.mdMaxChars": { label: "cfg.mdMaxChars", help: "cfg.mdMaxChars.help" },
  "memory.recallLimit": { label: "cfg.recallLimit", help: "cfg.recallLimit.help" },
  "memory.halfLifeDays": { label: "cfg.halfLifeDays", help: "cfg.halfLifeDays.help" },
  "security.execApproval": { label: "cfg.execApproval", help: "cfg.execApproval.help" },
  "security.execAllowlist": { label: "cfg.execAllowlist", help: "cfg.execAllowlist.help" },
  "security.sanitizeRules.fakeSystemPrompt": { label: "cfg.sanitizeFakeSystemPrompt", help: "cfg.sanitizeFakeSystemPrompt.help" },
  "security.sanitizeRules.hijackDirective": { label: "cfg.sanitizeHijackDirective", help: "cfg.sanitizeHijackDirective.help" },
  "security.sanitizeRules.fakeToolResult": { label: "cfg.sanitizeFakeToolResult", help: "cfg.sanitizeFakeToolResult.help" },
  "security.sanitizeRules.hiddenUnicode": { label: "cfg.sanitizeHiddenUnicode", help: "cfg.sanitizeHiddenUnicode.help" },
  "security.authKey": { label: "cfg.authKey", help: "cfg.authKey.help" },
  "security.execUser": { label: "cfg.execUser", help: "cfg.execUser.help" },
  "security.execHome": { label: "cfg.execHome", help: "cfg.execHome.help" },
  "automation.heartbeatEnabled": { label: "cfg.heartbeatEnabled", help: "cfg.heartbeatEnabled.help" },
  "automation.heartbeatIntervalMinutes": { label: "cfg.heartbeatInterval", help: "cfg.heartbeatInterval.help" },
  "automation.heartbeatPrompt": { label: "cfg.heartbeatPrompt", help: "cfg.heartbeatPrompt.help" },
  "automation.autoAdapt": { label: "cfg.autoAdapt", help: "cfg.autoAdapt.help" },
  "automation.unitMaxSteps": { label: "cfg.unitMaxSteps", help: "cfg.unitMaxSteps.help" },
  "automation.timezone": { label: "cfg.automationTimezone", help: "cfg.automationTimezone.help" },
  "automation.heartbeatAgent": { label: "cfg.heartbeatAgent", help: "cfg.heartbeatAgent.help" },
  "security.terminalTimeoutSecs": { label: "cfg.terminalTimeout", help: "cfg.terminalTimeout.help" },
  "security.toolOutputSpillChars": { label: "cfg.spillChars", help: "cfg.spillChars.help" },
  "security.approvalWaitSecs": { label: "cfg.approvalWait", help: "cfg.approvalWait.help" },
  "computer.enabled": { label: "cfg.computerEnabled", help: "cfg.computerEnabled.help" },
  "computer.approval": { label: "cfg.computerApproval", help: "cfg.computerApproval.help" },
  "computer.maxEdge": { label: "cfg.computerMaxEdge", help: "cfg.computerMaxEdge.help" },
  "computer.actionIntervalMs": { label: "cfg.computerInterval", help: "cfg.computerInterval.help" },
  "search.provider": { label: "cfg.searchProvider", help: "cfg.searchProvider.help" },
  "search.endpoint": { label: "cfg.searchEndpoint", help: "cfg.searchEndpoint.help" },
  "search.apiKey": { label: "cfg.searchApiKey", help: "cfg.searchApiKey.help" },
  "search.maxResults": { label: "cfg.searchMax", help: "cfg.searchMax.help" },
  "search.contentFetchN": { label: "cfg.searchContentFetchN", help: "cfg.searchContentFetchN.help" },
  "sandbox.mode": { label: "cfg.sandboxMode", help: "cfg.sandboxMode.help" },
  "sandbox.allowNetwork": { label: "cfg.sandboxNet", help: "cfg.sandboxNet.help" },
  "sandbox.useBwrap": { label: "cfg.sandboxBwrap", help: "cfg.sandboxBwrap.help" },
  "sandbox.memoryMb": { label: "cfg.sandboxMem", help: "cfg.sandboxMem.help" },
  "sandbox.maxProcesses": { label: "cfg.sandboxProcs", help: "cfg.sandboxProcs.help" },
  "browser.executable": { label: "cfg.browserExe", help: "cfg.browserExe.help" },
  "browser.headless": { label: "cfg.browserHeadless", help: "cfg.browserHeadless.help" },
  "browser.timeoutSecs": { label: "cfg.browserTimeout", help: "cfg.browserTimeout.help" },
  "browser.maxChars": { label: "cfg.browserMaxChars", help: "cfg.browserMaxChars.help" },
  "hooks.preTool": { label: "cfg.hookPre", help: "cfg.hookPre.help" },
  "hooks.postTool": { label: "cfg.hookPost", help: "cfg.hookPost.help" },
  "hooks.onRunEnd": { label: "cfg.hookRun", help: "cfg.hookRun.help" },
  "tools.custom": { label: "cfg.toolsCustom", help: "cfg.toolsCustom.help" },
  "identity.identityRequired": { label: "cfg.identityRequired", help: "cfg.identityRequired.help" },
  "identity.pairingTtlSecs": { label: "cfg.pairingTtl", help: "cfg.pairingTtl.help" },
  "identity.groupGateDefault": { label: "cfg.groupGateDefault", help: "cfg.groupGateDefault.help" },
  "limits.enabled": { label: "cfg.limitsEnabled", help: "cfg.limitsEnabled.help" },
  "limits.windowSecs": { label: "cfg.limitsWindow", help: "cfg.limitsWindow.help" },
  "limits.maxRequests": { label: "cfg.limitsMaxReq", help: "cfg.limitsMaxReq.help" },
  "limits.quotaPeriod": { label: "cfg.limitsQuotaPeriod", help: "cfg.limitsQuotaPeriod.help" },
  "limits.quotaTokens": { label: "cfg.limitsQuotaTokens", help: "cfg.limitsQuotaTokens.help" },
  "limits.quotaRequests": { label: "cfg.limitsQuotaRequests", help: "cfg.limitsQuotaRequests.help" },
  "limits.quotaRunningPolicy": { label: "cfg.limitsQuotaPolicy", help: "cfg.limitsQuotaPolicy.help" },
  "triggers.fileWatch.enabled": { label: "cfg.fileWatchEnabled", help: "cfg.fileWatchEnabled.help" },
  "triggers.fileWatch.debounceMs": { label: "cfg.fileWatchDebounce", help: "cfg.fileWatchDebounce.help" },
  "triggers.fileWatch.exclude": { label: "cfg.fileWatchExclude", help: "cfg.fileWatchExclude.help" },
  "triggers.eventWebhook.enabled": { label: "cfg.eventWebhookEnabled", help: "cfg.eventWebhookEnabled.help" },
  "triggers.eventWebhook.secret": { label: "cfg.eventWebhookSecret", help: "cfg.eventWebhookSecret.help" },
  "mcpServe.enabled": { label: "cfg.mcpEnabled", help: "cfg.mcpEnabled.help" },
  "mcpServe.httpPath": { label: "cfg.mcpPath", help: "cfg.mcpPath.help" },
  "mcpServe.allowedGroups": { label: "cfg.mcpGroups", help: "cfg.mcpGroups.help" },
  "mcpServe.allowedTools": { label: "cfg.mcpTools", help: "cfg.mcpTools.help" },
  "notify.enabled": { label: "cfg.notifyEnabled", help: "cfg.notifyEnabled.help" },
  "notify.maxChars": { label: "cfg.notifyMaxChars", help: "cfg.notifyMaxChars.help" },
};

/** 特定字段用选择器而非自由文本（显示名走 i18n，值是发给后端的枚举） */
export const ENUM_OVERRIDES: Record<string, { value: string; labelKey: TKey }[]> = {
  "security.execApproval": [
    { value: "off", labelKey: "cfg.approval.off" },
    { value: "risky", labelKey: "cfg.approval.risky" },
    { value: "always", labelKey: "cfg.approval.always" },
  ],
  "sandbox.mode": [
    { value: "off", labelKey: "cfg.sandbox.off" },
    { value: "workspace", labelKey: "cfg.sandbox.workspace" },
    { value: "strict", labelKey: "cfg.sandbox.strict" },
  ],
  "search.provider": [
    { value: "", labelKey: "cfg.search.none" },
    { value: "tavily", labelKey: "cfg.search.tavily" },
    { value: "brave", labelKey: "cfg.search.brave" },
    { value: "searxng", labelKey: "cfg.search.searxng" },
    { value: "custom", labelKey: "cfg.search.custom" },
  ],
  "limits.quotaPeriod": [
    { value: "daily", labelKey: "cfg.quota.daily" },
    { value: "weekly", labelKey: "cfg.quota.weekly" },
    { value: "monthly", labelKey: "cfg.quota.monthly" },
  ],
  "limits.quotaRunningPolicy": [
    { value: "finish", labelKey: "cfg.policy.finish" },
    { value: "interrupt", labelKey: "cfg.policy.interrupt" },
  ],
};

/** 钩子/列表类字段在 UI 里以「逗号分隔字符串」编辑，后端亦接受数组 */
export const listToText = (v: unknown): string => (Array.isArray(v) ? v.join(", ") : typeof v === "string" ? v : "");

export function readCurrent(cfg: GatewayConfig | null, key: string): unknown {
  if (!cfg) return undefined;
  switch (key) {
    case "maxConcurrency":
      return cfg.maxConcurrency;
    case "maxSessionTokens":
      return cfg.maxSessionTokens;
    case "memory.enabled":
      return cfg.memory.enabled;
    case "memory.recallLimit":
      return cfg.memory.recallLimit;
    case "memory.halfLifeDays":
      return cfg.memory.halfLifeDays;
    case "memory.mdMaxChars":
      return cfg.memory.mdMaxChars;
    case "tools.custom":
      return JSON.stringify(cfg.tools?.custom ?? [], null, 2);
    case "trash.retentionDays":
      return cfg.trash?.retentionDays;
    case "trash.maxBytes":
      return cfg.trash?.maxBytes;
    case "security.execApproval":
      return cfg.security.execApproval;
    case "security.execAllowlist":
      return cfg.security.execAllowlist;
    case "security.authKey":
      return cfg.security.authKey;
    case "security.execUser":
      return cfg.security.execUser ?? "";
    case "security.execHome":
      return cfg.security.execHome ?? "";
    case "automation.heartbeatEnabled":
      return cfg.automation.heartbeatEnabled;
    case "automation.heartbeatIntervalMinutes":
      return cfg.automation.heartbeatIntervalMinutes;
    case "automation.heartbeatPrompt":
      return cfg.automation.heartbeatPrompt;
    case "automation.autoAdapt":
      return cfg.automation.autoAdapt;
    case "automation.unitMaxSteps":
      return cfg.automation.unitMaxSteps;
    case "automation.heartbeatAgent":
      return cfg.automation.heartbeatAgent ?? "";
    case "security.terminalTimeoutSecs":
      return cfg.security.terminalTimeoutSecs;
    case "security.toolOutputSpillChars":
      return cfg.security.toolOutputSpillChars;
    case "security.approvalWaitSecs":
      return cfg.security.approvalWaitSecs;
    case "search.provider":
      return cfg.search?.provider;
    case "search.endpoint":
      return cfg.search?.endpoint;
    case "search.apiKey":
      return cfg.search?.apiKey;
    case "search.maxResults":
      return cfg.search?.maxResults;
    case "search.contentFetchN":
      return cfg.search?.contentFetchN;
    case "sandbox.mode":
      return cfg.sandbox?.mode;
    case "sandbox.allowNetwork":
      return cfg.sandbox?.allowNetwork;
    case "sandbox.useBwrap":
      return cfg.sandbox?.useBwrap;
    case "sandbox.memoryMb":
      return cfg.sandbox?.memoryMb;
    case "sandbox.maxProcesses":
      return cfg.sandbox?.maxProcesses;
    case "browser.executable":
      return cfg.browser?.executable;
    case "browser.headless":
      return cfg.browser?.headless;
    case "browser.timeoutSecs":
      return cfg.browser?.timeoutSecs;
    case "browser.maxChars":
      return cfg.browser?.maxChars;
    case "hooks.preTool":
      return listToText(cfg.hooks?.preTool);
    case "hooks.postTool":
      return listToText(cfg.hooks?.postTool);
    case "hooks.onRunEnd":
      return listToText(cfg.hooks?.onRunEnd);
    // —— 实验性功能（含挪组的配套参数：key 保持 automation.*，回显读 automation）——
    case "experiments.dynamicCompaction":
      return cfg.experiments?.dynamicCompaction;
    case "experiments.groupSharedContext":
      return cfg.experiments?.groupSharedContext;
    case "experiments.llmDrivenMemory":
      return cfg.experiments?.llmDrivenMemory;
    case "experiments.slidingContextWindow":
      return cfg.experiments?.slidingContextWindow;
    case "experiments.superAgent":
      return cfg.experiments?.superAgent;
    case "experiments.autoAgent":
      return cfg.experiments?.autoAgent;
    case "experiments.metaAgent":
      return cfg.experiments?.metaAgent;
    case "experiments.autoMemoryDigest":
      return cfg.experiments?.autoMemoryDigest;
    case "automation.autoAgentIntervalMinutes":
      return cfg.automation?.autoAgentIntervalMinutes;
    case "automation.autoAgentMaxSteps":
      return cfg.automation?.autoAgentMaxSteps;
    case "automation.autoAgentBackoffMax":
      return cfg.automation?.autoAgentBackoffMax;
    case "automation.metaIntervalMin":
      return cfg.automation?.metaIntervalMin;
    case "automation.memoryDigestMinTurns":
      return cfg.automation?.memoryDigestMinTurns;
    // —— 五组补链（此前 get 不回显：设置页永远显示 schema 默认值）——
    case "computer.enabled":
      return cfg.computer?.enabled;
    case "computer.approval":
      return cfg.computer?.approval;
    case "computer.maxEdge":
      return cfg.computer?.maxEdge;
    case "computer.actionIntervalMs":
      return cfg.computer?.actionIntervalMs;
    case "identity.identityRequired":
      return cfg.identity?.identityRequired;
    case "identity.pairingTtlSecs":
      return cfg.identity?.pairingTtlSecs;
    case "identity.pairingMaxFailures":
      return cfg.identity?.pairingMaxFailures;
    case "identity.pairingLockoutSecs":
      return cfg.identity?.pairingLockoutSecs;
    case "identity.groupGateDefault":
      return cfg.identity?.groupGateDefault;
    case "limits.enabled":
      return cfg.limits?.enabled;
    case "limits.windowSecs":
      return cfg.limits?.windowSecs;
    case "limits.maxRequests":
      return cfg.limits?.maxRequests;
    case "limits.quotaPeriod":
      return cfg.limits?.quotaPeriod;
    case "limits.quotaTokens":
      return cfg.limits?.quotaTokens;
    case "limits.quotaRequests":
      return cfg.limits?.quotaRequests;
    case "limits.quotaRunningPolicy":
      return cfg.limits?.quotaRunningPolicy;
    case "triggers.fileWatch.enabled":
      return cfg.triggers?.fileWatch?.enabled;
    case "triggers.fileWatch.debounceMs":
      return cfg.triggers?.fileWatch?.debounceMs;
    case "triggers.fileWatch.exclude":
      return listToText(cfg.triggers?.fileWatch?.exclude);
    case "triggers.eventWebhook.enabled":
      return cfg.triggers?.eventWebhook?.enabled;
    case "triggers.eventWebhook.secret":
      return cfg.triggers?.eventWebhook?.secret;
    case "mcpServe.enabled":
      return cfg.mcpServe?.enabled;
    case "mcpServe.httpPath":
      return cfg.mcpServe?.httpPath;
    case "mcpServe.allowedGroups":
      return listToText(cfg.mcpServe?.allowedGroups);
    case "mcpServe.allowedTools":
      return listToText(cfg.mcpServe?.allowedTools);
    case "notify.enabled":
      return cfg.notify?.enabled;
    case "notify.maxChars":
      return cfg.notify?.maxChars;
    default:
      return undefined;
  }
}

