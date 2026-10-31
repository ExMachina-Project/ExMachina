#!/usr/bin/env bash
# EXMACHINA 交互式安装：一条命令完成 安装方式选择 → 安装 → 启动
#
# 两种方式，脚本交互式询问（默认预编译）：
#   · 预编译发行版 —— 下载即用，无需 Rust / Node.js（原 quickstart）
#   · 源码构建     —— 工具链自举 + 克隆 + 编译，任意平台可用的兜底路径
#
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/ExMachina-Project/ExMachina/main/scripts/install.sh | bash
#   ./install.sh [--bin|--src] [--port 4173]
#
# 兼容 macOS 自带 bash 3.2（POSIX 风格，未使用 bash4 特性）。
set -Eeuo pipefail

REPO="ExMachina-Project/ExMachina"
BRANCH="${EXM_BRANCH:-main}"
NODE_MIN=22
RUST_MIN=56   # edition 2021 所需最低 minor（rustc 1.56+）
PORT="${EXM_PORT:-4173}"
VER="${EXM_VERSION:-}"
MODE=""        # bin | src；空 = 交互询问
TOTAL=5

# ── 输出基建：NO_COLOR 或非 TTY 自动降级 ──────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_G=$'\033[32m'; C_R=$'\033[31m'; C_Y=$'\033[33m'; C_B=$'\033[36m'; C_DIM=$'\033[2m'; C_0=$'\033[0m'
else
  C_G=''; C_R=''; C_Y=''; C_B=''; C_DIM=''; C_0=''
fi

ok()   { printf '  %s✓%s %s\n' "$C_G" "$C_0" "$*"; }
info() { printf '  %s·%s %s\n' "$C_B" "$C_0" "$*"; }
warn() { printf '  %s!%s %s\n' "$C_Y" "$C_0" "$*"; }
fail() {
  printf '  %s✗ %s%s\n' "$C_R" "$1" "$C_0" >&2
  if [ "${2:-}" != "" ]; then printf '    %s建议：%s%s\n' "$C_Y" "$2" "$C_0" >&2; fi
  exit 1
}
step() {
  STEP=$((STEP + 1))
  printf '\n%s[%d/%d]%s %s\n' "$C_B" "$STEP" "$TOTAL" "$C_0" "$1"
}
elapsed() { echo $(( $(date +%s) - $1 )); }

# 交互读入走 /dev/tty：curl | bash 时 stdin 是脚本管道，直接 read 永远读到 EOF。
# 提示全部走 stderr——函数 stdout 被 $( ) 命令替换捕获，混入提示会污染返回值。
ask_mode() {
  printf '\n' >&2
  printf '  %s请选择安装方式：%s\n' "$C_B" "$C_0" >&2
  printf '  %s1)%s 预编译发行版  下载即用，无需 Rust / Node.js%s（推荐）%s\n' "$C_G" "$C_0" "$C_DIM" "$C_0" >&2
  printf '  %s2)%s 源码构建      自举工具链 + 克隆 + 编译（任意平台，约 3-5 分钟）\n' "$C_Y" "$C_0" >&2
  printf '  %s回车 = 1%s\n' "$C_DIM" "$C_0" >&2
  printf '  > ' >&2
  local reply=""
  reply=$(head -n 1 < /dev/tty 2>/dev/null || true)
  case "$reply" in
    ""|1|bin) echo "bin" ;;
    2|src)   echo "src" ;;
    *)       echo "bin" ;;
  esac
}

usage() {
  cat <<'EOF'
EXMACHINA 交互式安装（预编译发行版 或 源码构建，二选一）

用法：install.sh [--bin|--src] [--port N]

选项：
  --bin       预编译发行版（跳过询问，下载即用，无需 Rust / Node.js）
  --src       源码构建（跳过询问：工具链自举 + 克隆 + 编译）
  --port N    网关监听端口（默认 4173）
  --help      显示本帮助（纯本地，不发起网络请求）

环境变量：
  EXM_VERSION   预编译模式指定版本（如 v1.2.0）；缺省取 GitHub Releases latest
  EXM_BRANCH    源码模式克隆的分支（默认 main）
  EXM_DIR       源码模式安装目录（默认 ./ExMachina）
  EXM_PORT      网关监听端口（--port 可覆盖）
  NO_COLOR      置任意非空值禁用彩色输出

说明：
  - 无 --bin/--src 时交互询问（默认预编译）；curl | bash 管道安装同样可交互
  - 已安装目录再次运行 = 升级：覆盖程序文件，保留 .exmachina 数据目录
  - 安装后浏览器打开 http://127.0.0.1:<端口>，到「模型设置」页配置 API Key
EOF
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) MODE="bin"; shift ;;
    --src) MODE="src"; shift ;;
    --port)
      [ $# -ge 2 ] || fail "--port 需要一个端口号参数"
      PORT="$2"; shift 2 ;;
    --help|-h) usage ;;
    *) fail "未知参数：$1" "运行 install.sh --help 查看用法" ;;
  esac
done

trap 'fail "第 $LINENO 行执行失败" "重跑查看细节：bash -x install.sh --port $PORT"' ERR

banner() {
  printf '%s' "$C_B"
  cat <<'EOF'

  ┌──────────────────────────────────────────────
  │  EX·MACHINA — 智能集群 · 安装
  └──────────────────────────────────────────────
EOF
  printf '%s\n' "$C_0"
}

disk_free_mb() { df -Pk . 2>/dev/null | awk 'NR==2 {print int($4 / 1024)}'; }
port_busy() {
  if command -v lsof >/dev/null 2>&1; then
    lsof -nP -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
  elif command -v netstat >/dev/null 2>&1; then
    netstat -an 2>/dev/null | grep -Eq "[:.]$1[[:space:]].*LISTEN"
  else
    return 1
  fi
}
rust_ok() { [ "$(rustc -vV 2>/dev/null | awk '/^release:/ {split($2, a, "."); print a[2] + 0}')" -ge "$RUST_MIN" ]; }
node_ok() { [ "$(node -p 'process.versions.node.split(".")[0]' 2>/dev/null)" -ge "$NODE_MIN" ]; }
install_node() {
  info "通过 nvm 安装 Node.js LTS…"
  if [ ! -s "$HOME/.nvm/nvm.sh" ]; then
    curl -fsSL --retry 3 --retry-delay 2 -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
  fi
  # shellcheck disable=SC1091
  export NVM_DIR="$HOME/.nvm"
  # shellcheck disable=SC1091
  . "$NVM_DIR/nvm.sh"
  nvm install --lts
  nvm use --lts
}

# ── 启动收尾（两种方式共用） ─────────────────────────────────
launch() {  # $1 = 安装目录 $2 = 版本描述
  if port_busy "$PORT"; then
    fail "端口 $PORT 已被占用" "换端口启动：install.sh --port 8173；或结束占用进程：lsof -ti :$PORT | xargs kill"
  fi
  printf '\n%s' "$C_G"
  cat <<EOF
  ══════════════════════════════════════════════
  ✓ 安装完成
     版本   $2
     目录   $(pwd)/$1
     端口   $PORT
  ══════════════════════════════════════════════
EOF
  printf '%s\n\n' "$C_0"
  info "浏览器打开  http://127.0.0.1:$PORT"
  info "首次使用：到「模型设置」页配置 API Key（新增档案 → 选厂商 → 填 Key → 设为默认）"
  info "停止：Ctrl+C"
  printf '\n'
}

# ════════════════════════════════════════════════════════════
# 入口：方式选择
# ════════════════════════════════════════════════════════════
banner
if [ -z "$MODE" ]; then
  MODE=$(ask_mode)
fi
if [ "$MODE" = "bin" ]; then
  # ──────────────────────────────────────────────────────────
  # 路径 A：预编译发行版（原 quickstart）
  # ──────────────────────────────────────────────────────────
  DIR="exmachina"
  STEP=0
  T_ALL=$(date +%s)

  step "环境预检"
  for tool in curl tar; do
    command -v "$tool" >/dev/null 2>&1 || fail "缺少依赖：$tool" "请先安装 $tool（curl 与 tar 为下载解压所需）"
  done
  FREE=$(disk_free_mb)
  [ "${FREE:-0}" -ge 512 ] || fail "磁盘剩余空间不足（${FREE}MB < 512MB）" "清理磁盘后重试，或更换到空间充足的工作目录"
  ok "依赖齐备（curl / tar），磁盘剩余 ${FREE}MB"

  step "解析版本与平台"
  OS=$(uname -s)
  ARCH=$(uname -m)
  case "$OS:$ARCH" in
    Linux:x86_64 | Linux:amd64) FILE="exmachina-linux-x64.tar.gz";  PLAT="linux-x64" ;;
    Darwin:arm64)               FILE="exmachina-macos-arm64.tar.gz"; PLAT="macos-arm64" ;;
    Darwin:x86_64)              FILE="exmachina-macos-x64.tar.gz";   PLAT="macos-x64" ;;
    *)
      fail "不支持的平台：$OS $ARCH" "改用源码构建：install.sh --src（任意平台，需 Rust + Node.js）"
      ;;
  esac
  TAG="$VER"
  if [ -z "$TAG" ]; then
    API_JSON=$(curl -fsSL --retry 3 --retry-delay 2 "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null) || true
    TAG=$(printf '%s' "$API_JSON" | grep -o '"tag_name":[[:space:]]*"[^"]*"' | head -1 | sed 's/.*: *//; s/"//g')
  fi
  [ -n "$TAG" ] || fail "无法获取最新版本号" "网络受限或 GitHub API 限流；可指定版本重试：EXM_VERSION=v1.2.0 bash install.sh --bin"
  case "$TAG" in v*) ;; *) TAG="v$TAG" ;; esac
  ok "版本 $TAG · 平台 $PLAT"

  step "下载发行版（$FILE）"
  DL_URL="https://github.com/$REPO/releases/download/$TAG/$FILE"
  TMP=$(mktemp -d 2>/dev/null) || TMP="/tmp/exm-install-$$"
  mkdir -p "$TMP"
  T_DL=$(date +%s)
  if ! curl -fSL --retry 3 --retry-delay 2 -# -o "$TMP/$FILE" "$DL_URL"; then
    rm -rf "$TMP"
    fail "下载失败：$DL_URL" "检查网络；或指定版本重试：EXM_VERSION=v1.2.0 bash install.sh --bin"
  fi
  ok "下载完成（耗时 $(elapsed $T_DL)s）"

  # 校验和：release 附带 .sha256 则校验，缺失则提示后跳过
  if curl -fsSL --retry 2 -o "$TMP/$FILE.sha256" "$DL_URL.sha256" 2>/dev/null; then
    WANT=$(awk '{print $1}' "$TMP/$FILE.sha256")
    HAVE=$( { sha256sum "$TMP/$FILE" 2>/dev/null || shasum -a 256 "$TMP/$FILE"; } | awk '{print $1}')
    [ -n "$HAVE" ] || fail "本机缺少 sha256sum / shasum，无法校验" "安装 coreutils 或跳过校验（不推荐）"
    [ "$WANT" = "$HAVE" ] || fail "校验和不匹配（期望 $WANT，实际 $HAVE）" "下载可能损坏或被篡改；删除 $TMP 后重试"
    ok "校验通过（sha256）"
  else
    warn "Release 未附带 .sha256，跳过校验"
  fi

  step "解压就绪"
  if [ -d "$DIR" ]; then
    info "检测到已安装目录 → 升级路径（覆盖程序文件，保留 .exmachina 数据目录）"
  fi
  mkdir -p "$DIR"
  STAGE="$DIR/.stage-$$"
  mkdir -p "$STAGE"
  tar -xzf "$TMP/$FILE" -C "$STAGE"
  cp -Rf "$STAGE/." "$DIR/"
  rm -rf "$STAGE"
  rm -rf "$TMP"
  ok "就绪（耗时含在下载计时内）"

  step "启动网关（端口 $PORT）"
  chmod +x "$DIR"/exm-gateway "$DIR"/exm 2>/dev/null || true
  launch "$DIR" "$TAG（$PLAT）"
  cd "$DIR"
  exec env EXM_PORT="$PORT" ./exm-gateway
else
  # ──────────────────────────────────────────────────────────
  # 路径 B：源码构建
  # ──────────────────────────────────────────────────────────
  DIR="${EXM_DIR:-ExMachina}"
  TOTAL=7
  STEP=0
  T_ALL=$(date +%s)

  step "环境预检"
  for tool in git curl tar; do
    command -v "$tool" >/dev/null 2>&1 || fail "缺少依赖：$tool" "请先安装 $tool 并确保在 PATH 中"
  done
  FREE=$(disk_free_mb)
  [ "${FREE:-0}" -ge 3072 ] || fail "磁盘剩余空间不足（${FREE}MB < 3072MB）" "Rust 编译产物较大，清理磁盘或更换工作目录后重试"
  ok "依赖齐备（git / curl / tar），磁盘剩余 ${FREE}MB"

  step "Rust 工具链"
  if ! command -v cargo >/dev/null 2>&1; then
    info "未检测到 cargo → 安装 rustup（stable）…"
    curl --proto '=https' --tlsv1.2 -sSf --retry 3 https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  elif ! rust_ok; then
    warn "rustc 版本过旧（需 1.$RUST_MIN+），升级中…"
    rustup update stable 2>/dev/null || fail "rustup 升级失败" "手动执行：rustup update stable"
    # shellcheck disable=SC1091
    [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  else
    ok "cargo 已就绪（$(cargo --version 2>/dev/null || echo '未知版本')）"
  fi

  step "Node.js 工具链"
  if ! node_ok; then
    install_node
    node_ok || fail "Node.js 安装后仍不满足（需 ≥ $NODE_MIN）" "手动安装：https://nodejs.org/ 后重跑本脚本"
  else
    ok "node 已就绪（$(node -v 2>/dev/null || echo '未知版本')）"
  fi

  step "克隆仓库（$BRANCH → $DIR）"
  if [ -d "$DIR/.git" ]; then
    info "已存在仓库 → 拉取更新"
    git -C "$DIR" fetch --all --prune 2>/dev/null || true
    git -C "$DIR" checkout "$BRANCH" 2>/dev/null || fail "分支 $BRANCH 不存在" "检查 EXM_BRANCH 或删除 $DIR 后重跑"
    git -C "$DIR" pull --ff-only 2>/dev/null || warn "pull 失败（本地有改动？），沿用当前工作树"
  else
    git clone --depth 1 -b "$BRANCH" "https://github.com/$REPO.git" "$DIR" || fail "克隆失败" "检查网络与分支名（EXM_BRANCH）"
  fi
  ok "代码就绪"

  step "编译构建（首次约 3-5 分钟）"
  T_BUILD=$(date +%s)
  cd "$DIR"
  npm install --no-audit --no-fund || fail "npm install 失败" "检查 Node 版本（≥ $NODE_MIN）与网络（可换 npm 镜像）"
  npm run build:webui || fail "WebUI 构建失败" "进 webui/ 手动 npm install && npm run build 排查"
  cargo build --release -p exm-gateway || fail "Rust 编译失败" "查看上方报错；常见为依赖下载失败，重跑即可续编"
  ok "构建完成（耗时 $(elapsed $T_BUILD)s）"

  step "启动网关（端口 $PORT）"
  launch "$DIR" "源码构建（$BRANCH 分支）"
  exec env EXM_PORT="$PORT" ./target/release/exm-gateway
fi
