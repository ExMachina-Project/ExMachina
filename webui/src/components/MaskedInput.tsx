import { Input } from "antd";
import type { InputProps } from "antd";
import { useEffect, useState } from "react";
import { KEY_MASK } from "../api";
import { useT } from "../i18n/core";

/**
 * 掩码哨兵输入框（敏感字段回显专用）。
 *
 * 后端对已配置的密钥回显哨兵串（KEY_MASK），提交契约是
 * 「回传掩码 = 沿用旧值 / 空串 = 清除 / 明文 = 新值」。
 * 把哨兵串直接塞进 value，用户看到的是一串「像自己输入过」的假文本；
 * 此组件只在呈现层隐藏哨兵：表单 store 的值不动（初始即掩码，未触碰
 * 提交即原样回传 = 沿用），掩码时渲染空框 + 灰色 placeholder（已配置），
 * 用户一旦键入，表单值即整体替换为明文（清空 = 走删除语义）。
 */
function useMaskedDisplay(value: unknown) {
  const t = useT();
  const strValue = typeof value === "string" ? value : "";
  const isMasked = strValue === KEY_MASK;
  const [display, setDisplay] = useState("");
  // 外部表单值变化时同步显示：掩码 → 空框；明文/空 → 原样
  // （切换编辑对象时上一轮的明文不得残留）
  useEffect(() => {
    setDisplay(isMasked ? "" : strValue);
  }, [strValue, isMasked]);
  return { t, isMasked, display };
}

export function MaskedInput(props: InputProps) {
  const { value, onChange, placeholder, ...rest } = props;
  const { t, isMasked, display } = useMaskedDisplay(value);
  return (
    <Input
      {...rest}
      value={isMasked ? display : value}
      placeholder={isMasked ? t("common.maskConfigured") : placeholder}
      onChange={onChange}
    />
  );
}

/** Password 形态：同语义，眼睛可见的也只是明文（哨兵永不露出） */
export function MaskedPasswordInput(props: InputProps) {
  const { value, onChange, placeholder, ...rest } = props;
  const { t, isMasked, display } = useMaskedDisplay(value);
  return (
    <Input.Password
      {...rest}
      value={isMasked ? display : value}
      placeholder={isMasked ? t("common.maskConfigured") : placeholder}
      onChange={onChange}
    />
  );
}
