// The entry. main.ts is a symlink to this file: node runs the entry by its real path, so it loads this as plain
// CommonJS and never starts its TypeScript stripper (amaro, a ~3 MB WebAssembly module that costs ~30 ms of CPU on
// a .ts entry, more than the whole native build). Should the link ever arrive flattened into a copy of this file,
// the copy still runs as main.ts, only paying the stripper like any .ts entry.
//
// Fast path: process.dlopen() bin/sitegen.node, whose ELF constructor execve()s the native engine bin/sitegen (named in
// $SITEGEN_EXEC); the engine replaces this node process and never comes back. That skips process.execve's own JS side
// (it loads internal/worker first), and a failed exec only makes dlopen throw ("did not self-register") instead of
// aborting node. Next, node's process.execve, guarded (a failed execve aborts node, so every way it could fail is
// checked first); last, the pure-JS port in solve.js.
const fs = require('node:fs');
const bin = __dirname + '/bin/sitegen';

function tryShim() {
  process.env.SITEGEN_EXEC = bin;
  let execTried = false;
  try {
    process.dlopen({ exports: {} }, bin + '.node');
  } catch (e) {
    // the object loaded and ran, so the engine itself could not be exec'd: process.execve would fail the same way
    execTried = String(e && e.message).includes('self-register');
  }
  delete process.env.SITEGEN_EXEC;
  return execTried;
}

function tryExecve() {
  if (typeof process.execve !== 'function' || process.platform !== 'linux' || process.arch !== 'x64') return;
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

if (!process.env.SITEGEN_NO_NATIVE && !tryShim()) tryExecve();

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
