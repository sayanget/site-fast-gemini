const { setFlagsFromString } = require('node:v8');
setFlagsFromString('--no-incremental-marking');

const { readFileSync, openSync, writeSync, closeSync } = require('node:fs');
const { run } = require('./solve.js');

const inDir = process.env.MAKEFUN_INPUT_DIR ?? '/input';
const outDir = process.env.MAKEFUN_OUTPUT_DIR ?? '/output';

const cases = JSON.parse(readFileSync(inDir + '/cases.json', 'utf8'));
const n = cases.length;
const fd = openSync(outDir + '/answers.json', 'w');
writeSync(fd, '[');
for (let i = 0; i < n; i++) {
  if (i > 0) writeSync(fd, ',');
  const inp = cases[i].input;
  cases[i] = null;
  writeSync(fd, JSON.stringify(run(inp)));
}
writeSync(fd, ']');
closeSync(fd);
