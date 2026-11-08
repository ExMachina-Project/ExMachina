/** 二级侧边栏：页面把自己的侧栏（会话 / 聊天流 / 任务会话）portal 到壳层插槽。
 *  窄屏仍由页面自己在页内渲染（保留原有列表页/会话页切换），不走这里。 */
import { createContext, useContext } from "react";

/** 插槽 DOM；null = 二级侧边栏未展开（桌面端此时不渲染页面侧栏） */
export const SubbarSlotContext = createContext<HTMLElement | null>(null);

export function useSubbarSlot() {
  return useContext(SubbarSlotContext);
}
