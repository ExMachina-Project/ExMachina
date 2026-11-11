//! hooks/useAttachments.ts —— 附件输入（codebase-refactor 7.2 拆分）：
//! 图片压缩入列（Canvas ≤1280px / JPEG 82%）、删除、清空。纯搬移。

import { useCallback, useState } from "react";

export function useAttachments() {
  const [images, setImages] = useState<string[]>([]);


  // 附件压缩：Canvas 缩到 ≤1280px、JPEG 82%（控制多模态请求体尺寸）
  const addImage = (file: File) => {
    const reader = new FileReader();
    reader.onload = () => {
      const img = new Image();
      img.onload = () => {
        const scale = Math.min(1, 1280 / Math.max(img.width, img.height));
        const canvas = document.createElement("canvas");
        canvas.width = Math.round(img.width * scale);
        canvas.height = Math.round(img.height * scale);
        canvas.getContext("2d")?.drawImage(img, 0, 0, canvas.width, canvas.height);
        const dataUrl = canvas.toDataURL("image/jpeg", 0.82);
        setImages((prev) => [...prev, dataUrl].slice(0, 4));
      };
      img.src = String(reader.result);
    };
    reader.readAsDataURL(file);
  };

  const removeImage = useCallback((i: number) => {
    setImages((prev) => prev.filter((_, j) => j !== i));
  }, []);

  const clearImages = useCallback(() => setImages([]), []);

  return { images, addImage, removeImage, clearImages, setImages };
}
