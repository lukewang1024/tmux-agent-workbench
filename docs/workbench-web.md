# Workbench Web

Workbench Web is an optional, single-operator PWA distributed in the
tmux-agent-workbench source tree. It uses the Workbench snapshot for running
Agent identity, titles, tmux grouping and status. The gateway joins the local
tmux server with any machines you explicitly pair.

The Agent view opens the original Codex conversation when the Workbench snapshot
identifies its thread. History comes from the Codex rollout on that machine.
Other Agent types can be opened in their terminal. Chat sends into a live Codex
pane when its verified thread is idle or done. For a permission prompt, open
the terminal and respond there.

## Start a local preview

Install the Workbench core and use its launcher from the same checkout. The Web
module requires Node.js 22 or newer and the dependencies in its lockfile.

    npm ci --prefix web
    tmux-agent-workbench web serve --name my-devbox

Run the service as the user who owns the tmux server. To choose a specific
server, pass its absolute Unix socket to --socket. Inside tmux, get the current
path with tmux display-message -p '#{socket_path}'.

The default listener is 127.0.0.1:7080 and the local browser origin is
http://localhost:7080. In a separate terminal under the same account, issue
an invitation:

    tmux-agent-workbench web invite --origin http://localhost:7080

Open the resulting link and create a Passkey using the device's screen lock.
Invitations expire after ten minutes and are consumed by successful enrollment.
Treat an unused invitation like a credential. For additional devices, use the
Passkey page in the PWA or issue another invitation from the gateway account.

## Deploy for Android

Choose a stable HTTPS origin reachable by the Android browser. Use a certificate
trusted on the phone. A private network or VPN can limit who reaches the
endpoint. The origin determines the Passkey relying party and PWA identity.

A gateway behind a reverse proxy can listen on loopback:

    tmux-agent-workbench web serve \
      --listen 127.0.0.1:7080 \
      --origin https://workbench.example.net \
      --name my-devbox

Forward HTTP and WebSocket traffic for that origin to the loopback listener.
For direct TLS, set --cert and --key to the certificate chain and private key,
and choose an appropriate listener. Issue the Passkey invitation against the
same HTTPS origin.

Open the HTTPS link in Chrome on Android, complete enrollment, then choose
Install app from the Chrome menu or use the installation control in
我的 → 设置. The installed app opens in a standalone window. The service
worker caches the application shell. Conversation history and terminal data
are fetched on demand.

## Pair a remote host

Issue a separate invitation for each machine on the gateway:

    tmux-agent-workbench web node invite node-a

On that machine, install the same Workbench checkout and Web dependencies.
Run the connector as the operating-system user who owns the tmux server:

    tmux-agent-workbench web node join --gateway https://workbench.example.net
    tmux-agent-workbench web node run

The join command reads its invitation from standard input. It stores a
machine-specific credential in the private Web state directory. The connector
initiates an outbound WebSocket, keeps it alive, and reconnects when the route
returns. Run it as a user service on Linux or as a LaunchAgent on macOS where
appropriate. The process needs the Workbench launcher, Node.js, and tmux on PATH.

Hosts that have the Workbench core binary and an existing tmux server can run
the connector with --observe-snapshot. Set WORKBENCH_WEB_CORE to the absolute
core path when running web/cli.mjs directly. Agent inventory in this mode is a
one-shot observation through the Workbench detector, so it works without a
daemon, hooks, or changes to the server's layout and options. The display can
only include thread identity and status that the detector can infer from live
process and pane data.

A machine with an existing SSH route can connect through a loopback forward
to the gateway's listener. For example, running an SSH forward from a remote
host supports a Mac with its SSH server disabled:

    ssh -N -L 17080:127.0.0.1:7080 gateway-host
    tmux-agent-workbench web node join --gateway http://127.0.0.1:17080
    tmux-agent-workbench web node run

The public browser origin remains the configured HTTPS address. Node transport
accepts loopback HTTP within the SSH tunnel. Workbench also supports optional
Machine Fabric inventory hints. Pass --fabric-node node-a=executor-id to the
gateway to show the named host as pending before it pairs. Fabric-provisioned
SSH forwards can carry the connector using the same loopback address.

Use web node list on the gateway to inspect pairings. Use web node revoke NAME
to remove a node's authorization. To pair it again, stop the connector, run
web node forget on that host, and enroll with a fresh invitation. A node that
has previously connected shows as offline when its connection is lost.

## Panes and login

The terminal view starts in watch mode. Enable input to become the single
browser writer for that Pane. You can open a Shell in a dedicated Workbench Web
window, or split the selected task window on the same machine. Independent
Web windows can resize to the terminal viewport. Existing panes follow their
tmux geometry. Reconnecting attaches to the original Pane while it still exists.

The gateway runs with the authority of the tmux user on paired machines. A
signed-in operator can open terminals and send their input. Protect the gateway
account and the Web state directory. By default the state is stored under
$XDG_STATE_HOME/tmux-agent-workbench/web and the CLI accepts a --state override.
Passkey sessions expire and can be revoked. Pairing and login attempts are rate
limited. Browser terminal connections validate their origin and session.

## Verify a local checkout

    npm test --prefix web
    sh tests/p0.sh

The Web tests create and remove disposable tmux servers on isolated sockets.
