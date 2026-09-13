"""Command-line interface for building Codex package directories."""

import argparse
from collections.abc import Iterator
from contextlib import contextmanager
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from .archive import write_archive
from .cargo import build_source_binaries
from .layout import build_package_dir
from .layout import prepare_package_dir
from .layout import validate_package_dir
from .ripgrep import resolve_rg_bin
from .targets import PACKAGE_VARIANTS
from .targets import TARGET_SPECS
from .targets import PackageInputs
from .targets import TargetSpec
from .targets import default_target
from .targets import resolve_input_path
from .targets import resolve_node_bin
from .targets import WORKFLOW_RUNTIME_SOURCE_DIR
from .zsh import resolve_zsh_bin
from .version import read_workspace_version


# Release pipelines run this builder with system Python, so avoid new dependencies.
SEMVER_PATTERN = re.compile(
    r"(?P<major>0|[1-9][0-9]*)\."
    r"(?P<minor>0|[1-9][0-9]*)\."
    r"(?P<patch>0|[1-9][0-9]*)"
    r"(?:-(?P<prerelease>[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)


def parse_package_version(value: str) -> str:
    match = SEMVER_PATTERN.fullmatch(value)
    if match is not None:
        components = (match.group(name) for name in ("major", "minor", "patch"))
        prerelease = match.group("prerelease") or ""
        if all(int(component) <= 2**64 - 1 for component in components) and all(
            not (part.isdigit() and len(part) > 1 and part.startswith("0"))
            for part in prerelease.split(".")
        ):
            return value

    raise argparse.ArgumentTypeError(f"invalid semantic version: {value!r}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build a canonical Codex package directory and optional archive.",
        formatter_class=argparse.ArgumentDefaultsHelpFormatter,
    )
    parser.add_argument(
        "--target",
        default=argparse.SUPPRESS,
        choices=sorted(TARGET_SPECS),
        help=(
            "Rust target triple for the package. Defaults to the release target "
            "for this host platform."
        ),
    )
    parser.add_argument(
        "--variant",
        choices=sorted(PACKAGE_VARIANTS),
        default="codex",
        help="Package variant to build.",
    )
    parser.add_argument(
        "--package-version",
        type=parse_package_version,
        default=read_workspace_version(),
        help="Semantic version to record in codex-package.json.",
    )
    parser.add_argument(
        "--package-dir",
        type=Path,
        default=argparse.SUPPRESS,
        help=(
            "Output directory to create as the package root. Defaults to a new temporary directory."
        ),
    )
    parser.add_argument(
        "--archive-output",
        type=Path,
        action="append",
        default=[],
        help=(
            "Optional archive output path. May be repeated. Supported suffixes: "
            ".tar.gz, .tgz, .tar.zst, .zip."
        ),
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="Replace an existing package directory or archive output.",
    )
    parser.add_argument(
        "--cargo",
        default="cargo",
        help="Cargo executable to use for source-built package artifacts.",
    )
    parser.add_argument(
        "--cargo-profile",
        default="dev-small",
        help=(
            "Cargo profile for source-built package artifacts. Use release for release packages."
        ),
    )
    parser.add_argument(
        "--npm",
        default="npm",
        help="Npm executable used to install workflow runtime dependencies.",
    )
    parser.add_argument(
        "--node-bin",
        type=Path,
        help=(
            "Optional prebuilt Node executable bundled with workflow runtime. "
            "Required for non-host targets."
        ),
    )
    parser.add_argument(
        "--entrypoint-bin",
        type=Path,
        help=(
            "Optional prebuilt entrypoint executable for the selected package "
            "variant. If omitted, the entrypoint is built with Cargo."
        ),
    )
    parser.add_argument(
        "--code-mode-host-bin",
        type=Path,
        help=(
            "Optional prebuilt codex-code-mode-host executable. If omitted, "
            "the host is built with Cargo."
        ),
    )
    parser.add_argument(
        "--bwrap-bin",
        type=Path,
        help=(
            "Optional prebuilt Linux bwrap executable. If omitted for Linux "
            "targets, bwrap is built with Cargo."
        ),
    )
    zsh_source = parser.add_mutually_exclusive_group()
    zsh_source.add_argument(
        "--zsh-manifest",
        type=Path,
        help=(
            "Optional DotSlash manifest for the patched zsh fork instead of "
            "scripts/codex_package/codex-zsh."
        ),
    )
    zsh_source.add_argument(
        "--zsh-bin",
        type=Path,
        help="Optional prebuilt zsh executable instead of fetching from a manifest.",
    )
    parser.add_argument(
        "--codex-command-runner-bin",
        type=Path,
        help=(
            "Optional prebuilt Windows codex-command-runner.exe executable. "
            "If omitted for Windows targets, codex-command-runner is built "
            "with Cargo."
        ),
    )
    parser.add_argument(
        "--codex-windows-sandbox-setup-bin",
        type=Path,
        help=(
            "Optional prebuilt Windows codex-windows-sandbox-setup.exe "
            "executable. If omitted for Windows targets, "
            "codex-windows-sandbox-setup is built with Cargo."
        ),
    )
    parser.add_argument(
        "--rg-bin",
        type=Path,
        help=(
            "Optional local ripgrep executable override instead of fetching from "
            "scripts/codex_package/rg."
        ),
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    spec = TARGET_SPECS[getattr(args, "target", None) or default_target()]
    variant = PACKAGE_VARIANTS[args.variant]
    package_dir_arg = getattr(args, "package_dir", None)
    package_dir = (
        package_dir_arg.resolve()
        if package_dir_arg is not None
        else Path(tempfile.mkdtemp(prefix="codex-package-")).resolve()
    )

    node_bin = resolve_node_bin(spec, args.node_bin)
    validate_node_bin(spec, node_bin)
    source_outputs = build_source_binaries(
        spec,
        variant,
        cargo=args.cargo,
        profile=args.cargo_profile,
        entrypoint_bin=resolve_optional_input_path(
            args.entrypoint_bin,
            "prebuilt entrypoint executable",
            "--entrypoint-bin",
        ),
        code_mode_host_bin=resolve_optional_input_path(
            args.code_mode_host_bin,
            "prebuilt code-mode host executable",
            "--code-mode-host-bin",
        ),
        bwrap_bin=resolve_optional_input_path(
            args.bwrap_bin,
            "prebuilt Linux bwrap executable",
            "--bwrap-bin",
        ),
        codex_command_runner_bin=resolve_optional_input_path(
            args.codex_command_runner_bin,
            "prebuilt Windows codex-command-runner.exe executable",
            "--codex-command-runner-bin",
        ),
        codex_windows_sandbox_setup_bin=resolve_optional_input_path(
            args.codex_windows_sandbox_setup_bin,
            "prebuilt Windows codex-windows-sandbox-setup.exe executable",
            "--codex-windows-sandbox-setup-bin",
        ),
    )
    with prepare_workflow_runtime(
        WORKFLOW_RUNTIME_SOURCE_DIR,
        npm=args.npm,
    ) as workflow_runtime_dir:
        inputs = PackageInputs(
            entrypoint_bin=source_outputs.entrypoint_bin,
            code_mode_host_bin=source_outputs.code_mode_host_bin,
            workflow_runtime_dir=workflow_runtime_dir,
            node_bin=node_bin,
            rg_bin=resolve_rg_bin(spec, args.rg_bin),
            zsh_bin=resolve_zsh_bin(spec, args.zsh_manifest, zsh_bin=args.zsh_bin),
            bwrap_bin=source_outputs.bwrap_bin,
            codex_command_runner_bin=source_outputs.codex_command_runner_bin,
            codex_windows_sandbox_setup_bin=source_outputs.codex_windows_sandbox_setup_bin,
        )
        prepare_package_dir(package_dir, force=args.force)
        build_package_dir(package_dir, args.package_version, variant, spec, inputs)
        validate_package_dir(
            package_dir, variant, spec, include_zsh=inputs.zsh_bin is not None
        )

        for archive_output in args.archive_output:
            archive_path = archive_output.resolve()
            write_archive(package_dir, archive_path, force=args.force)
            print(f"Built Codex package archive at {archive_path}")

        print(f"Built Codex package directory at {package_dir}")
    return 0


@contextmanager
def prepare_workflow_runtime(source_dir: Path, *, npm: str) -> Iterator[Path]:
    if not source_dir.is_dir():
        raise RuntimeError(f"Workflow runtime source does not exist: {source_dir}")
    with tempfile.TemporaryDirectory(prefix="codex-workflow-runtime-") as staging_root:
        staged_dir = Path(staging_root) / source_dir.name
        for directory_name in ("bin", "src"):
            shutil.copytree(source_dir / directory_name, staged_dir / directory_name)
        for file_name in ("package.json", "package-lock.json"):
            shutil.copyfile(source_dir / file_name, staged_dir / file_name)
        subprocess.run([npm, "ci", "--ignore-scripts"], cwd=staged_dir, check=True)
        yield staged_dir


def validate_node_bin(spec: TargetSpec, node_bin: Path) -> None:
    if spec.target != default_target():
        return
    with tempfile.TemporaryDirectory(prefix="codex-node-preflight-") as staging_root:
        staged_node = Path(staging_root) / spec.node_name
        shutil.copy2(node_bin, staged_node)
        try:
            result = subprocess.run(
                [staged_node, "--version"],
                check=True,
                capture_output=True,
                text=True,
            )
        except (OSError, subprocess.CalledProcessError) as error:
            raise RuntimeError(
                f"Node executable cannot run after relocation: {node_bin}. "
                "Use a self-contained --node-bin."
            ) from error
    version = result.stdout.strip()
    match = re.match(r"^v?(\d+)(?:\.|$)", version)
    if match is None or int(match.group(1)) < 24:
        raise RuntimeError(
            f"Node executable must be version 24 or newer: {node_bin}. "
            "Use --node-bin with a supported Node executable."
        )


def resolve_optional_input_path(
    explicit_path: Path | None,
    description: str,
    flag_name: str,
) -> Path | None:
    if explicit_path is None:
        return None

    return resolve_input_path(explicit_path, description, flag_name)
