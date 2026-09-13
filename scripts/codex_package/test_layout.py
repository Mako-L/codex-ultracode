#!/usr/bin/env python3

from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.layout import build_package_dir
from codex_package.layout import validate_package_dir
from codex_package.targets import PACKAGE_VARIANTS
from codex_package.targets import PackageInputs
from codex_package.targets import TARGET_SPECS


class PackageLayoutTest(unittest.TestCase):
    def test_macos_package_preserves_prebuilt_resource_binaries(self) -> None:
        for variant_name in ("codex", "codex-app-server"):
            for target in ("aarch64-apple-darwin", "x86_64-apple-darwin"):
                with self.subTest(variant=variant_name, target=target):
                    with tempfile.TemporaryDirectory() as temp_dir:
                        root = Path(temp_dir)
                        package_dir = root / "package"
                        package_dir.mkdir()
                        rg_bin = touch_executable(root / "signed-rg")
                        zsh_bin = touch_executable(root / "signed-zsh")
                        rg_bin.write_bytes(b"signed ripgrep binary")
                        zsh_bin.write_bytes(b"signed zsh binary")
                        workflow_runtime_dir = make_workflow_runtime(root)
                        variant = PACKAGE_VARIANTS[variant_name]
                        spec = TARGET_SPECS[target]
                        inputs = PackageInputs(
                            entrypoint_bin=touch_executable(
                                root / variant.executable_stem
                            ),
                            code_mode_host_bin=touch_executable(
                                root / "codex-code-mode-host"
                            ),
                            workflow_runtime_dir=workflow_runtime_dir,
                            node_bin=touch_executable(root / "node"),
                            rg_bin=rg_bin,
                            zsh_bin=zsh_bin,
                            bwrap_bin=None,
                            codex_command_runner_bin=None,
                            codex_windows_sandbox_setup_bin=None,
                        )

                        build_package_dir(package_dir, "1.2.3", variant, spec, inputs)
                        validate_package_dir(
                            package_dir, variant, spec, include_zsh=True
                        )

                        self.assertEqual(
                            {
                                "rg": (package_dir / "codex-path" / "rg").read_bytes(),
                                "zsh": (
                                    package_dir
                                    / "codex-resources"
                                    / "zsh"
                                    / "bin"
                                    / "zsh"
                                ).read_bytes(),
                            },
                            {
                                "rg": b"signed ripgrep binary",
                                "zsh": b"signed zsh binary",
                            },
                        )

    def test_app_server_package_places_code_mode_host_beside_entrypoint(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            package_dir = root / "package"
            package_dir.mkdir()
            inputs = PackageInputs(
                entrypoint_bin=touch_executable(root / "codex-app-server"),
                code_mode_host_bin=touch_executable(root / "codex-code-mode-host"),
                workflow_runtime_dir=make_workflow_runtime(root),
                node_bin=touch_executable(root / "node"),
                rg_bin=touch_executable(root / "rg"),
                zsh_bin=None,
                bwrap_bin=touch_executable(root / "bwrap"),
                codex_command_runner_bin=None,
                codex_windows_sandbox_setup_bin=None,
            )

            build_package_dir(
                package_dir,
                "1.2.3",
                PACKAGE_VARIANTS["codex-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-musl"],
                inputs,
            )
            validate_package_dir(
                package_dir,
                PACKAGE_VARIANTS["codex-app-server"],
                TARGET_SPECS["x86_64-unknown-linux-musl"],
                include_zsh=False,
            )

            self.assertTrue((package_dir / "bin" / "codex-code-mode-host").is_file())
            self.assertTrue(
                (
                    package_dir
                    / "codex-resources"
                    / "workflow-runtime"
                    / "bin"
                    / "ultracode.mjs"
                ).is_file()
            )
            self.assertFalse(
                (
                    package_dir
                    / "codex-resources"
                    / "workflow-runtime"
                    / ".codex-plugin"
                ).exists()
            )
            self.assertFalse(
                (
                    package_dir / "codex-resources" / "workflow-runtime" / "skills"
                ).exists()
            )


def touch_executable(path: Path) -> Path:
    path.touch(mode=0o755)
    return path


def make_workflow_runtime(root: Path) -> Path:
    runtime = root / "workflow-runtime"
    (runtime / "bin").mkdir(parents=True)
    (runtime / "src").mkdir()
    (runtime / "node_modules").mkdir()
    (runtime / "bin" / "ultracode.mjs").write_text("runtime", encoding="utf-8")
    (runtime / "src" / "runtime.mjs").write_text("runtime", encoding="utf-8")
    (runtime / "package.json").write_text("{}\n", encoding="utf-8")
    (runtime / "package-lock.json").write_text("{}\n", encoding="utf-8")
    (runtime / ".codex-plugin").mkdir()
    (runtime / "skills").mkdir()
    return runtime


if __name__ == "__main__":
    unittest.main()
