# fast-makefun-site

Fast, dependency-free JavaScript drop-in replacement for `makefun-site` 1.0.0.

## Architecture

- `solve.js`: ES module exporting `run(input)` with exact byte-for-byte parity with `makefun-site` 1.0.0:
  - Single-pass offset-based front matter parsing without splitting the full Markdown body.
  - Single-pass Markdown block + inline renderer with lookup-table scanning, slice-based HTML escaping, and cached relative link resolution.
  - ASCII fast-path slugification with per-case memoization and zero-allocation word counting.
  - Static template include inlining, site-wide partial evaluation / constant folding, text coalescing, and pure loop-include memoization (e.g. post cards shared across index and tag pages).
- `main.ts`: Minimal driver reading `cases.json`, running `run(c.input)` per case, and writing `answers.json`.
