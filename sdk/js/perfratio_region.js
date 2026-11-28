'use strict';
/**
 * perfratio region client for JavaScript and TypeScript (protocol 1, SDK 1.0.0).
 *
 * License: MIT OR Apache-2.0. One file, Node.js 12 or later, no dependencies. Types are in
 * perfratio_region.d.ts.
 *
 *     const pr = require('./perfratio_region');
 *     pr.region('sort', () => sortTheData());      // stops also when the callback throws
 *     // or: pr.start('sort'); ...; pr.stop('sort');
 *
 * Run the program with `perfratio --region=sort node program.js`. Without perfratio
 * (PERFRATIO_REGION_FILE unset or empty) every call returns at once. Events are buffered in
 * memory and written when the process exits (the 'exit' event, so process.exit() and an
 * uncaught exception are covered) or on flush(). A fatal signal or process.abort() skips it:
 * call flush() first when that matters.
 *
 * Persistent mode (`perfratio --persistent node program.js`): serve(fn) calls fn() once per
 * iteration perfratio requests (fn may return a promise) and reports the time of every call.
 * Without perfratio fn runs once. Protocol: docs/specs/persistent-mode.md.
 *
 * Names are 1-64 ASCII letters, digits or `_`; invalid names are ignored with one warning on
 * stderr. Wire format: docs/specs/region-protocol.md.
 */

const fs = require('fs');
const readline = require('readline');

const PROTOCOL = 1;
const SDK_VERSION = '1.0.0';
const SDK_ID = 'javascript/' + SDK_VERSION;
const DEFAULT_REGION = 'default';
const NAME = /^[A-Za-z0-9_]{1,64}$/;

const path = process.env.PERFRATIO_REGION_FILE || null;
const live = process.env.PERFRATIO_REGION_LIVE === '1';
let events = [];
let helloWritten = false;
let warned = false;
let exitHookInstalled = false;

/** Monotonic clock in nanoseconds (an integer). */
function now() {
  return process.hrtime.bigint();
}

/** True when perfratio asked for region events. */
function enabled() {
  return path !== null;
}

function record(kind, name) {
  if (path === null) return;
  // A stop reads the clock first and a start last, so the bookkeeping stays outside the region.
  let t = kind === 'stop' ? now() : 0n;
  if (typeof name !== 'string' || !NAME.test(name)) {
    if (!warned) {
      warned = true;
      process.stderr.write(
        'perfratio_region: ignoring invalid region name ' + JSON.stringify(name) +
          " (use 1-64 letters, digits or '_')\n");
    }
    return;
  }
  if (!exitHookInstalled) {
    exitHookInstalled = true;
    process.on('exit', flush);
  }
  if (kind === 'start') t = now();
  events.push(kind + ' ' + name + ' ' + t.toString() + '\n');
  if (live) flush();
}

/** Open the region `name`. */
function start(name = DEFAULT_REGION) {
  record('start', name);
}

/** Close the region `name`. */
function stop(name = DEFAULT_REGION) {
  record('stop', name);
}

/** Run `fn` inside the region `name`; the region stops also when `fn` throws. Returns fn()'s value. */
function region(name, fn) {
  if (typeof name === 'function') {
    fn = name;
    name = DEFAULT_REGION;
  }
  start(name);
  try {
    return fn();
  } finally {
    stop(name);
  }
}

/** Write the buffered events now. Done automatically at exit. */
function flush() {
  if (path === null) return;
  if (helloWritten && events.length === 0) return;
  const text = (helloWritten ? '' : 'hello ' + PROTOCOL + ' ' + SDK_ID + '\n') + events.join('');
  try {
    fs.appendFileSync(path, text, 'utf8');
  } catch (_) {
    return; // never break the program
  }
  helloWritten = true;
  events = [];
}

function send(output, lines) {
  output.write(lines.map((line) => '@perfratio ' + line + '\n').join(''));
}

/**
 * Persistent mode: call `fn()` for every `run <n>` perfratio sends on stdin and answer on
 * stdout. Resolves on `quit` or at the end of input. An exception (or rejected promise) is
 * reported as a failed iteration and the loop goes on. Without `--persistent`
 * (PERFRATIO_PERSISTENT_PROTOCOL unset) `fn` runs once. `input` and `output` are streams for tests.
 */
async function serve(fn, input, output) {
  if (!input && !output && process.env.PERFRATIO_PERSISTENT_PROTOCOL === undefined) {
    await fn();
    return;
  }
  input = input || process.stdin;
  output = output || process.stdout;
  send(output, ['hello ' + PROTOCOL + ' ' + SDK_ID]);
  const lines = readline.createInterface({ input, crlfDelay: Infinity });
  for await (const request of lines) {
    const words = request.trim().split(/\s+/);
    if (words[0] === 'quit') break;
    if (words[0] !== 'run' || words.length !== 2) continue;
    let failure = null;
    const t0 = now();
    try {
      await fn();
    } catch (error) {
      failure = error;
    }
    const t1 = now();
    if (failure !== null) {
      const text = failure && failure.message !== undefined ? failure.name + ': ' + failure.message : String(failure);
      send(output, ['fail ' + words[1] + ' ' + text.split(/\s+/).join(' ')]);
    } else {
      send(output, [
        'start iteration ' + t0, 'stop iteration ' + t1, 'done ' + words[1]]);
    }
  }
  lines.close();
}

module.exports = {
  PROTOCOL, SDK_VERSION, SDK_ID, DEFAULT_REGION, enabled, start, stop, region, flush, serve,
};
