// Persistent mode: a function that is slow in its first calls and fast afterwards.
//
// Run: perfratio --persistent -w 0 -r 40 node persistent_example.js
// Without perfratio the function runs once.
'use strict';
const pr = require('./perfratio_region');

let calls = 0;
const work = () => {
  calls += 1;
  // a cold start that settles after 5 calls, like a JIT-compiled function
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, calls <= 5 ? 40 : 10);
};

pr.serve(work);
