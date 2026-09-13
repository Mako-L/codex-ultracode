# Codex workflow runtime

This is the Codex-owned workflow engine. The native CLI ships this directory's
runtime files and dependencies with a Node executable in
`codex-resources/workflow-runtime`. It does not load an installed Ultracode plugin,
plugin skills, a plugin checkout, or a global Node installation.

The internal `bin/ultracode.mjs bridge` entrypoint preserves the existing native
host protocol. Codex supplies the parent authority, worker execution, controls,
consent, and built-in authoring instructions. Workflow scripts are discovered in
project and user directories; installed plugins may contribute optional templates.

Install locked build dependencies with `npm ci`; run engine tests with `npm test`.
Package assembly copies `bin`, `src`, `package.json`, `package-lock.json`, and
`node_modules`, plus the target's Node executable. Tests and optional plugin
manifests, skills, and templates are not runtime resources.

`UPSTREAM.json` records the initial source import and per-file hashes. This source
tree is maintained with Codex; subsequent engine changes do not require a plugin
release or installation. The standalone Ultracode plugin remains a separate
delivery for official Codex.
