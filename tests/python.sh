#!/bin/sh
set -eu

# Prefer the package-manager Python on macOS. The system Python is an
# initialization/runtime dependency only and may be an old Apple-provided
# interpreter without tomllib (Python 3.11+). Keep a portable fallback for CI
# and Linux hosts where Homebrew is not installed.

python_bin=""
for candidate in \
	/opt/homebrew/bin/python \
	/opt/homebrew/bin/python3 \
	/opt/homebrew/bin/python3.14 \
	/opt/homebrew/bin/python3.13 \
	/opt/homebrew/bin/python3.12 \
	/opt/homebrew/bin/python3.11 \
	/usr/local/bin/python \
	/usr/local/bin/python3 \
	/usr/local/bin/python3.14 \
	/usr/local/bin/python3.13 \
	/usr/local/bin/python3.12 \
	/usr/local/bin/python3.11 \
	"$(command -v python 2>/dev/null || true)" \
	"$(command -v python3 2>/dev/null || true)"; do
	[ -n "$candidate" ] && [ -x "$candidate" ] || continue
	if "$candidate" -c 'import sys; raise SystemExit(sys.version_info < (3, 11))' >/dev/null 2>&1; then
		python_bin=$candidate
		break
	fi
done

[ -n "$python_bin" ] || {
	echo "tmux-agent-workbench: Python 3.11+ is required for verification" >&2
	exit 1
}

# Test helpers must not leave bytecode caches in the checkout.
exec "$python_bin" -B "$@"
