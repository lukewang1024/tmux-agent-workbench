// tmux-agent-workbench-v1
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { execFile, spawn } from "node:child_process";

// A v2 shared service may serve several terminals. A private stdio server has
// one identifiable --standalone parent; the daemon still verifies that exact
// parent PID against its live tmux inventory before accepting any report.
function standaloneOwner() {
  if (process.platform !== "linux" || !process.argv.includes("serve") || !process.argv.includes("--stdio")) return;
  try {
    const args = readFileSync(`/proc/${process.ppid}/cmdline`, "utf8").split("\0").filter(Boolean);
    if (!/^opencode(?:\.exe)?$/i.test(basename(args[0] || "")) || !args.includes("--standalone")) return;
    if (args.slice(1, 4).some(arg => ["run", "serve", "acp", "--server"].includes(arg))) return;
    return process.ppid;
  } catch (_) { return; }
}

// v2 rules use last-match-wins. Only removing allows and adding denies can
// make this sequence narrower for every possible action and resource.
function noBroader(parent, child) {
  const valid = rules => Array.isArray(rules) && rules.every(rule =>
    rule && Object.keys(rule).sort().join(",") === "action,effect,resource" &&
    typeof rule.action === "string" && typeof rule.resource === "string" &&
    ["allow", "ask", "deny"].includes(rule.effect));
  if (!valid(parent) || !valid(child) || parent.length > 1024 || child.length > 1024) return false;
  const same = (a, b) => a.action === b.action && a.resource === b.resource && a.effect === b.effect;
  const seen = new Set(["0:0"]), queue = [[0, 0]];
  for (let k = 0; k < queue.length; k++) {
    const [i, j] = queue[k];
    if (i === parent.length && j === child.length) return true;
    const next = [];
    if (i < parent.length && parent[i].effect === "allow") next.push([i + 1, j]);
    if (j < child.length && child[j].effect === "deny") next.push([i, j + 1]);
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
    const finish = (error, value) => {
      if (done) return;
      done = true; clearTimeout(timer); error ? reject(error) : resolve(value);
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

export default {
  id: "tmux-agent-workbench-v2",
  async setup(ctx) {
    const owner = standaloneOwner();
    if (!owner) return;
    let session, primaryAgent, context = "", ready = false, versionCheck;
    const supported = () => versionCheck ||= new Promise(resolve => {
      execFile(process.execPath, ["--version"], { timeout: 2000, maxBuffer: 1024 },
        (error, stdout) => resolve(!error && /^(?:opencode v)?2\.0\.25$/.test(stdout.trim())));
    });
    const send = (event, id) => workbench(["hook", "ingest", "opencode", event], {
      session_id: id, agent_pid: owner,
    });
    const pending = async () => {
      let status;
      try { status = await workbench(["auto-split", "status"]); }
      catch (_) { throw new Error("Workbench cannot verify native subagent permissions; retry after checking Workbench."); }
      if (!Array.isArray(status.pending)) throw new Error("Invalid Workbench split status.");
      return status.pending.find(item => item.owner_session === session);
    };
    const unavailable = ticket => `Workbench cannot prove the native subagent has no broader permissions on this OpenCode version. Run tmux-agent-workbench auto-split finish --ticket ${ticket.id} --result unavailable, then continue without moving windows.`;
    const permissions = async (ticket, parentName, childName, childSession) => {
      if (!parentName || !childName || !(await supported())) throw new Error(unavailable(ticket));
      try {
        const [parent, result] = await Promise.all([ctx.session.get({ sessionID: session }), ctx.agent.list()]);
        const agents = result?.data;
        if (!parent || parent.id !== session || parent.parentID || !Array.isArray(agents)) throw new Error();
        const from = agents.find(agent => agent.id === parentName);
        const to = agents.find(agent => agent.id === childName);
        const parentRules = parent.permissions ?? [], childRules = childSession?.permissions ?? [];
        if (!from || !to || !Array.isArray(parentRules) || !Array.isArray(childRules)) throw new Error();
        if (!noBroader([...from.permissions, ...parentRules], [...to.permissions, ...childRules])) throw new Error();
      } catch (_) { throw new Error(unavailable(ticket)); }
    };

    await ctx.session.hook("prompt", async event => {
      let info;
      try { info = await ctx.session.get({ sessionID: event.sessionID }); }
      catch (_) {
        if (session && ready && event.sessionID !== session && await pending()) throw new Error("Workbench cannot verify the child session.");
        return;
      }
      if (!info || info.id !== event.sessionID) {
        if (session && ready && event.sessionID !== session && await pending()) throw new Error("Workbench cannot verify the child session.");
        return;
      }
      if (info.parentID) {
        if (session && ready && info.parentID === session) {
          const ticket = await pending();
          if (ticket) {
            if (ticket.manager_managed === true) throw new Error("Workbench's single manager session owns this split; do not start another agent.");
            const created = typeof info.time?.created === "number" ? info.time.created : Date.parse(info.time?.created);
            if (!Number.isFinite(ticket.issued_at_ms) || !Number.isFinite(created) || created < ticket.issued_at_ms) throw new Error(unavailable(ticket));
            await permissions(ticket, primaryAgent, info.agent, info);
          }
        }
        return;
      }
      if (session && session !== event.sessionID) return;
      try {
        if (!session) { session = event.sessionID; await send("SessionStart", session); }
        const result = await send("UserPromptSubmit", session);
        context = result?.hookSpecificOutput?.additionalContext || "";
        ready = true;
      } catch (_) { context = ""; }
    });
    await ctx.session.hook("context", async event => {
      if (event.sessionID !== session) return;
      primaryAgent = event.agent;
      if (context) event.system.push({ type: "text", text: context });
    });
    await ctx.tool.hook("execute.before", async event => {
      if (event.tool !== "subagent" || !session || !ready || event.sessionID !== session) return;
      const ticket = await pending();
      if (!ticket) return;
      if (ticket.manager_managed === true) throw new Error("Workbench's single manager session owns this split; do not start another agent.");
      const args = event.input || {};
      if (args.sessionID || args.background === true || args.model) throw new Error(unavailable(ticket));
      await permissions(ticket, event.agent || primaryAgent, args.agent);
    });

    const controller = new AbortController();
    void (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          if (!session || event?.data?.sessionID !== session) continue;
          const mapped = { "session.step.started": "busy", "session.execution.succeeded": "idle", "session.execution.failed": "session-error", "session.execution.interrupted": "idle", "permission.asked": "permission-asked" }[event.type];
          if (mapped) try { await send(mapped, session); } catch (_) {}
        }
      } catch (_) {}
    })();
    return () => controller.abort();
  },
};
