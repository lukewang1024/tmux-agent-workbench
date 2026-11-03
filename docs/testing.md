# Test levels

Tests are grouped by the cost and failure impact of the behavior they protect.
The required pull-request and main-push check is P0. Broader coverage remains
available in manual CI runs and release validation.

| Level | Coverage | Where it runs |
| --- | --- | --- |
| P0 | Rust library and binary unit tests; CLI startup; task/workspace isolation; pane routing; daemon shutdown | Every PR and main push on Ubuntu; five-minute job timeout |
| P1 | Full Rust suite, tmux UI/lifecycle integration, relay pairing, installer, and formatting | tests/verify.sh; manual CI dispatch on Ubuntu and macOS |
| P2 | iSH i586 static target, Intel macOS title hooks, and release-target compatibility | Manual CI dispatch and release workflow |

P0 protects the common task workflow and the routing/lifecycle boundaries where
a regression can affect another task or leave a background process behind.
P1 and P2 cover broader UI behavior, platform differences, packaging, and less
common integration paths without extending the required PR gate.
