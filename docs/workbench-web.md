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

The gateway listens on IPv4 loopback. The default listener is
127.0.0.1:7080 and the default browser origin is http://localhost:7080.
In a separate terminal under the same account, issue an invitation for
the first device.

    tmux-agent-workbench web invite

For a custom state directory or browser origin, pass the gateway's --state
or --origin value to the invite command.

Open the resulting link and create a Passkey using the device's screen lock.
Invitations expire after ten minutes and are consumed by successful enrollment.
Treat an unused invitation like a credential. For another device, open
Profile → Passkeys on a signed-in device and choose **Set up another device**.
Copy the link to the device you want to set up and open it in Chrome.

## Language

The gateway selects the Web interface language when it starts. Set LOCALE to a
language tag or a POSIX locale to choose explicitly.

    LOCALE=zh_CN.UTF-8 tmux-agent-workbench web serve

With LOCALE unset or set to auto, the gateway checks
TMUX_AGENT_WORKBENCH_LANGUAGE, then the system message locale. LC_ALL has
precedence over LC_MESSAGES and LANG. LANGUAGE can select the display language
for a locale other than C or POSIX. Chinese locale variants use Simplified
Chinese. Unset or unsupported locales use English. Restart the gateway after
changing its locale, then close and reopen Workbench in the browser or installed
app to load the updated shell. CLI prompts and diagnostics use English.

## Use Android over SSH

Keep the default listener and browser origin on the gateway. In Termux,
use an existing SSH route to the gateway that permits local TCP forwarding.
Replace gateway-host with the SSH host or alias used on the phone.

    ssh -NT -o ExitOnForwardFailure=yes \
      -L 127.0.0.1:7080:127.0.0.1:7080 gateway-host

With the tunnel running, open http://localhost:7080 in Chrome on the phone.
Use that exact origin for the invitation and subsequent visits. Chrome treats
localhost as a secure context for Service Workers and WebAuthn.

Complete Passkey enrollment and choose Add to home screen, then Install in
Chrome. You can also use the installation control in Profile → Settings. Keep the
tunnel running whenever you open or use the installed app. The service worker
caches the app shell. Conversation history and terminal data are fetched on
demand.

## Use a browser HTTPS origin

When deploying behind an HTTPS reverse proxy, choose a stable browser origin
and a certificate trusted by the browser. Keep the gateway on loopback.

    tmux-agent-workbench web serve \
      --listen 127.0.0.1:7080 \
      --origin https://workbench.example.net \
      --name my-devbox

Forward HTTP and WebSocket traffic for that origin to the loopback listener.
Issue the Passkey invitation against the same HTTPS origin. When the loopback
hop needs TLS, provide both --cert and --key on the gateway.

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

The browser uses the address configured with --origin. Node transport uses
the connector's own loopback HTTP address within the SSH tunnel. Workbench
also supports optional Machine Fabric inventory hints. Pass
--fabric-node node-a=executor-id to the gateway to show the named host as
pending before it pairs. Fabric-provisioned SSH forwards can carry the
connector using the same loopback address.

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
