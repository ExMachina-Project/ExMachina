// channels 域文件：平台目录（表单字段/凭证判定/默认值，接新平台加一条即可；自 ChannelsView.tsx 拆出）。
import type { TKey } from "../../i18n/core";

/** 平台目录：表单字段、凭证判定、回复形态全部由目录驱动 —— 接新平台加一条即可 */
export interface ChanField {
  key: string;
  labelKey: TKey;
  secret?: boolean;
  required?: boolean;
  ph?: string;
  helpKey?: TKey;
  /** 存顶层字段（telegram.token / 桥接.secret）；缺省存 config.<key> */
  top?: "token" | "secret";
  /** 布尔开关（存 "true"/"false"） */
  toggle?: boolean;
  /** 新建/切换平台时的默认值（布尔开关 / 字符串） */
  default?: boolean | string;
  /** 下拉选项（存字符串） */
  options?: { value: string; labelKey: TKey }[];
  /** 多行文本 */
  multiline?: boolean;
  /** B 站扫码登录挂点：该字段渲染前插「扫码登录」按钮（回填本平台 Cookie 字段） */
  qrLogin?: boolean;
  /** QQ 官方机器人扫码绑定挂点：该字段渲染前插「扫码创建机器人」按钮（回填 AppID/AppSecret） */
  qqBind?: boolean;
}
export interface ChanPlat {
  key: string;
  color?: string;
  /** 表格里的平台标牌（品牌名，各语言一致） */
  tag?: string;
  tagKey?: TKey;
  labelKey: TKey;
  helpKey: TKey;
  fields: ChanField[];
  /** 回复形态：平台原生回信（适配器直接发回原会话）；缺省 = webhook 回调 */
  replyBuiltin?: boolean;
  /** 主动型平台（视频观看/评论/回复）：不依赖入站消息 */
  proactive?: boolean;
}

export const PLATFORMS: ChanPlat[] = [
  {
    key: "qqbot",
    color: undefined,
    tag: "QQ Bot",
    labelKey: "channels.plat.qqbot",
    helpKey: "channels.platHelp.qqbot",
    replyBuiltin: true,
    fields: [
      { key: "appId", labelKey: "channels.f.appId", required: true, ph: "AppID", helpKey: "channels.f.appIdHelp", qqBind: true },
      { key: "appSecret", labelKey: "channels.f.appSecret", secret: true, required: true, ph: "AppSecret", helpKey: "channels.f.appSecretHelp" },
      { key: "sandbox", labelKey: "channels.f.sandbox", toggle: true, helpKey: "channels.f.sandboxHelp" },
    ],
  },
  {
    key: "napcat",
    color: undefined,
    tag: "NapCat",
    labelKey: "channels.plat.napcat",
    helpKey: "channels.platHelp.napcat",
    replyBuiltin: true,
    fields: [
      { key: "url", labelKey: "channels.f.napUrl", required: true, ph: "ws://127.0.0.1:3001", helpKey: "channels.f.napUrlHelp" },
      { key: "token", labelKey: "channels.f.napToken", secret: true, helpKey: "channels.f.napTokenHelp" },
    ],
  },
  {
    key: "snowluma",
    color: undefined,
    tag: "SnowLuma",
    labelKey: "channels.plat.snowluma",
    helpKey: "channels.platHelp.snowluma",
    replyBuiltin: true,
    fields: [
      { key: "url", labelKey: "channels.f.napUrl", required: true, ph: "ws://127.0.0.1:3001", helpKey: "channels.f.napUrlHelp" },
      { key: "token", labelKey: "channels.f.napToken", secret: true, helpKey: "channels.f.napTokenHelp" },
    ],
  },
  {
    key: "telegram",
    color: undefined,
    tag: "Telegram",
    labelKey: "channels.plat.telegram",
    helpKey: "channels.platHelp.telegram",
    replyBuiltin: true,
    fields: [{ top: "token", key: "token", labelKey: "channels.f.token", secret: true, required: true, ph: "123456:ABC-DEF…" }],
  },
  {
    key: "discord",
    color: undefined,
    tag: "Discord",
    labelKey: "channels.plat.discord",
    helpKey: "channels.platHelp.discord",
    replyBuiltin: true,
    fields: [
      { top: "token", key: "token", labelKey: "channels.f.discordToken", secret: true, required: true, ph: "MTIz…（Bot Token）" },
    ],
  },
  {
    key: "slack",
    color: undefined,
    tag: "Slack",
    labelKey: "channels.plat.slack",
    helpKey: "channels.platHelp.slack",
    replyBuiltin: true,
    fields: [
      { top: "token", key: "token", labelKey: "channels.f.botToken", secret: true, required: true, ph: "xoxb-…" },
      { key: "appToken", labelKey: "channels.f.appToken", secret: true, required: true, ph: "xapp-…", helpKey: "channels.f.appTokenHelp" },
    ],
  },
  {
    key: "matrix",
    color: undefined,
    tag: "Matrix",
    labelKey: "channels.plat.matrix",
    helpKey: "channels.platHelp.matrix",
    replyBuiltin: true,
    fields: [
      { key: "homeserver", labelKey: "channels.f.homeserver", required: true, ph: "https://matrix.org", helpKey: "channels.f.homeserverHelp" },
      { top: "token", key: "token", labelKey: "channels.f.accessToken", secret: true, required: true },
    ],
  },
  {
    key: "bilibili",
    color: undefined,
    tag: "B站",
    labelKey: "channels.plat.bilibili",
    helpKey: "channels.platHelp.bilibili",
    proactive: true,
    fields: [
      { key: "sessdata", labelKey: "channels.f.biliSessdata", secret: true, required: true, helpKey: "channels.f.biliSessdataHelp", qrLogin: true },
      { key: "biliJct", labelKey: "channels.f.biliJct", secret: true, required: true, helpKey: "channels.f.biliJctHelp" },
      { key: "uid", labelKey: "channels.f.biliUid", helpKey: "channels.f.biliUidHelp" },
      { key: "watch", labelKey: "channels.f.watch", toggle: true, default: true, helpKey: "channels.f.watchHelp" },
      { key: "comment", labelKey: "channels.f.comment", toggle: true, default: false, helpKey: "channels.f.commentHelp" },
      { key: "reply", labelKey: "channels.f.reply", toggle: true, default: false, helpKey: "channels.f.replyHelp" },
      { key: "dryRun", labelKey: "channels.f.dryRun", toggle: true, default: true, helpKey: "channels.f.dryRunHelp" },
      {
        key: "source",
        labelKey: "channels.f.videoSource",
        default: "popular",
        options: [
          { value: "popular", labelKey: "channels.f.videoSourcePopular" },
          { value: "search", labelKey: "channels.f.videoSourceSearch" },
        ],
        helpKey: "channels.f.videoSourceHelp",
      },
      { key: "keywords", labelKey: "channels.f.keywords", helpKey: "channels.f.keywordsHelp" },
      { key: "interests", labelKey: "channels.f.interests", multiline: true, ph: "channels.f.interestsPh", helpKey: "channels.f.interestsHelp" },
      { key: "commentStyle", labelKey: "channels.f.commentStyle", multiline: true, helpKey: "channels.f.commentStyleHelp" },
      { key: "intervalMinutes", labelKey: "channels.f.intervalMinutes", default: "10", helpKey: "channels.f.intervalMinutesHelp" },
      { key: "maxActionsPerRun", labelKey: "channels.f.maxActionsPerRun", default: "1", helpKey: "channels.f.maxActionsPerRunHelp" },
      { key: "dailyLimit", labelKey: "channels.f.dailyLimit", default: "6", helpKey: "channels.f.dailyLimitHelp" },
      {
        key: "approval",
        labelKey: "channels.f.approval",
        default: "off",
        options: [
          { value: "off", labelKey: "channels.f.approvalOff" },
          { value: "always", labelKey: "channels.f.approvalAlways" },
        ],
        helpKey: "channels.f.approvalHelp",
      },
      {
        key: "actRole",
        labelKey: "channels.f.actRole",
        default: "admin",
        options: [
          { value: "admin", labelKey: "channels.f.actRoleAdmin" },
          { value: "member", labelKey: "channels.f.actRoleMember" },
        ],
        helpKey: "channels.f.actRoleHelp",
      },
      { key: "maxWatchSteps", labelKey: "channels.f.maxWatchSteps", default: "8", helpKey: "channels.f.maxWatchStepsHelp" },
      { key: "maxCommentChars", labelKey: "channels.f.maxCommentChars", default: "120", helpKey: "channels.f.maxCommentCharsHelp" },
    ],
  },
  {
    key: "youtube",
    color: "red",
    tag: "YouTube",
    labelKey: "channels.plat.youtube",
    helpKey: "channels.platHelp.youtube",
    proactive: true,
    fields: [
      { key: "apiKey", labelKey: "channels.f.ytApiKey", secret: true, required: true, helpKey: "channels.f.ytApiKeyHelp" },
      { key: "accessToken", labelKey: "channels.f.ytAccessToken", secret: true, helpKey: "channels.f.ytAccessTokenHelp" },
      { key: "region", labelKey: "channels.f.ytRegion", default: "US", helpKey: "channels.f.ytRegionHelp" },
      { key: "watch", labelKey: "channels.f.watch", toggle: true, default: true, helpKey: "channels.f.watchHelp" },
      { key: "comment", labelKey: "channels.f.comment", toggle: true, default: false, helpKey: "channels.f.commentHelp" },
      { key: "reply", labelKey: "channels.f.reply", toggle: true, default: false, helpKey: "channels.f.replyHelp" },
      { key: "dryRun", labelKey: "channels.f.dryRun", toggle: true, default: true, helpKey: "channels.f.dryRunHelp" },
      {
        key: "source",
        labelKey: "channels.f.videoSource",
        default: "popular",
        options: [
          { value: "popular", labelKey: "channels.f.videoSourcePopular" },
          { value: "search", labelKey: "channels.f.videoSourceSearch" },
        ],
        helpKey: "channels.f.videoSourceHelp",
      },
      { key: "keywords", labelKey: "channels.f.keywords", helpKey: "channels.f.keywordsHelp" },
      { key: "interests", labelKey: "channels.f.interests", multiline: true, ph: "channels.f.interestsPh", helpKey: "channels.f.interestsHelp" },
      { key: "commentStyle", labelKey: "channels.f.commentStyle", multiline: true, helpKey: "channels.f.commentStyleHelp" },
      { key: "intervalMinutes", labelKey: "channels.f.intervalMinutes", default: "10", helpKey: "channels.f.intervalMinutesHelp" },
      { key: "maxActionsPerRun", labelKey: "channels.f.maxActionsPerRun", default: "1", helpKey: "channels.f.maxActionsPerRunHelp" },
      { key: "dailyLimit", labelKey: "channels.f.dailyLimit", default: "6", helpKey: "channels.f.dailyLimitHelp" },
      {
        key: "approval",
        labelKey: "channels.f.approval",
        default: "off",
        options: [
          { value: "off", labelKey: "channels.f.approvalOff" },
          { value: "always", labelKey: "channels.f.approvalAlways" },
        ],
        helpKey: "channels.f.approvalHelp",
      },
      {
        key: "actRole",
        labelKey: "channels.f.actRole",
        default: "admin",
        options: [
          { value: "admin", labelKey: "channels.f.actRoleAdmin" },
          { value: "member", labelKey: "channels.f.actRoleMember" },
        ],
        helpKey: "channels.f.actRoleHelp",
      },
      { key: "maxWatchSteps", labelKey: "channels.f.maxWatchSteps", default: "8", helpKey: "channels.f.maxWatchStepsHelp" },
      { key: "maxCommentChars", labelKey: "channels.f.maxCommentChars", default: "120", helpKey: "channels.f.maxCommentCharsHelp" },
    ],
  },
  {
    key: "rss",
    color: undefined,
    tag: "RSS",
    labelKey: "channels.plat.rss",
    helpKey: "channels.platHelp.rss",
    proactive: true,
    fields: [
      { key: "feeds", labelKey: "channels.f.rssFeeds", multiline: true, required: true, ph: "channels.f.rssFeedsPh", helpKey: "channels.f.rssFeedsHelp" },
      { key: "intervalMinutes", labelKey: "channels.f.rssInterval", default: "30", helpKey: "channels.f.rssIntervalHelp" },
      { key: "maxPerRound", labelKey: "channels.f.rssMaxPerRound", default: "10", helpKey: "channels.f.rssMaxPerRoundHelp" },
      { key: "push", labelKey: "channels.f.rssPush", helpKey: "channels.f.rssPushHelp" },
    ],
  },
  {
    key: "webhook",
    color: undefined,
    tag: "Webhook",
    labelKey: "channels.plat.webhook",
    helpKey: "channels.platHelp.webhook",
    fields: [{ top: "secret", key: "secret", labelKey: "channels.f.secret", secret: true, ph: "channels.f.secretPh" }],
  },
  {
    key: "qq",
    color: undefined,
    tagKey: "channels.tag.qq",
    labelKey: "channels.plat.qq",
    helpKey: "channels.platHelp.qq",
    fields: [{ top: "secret", key: "secret", labelKey: "channels.f.secret", secret: true, ph: "channels.f.secretPh" }],
  },
  {
    key: "wechat",
    color: "green",
    tagKey: "channels.tag.wechat",
    labelKey: "channels.plat.wechat",
    helpKey: "channels.platHelp.wechat",
    fields: [{ top: "secret", key: "secret", labelKey: "channels.f.secret", secret: true, ph: "channels.f.secretPh" }],
  },
];

/** webhook 桥接语义的平台：入站靠外部桥 POST，出站靠回调 */
export const BRIDGE_TYPES = ["webhook", "qq", "wechat"];

/** 平台目录声明的默认值 → 表单 config 预填（新建 / 切换平台时使用） */
export function platformDefaults(kind: string): Record<string, unknown> {
  const cfg: Record<string, unknown> = {};
  const plat = PLATFORMS.find((p) => p.key === kind);
  for (const f of plat?.fields ?? []) {
    if (f.default === undefined) {
      if (f.toggle) cfg[f.key] = false;
    } else {
      cfg[f.key] = f.default;
    }
  }
  return cfg;
}

