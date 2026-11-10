// Managed by berger init; forwards reliable Pi lifecycle events.
import { spawn } from "node:child_process";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const berger = __BERGER_BIN__;
let queue = Promise.resolve();

const send = (event: string, isError?: boolean) => {
  queue = queue.then(() => new Promise<void>((resolve) => {
    const p = spawn(berger, ['event'], { detached: true, stdio: ['pipe', 'ignore', 'ignore'] });
    let finished = false;
    const done = () => { if (!finished) { finished = true; clearTimeout(timer); resolve(); } };
    p.on('error', done);
    p.on('close', done);
    p.stdin?.on('error', () => {});
    const timer = setTimeout(() => { p.kill(); done(); }, 1000);
    p.stdin?.end(JSON.stringify({ hook_event_name: event, harness: 'pi', is_error: isError }));
    p.unref();
  }));
  return queue;
};

export default function(pi: ExtensionAPI) {
  let hadError = false;
  pi.on('agent_start', () => { void send('agent_start'); });
  pi.on('ui_prompt_start', (_event, ctx) => { if (!ctx.isIdle()) void send('ui_prompt_start'); });
  pi.on('ui_prompt_end', (_event, ctx) => { if (!ctx.isIdle()) void send('ui_prompt_end'); });
  pi.on('tool_execution_end', (e: any) => {
    if (e?.isError) { hadError = true; void send('tool_execution_end', true); }
  });
  pi.on('agent_settled', (e: any) => {
    const isError = hadError || e?.aborted;
    hadError = false;
    void send('agent_settled', isError);
  });
  pi.on('session_shutdown', async (e: any) => {
    if (e?.reason === 'quit') await send('session_shutdown');
    else await queue;
  });
}
