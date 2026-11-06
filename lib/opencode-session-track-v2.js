// tmux-agent-workbench-v1: OpenCode 2 foreground TUI session tracker.
// Server events also describe background/child sessions, so only the TUI's
// current route is authoritative for the conversation displayed in this pane.
import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { homedir } from "node:os";
import { dirname } from "node:path";

export default {
  id: "workbench-session-tracker-v2",
  setup(context) {
    if (!process.env.TMUX_PANE) return;
    const dir = process.env.TMUX_ASSISTANT_RESURRECT_DIR ||
      `${process.env.HOME || homedir()}/.local/state/tmux-assistant-resurrect`;
    const path = `${dir}/opencode-${process.pid}.json`;
    const instance = randomUUID();
    let previous = "";
    let capture = [];
    try {
      const raw = execFileSync("tmux", ["show-option", "-gqv", "@assistant-resurrect-capture-env"],
        { encoding: "utf8", timeout: 2000, stdio: ["ignore", "pipe", "ignore"] }).trim();
      if (raw) capture = raw.split(/\s+/).filter((name) => /^[A-Za-z_][A-Za-z0-9_]*$/.test(name));
    } catch { /* tmux option unavailable */ }

    const removeOwned = () => {
      try {
        if (JSON.parse(readFileSync(path, "utf8")).workbench_instance === instance) unlinkSync(path);
      } catch { /* absent or owned by another instance */ }
      previous = "";
    };
    const update = () => {
      try {
        const route = context.ui.router.current();
        if (route?.type !== "session" || typeof route.sessionID !== "string" || !route.sessionID) {
          removeOwned();
          return;
        }
        const selected = context.ui.model.current();
        const model = selected?.providerID && selected?.modelID ? `${selected.providerID}/${selected.modelID}` : "";
        const env = { tmux_pane: process.env.TMUX_PANE || "", shell: process.env.SHELL || "" };
        for (const name of capture) env[name] = process.env[name] || "";
        const payload = { tool: "opencode", session_id: route.sessionID, pid: process.pid,
          cwd: context.location?.directory || process.cwd(), model, env, workbench_instance: instance };
        const content = JSON.stringify(payload);
        if (content === previous && existsSync(path)) return;
        if (!existsSync(dir)) {
          mkdirSync(dirname(dir), { recursive: true });
          try { mkdirSync(dir, { mode: 0o700 }); }
          catch (error) { if (error.code !== "EEXIST") throw error; }
        }
        const temp = `${path}.${randomUUID()}.tmp`;
        try {
          writeFileSync(temp, content, { mode: 0o600, flag: "wx" });
          renameSync(temp, path);
          previous = content;
        } finally {
          try { unlinkSync(temp); } catch { /* renamed or never created */ }
        }
      } catch { /* tracking must never interrupt the TUI */ }
    };
    update();
    const timer = setInterval(update, 250);
    timer.unref?.();
    process.on("exit", removeOwned);
    return () => {
      clearInterval(timer);
      process.off("exit", removeOwned);
      removeOwned();
    };
  },
};
