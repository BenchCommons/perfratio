// Three phases of sleep; only the middle one is the region "middle".
//
// Run: perfratio --region=middle node example.js   (expected region time: 0.3 s)
// Modes: example.js [exit|throw]   "exit" calls process.exit(3) and "throw" throws, both
// inside the region (flush on exit and on error).
'use strict';
const pr = require('./perfratio_region');

const sleep = (seconds) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, seconds * 1000);
const mode = process.argv[2];

sleep(0.1);
pr.region('middle', () => {
  sleep(0.3);
  if (mode === 'exit') process.exit(3);
  if (mode === 'throw') throw new Error('boom');
});
sleep(0.1);
