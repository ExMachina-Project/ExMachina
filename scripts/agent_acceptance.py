#!/usr/bin/env python3
"""Run an explicitly selected, resource-bounded disposable agent acceptance container."""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path
from typing import Callable, Sequence


DEFAULT_CONFIG = Path(__file__).with_name("agent-acceptance.json")
APPROVED_TEMP_ROOT = Path("/tmp/opencode").resolve()
CONTAINER_WORKSPACE = "/workspace"
MIN_MEMORY = 256 * 1024 * 1024
MAX_MEMORY = 8 * 1024 * 1024 * 1024
MAX_PROJECT_DISK = 16 * 1024 * 1024 * 1024
MAX_PROCESSES = 512
MAX_TIMEOUT_SECONDS = 3600
IMAGE_PATTERN = re.compile(r"^[a-zA-Z0-9][a-zA-Z0-9._/:@-]{0,159}$")


class PreflightError(ValueError):
    pass


def load_config(path: Path = DEFAULT_CONFIG) -> dict[str, object]:
    try:
        config = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PreflightError(f"验收配置不可读取: {error}") from error
    if not isinstance(config, dict):
        raise PreflightError("验收配置必须是 JSON 对象")
    return config


def _bounded_int(config: dict[str, object], key: str, low: int, high: int) -> int:
    value = config.get(key)
    if isinstance(value, bool) or not isinstance(value, int) or not low <= value <= high:
        raise PreflightError(f"{key} 必须在 {low}..{high} 范围内")
    return value


def _resolved_path(value: object, key: str) -> Path:
    if not isinstance(value, str) or not value:
        raise PreflightError(f"{key} 必须是非空绝对路径")
    path = Path(value)
    if not path.is_absolute():
        raise PreflightError(f"{key} 必须是绝对路径")
    return path.resolve(strict=False)


def _is_within(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _available_memory_bytes() -> int:
    """Return the tighter of host MemAvailable and the current cgroup headroom."""
    host_available: int | None = None
    try:
        for line in Path("/proc/meminfo").read_text(encoding="ascii").splitlines():
            if line.startswith("MemAvailable:"):
                host_available = int(line.split()[1]) * 1024
                break
    except (OSError, ValueError, IndexError):
        pass

    cgroup_available: int | None = None
    for base in (Path("/sys/fs/cgroup"), Path("/sys/fs/cgroup/memory")):
        try:
            limit_raw = (base / "memory.max").read_text(encoding="ascii").strip()
            current_raw = (base / "memory.current").read_text(encoding="ascii").strip()
            if limit_raw != "max":
                cgroup_available = max(0, int(limit_raw) - int(current_raw))
                break
        except (OSError, ValueError):
            continue

    values = [value for value in (host_available, cgroup_available) if value is not None]
    if not values:
        raise PreflightError("无法读取可用内存；为避免超额验收，已停止")
    return min(values)


def validate_config(config: dict[str, object]) -> tuple[Path, Path]:
    if config.get("schemaVersion") != 1:
        raise PreflightError("不支持的验收配置 schemaVersion")

    temp_root = _resolved_path(config.get("tempRoot"), "tempRoot")
    project_dir = _resolved_path(config.get("projectDir"), "projectDir")
    if not _is_within(temp_root, APPROVED_TEMP_ROOT) or temp_root == APPROVED_TEMP_ROOT:
        raise PreflightError("tempRoot 必须是 /tmp/opencode 下的专用子目录")
    if not _is_within(project_dir, temp_root) or project_dir == temp_root:
        raise PreflightError("projectDir 必须是 tempRoot 内的专用子目录")

    # This runner accepts no host mounts. The only writable project filesystem is a
    # size-bounded container tmpfs populated through docker cp.
    forbidden_mount_keys = {"mounts", "volumes", "dockerArgs", "extraArgs", "network"}
    unexpected = forbidden_mount_keys.intersection(config)
    if unexpected:
        raise PreflightError(
            "验收配置不得声明额外挂载、Docker 参数或网络选项: "
            + ", ".join(sorted(unexpected))
        )

    _bounded_int(config, "minimumFreeDiskBytes", 1, 1024**4)
    _bounded_int(config, "minimumAvailableMemoryBytes", 1, MAX_MEMORY)
    memory_limit = _bounded_int(config, "memoryLimitBytes", MIN_MEMORY, MAX_MEMORY)
    _bounded_int(config, "projectDiskLimitBytes", 1024 * 1024, MAX_PROJECT_DISK)
    if memory_limit < config["minimumAvailableMemoryBytes"]:
        raise PreflightError("memoryLimitBytes 不能低于最低可用内存要求")
    _bounded_int(config, "processLimit", 1, MAX_PROCESSES)
    _bounded_int(config, "timeoutSeconds", 1, MAX_TIMEOUT_SECONDS)

    cpu_limit = config.get("cpuLimit")
    if isinstance(cpu_limit, bool) or not isinstance(cpu_limit, (int, float)) or not 0 < cpu_limit <= 8:
        raise PreflightError("cpuLimit 必须大于 0 且不超过 8")

    image = config.get("image")
    if not isinstance(image, str) or not IMAGE_PATTERN.fullmatch(image):
        raise PreflightError("image 格式无效")
    return temp_root, project_dir


def preflight(
    config: dict[str, object],
    *,
    available_disk_bytes: int | None = None,
    available_memory_bytes: int | None = None,
) -> tuple[Path, Path]:
    temp_root, project_dir = validate_config(config)
    probe = temp_root.parent
    while not probe.exists() and probe != probe.parent:
        probe = probe.parent
    if not probe.is_dir():
        raise PreflightError(f"临时目录父路径不是目录: {probe}")

    free_disk = shutil.disk_usage(probe).free if available_disk_bytes is None else available_disk_bytes
    required_disk = int(config["minimumFreeDiskBytes"]) + int(config["projectDiskLimitBytes"])
    if free_disk < required_disk:
        raise PreflightError(
            f"临时目录磁盘空间不足: 可用 {free_disk} bytes，"
            f"至少需要 {required_disk} bytes（含项目结果上限与保留空间）"
        )

    memory = _available_memory_bytes() if available_memory_bytes is None else available_memory_bytes
    required_memory = max(
        int(config["minimumAvailableMemoryBytes"]), int(config["memoryLimitBytes"])
    )
    if memory < required_memory:
        raise PreflightError(
            f"可用内存不足: 可用 {memory} bytes，至少需要 {required_memory} bytes"
        )
    return temp_root, project_dir


def docker_command(
    config: dict[str, object],
    command: Sequence[str],
    *,
    container_name: str | None = None,
) -> list[str]:
    if not command or not command[0]:
        raise PreflightError("run 需要显式指定容器内命令")
    _temp_root, project_dir = validate_config(config)
    uid, gid = _container_identity()
    argv = [
        "docker",
        "create",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--init",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        f"--pids-limit={config['processLimit']}",
        f"--memory={config['memoryLimitBytes']}",
        f"--memory-swap={config['memoryLimitBytes']}",
        f"--cpus={config['cpuLimit']}",
        f"--user={uid}:{gid}",
        f"--tmpfs={CONTAINER_WORKSPACE}:rw,nosuid,nodev,size={config['projectDiskLimitBytes']},uid={uid},gid={gid},mode=0770",
        f"--workdir={CONTAINER_WORKSPACE}",
        "--tmpfs=/tmp:rw,nosuid,nodev,size=268435456",
        "--env=HOME=/tmp",
        "--env=TMPDIR=/tmp",
    ]
    if container_name:
        argv.append(f"--name={container_name}")
    argv.extend([str(config["image"]), *command])
    return argv


def _container_identity() -> tuple[int, int]:
    """Use the invoking non-root identity; root callers get the nobody identity."""
    uid = os.getuid() if hasattr(os, "getuid") else 1000
    gid = os.getgid() if hasattr(os, "getgid") else uid
    if uid == 0:
        return 65532, 65532
    return uid, gid


def _docker_control(
    reference: str,
    *,
    runner: Callable[..., subprocess.CompletedProcess[str]],
    action: str,
) -> bool:
    """Run a bounded best-effort Docker control command."""
    argv = ["docker", action]
    if action == "rm":
        argv.append("--force")
    argv.append(reference)
    try:
        result = runner(
            argv,
            check=False,
            timeout=10,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _project_tree_bytes(project_dir: Path) -> int:
    total = 0
    for path in project_dir.rglob("*"):
        if path.is_symlink():
            raise PreflightError(f"隔离项目不得包含符号链接: {path.relative_to(project_dir)}")
        if path.is_dir():
            continue
        if not path.is_file():
            raise PreflightError(f"隔离项目只允许普通文件和目录: {path.relative_to(project_dir)}")
        total += path.stat().st_size
    return total


def _copy_project_out(
    container_id: str,
    result_dir: Path,
    *,
    runner: Callable[..., subprocess.CompletedProcess[str]],
    timeout: int = 60,
) -> bool:
    """Best-effort result recovery, including after the container is killed."""
    try:
        result = runner(
            [
                "docker",
                "cp",
                "--archive",
                f"{container_id}:{CONTAINER_WORKSPACE}/.",
                str(result_dir),
            ],
            check=False,
            timeout=timeout,
        )
        return result.returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


def _prepare_project_access(project_dir: Path, uid: int, gid: int) -> list[tuple[Path, int, int, int]]:
    """Temporarily make only the approved fixture usable by the unprivileged container user."""
    paths = [project_dir, *sorted(project_dir.rglob("*"))]
    snapshot: list[tuple[Path, int, int, int]] = []
    try:
        for path in paths:
            info = path.lstat()
            if path.is_symlink() or not (path.is_dir() or path.is_file()):
                raise PreflightError(f"隔离项目只允许普通文件和目录: {path.relative_to(project_dir)}")
            mode = info.st_mode & 0o7777
            snapshot.append((path, info.st_uid, info.st_gid, mode))
            if os.geteuid() == 0 and (info.st_uid != uid or info.st_gid != gid):
                os.chown(path, uid, gid, follow_symlinks=False)
            elif (info.st_uid, info.st_gid) != (uid, gid):
                raise PreflightError(
                    f"当前用户无法把隔离项目交给容器身份: {path.relative_to(project_dir)}"
                )
            required = 0o700 if path.is_dir() else 0o600
            os.chmod(path, mode | required, follow_symlinks=False)
    except (OSError, PreflightError):
        _restore_project_access(snapshot)
        raise
    return snapshot


def _restore_project_access(snapshot: Sequence[tuple[Path, int, int, int]]) -> None:
    for path, uid, gid, mode in reversed(snapshot):
        try:
            if not path.exists() and not path.is_symlink():
                continue
            if os.geteuid() == 0:
                os.chown(path, uid, gid, follow_symlinks=False)
            os.chmod(path, mode, follow_symlinks=False)
        except OSError as error:
            print(f"无法恢复验收项目元数据 {path}: {error}", file=sys.stderr)


def run_container(
    config: dict[str, object],
    command: Sequence[str],
    *,
    available_disk_bytes: int | None = None,
    available_memory_bytes: int | None = None,
    runner: Callable[..., subprocess.CompletedProcess[str]] = subprocess.run,
    process_factory: Callable[..., subprocess.Popen] = subprocess.Popen,
    clock: Callable[[], float] = time.monotonic,
    sleeper: Callable[[float], None] = time.sleep,
    dry_run: bool = False,
) -> int:
    temp_root, project_dir = preflight(
        config,
        available_disk_bytes=available_disk_bytes,
        available_memory_bytes=available_memory_bytes,
    )
    container_name = f"exm-agent-acceptance-{os.getpid()}-{time.monotonic_ns():x}"
    create_command = docker_command(config, command, container_name=container_name)
    if dry_run:
        print(
            json.dumps(
                {
                    "tempRoot": str(temp_root),
                    "projectDir": str(project_dir),
                    "createCommand": docker_command(config, command, container_name="<generated>"),
                    "copyInCommand": [
                        "docker",
                        "cp",
                        "--archive",
                        "<projectDir>/.",
                        "<container-id>:/workspace/",
                    ],
                    "startCommand": ["docker", "start", "--attach", "<container-id>"],
                    "copyOutCommand": [
                        "docker",
                        "cp",
                        "--archive",
                        "<container-id>:/workspace/.",
                        "<resultDir>",
                    ],
                    "diskLimitBytes": config["projectDiskLimitBytes"],
                    "timeoutSeconds": config["timeoutSeconds"],
                },
                ensure_ascii=False,
            )
        )
        return 0

    temp_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    project_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    source_size = _project_tree_bytes(project_dir)
    if source_size > int(config["projectDiskLimitBytes"]):
        raise PreflightError(
            f"隔离项目超过大小上限: {source_size} bytes > {config['projectDiskLimitBytes']} bytes"
        )
    container_reference = container_name
    created = False
    result_dir = temp_root / "results" / container_name
    result_dir.mkdir(mode=0o700, parents=True, exist_ok=False)
    deadline = clock() + int(config["timeoutSeconds"])
    child: subprocess.Popen | None = None
    uid, gid = _container_identity()
    access_snapshot = _prepare_project_access(project_dir, uid, gid)
    create_attempted = False
    try:
        create_attempted = True
        created_result = runner(
            create_command,
            check=False,
            capture_output=True,
            text=True,
            timeout=min(30, int(config["timeoutSeconds"])),
        )
        if created_result.returncode != 0:
            if created_result.stderr:
                print(created_result.stderr.rstrip(), file=sys.stderr)
            _docker_control(container_name, runner=runner, action="kill")
            return int(created_result.returncode)
        container_id = (created_result.stdout or "").strip()
        if not re.fullmatch(r"[a-fA-F0-9]{12,64}", container_id):
            print("Docker 未返回有效容器 ID；已尝试按隔离名称清理", file=sys.stderr)
            _docker_control(container_name, runner=runner, action="kill")
            return 125
        container_reference = container_id
        created = True
        remaining = deadline - clock()
        if remaining <= 0:
            print("验收容器创建已耗尽总时长上限", file=sys.stderr)
            _docker_control(container_reference, runner=runner, action="kill")
            return 124
        copied_in = runner(
            [
                "docker",
                "cp",
                "--archive",
                f"{project_dir}/.",
                f"{container_id}:{CONTAINER_WORKSPACE}/",
            ],
            check=False,
            timeout=min(60, max(1, int(remaining))),
        )
        if copied_in.returncode != 0:
            print("无法将隔离项目复制到容器 tmpfs；验收未启动", file=sys.stderr)
            return 125
        # Restore host metadata immediately; the container owns only the archive copy.
        _restore_project_access(access_snapshot)
        access_snapshot = []
        remaining = deadline - clock()
        if remaining <= 0:
            print("验收容器创建已耗尽总时长上限", file=sys.stderr)
            _docker_control(container_reference, runner=runner, action="kill")
            return 124
        child = process_factory(
            ["docker", "start", "--attach", container_id],
            stdin=subprocess.DEVNULL,
        )
        while True:
            exit_code = child.poll()
            if exit_code is not None:
                break
            remaining = deadline - clock()
            if remaining <= 0:
                print(f"验收容器超过 {config['timeoutSeconds']} 秒上限，正在终止并清理", file=sys.stderr)
                _docker_control(container_reference, runner=runner, action="kill")
                _wait_or_kill(child)
                if _copy_project_out(container_reference, result_dir, runner=runner):
                    print(json.dumps({"resultDir": str(result_dir), "timedOut": True}, ensure_ascii=False))
                return 124
            sleeper(min(0.25, remaining))
        if not _copy_project_out(
            container_id,
            result_dir,
            runner=runner,
            timeout=min(60, max(1, int(config["timeoutSeconds"]))),
        ):
            print("无法从隔离容器回收项目结果；结果不得视为已交付", file=sys.stderr)
            return 125
        result_size = _project_tree_bytes(result_dir)
        if result_size > int(config["projectDiskLimitBytes"]):
            print("回收的隔离项目结果超过磁盘上限，验收失败", file=sys.stderr)
            return 125
        if not _docker_control(container_reference, runner=runner, action="rm"):
            print("无法确认验收容器已移除；本次不得作为验收通过", file=sys.stderr)
            return 125
        created = False
        print(
            json.dumps(
                {
                    "resultDir": str(result_dir),
                    "containerExitCode": exit_code,
                    "projectBytes": result_size,
                },
                ensure_ascii=False,
            )
        )
        return int(exit_code)
    except subprocess.TimeoutExpired:
        print(f"验收容器超过 {config['timeoutSeconds']} 秒上限，正在终止并清理", file=sys.stderr)
        _docker_control(container_reference, runner=runner, action="kill")
        if child is not None:
            _wait_or_kill(child)
        if created and _copy_project_out(container_reference, result_dir, runner=runner):
            print(json.dumps({"resultDir": str(result_dir), "timedOut": True}, ensure_ascii=False))
        return 124
    except OSError as error:
        print(f"Docker 调用失败: {error}", file=sys.stderr)
        if created:
            _docker_control(container_reference, runner=runner, action="kill")
        if child is not None:
            _wait_or_kill(child)
        return 127
    finally:
        if created:
            if not _docker_control(container_reference, runner=runner, action="rm"):
                print("警告：无法确认验收容器已移除", file=sys.stderr)
        elif create_attempted:
            _docker_control(container_name, runner=runner, action="rm")
        _restore_project_access(access_snapshot)


def _wait_or_kill(process: subprocess.Popen) -> None:
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        try:
            process.wait(timeout=2)
        except subprocess.SubprocessError:
            pass


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    subparsers = parser.add_subparsers(dest="action", required=True)
    subparsers.add_parser("check", help="仅执行本地资源与挂载边界预检")
    run_parser = subparsers.add_parser("run", help="预检后启动一个一次性隔离容器")
    run_parser.add_argument("--dry-run", action="store_true", help="打印受限 Docker 命令，不启动容器")
    run_parser.add_argument("command", nargs=argparse.REMAINDER, help="容器内命令，放在 -- 之后")
    args = parser.parse_args(argv)

    try:
        config = load_config(args.config)
        if args.action == "check":
            temp_root, project_dir = preflight(config)
            print(json.dumps({"ok": True, "tempRoot": str(temp_root), "projectDir": str(project_dir), "image": config["image"]}, ensure_ascii=False))
            return 0
        command = list(args.command)
        if command and command[0] == "--":
            command.pop(0)
        return run_container(config, command, dry_run=args.dry_run)
    except PreflightError as error:
        print(f"验收预检失败: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
