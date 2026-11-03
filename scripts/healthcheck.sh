#!/usr/bin/env bash
# 部署实例功能巡检：批量探测 REST 端点 + 主链路聊天 + WS 握手 + 索引目录。
# 用法：
#   scripts/healthcheck.sh [BASE_URL]        # 默认 http://localhost:4173
#   EXM_AUTH_KEY=xxx scripts/healthcheck.sh   # 密钥也可从环境变量取
# 退出码：0 全过 / 1 有失败项。CI 或部署后冒烟皆可用。
set -u

BASE="${1:-http://localhost:4173}"
KEY="${EXM_AUTH_KEY:-}"

# 密钥缺省时尝试部署目录 .env（EXM_AUTH_KEY=...）
if [ -z "$KEY" ] && [ -f "$(dirname "$0")/../.env" ]; then
  KEY=$(grep '^EXM_AUTH_KEY=' "$(dirname "$0")/../.env" | head -1 | cut -d= -f2)
fi

PASS=0
FAIL=0
FAILED=""

req() { # req <方法> <路径> [body] -> 状态码
  local method="$1" path="$2" body="${3:-}"
  if [ -n "$body" ]; then
    curl -s -o /dev/null -w '%{http_code}' -X "$method" "$BASE/api/$path" \
      -H "X-Auth-Key: $KEY" -H 'Content-Type: application/json' -d "$body" -m 20
  else
    curl -s -o /dev/null -w '%{http_code}' -X "$method" "$BASE/api/$path" \
      -H "X-Auth-Key: $KEY" -m 20
  fi
}

check() { # check <名称> <期望码> <实际码>
  if [ "$2" = "$3" ]; then
    PASS=$((PASS + 1)); printf '  ok  %s\n' "$1"
  else
    FAIL=$((FAIL + 1)); FAILED="$FAILED $1"; printf 'FAIL  %s（期望 %s 实得 %s）\n' "$1" "$2" "$3"
  fi
}

echo "== REST 端点巡检（$BASE）=="
for ep in health agents groups/active sessions channels cron skills memory/stats \
          approvals audit checkpoints config config/schema nexus/links playbooks \
          identity/list limits/stats version plugins plugins/index auth/status; do
  check "GET /api/$ep" 200 "$(req GET "$ep")"
done

echo "== 主链路：会话 + 聊天往返 =="
SID=$(curl -s -X POST "$BASE/api/sessions" -H "X-Auth-Key: $KEY" \
  -H 'Content-Type: application/json' -d '{"title":"部署巡检"}' -m 20 \
  | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)
# 巡检会话用完即删（无论成败）——不留垃圾：曾长期只建不删，生产堆积数十个「部署巡检」
cleanup_inspect() {
  if [ -n "$SID" ]; then
    CODE=$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/api/sessions/$SID" \
      -H "X-Auth-Key: $KEY" -m 20 2>/dev/null || true)
    if [ "$CODE" = "200" ]; then echo "  ok  巡检会话已清理 $SID"; else
      echo "  !   巡检会话清理失败（$CODE）: $SID —— 请手动删除"
    fi
  fi
}
trap cleanup_inspect EXIT
if [ -n "$SID" ]; then
  PASS=$((PASS + 1)); echo "  ok  会话创建 $SID"
  check "POST /api/sessions/$SID/chat（受理）" 202 "$(req POST "sessions/$SID/chat" '{"text":"巡检健康检查：请直接回复「正常」两个字。不要调用任何工具，不要执行任何命令——本项只验证聊天链路往返。"}')"
  # 异步受理：轮询消息面确认 orchestrator 已回（最多 ~60s）
  REPLY=""
  for _ in $(seq 1 20); do
    sleep 3
    REPLY=$(curl -s "$BASE/api/sessions/$SID/messages?limit=8" -H "X-Auth-Key: $KEY" -m 20 \
      | grep -o '"role":"orchestrator"' | head -1)
    [ -n "$REPLY" ] && break
  done
  if [ -n "$REPLY" ]; then PASS=$((PASS + 1)); echo "  ok  orchestrator 回复落流"; else
    FAIL=$((FAIL + 1)); FAILED="$FAILED 聊天回复"; echo "FAIL  orchestrator 60s 内未回复"
  fi
else
  FAIL=$((FAIL + 1)); FAILED="$FAILED 会话创建"; echo "FAIL  会话创建失败"
fi

echo "== WS 握手（?key= 查询参数鉴权）=="
WSCODE=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/ws?sessionId=probe&key=$KEY" \
  -H 'Upgrade: websocket' -H 'Connection: Upgrade' \
  -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' -H 'Sec-WebSocket-Version: 13' -m 5 2>/dev/null || true)
# 101 后挂住属正常（curl 无 WS 帧，超时退出码 28 不算失败）
if [ "$WSCODE" = "101" ]; then PASS=$((PASS + 1)); echo "  ok  WS 升级握手 101"; else
  FAIL=$((FAIL + 1)); FAILED="$FAILED WS握手"; echo "FAIL  WS 握手得 $WSCODE（期望 101）"
fi

echo "== WebUI 静态托管 =="
check "GET /（index.html）" 200 "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/" -m 20)"

echo
echo "结果：$PASS 过 / $FAIL 失败${FAILED:+（失败项:$FAILED ）}"
[ "$FAIL" -eq 0 ]
