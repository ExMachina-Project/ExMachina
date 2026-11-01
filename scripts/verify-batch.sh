#!/usr/bin/env bash
# 批次验收：一键回归（long-horizon-iteration 每批次收口必跑）。
# 串联：clippy（-D warnings）→ workspace 测试 → e2e → webui tsc + 构建。
# 任一步失败即退出非零；全过打印各步耗时与总计。
# 用法：scripts/verify-batch.sh [--skip-e2e]   # --skip-e2e 跳过 e2e（无数据库环境时）
set -euo pipefail
cd "$(dirname "$0")/.."

SKIP_E2E="${1:-}"
STEPS_TOTAL=0
STEPS_PASS=0
declare -a SUMMARY

run_step() {
  local name="$1"; shift
  local start=$(date +%s.%N)
  echo ""
  echo "════════ $name ════════"
  if "$@"; then
    local end=$(date +%s.%N)
    local dur=$(echo "$end $start" | awk '{printf "%.0fs", $1-$2}')
    SUMMARY+=("✓ $name ($dur)")
    STEPS_PASS=$((STEPS_PASS+1))
  else
    SUMMARY+=("✗ $name")
    echo ""
    echo "失败于：$name"
    for s in "${SUMMARY[@]}"; do echo "  $s"; done
    exit 1
  fi
  STEPS_TOTAL=$((STEPS_TOTAL+1))
}

TOTAL_START=$(date +%s.%N)

run_step "clippy -D warnings" cargo clippy --workspace --all-targets --locked -- -D warnings
run_step "workspace 测试" cargo test --workspace --locked
if [ "$SKIP_E2E" != "--skip-e2e" ]; then
  run_step "e2e" bash -c 'cargo run -p exm-gateway --example e2e --locked && rm -rf entities/skills/e2e-skill'
else
  echo "（--skip-e2e：跳过 e2e）"
fi
run_step "webui tsc" bash -c 'cd webui && npx tsc --noEmit'
run_step "webui 构建" bash -c 'cd webui && npm run build'

TOTAL_END=$(date +%s.%N)
TOTAL_DUR=$(echo "$TOTAL_END $TOTAL_START" | awk '{printf "%.0fs", $1-$2}')

echo ""
echo "════════ 批次验收全部通过 ════════"
for s in "${SUMMARY[@]}"; do echo "  $s"; done
echo "总计：$STEPS_PASS/$STEPS_TOTAL 步，耗时 $TOTAL_DUR"
