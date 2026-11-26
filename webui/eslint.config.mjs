import js from "@eslint/js";
import tseslint from "typescript-eslint";
import hooks from "eslint-plugin-react-hooks";

// 仅静态检查 Hooks 规则（顺序/条件调用），不做风格检查——
// 目的：拦截条件 Hooks 调用，避免组件状态顺序在运行时改变。
export default tseslint.config(
  { ignores: ["dist", "node_modules", "public"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["src/**/*.{ts,tsx}"],
    plugins: { "react-hooks": hooks },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",
      // 只查 Hooks 规则，TS 推荐集里与运行时无关的风格项关掉（tsc 已覆盖类型层）
      "@typescript-eslint/no-unused-vars": "warn",
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
);
