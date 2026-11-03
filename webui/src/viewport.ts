/** visualViewport 高度兜底（移动端键盘遮挡，long-horizon-iteration 4.6）。
 *
 *  iOS Safari 软键盘弹出时 100dvh 不更新（dvh 只响应地址栏收张），
 *  键盘会直接盖住底部输入区；而 visualViewport.height 会实时缩小。
 *  这里把可视高写入 --vvh，壳层容器用 min(100dvh, var(--vvh, 100dvh)) 取小者，
 *  键盘弹出即收缩可视壳。桌面端 vvh ≈ dvh，无副作用。
 *  index.html 的 viewport 已带 interactive-widget=resizes-content（安卓 Chrome
 *  原生 resize），此兜底主要覆盖 iOS Safari / 部分内嵌 WebView。
 */
export function installViewportFix(): void {
  if (typeof window === "undefined") return;
  const vv = window.visualViewport;
  if (!vv) return;
  const apply = () => {
    const px = `${Math.round(vv.height)}px`;
    document.documentElement.style.setProperty("--vvh", px);
  };
  vv.addEventListener("resize", apply);
  vv.addEventListener("scroll", apply);
  apply();
}
