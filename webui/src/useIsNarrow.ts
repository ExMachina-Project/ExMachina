/** 窄屏判定（≤860px，与 styles.css 的竖屏断点同口径）。
 *
 *  ChatView 的移动端是「列表页 / 会话页」两屏切换，需要在 JS 侧知道当前是否窄屏；
 *  放在组件里每次 matchMedia 会导致 SSR/测试环境无 window，这里做一次降级保护。
 */
import { useEffect, useState } from "react";

export const NARROW_QUERY = "(max-width: 860px)";

export function useIsNarrow(): boolean {
  const [narrow, setNarrow] = useState<boolean>(() =>
    typeof window === "undefined" ? false : window.matchMedia(NARROW_QUERY).matches,
  );
  useEffect(() => {
    const mq = window.matchMedia(NARROW_QUERY);
    const on = () => setNarrow(mq.matches);
    on();
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  }, []);
  return narrow;
}