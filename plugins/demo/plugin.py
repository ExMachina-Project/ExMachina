#!/usr/bin/env python3
"""示例进程插件：ExMachina T2 形态最小实现（stdio 行分隔 JSON-RPC）。

协议面：initialize / tools/list / tools/call / config/set / watcher.tick / shutdown。
文档：docs/插件开发指南.md
"""
import json
import sys
import time

CREDS: dict[str, str] = {}  # 凭据仅内存（config/set 下发，绝不落盘）


def tools_list() -> list[dict]:
    return [
        {
            "name": "hello",
            "description": "问候（示例工具：回显 who 与插件持有凭据的有无）",
            "inputSchema": {
                "type": "object",
                "properties": {"who": {"type": "string", "description": "称呼"}},
                "required": [],
            },
        },
        {
            "name": "state_write",
            "description": "在插件数据目录写一个状态文件（验证文件系统边界：仅 data/ 内）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "value": {"type": "string"},
                },
                "required": ["key", "value"],
            },
        },
    ]


def handle_call(name: str, args: dict) -> dict:
    if name == "hello":
        who = args.get("who") or "world"
        cred_hint = "已配置" if CREDS.get("token") else "未配置"
        text = f"hello, {who}!（示例插件运行中；凭据 {cred_hint}）"
        return {"content": [{"type": "text", "text": text}]}
    if name == "state_write":
        key = str(args.get("key", ""))[:64]
        if not key or "/" in key or ".." in key:
            return {"content": [{"type": "text", "text": f"非法 key：{key!r}"}], "isError": True}
        # 插件数据目录（plugins/demo/data/）——由宿主沙箱口径保证
        import os

        data_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "data")
        os.makedirs(data_dir, exist_ok=True)
        path = os.path.join(data_dir, f"{key}.txt")
        with open(path, "w", encoding="utf-8") as f:
            f.write(str(args.get("value", "")))
        return {"content": [{"type": "text", "text": f"已写入 {key}.txt"}]}
    return {"content": [{"type": "text", "text": f"未知工具：{name}"}], "isError": True}


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        req = json.loads(line)
        method = req.get("method")
        rid = req.get("id")

        if method == "initialize":
            resp = {"result": {"serverInfo": {"name": "demo", "version": "0.1.0"}}}
        elif method == "tools/list":
            resp = {"result": {"tools": tools_list()}}
        elif method == "tools/call":
            params = req.get("params") or {}
            resp = handle_call(params.get("name", ""), params.get("arguments") or {})
        elif method == "config/set":
            cfg = (req.get("params") or {}).get("config") or {}
            CREDS.clear()
            CREDS.update({k: str(v) for k, v in cfg.items()})
            resp = {"result": {}}
        elif method == "watcher.tick":
            # 通知（无 id）不回复；仅记录轮询时刻
            print(
                json.dumps({"jsonrpc": "2.0", "method": "log", "params": {"tick": time.time()}}),
                flush=True,
            )
            continue
        elif method == "shutdown":
            sys.exit(0)
        else:
            continue  # 未知方法静默跳过（不破坏帧同步）

        out = dict(resp)
        out["jsonrpc"] = "2.0"
        out["id"] = rid
        print(json.dumps(out, ensure_ascii=False), flush=True)


if __name__ == "__main__":
    main()
