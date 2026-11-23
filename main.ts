// Entry: hand the whole job to the native binary bin/sitegen (process.execve replaces this node process), and only
// when it cannot run here fall back to the pure-JS port in solve.js. A failed execve aborts node instead of throwing,
// so every way it could fail is checked first: the API, the platform, the exec bit and the ELF header (x86_64).
const fs = require('node:fs');

function tryNative() {
  if (typeof process.execve !== 'function' || process.platform !== 'linux' || process.arch !== 'x64') return;
  const bin = __dirname + '/bin/sitegen';
  try {
    fs.accessSync(bin, fs.constants.X_OK);
    const fd = fs.openSync(bin, 'r');
    const h = Buffer.alloc(20);
    const n = fs.readSync(fd, h, 0, 20, 0);
    fs.closeSync(fd);
    if (n !== 20 || h[0] !== 0x7f || h[1] !== 0x45 || h[2] !== 0x4c || h[3] !== 0x46 || h[4] !== 2 || h[18] !== 0x3e || h[19] !== 0) return;
  } catch {
    return;
  }
  process.execve(bin, [bin], process.env);
}

if (!process.env.SITEGEN_NO_NATIVE) tryNative();

const { setFlagsFromString } = require('node:v8');
setFlagsFromString('--no-incremental-marking');

const { run } = require('./solve.js');

const inDir = process.env.MAKEFUN_INPUT_DIR ?? '/input';
const outDir = process.env.MAKEFUN_OUTPUT_DIR ?? '/output';

const cases = JSON.parse(fs.readFileSync(inDir + '/cases.json', 'utf8'));
const n = cases.length;
const fd = fs.openSync(outDir + '/answers.json', 'w');
fs.writeSync(fd, '[');
for (let i = 0; i < n; i++) {
  if (i > 0) fs.writeSync(fd, ',');
  const inp = cases[i].input;
  cases[i] = null;
  fs.writeSync(fd, JSON.stringify(run(inp)));
}
fs.writeSync(fd, ']');
fs.closeSync(fd);
