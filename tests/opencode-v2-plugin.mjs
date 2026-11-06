import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = mkdtempSync(join(tmpdir(), "wb-opencode-v2-plugin-"));
const oldPath = process.env.PATH;
try {
  const bin = join(root, "bin"); mkdirSync(bin);
  writeFileSync(join(bin, "tmux-agent-workbench"), `#!/usr/bin/env node
const fs=require('fs');let data='';process.stdin.on('data',x=>data+=x);
process.stdin.on('end',()=>{
 if(process.argv[2]==='hook'){
  fs.appendFileSync(process.env.WB_EVENTS,JSON.stringify({event:process.argv[5],payload:JSON.parse(data)})+'\\n');
  console.log(JSON.stringify({hookSpecificOutput:{additionalContext:'WB_V2_CONTEXT'}}));
 }else console.log(fs.readFileSync(process.env.WB_STATUS,'utf8'));
});
`, { mode: 0o755 });
  const version = join(bin, "version");
  writeFileSync(version, '#!/bin/sh\nprintf "opencode v2.0.25\\n"\n', { mode: 0o755 });
  process.env.PATH = bin + ":" + oldPath;
  process.env.WB_EVENTS = join(root, "events"); process.env.WB_STATUS = join(root, "status");
  writeFileSync(process.env.WB_STATUS, JSON.stringify({ pending: [{ id: "ticket", owner_session: "root", issued_at_ms: 10 }] }));
  const rule = (action, effect, resource = "*") => ({ action, resource, effect });
  const parent = [rule("*", "allow"), rule("read", "ask", "*.env"), rule("edit", "deny", "/secret/*"), rule("question", "allow")];
  let child = [rule("*", "allow"), rule("read", "ask", "*.env"), rule("edit", "deny", "/secret/*"), rule("question", "deny")];
  const hooks = {};
  const ctx = {
    session: { hook: async (name, fn) => { hooks[name] = fn; }, get: async ({ sessionID }) => sessionID === "child"
      ? { id: "child", parentID: "root", agent: "general", time: { created: 20 }, permissions: [] }
      : { id: sessionID, permissions: [] } },
    agent: { list: async () => ({ data: [{ id: "build", permissions: parent }, { id: "general", permissions: child }] }) },
    tool: { hook: async (name, fn) => { hooks[name] = fn; } },
    event: { subscribe: ({ signal }) => ({ async *[Symbol.asyncIterator]() { await new Promise(resolve => signal.addEventListener("abort", resolve, { once: true })); } }) },
  };
  const source = readFileSync(new URL("../lib/opencode-workbench-v2.js", import.meta.url), "utf8");
  const original = await import("data:text/javascript;base64," + Buffer.from(source).toString("base64"));
  await original.default.setup(ctx); // A shared or unverified server registers nothing.
  assert.deepEqual(Object.keys(hooks), []);
  const testSource = source.replace('const owner = standaloneOwner();', 'const owner = process.pid;').replace('process.execPath', JSON.stringify(version));
  const plugin = await import("data:text/javascript;base64," + Buffer.from(testSource).toString("base64"));
  const cleanup = await plugin.default.setup(ctx);
  await hooks.prompt({ sessionID: "child" });
  await hooks.prompt({ sessionID: "root" });
  const context = { sessionID: "root", agent: "build", system: [] };
  await hooks.context(context);
  assert.deepEqual(context.system, [{ type: "text", text: "WB_V2_CONTEXT" }]);
  const events = readFileSync(process.env.WB_EVENTS, "utf8").trim().split("\n").map(JSON.parse);
  assert.deepEqual(events.map(x => x.event), ["SessionStart", "UserPromptSubmit"]);
  assert.ok(events.every(x => x.payload.session_id === "root" && x.payload.agent_pid === process.pid));
  const task = (input = {}) => hooks["execute.before"]({ tool: "subagent", sessionID: "root", agent: "build", input: { agent: "general", ...input } });
  await task();
  await hooks.prompt({ sessionID: "child" });
  child = [rule("*", "allow"), rule("read", "ask", "*.env"), rule("edit", "allow", "/secret/*")];
  await assert.rejects(task(), /no broader permissions/);
  await assert.rejects(hooks.prompt({ sessionID: "child" }), /no broader permissions/);
  await assert.rejects(task({ sessionID: "old" }), /no broader permissions/);
  await assert.rejects(task({ background: true }), /no broader permissions/);
  await assert.rejects(task({ model: "custom/model" }), /no broader permissions/);
  writeFileSync(process.env.WB_STATUS, JSON.stringify({ pending: [] }));
  await task({ sessionID: "ordinary-child" });
  cleanup();
  console.log("PASS OpenCode v2 plugin: verified owner, native child permissions and fail-closed cases");
} finally {
  process.env.PATH = oldPath;
  delete process.env.WB_EVENTS; delete process.env.WB_STATUS;
  rmSync(root, { recursive: true, force: true });
}
