"""Verify a CLI Voice package with real local tools and a localhost model fixture.

Run with the package smoke dependencies installed. No account or voice session
is used; all state lives in a disposable directory.
"""

from pathlib import Path
import sys, os, json, tempfile

repo = Path(__file__).resolve().parents[3]
sys.path[:0] = [
    str(repo / "sdk/python/src"),
    str(repo / "sdk/python/tests"),
    str(repo / "scripts/codex_package/smoke_tests"),
]
from openai_codex import Codex, CodexConfig, ApprovalMode, Sandbox
from app_server_harness import (
    MockResponsesServer,
    ev_response_created,
    ev_completed,
    sse,
)
from fixtures import SmokePackage
from test_codex_package import test_packaged_executor_initializes_and_enforces_sandbox

package = Path(sys.argv[1]).resolve(strict=True)
with tempfile.TemporaryDirectory(prefix="codex-tool-verification-") as tmp:
    directory = Path(tmp)
    home = directory / "config"
    home.mkdir()
    env = dict(
        os.environ,
        CODEX_HOME=str(home),
        ZDOTDIR=str(directory),
        CODEX_REALTIME_VERBATIM_SPEECH="0",
        NO_PROXY="127.0.0.1,localhost",
    )
    env.pop("BASH_ENV", None)
    smoke = SmokePackage(
        "aarch64-apple-darwin",
        package / "bin/codex",
        package,
        package / "codex-path",
        package / "bin/codex",
        package,
        package / "codex-path",
        directory,
        env,
    )
    test_packaged_executor_initializes_and_enforces_sandbox(smoke, "cli_root")
    print(
        "PASS: executor initializes, writes permitted file, blocks forbidden write and executes sandboxed process",
        flush=True,
    )
    with MockResponsesServer() as server:
        (home / "config.toml").write_text(f'''model = "gpt-5.5"
model_provider = "package_smoke"
approval_policy = "never"
sandbox_mode = "workspace-write"
suppress_unstable_features_warning = true
[sandbox_workspace_write]
network_access = true
[features]
code_mode_only = true
code_mode_host = true
memories = false
apps = false
plugins = false
[analytics]
enabled = false
[otel]
exporter = "none"
trace_exporter = "none"
metrics_exporter = "none"
[model_providers.package_smoke]
name = "local package verification"
base_url = "{server.url}/v1"
wire_api = "responses"
request_max_retries = 0
stream_max_retries = 0
''')
        command = {
            "cmd": "printf tool-command-ok; command -v rg; rg --version",
            "login": False,
            "yield_time_ms": 10000,
        }
        add = "*** Begin Patch\n*** Add File: tool-proof.txt\n+created by packaged Codex\n*** End Patch\n"
        change = "*** Begin Patch\n*** Update File: tool-proof.txt\n@@\n-created by packaged Codex\n+modified by packaged Codex\n*** End Patch\n"
        code = (
            "text(await tools.exec_command("
            + json.dumps(command)
            + "));\ntext(await tools.apply_patch("
            + json.dumps(add)
            + "));\ntext(await tools.apply_patch("
            + json.dumps(change)
            + '));\ntext(await tools.exec_command({cmd:"cat tool-proof.txt",login:false,yield_time_ms:10000}));'
        )
        server.enqueue_sse(
            sse(
                [
                    ev_response_created("tools-check"),
                    {
                        "type": "response.output_item.done",
                        "item": {
                            "type": "custom_tool_call",
                            "call_id": "tools-proof",
                            "name": "exec",
                            "input": code,
                        },
                    },
                    ev_completed("tools-check"),
                ]
            )
        )
        server.enqueue_assistant_message(
            "Tool verification complete", response_id="tools-done"
        )
        config = CodexConfig(
            codex_bin=str(package / "bin/codex"), cwd=str(directory), env=env
        )
        with Codex(config=config) as client:
            turn = client.thread_start(
                ephemeral=True,
                approval_mode=ApprovalMode.deny_all,
                sandbox=Sandbox.workspace_write,
            ).run("Verify packaged tools using local fixture")
            assert turn.final_response == "Tool verification complete", turn
        outputs = [
            i
            for r in server.requests()
            if r.path == "/v1/responses"
            for i in r.input()
            if i.get("type") == "custom_tool_call_output"
            and i.get("call_id") == "tools-proof"
        ]
        assert outputs, "Missing tool result"
        assert "Script completed" in json.dumps(outputs), (
            "Packaged tool execution failed"
        )
        assert (
            directory / "tool-proof.txt"
        ).read_text() == "modified by packaged Codex\n"
        assert "tool-command-ok" in json.dumps(outputs)
        assert str(package / "codex-path/rg") in json.dumps(outputs)
        print(
            "PASS: packaged CLI -> code-mode host -> executor; shell, bundled rg, file creation, patch modification and readback",
            flush=True,
        )
