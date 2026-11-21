import copy
import contextlib
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock


SCRIPT = Path(__file__).with_name("agent_acceptance.py")
SPEC = importlib.util.spec_from_file_location("agent_acceptance", SCRIPT)
agent_acceptance = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(agent_acceptance)


class AgentAcceptancePreflightTests(unittest.TestCase):
    def setUp(self):
        self._temp_root = tempfile.TemporaryDirectory(
            prefix="agent-acceptance-test-",
            dir=agent_acceptance.APPROVED_TEMP_ROOT,
        )
        self.addCleanup(self._temp_root.cleanup)
        self.config = copy.deepcopy(agent_acceptance.load_config())
        self.config["tempRoot"] = self._temp_root.name
        self.config["projectDir"] = str(Path(self._temp_root.name) / "project")

    class FinishedProcess:
        def __init__(self, exit_code=0):
            self.exit_code = exit_code
            self.waited = False

        def poll(self):
            return self.exit_code

        def wait(self, timeout=None):
            self.waited = True
            return self.exit_code

        def kill(self):
            self.exit_code = -9

    class RunningProcess(FinishedProcess):
        def __init__(self):
            super().__init__(None)

        def poll(self):
            return None

    def test_low_disk_rejects_before_runner_is_called(self):
        runner = Mock()
        with self.assertRaisesRegex(agent_acceptance.PreflightError, "磁盘空间不足"):
            agent_acceptance.run_container(
                self.config,
                ["python3", "-c", "pass"],
                available_disk_bytes=1,
                available_memory_bytes=4 * 1024**3,
                runner=runner,
            )
        runner.assert_not_called()

    def test_production_project_mount_is_rejected_before_runner_is_called(self):
        config = copy.deepcopy(self.config)
        config["projectDir"] = str(Path(__file__).resolve().parents[1])
        runner = Mock()
        with self.assertRaisesRegex(agent_acceptance.PreflightError, "projectDir"):
            agent_acceptance.run_container(
                config,
                ["python3", "-c", "pass"],
                available_disk_bytes=4 * 1024**3,
                available_memory_bytes=4 * 1024**3,
                runner=runner,
            )
        runner.assert_not_called()

    def test_additional_mount_configuration_is_rejected(self):
        config = copy.deepcopy(self.config)
        config["mounts"] = ["/var/run/docker.sock:/var/run/docker.sock"]
        with self.assertRaisesRegex(agent_acceptance.PreflightError, "额外挂载"):
            agent_acceptance.validate_config(config)

    def test_docker_invocation_has_bounded_tmpfs_and_no_host_mount(self):
        temp_root, project_dir = agent_acceptance.validate_config(self.config)
        temp_root.mkdir(parents=True, exist_ok=True)
        project_dir.mkdir(parents=True, exist_ok=True)
        command = agent_acceptance.docker_command(
            self.config,
            ["python3", "-c", "print('ok')"],
            container_name="exm-agent-acceptance-test",
        )
        joined = " ".join(command)
        self.assertEqual(command[1], "create")
        self.assertIn("--network=none", command)
        self.assertIn("--read-only", command)
        self.assertIn(f"--pids-limit={self.config['processLimit']}", command)
        self.assertIn(f"--memory={self.config['memoryLimitBytes']}", command)
        self.assertIn(f"--memory-swap={self.config['memoryLimitBytes']}", command)
        self.assertIn(f"--cpus={self.config['cpuLimit']}", command)
        uid, gid = agent_acceptance._container_identity()
        self.assertIn(
            f"--tmpfs=/workspace:rw,nosuid,nodev,size={self.config['projectDiskLimitBytes']},uid={uid},gid={gid},mode=0770",
            command,
        )
        self.assertNotIn(str(project_dir), joined)
        self.assertNotIn("--mount=", joined)
        self.assertNotIn("/var/run/docker.sock", joined)
        self.assertNotIn(str(Path(__file__).resolve().parents[1]), joined)
        self.assertIn("--name=exm-agent-acceptance-test", command)

    def test_project_is_copied_in_and_results_are_recovered_before_cleanup(self):
        container_id = "0123456789abcdef"

        def run_docker(argv, **_kwargs):
            if argv[:2] == ["docker", "cp"] and argv[3].startswith(f"{container_id}:"):
                result_dir = Path(argv[4])
                result_dir.mkdir(parents=True, exist_ok=True)
                (result_dir / "acceptance-result.txt").write_text("real output", encoding="utf-8")
            if argv[:2] == ["docker", "create"]:
                return subprocess.CompletedProcess(argv, 0, stdout=container_id, stderr="")
            return subprocess.CompletedProcess(argv, 0)

        runner = Mock(side_effect=run_docker)
        process_factory = Mock(return_value=self.FinishedProcess())
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            result = agent_acceptance.run_container(
                self.config,
                ["python3", "-c", "pass"],
                available_disk_bytes=4 * 1024**3,
                available_memory_bytes=4 * 1024**3,
                runner=runner,
                process_factory=process_factory,
            )
        self.assertEqual(result, 0)
        self.assertEqual(runner.call_args_list[0].args[0][1], "create")
        self.assertEqual(runner.call_args_list[1].args[0][:3], ["docker", "cp", "--archive"])
        self.assertEqual(
            process_factory.call_args.args[0][:4],
            ["docker", "start", "--attach", container_id],
        )
        self.assertEqual(runner.call_args_list[2].args[0][:3], ["docker", "cp", "--archive"])
        self.assertEqual(runner.call_args_list[3].args[0], ["docker", "rm", "--force", container_id])
        evidence = json.loads(output.getvalue())
        result_dir = Path(evidence["resultDir"])
        self.assertEqual(evidence["containerExitCode"], 0)
        self.assertEqual(evidence["projectBytes"], len("real output"))
        self.assertEqual((result_dir / "acceptance-result.txt").read_text(encoding="utf-8"), "real output")

    def test_timeout_kills_and_removes_created_container(self):
        config = copy.deepcopy(self.config)
        config["timeoutSeconds"] = 1
        container_id = "0123456789abcdef"
        runner = Mock(
            side_effect=[
                subprocess.CompletedProcess([], 0, stdout=container_id, stderr=""),
                subprocess.CompletedProcess([], 0),  # copy in
                subprocess.CompletedProcess([], 0),  # kill
                subprocess.CompletedProcess([], 0),  # copy partial results
                subprocess.CompletedProcess([], 0),  # rm
            ]
        )
        process = self.RunningProcess()
        process_factory = Mock(return_value=process)
        clock_values = iter([0.0, 0.0, 0.0, 2.0])
        result = agent_acceptance.run_container(
            config,
            ["python3", "-c", "pass"],
            available_disk_bytes=4 * 1024**3,
            available_memory_bytes=4 * 1024**3,
            runner=runner,
            process_factory=process_factory,
            clock=lambda: next(clock_values),
            sleeper=lambda _duration: None,
        )
        self.assertEqual(result, 124)
        self.assertEqual(runner.call_args_list[2].args[0], ["docker", "kill", container_id])
        self.assertTrue(process.waited)
        self.assertEqual(
            runner.call_args_list[3].args[0][:3],
            ["docker", "cp", "--archive"],
        )
        self.assertEqual(
            runner.call_args_list[4].args[0],
            ["docker", "rm", "--force", container_id],
        )

    def test_source_larger_than_disk_limit_rejects_before_container_creation(self):
        config = copy.deepcopy(self.config)
        config["projectDiskLimitBytes"] = 1024 * 1024
        with tempfile.TemporaryDirectory(
            prefix="agent-acceptance-source-limit-",
            dir=agent_acceptance.APPROVED_TEMP_ROOT,
        ) as temp_root:
            project_dir = Path(temp_root) / "project"
            project_dir.mkdir()
            (project_dir / "oversized.bin").write_bytes(b"x" * (2 * 1024 * 1024))
            config["tempRoot"] = str(temp_root)
            config["projectDir"] = str(project_dir)
            runner = Mock()
            with self.assertRaisesRegex(agent_acceptance.PreflightError, "超过大小上限"):
                agent_acceptance.run_container(
                    config,
                    ["python3", "-c", "pass"],
                    available_disk_bytes=4 * 1024**3,
                    available_memory_bytes=4 * 1024**3,
                    runner=runner,
                )
            runner.assert_not_called()


if __name__ == "__main__":
    unittest.main()
