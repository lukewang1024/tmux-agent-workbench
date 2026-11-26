const callTypes = new Set(['function_call', 'custom_tool_call']);
const outputTypes = new Set(['function_call_output', 'custom_tool_call_output']);
const turnBoundaries = new Set(['task_started', 'task_complete', 'turn_started', 'turn_completed', 'turn_aborted', 'user_message']);
const toolName = /^[\w.:-]{1,150}$/;
const maxInvocation = 12_000;

function execInvocation(item) {
  let text;
  const name = item.name.replace(/^functions\./, '');
  if (name === 'exec' && item.type === 'custom_tool_call') text = item.input;
  if (name === 'exec_command' && item.type === 'function_call' && typeof item.arguments === 'string') {
    try { text = JSON.parse(item.arguments)?.cmd; } catch {}
  }
  if (typeof text !== 'string' || !text.trim()) return {};
  return { invocation: text.slice(0, maxInvocation), ...(text.length > maxInvocation && { invocationTruncated: true }) };
}

// History pages are scanned from the latest bytes toward the earliest messages.
export class HistoryTools {
  #calls = [];
  #outputs = new Map();
  #following;
  #currentTurn;
  runningNames = [];

  constructor(latest) { this.#currentTurn = latest; }

  #attach(message) {
    if (message && this.#calls.length) message.tools = [...this.#calls.reverse(), ...(message.tools || [])];
    this.#calls = [];
  }

  observe(row, message) {
    const item = row?.payload;
    if (message?.role === 'user' || row?.type === 'event_msg' && turnBoundaries.has(item?.type)) {
      this.finish();
      this.#following = undefined;
      this.#outputs.clear();
      this.#currentTurn = false;
      return;
    }
    if (message?.role === 'assistant') {
      this.#attach(message);
      this.#following = message;
      if (message.phase === 'final_answer') this.#currentTurn = false;
      return;
    }
    if (row?.type !== 'response_item' || typeof item?.call_id !== 'string' || !item.call_id) return;
    if (outputTypes.has(item.type)) {
      this.#outputs.set(item.call_id, Date.parse(row.timestamp));
      return;
    }
    if (!callTypes.has(item.type) || typeof item.name !== 'string' || !toolName.test(item.name)) return;
    const finished = this.#outputs.has(item.call_id);
    const elapsed = this.#outputs.get(item.call_id) - Date.parse(row.timestamp);
    const state = finished ? 'finished' : this.#currentTurn ? 'running' : 'unconfirmed';
    this.#outputs.delete(item.call_id);
    if (state === 'running') this.runningNames.push(item.name);
    this.#calls.push({ name: item.name, state, ...(elapsed >= 0 && { elapsed }), ...execInvocation(item) });
  }

  finish() { this.#attach(this.#following); }
}
