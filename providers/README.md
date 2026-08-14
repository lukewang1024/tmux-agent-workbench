# distributed-workbench provider

`workbench-tmux-provider` exposes `workspace.inspect`, `agent.launch`, and
`logs.open` without coupling the Fabric Core to tmux. Domain adapters invoke
these capabilities through an Executor and never call tmux directly.

