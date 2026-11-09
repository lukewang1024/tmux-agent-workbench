// tmux-agent-workbench-v1
import { execFile, spawn } from "node:child_process";

// OpenCode applies the last matching rule. Deleting an allow or adding a deny
// cannot make a rule sequence more permissive; unknown rule shapes fail closed.
function noBroader(parent, child) {
  const valid = rules => Array.isArray(rules) && rules.every(rule =>
    rule && Object.keys(rule).sort().join(",") === "action,pattern,permission" &&
    typeof rule.permission === "string" && typeof rule.pattern === "string" &&
    ["allow", "ask", "deny"].includes(rule.action));
  if (!valid(parent) || !valid(child) || parent.length > 1024 || child.length > 1024) return false;
  const same = (a, b) => a.action === b.action && a.pattern === b.pattern && a.permission === b.permission;
  const seen = new Set(["0:0"]), queue = [[0, 0]];
  for (let k = 0; k < queue.length; k++) {
    const [i, j] = queue[k];
    if (i === parent.length && j === child.length) return true;
    const next = [];
    if (i < parent.length && parent[i].action === "allow") next.push([i + 1, j]);
    if (j < child.length && child[j].action === "deny") next.push([i, j + 1]);
    if (i < parent.length && j < child.length && same(parent[i], child[j])) next.push([i + 1, j + 1]);
    for (const point of next) {
      const key = point.join(":");
      if (!seen.has(key)) { seen.add(key); queue.push(point); }
    }
  }
  return false;
}

function workbench(args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn("tmux-agent-workbench", args, { stdio: ["pipe", "pipe", "ignore"] });
    let out = "", done = false;
    const finish = (err, value) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      err ? reject(err) : resolve(value);
    };
    const timer = setTimeout(() => { child.kill(); finish(new Error("Workbench timed out")); }, 2500);
    child.on("error", error => finish(error));
    child.stdin.on("error", () => {});
    child.stdout.on("data", chunk => {
      out += chunk.toString();
      if (out.length > 262144) { child.kill(); finish(new Error("Workbench output too large")); }
    });
    child.on("close", code => {
      if (code !== 0) return finish(new Error("Workbench command failed"));
      try { finish(null, JSON.parse(out)); } catch (_) { finish(new Error("invalid Workbench output")); }
    });
    child.stdin.end(input === undefined ? "" : JSON.stringify(input));
  });
}

const Workbench = async ({ client, directory }) => {
  // Server/attach modes can serve a different client from the inherited tmux
  // environment. Only a directly owned process is eligible.
  const shared = process.argv.slice(1, 4).some(arg => ["serve", "web", "attach", "acp"].includes(arg));
  let session, primaryAgent, context = "", ready = false;
  let versionCheck;
  const supported = () => versionCheck ||= new Promise(resolve => {
    // The permission inheritance proof below is verified against this exact
    // runtime version. Revalidate before allowing a newer implementation.
    execFile(process.execPath, ["--version"], { timeout: 2000, maxBuffer: 1024 },
      (error, stdout) => resolve(!error && stdout.trim() === "1.18.32"));
  });
  const send = (event, id) => workbench(["hook", "ingest", "opencode", event], {
    session_id: id, agent_pid: process.pid, cwd: directory,
  });
  const pending = async () => {
    let status;
    try { status = await workbench(["auto-split", "status"]); }
    catch (_) { throw new Error("Workbench cannot verify native subagent permissions; retry after checking Workbench."); }
    if (!Array.isArray(status.pending)) throw new Error("Invalid Workbench split status.");
    return status.pending.find(item => item.owner_session === session);
  };
  const unavailable = ticket => `Workbench cannot prove the native subagent has no broader permissions on this OpenCode version. Run tmux-agent-workbench auto-split finish --ticket ${ticket.id} --result unavailable, then continue without moving windows.`;
  const permissions = async (ticket, childName, childSession) => {
    if (!primaryAgent || !childName || !(await supported())) throw new Error(unavailable(ticket));
    try {
      const [sessionResult, agentsResult] = await Promise.all([
        client.session.get({ path: { id: session } }), client.app.agents(),
      ]);
      const info = sessionResult.data, agents = agentsResult.data;
      if (!info || info.id !== session || info.parentID || !Array.isArray(agents)) throw new Error();
      const parent = agents.find(item => item.name === primaryAgent);
      const child = agents.find(item => item.name === childName);
      const rules = info.permission ?? [];
      if (!parent || !child || !Array.isArray(rules)) throw new Error();
      const inherited = childSession?.permission ?? rules.filter(rule => rule?.action === "deny" || rule?.permission === "external_directory");
      if (!Array.isArray(inherited) || !noBroader([...parent.permission, ...rules], [...child.permission, ...inherited])) throw new Error();
    } catch (_) { throw new Error(unavailable(ticket)); }
  };
  return {
    "chat.message": async (input, output) => {
      if (shared || !input.sessionID) return;
      let info;
      try {
        info = (await client.session.get({ path: { id: input.sessionID } })).data;
      } catch (_) {
        if (session && ready && input.sessionID !== session && await pending()) throw new Error("Workbench cannot verify the child session.");
        return;
      }
      if (!info || info.id !== input.sessionID) {
        if (session && ready && input.sessionID !== session && await pending()) throw new Error("Workbench cannot verify the child session.");
        return;
      }
      if (info.parentID) {
        if (session && ready && info.parentID === session) {
          const ticket = await pending();
          if (ticket) {
            if (ticket.manager_managed === true) throw new Error("Workbench's single manager session owns this split; do not start another agent.");
            if (!Number.isFinite(ticket.issued_at_ms) || !Number.isFinite(info.time?.created) || info.time.created < ticket.issued_at_ms) throw new Error(unavailable(ticket));
            await permissions(ticket, input.agent || output.message?.agent, info);
          }
        }
        return;
      }
      if (session && session !== input.sessionID) return;
      try {
        if (!session) {
          session = input.sessionID;
          await send("SessionStart", session);
        }
        primaryAgent = input.agent || output.message?.agent;
        const result = await send("UserPromptSubmit", session);
        ready = true;
        context = result?.hookSpecificOutput?.additionalContext || "";
      } catch (_) { context = ""; }
    },
    "experimental.chat.system.transform": async (input, output) => {
      if (input.sessionID === session && context) output.system.push(context);
    },
    event: async ({ event }) => {
      if (!session || event?.properties?.sessionID !== session) return;
      const type = event.type === "session.status" ? event.properties.status?.type : event.type;
      const mapped = { busy: "busy", retry: "busy", idle: "idle", "session.idle": "idle", "session.error": "session-error", "permission.asked": "permission-asked" }[type];
      if (mapped) try { await send(mapped, session); } catch (_) {}
    },
    "tool.execute.before": async (input, output) => {
      if (input.tool !== "task" || !session || !ready || input.sessionID !== session) return;
      const ticket = await pending();
      if (!ticket) return;
      if (ticket.manager_managed === true) throw new Error("Workbench's single manager session owns this split; do not start another agent.");
      const args = output.args || {};
      if (args.task_id || args.background === true) throw new Error(unavailable(ticket));
      await permissions(ticket, args.subagent_type);
    },
  };
};

// v1 loads `server`; v2 accepts the same module without registering v1 hooks.
export default { id: "workbench-v1-compat", server: Workbench, setup: async () => {} };
