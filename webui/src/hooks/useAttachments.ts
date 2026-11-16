//! hooks/useAttachments.ts —— 附件输入（codebase-refactor 7.2 拆分）：
//! 会话内图片压缩（Canvas ≤1280px / JPEG 82%）、删除、清空。

import { useCallback, useEffect, useRef, useState } from "react";
import type { SetStateAction } from "react";

export function useAttachments(sessionId?: string | null) {
  const scope = sessionId ?? "";
  const [bySession, setBySession] = useState<Record<string, string[]>>({});
  const active = useRef(true);
  useEffect(() => {
    active.current = true;
    return () => { active.current = false; };
  }, []);
  // A callback captured before a session switch still updates its original session.
  const setImages = useCallback((action: SetStateAction<string[]>) => {
    if (!active.current) return;
    setBySession((current) => ({
      ...current,
      [scope]: typeof action === "function" ? action(current[scope] ?? []) : action,
    }));
  }, [scope]);
  const images = bySession[scope] ?? [];


  // 附件压缩：Canvas 缩到 ≤1280px、JPEG 82%（控制多模态请求体尺寸）
  const addImage = useCallback((file: File) => {
    if (!file.type.startsWith("image/")) return;
    const reader = new FileReader();
    reader.onload = () => {
      const img = new Image();
      img.onload = () => {
        const scale = Math.min(1, 1280 / Math.max(img.width, img.height));
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(img.width * scale);
        canvas.height = Math.round(img.height * scale);
        const context = canvas.getContext("2d");
        if (!context) return;
        context.drawImage(img, 0, 0, canvas.width, canvas.height);
        const dataUrl = canvas.toDataURL("image/jpeg", 0.82);
        setImages((prev) => [...prev, dataUrl].slice(0, 4));
      };
      img.src = String(reader.result);
    };
    reader.readAsDataURL(file);
  }, [setImages]);

  const removeImage = useCallback((i: number) => {
    setImages((prev) => prev.filter((_, j) => j !== i));
  }, [setImages]);

  const clearImages = useCallback(() => setImages([]), [setImages]);

  return { images, addImage, removeImage, clearImages, setImages };
}
