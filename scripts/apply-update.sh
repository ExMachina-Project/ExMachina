#!/bin/sh
# EXMACHINA 一键更新:由网关容器发起,经挂载的 docker.sock 在宿主引擎上执行。
# 双模式:
#   image(/host 为部署目录,无 .git):compose pull → 辅助容器 up -d --force-recreate
#   git(/host 是仓库根,兼容旧源码部署):git pull → build → 辅助容器 up -d
# 直接在本容器内跑 up -d 会让 compose 客户端随旧容器一起被杀,recreate 做一半——
# 重启步骤必须借道一次性辅助容器(独立于本容器生命周期)。
# 状态:各阶段写 /tmp/exm-update.state(phase 小写,与网关读取侧 serde 契约一致);
#      收尾必写 exitCode(0=成功);幂等清理残留的 exmachina-updater。
set -u

STATE_FILE="${EXM_UPDATE_STATE:-/tmp/exm-update.state}"
HELPER_IMAGE="docker:27.5.1-cli"
PROJECT="${EXM_COMPOSE_PROJECT:-exmachina}"

# 推进 phase:读-改-写保留 pid/startedAt/exitCode(与网关 write_update_state 同契约)
set_phase() {
    phase="$1"
    now=$(date +%s)
    if [ -f "$STATE_FILE" ] && command -v python3 >/dev/null 2>&1; then
        python3 - "$STATE_FILE" "$phase" "$now" <<'PYEOF' 2>/dev/null || echo "{\"pid\":$$,\"startedAt\":$now,\"phase\":\"$phase\",\"exitCode\":null}" > "$STATE_FILE"
import json, sys
p, phase, now = sys.argv[1], sys.argv[2], int(sys.argv[3])
try:
    f = json.load(open(p))
except Exception:
    f = {}
f.setdefault("pid", 0)
f.setdefault("startedAt", now)
f["phase"] = phase
f.pop("exitCode", None)
json.dump(f, open(p, "w"))
PYEOF
    else
        echo "{\"pid\":$$,\"startedAt\":$now,\"phase\":\"$phase\",\"exitCode\":null}" > "$STATE_FILE"
    fi
}

# 收尾:写 exitCode 后退出
finish() {
    code="$1"
    now=$(date +%s)
    if [ -f "$STATE_FILE" ] && command -v python3 >/dev/null 2>&1; then
        python3 - "$STATE_FILE" "$code" "$now" <<'PYEOF' 2>/dev/null || true
import json, sys
p, code, now = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
try:
    f = json.load(open(p))
except Exception:
    f = {"pid": 0, "startedAt": now}
f["exitCode"] = code
f["startedAt"] = f.get("startedAt", now)
json.dump(f, open(p, "w"))
PYEOF
    else
        echo "{\"pid\":$$,\"startedAt\":$now,\"phase\":\"done\",\"exitCode\":$code}" > "$STATE_FILE"
    fi
    exit "$code"
}

trap 'finish 1' HUP INT TERM

# docker cli 需要 config.json(拉取镜像时解析仓库认证用)。
# 私有注册表(ghcr 私有包等)场景:部署目录放 .docker/config.json(docker login 产物),
# 更新时复制到容器内临时 DOCKER_CONFIG——凭据只留在部署目录,不进镜像、不进仓库。
# 无凭据文件时造空 config,公开镜像照常拉取。
export DOCKER_CONFIG="${DOCKER_CONFIG:-/tmp/.docker}"
mkdir -p "$DOCKER_CONFIG"
DEPLOY_DOCKER_CONFIG="/host/.docker/config.json"
if [ -f "$DEPLOY_DOCKER_CONFIG" ]; then
    cp "$DEPLOY_DOCKER_CONFIG" "$DOCKER_CONFIG/config.json"
    chmod 600 "$DOCKER_CONFIG/config.json" 2>/dev/null || true
    echo "▸ 已加载部署目录注册表凭据(.docker/config.json)"
else
    [ -f "$DOCKER_CONFIG/config.json" ] || echo '{}' > "$DOCKER_CONFIG/config.json"
fi

# 辅助容器在宿主侧运行:-v 的源必须是部署目录的宿主真实路径(/host 只是本容器内的挂载点),
# 从网关容器自身的 mount 信息反解,不硬编码
resolve_host_repo() {
    docker inspect exmachina --format '{{range .Mounts}}{{if eq .Destination "/host"}}{{.Source}}{{end}}{{end}}' 2>/dev/null
}

# 经一次性辅助容器执行 compose 重启(独立于本容器生命周期)
# 只操作网关自身服务:同项目里的附属服务(如 napcat)生命周期独立,
# 不该因版本更新被 pull/recreate(实测会把 275MB 无关镜像一起拉)。
SERVICE="exmachina"
recreate_via_helper() {
    docker pull -q "$HELPER_IMAGE"
    docker rm -f exmachina-updater 2>/dev/null || true
    HOST_REPO=$(resolve_host_repo)
    [ -n "$HOST_REPO" ] || { echo "⛔ 未能从容器 mount 反解出部署目录宿主路径"; finish 1; }
    set_phase recreating
    docker run --rm --name exmachina-updater \
        -v /var/run/docker.sock:/var/run/docker.sock \
        -v "$HOST_REPO:/host" -w /host \
        "$HELPER_IMAGE" compose -p "$PROJECT" up -d --force-recreate --no-deps "$SERVICE"
}

if [ -d /host/.git ]; then
    # ---------------- git 模式(源码部署兼容路径) ----------------
    git config --global --add safe.directory /host 2>/dev/null || true
    cd /host || finish 1
    echo "▸ [git 模式] 步骤 1/3:拉取最新代码"
    set_phase pulling
    git pull --ff-only || finish 1
    echo "▸ [git 模式] 步骤 2/3:构建镜像"
    set_phase building
    GIT_HASH=$(git rev-parse --short HEAD)
    export GIT_HASH
    docker compose -p "$PROJECT" build || finish 1
    echo "▸ [git 模式] 步骤 3/3:重启容器(经一次性辅助容器执行)"
    recreate_via_helper || finish 1
else
    # ---------------- image 模式(镜像部署) ----------------
    cd /host || finish 1
    echo "▸ [image 模式] 步骤 1/2:拉取最新镜像"
    set_phase pulling
    docker compose -p "$PROJECT" pull "$SERVICE" || finish 1
    echo "▸ [image 模式] 步骤 2/2:重启容器(经一次性辅助容器执行)"
    recreate_via_helper || finish 1
fi

echo "▸ 更新完成"
finish 0
