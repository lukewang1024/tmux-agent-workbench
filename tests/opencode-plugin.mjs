import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const root = mkdtempSync(join(tmpdir(), "wb-opencode-plugin-"));
const oldPath = process.env.PATH;
try {
  const bin = join(root, "bin");
  mkdirSync(bin);
  writeFileSync(join(bin, "tmux-agent-workbench"), `#!/usr/bin/env node
const fs=require('fs'); let data=''; process.stdin.on('data',x=>data+=x);
process.stdin.on('end',()=>{
  if(process.argv[2]==='hook') {
    fs.appendFileSync(process.env.WB_EVENTS,JSON.stringify({event:process.argv[5],payload:JSON.parse(data)})+'\\n');
    console.log(JSON.stringify({hookSpecificOutput:{additionalContext:'WB_CONTEXT'}}));
  } else if (process.env.WB_STATUS_ERROR) process.exit(1);
  else console.log(fs.readFileSync(process.env.WB_STATUS,'utf8'));
});
`, { mode: 0o755 });
  const version = join(bin, "opencode-version");
  writeFileSync(version, '#!/bin/sh\nprintf "1.18.32\\n"\n', { mode: 0o755 });
  process.env.PATH = bin + ":" + oldPath;
  process.env.WB_EVENTS = join(root, "events");
  process.env.WB_STATUS = join(root, "status");
  writeFileSync(process.env.WB_STATUS, JSON.stringify({ pending: [{ id: "ticket", owner_session: "root", issued_at_ms: 10 }] }));
  const allow = (permission, pattern = "*") => ({ permission, pattern, action: "allow" });
  const deny = (permission, pattern = "*") => ({ permission, pattern, action: "deny" });
  const ask = (permission, pattern = "*") => ({ permission, pattern, action: "ask" });
  const parent = [allow("*"), ask("bash", "*"), deny("edit", "/secret/*"), allow("question")];
  let child = [allow("*"), ask("bash", "*"), deny("edit", "/secret/*"), deny("todowrite")];
  const client = {
    session: { get: async ({ path }) => ({ data: path.id === "child"
      ? { id: "child", parentID: "root", permission: [], time: { created: 20 } } : { id: path.id, permission: [] } }) },
    app: { agents: async () => ({ data: [{ name: "build", permission: parent }, { name: "general", permission: child }] }) },
  };
  const source = readFileSync(new URL("../lib/opencode-workbench.js", import.meta.url), "utf8")
    .replace('process.execPath', JSON.stringify(version));
  const { Workbench } = await import("data:text/javascript;base64," + Buffer.from(source).toString("base64"));
  const plugin = await Workbench({ client, directory: root });
  const chat = id => plugin["chat.message"]({ sessionID: id, agent: "build" }, { message: {} });
  const task = (args = {}) => plugin["tool.execute.before"]({ tool: "task", sessionID: "root" }, { args: { subagent_type: "general", ...args } });
  await chat("child");
  await chat("root");
  await chat("other-root");
  const system = { system: [] };
  await plugin["experimental.chat.system.transform"]({ sessionID: "root" }, system);
  assert.deepEqual(system.system, ["WB_CONTEXT"]);
  await plugin.event({ event: { type: "session.idle", properties: { sessionID: "child" } } });
  const events = readFileSync(process.env.WB_EVENTS, "utf8").trim().split("\n").map(JSON.parse);
  assert.deepEqual(events.map(x => x.event), ["SessionStart", "UserPromptSubmit"]);
  assert.ok(events.every(x => x.payload.session_id === "root" && x.payload.agent_pid === process.pid));
  await task(); // Parent allows were removed and a child deny was added.
  await plugin["chat.message"]({ sessionID: "child", agent: "general" }, { message: {} });
  child = [allow("*"), ask("bash", "*"), allow("edit", "/secret/*")];
  await assert.rejects(task(), /no broader permissions/);
  await assert.rejects(plugin["chat.message"]({ sessionID: "child", agent: "general" }, { message: {} }), /no broader permissions/);
  child = [allow("*"), ask("bash", "*"), deny("edit", "/secret/*")];
  await assert.rejects(task({ task_id: "old-child" }), /no broader permissions/);
  await assert.rejects(task({ background: true }), /no broader permissions/);
  child = [allow("*"), { ...ask("bash"), unexpected: true }, deny("edit", "/secret/*")];
  await assert.rejects(task(), /no broader permissions/);
  process.env.WB_STATUS_ERROR = "1";
  await assert.rejects(task(), /cannot verify/);
  delete process.env.WB_STATUS_ERROR;
  writeFileSync(version, '#!/bin/sh\nprintf "1.18.33\\n"\n', { mode: 0o755 });
  const otherVersion = await Workbench({ client, directory: root });
  await otherVersion["chat.message"]({ sessionID: "root", agent: "build" }, { message: {} });
  await assert.rejects(otherVersion["tool.execute.before"]({ tool: "task", sessionID: "root" }, { args: { subagent_type: "general" } }), /OpenCode version/);
  writeFileSync(process.env.WB_STATUS, JSON.stringify({ pending: [] }));
  await task({ task_id: "ordinary-task" });
  console.log("PASS OpenCode plugin: context, root identity, native worker permissions and fail-closed cases");
} finally {
  process.env.PATH = oldPath;
  delete process.env.WB_EVENTS;
  delete process.env.WB_STATUS;
  delete process.env.WB_STATUS_ERROR;
  rmSync(root, { recursive: true, force: true });
}
