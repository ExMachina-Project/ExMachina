/**
 * 管理页交互反馈基线（ui-visual-language delta：管理页交互态基线）。
 *
 * 约定：写操作成功 = 轻量 toast（2~3s 自灭）；失败 = 明确错误文案（不吞错）；
 * 后端错误统一 `{ error: string }` 形态，这里集中提取——各页不再手写 String(e)。
 * 危险确认（modal.confirm 含目标名）与批量计数由各页自行组合，本模块只管 toast 出口。
 */
import { message } from "antd";

/** 从任意抛出值提取后端错误文案（fetch Response / Error / 字符串 / {error} 对象） */
export function errText(e: unknown, fallback: string): string {
  if (e instanceof Error) return e.message || fallback;
  if (typeof e === "string") return e || fallback;
  if (e && typeof e === "object") {
    const err = (e as { error?: unknown }).error;
    if (typeof err === "string" && err) return err;
    const msg = (e as { message?: unknown }).message;
    if (typeof msg === "string" && msg) return msg;
  }
  return fallback;
}

/** 写操作成功提示（轻量，2.5s 自灭） */
export function toastOk(content: string): void {
  message.success(content, 2.5);
}

/** 失败提示：自动提取后端 error 字段，绝不静默 */
export function toastErr(e: unknown, fallback: string): void {
  message.error(errText(e, fallback), 4);
}
