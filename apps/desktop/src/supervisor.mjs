import { SourceRuntime } from './runtime.mjs';
import path from 'node:path';
import { createLogWriter } from './logging.mjs';

let runtime;
let log;
let stopping = false;
function send(value) { if (process.connected) process.send(value); }

async function stop() {
  if (stopping) return;
  stopping = true;
  await runtime?.stop();
  await log?.close();
  process.exit(0);
}

process.on('message', async message => {
  if (message.type === 'stop') return void stop();
  if (message.type !== 'start' || runtime || stopping) return;
  try {
    if (stopping) return;
    log = createLogWriter({ directory: path.dirname(message.logPath), kind: 'runtime' });
    await log.write(`\n--- Desktop runtime ${new Date().toISOString()} ---\n`);
    runtime = new SourceRuntime({ log: chunk => log.write(chunk) });
    runtime.on('status', send);
    await runtime.start(message.config, message.cachePath);
  } catch (error) {
    send({ phase: 'error', message: error.message });
    await runtime?.stop();
  }
});
process.on('disconnect', () => void stop());
for (const signal of ['SIGTERM', 'SIGINT', 'SIGHUP']) process.on(signal, () => void stop());
