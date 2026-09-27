# Integrated assistant session persistence

This directory contains the assistant-session persistence implementation
integrated into `tmux-agent-workbench` from the upstream
`timvw/tmux-assistant-resurrect` project at commit
`f11ca73cc5591546fb20654dbe6378021ff9cd72`.

Workbench owns the runtime entrypoints and tmux hooks. The vendored scripts
keep the existing `assistant-sessions.json` schema and state paths so existing
resurrect data remains restorable. Workbench's native lifecycle hooks are the
preferred source for Claude, Codex, TraeX, and OpenCode session identities;
the vendored detectors remain the compatibility fallback for the other
supported assistants.

The upstream license is preserved in `LICENSE`.
