/** 数据流列：十六进制字节雨 —— 登录门背景特效（自 d38806f 恢复）
 *
 *  纯白低透明、慢速平移、纯 transform 动效；行内容由 useMemo 固定，
 *  内外两份拷贝首尾相接做无缝循环（translateY(-50%)）。
 *  静态文本 + CSS 动画，无 canvas；reduced-motion 由全局降级规则覆盖。
 */
import React, { useMemo } from "react";

export function DataStream({ side }: { side: "left" | "right" }): React.ReactElement {
  const lines = useMemo(() => {
    const rows: string[] = [];
    for (let i = 0; i < 28; i++) {
      let s = "";
      for (let j = 0; j < 8; j++) {
        s += Math.floor(Math.random() * 256)
          .toString(16)
          .padStart(2, "0")
          .toUpperCase();
        if (j < 7) s += " "; // 分隔符只夹在中间：尾随空格会让右列可见墨迹比左列缩进一个字宽
      }
      rows.push(s);
    }
    return rows;
  }, []);
  return (
    <div className={`data-stream data-stream-${side}`} aria-hidden="true">
      <div className="data-stream-inner">
        {[0, 1].map((dup) => (
          <div key={dup}>
            {lines.map((l, i) => (
              <div key={i} className="data-line">
                {l}
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}
