#!/usr/bin/env python3

import argparse
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from codex_package.cli import parse_package_version
from codex_package.cli import prepare_workflow_runtime
from codex_package.cli import validate_node_bin
from codex_package.targets import resolve_node_bin
from codex_package.targets import TARGET_SPECS


class PackageVersionTest(unittest.TestCase):
    def test_accepts_release_prerelease_and_build_versions(self) -> None:
        for version in (
            "0.0.0",
            "1.2.3",
            "0.0.0-internal.deadbeef",
            "1.2.3-alpha.1+build.01",
            "18446744073709551615.0.0",
        ):
            with self.subTest(version=version):
                self.assertEqual(parse_package_version(version), version)

    def test_rejects_versions_the_runtime_cannot_parse(self) -> None:
        for version in (
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3-alpha..1",
            "1.2.3-01",
            "1.2.3+",
            "1.2.3+build..1",
            "18446744073709551616.0.0",
        ):
            with self.subTest(version=version):
                with self.assertRaises(argparse.ArgumentTypeError):
                    parse_package_version(version)


class NodePackageTest(unittest.TestCase):
    def test_relocated_host_node_must_still_run(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            source_dir = Path(temp_dir) / "source"
            source_dir.mkdir()
            node = source_dir / "node"
            node.write_text(
                '#!/bin/sh\n[ "$(basename "$(dirname "$0")")" = source ] || exit 42\n'
                "printf 'v24.14.0\\n'\n",
                encoding="utf-8",
            )
            node.chmod(0o755)
            spec = TARGET_SPECS["aarch64-apple-darwin"]
            with patch("codex_package.cli.default_target", return_value=spec.target):
                with self.assertRaisesRegex(RuntimeError, "relocation"):
                    validate_node_bin(spec, node)

    def test_host_node_must_be_version_24_or_newer(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            node = Path(temp_dir) / "node"
            node.write_text("#!/bin/sh\nprintf 'v23.14.0\\n'\n", encoding="utf-8")
            node.chmod(0o755)
            spec = TARGET_SPECS["aarch64-apple-darwin"]
            with patch("codex_package.cli.default_target", return_value=spec.target):
                with self.assertRaisesRegex(RuntimeError, "version 24"):
                    validate_node_bin(spec, node)

    def test_host_node_v24_passes_relocation_preflight(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            node = Path(temp_dir) / "node"
            node.write_text("#!/bin/sh\nprintf 'v24.14.0\\n'\n", encoding="utf-8")
            node.chmod(0o755)
            spec = TARGET_SPECS["aarch64-apple-darwin"]
            with patch("codex_package.cli.default_target", return_value=spec.target):
                validate_node_bin(spec, node)

    def test_cross_target_requires_explicit_node(self) -> None:
        with patch(
            "codex_package.targets.default_target",
            return_value="aarch64-apple-darwin",
        ):
            with self.assertRaisesRegex(RuntimeError, "--node-bin"):
                resolve_node_bin(TARGET_SPECS["x86_64-unknown-linux-musl"], None)

    def test_explicit_node_is_resolved_for_cross_target(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            node = Path(temp_dir) / "node"
            node.touch(mode=0o755)
            resolved = resolve_node_bin(TARGET_SPECS["x86_64-unknown-linux-musl"], node)
            self.assertEqual(resolved, node.resolve())

    def test_workflow_runtime_dependencies_are_staged_with_npm(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "workflow-runtime"
            source.mkdir()
            (source / "bin").mkdir()
            (source / "src").mkdir()
            (source / "test").mkdir()
            (source / ".codex-plugin").mkdir()
            (source / "node_modules").mkdir()
            (source / "node_modules" / "old").write_text("stale", encoding="utf-8")
            (source / "package.json").write_text("{}\n", encoding="utf-8")
            (source / "package-lock.json").write_text("{}\n", encoding="utf-8")
            npm = root / "npm"
            npm.write_text(
                "#!/bin/sh\nmkdir -p node_modules\nprintf installed > node_modules/.marker\n",
                encoding="utf-8",
            )
            npm.chmod(0o755)

            with prepare_workflow_runtime(source, npm=str(npm)) as staged:
                staged_root = staged.parent
                self.assertNotEqual(staged, source)
                self.assertTrue((staged / "node_modules" / ".marker").is_file())
                self.assertFalse((staged / "node_modules" / "old").exists())
                self.assertFalse((staged / "test").exists())
                self.assertFalse((staged / ".codex-plugin").exists())
            self.assertTrue((source / "node_modules" / "old").exists())
            self.assertFalse(staged_root.exists())

    def test_workflow_runtime_staging_cleans_up_when_npm_fails(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            source = root / "workflow-runtime"
            (source / "bin").mkdir(parents=True)
            (source / "src").mkdir()
            (source / "bin" / "workflow.mjs").write_text("runtime", encoding="utf-8")
            (source / "package.json").write_text("{}\n", encoding="utf-8")
            (source / "package-lock.json").write_text("{}\n", encoding="utf-8")
            npm = root / "npm"
            cwd_capture = root / "npm-cwd"
            npm.write_text(
                f"#!/bin/sh\nprintf '%s' \"$PWD\" > '{cwd_capture}'\nexit 7\n",
                encoding="utf-8",
            )
            npm.chmod(0o755)

            with self.assertRaises(subprocess.CalledProcessError):
                with prepare_workflow_runtime(source, npm=str(npm)):
                    self.fail("npm failure should prevent yielding staged runtime")
            staged_root = Path(cwd_capture.read_text(encoding="utf-8")).parent
            self.assertFalse(staged_root.exists())


if __name__ == "__main__":
    unittest.main()
