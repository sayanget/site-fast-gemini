# fast-makefun-site

[![make.fun · site-generator-speed-01-run-20261122](https://make.fun/badge/site-generator-speed-01-run-20261122.svg)](https://make.fun/bounty/site-generator-speed-01-run-20261122?utm_source=github&utm_medium=readme&utm_campaign=site-generator-speed-01-run-20261122)
[![rank](https://make.fun/badge/site-generator-speed-01-run-20261122/sayanget/site-fast-gemini/rank.svg)](https://make.fun/bounty/site-generator-speed-01-run-20261122/leaderboard?utm_source=github&utm_medium=readme&utm_campaign=site-generator-speed-01-run-20261122)
[![reward](https://make.fun/badge/site-generator-speed-01-run-20261122/sayanget/site-fast-gemini/reward.svg)](https://make.fun/bounty/site-generator-speed-01-run-20261122?utm_source=github&utm_medium=readme&utm_campaign=site-generator-speed-01-run-20261122)

Built for the [make.fun](https://make.fun) bounty **[Build a static site faster, 22 November 2026](https://make.fun/bounty/site-generator-speed-01-run-20261122?utm_source=github&utm_medium=readme&utm_campaign=site-generator-speed-01-run-20261122)**.


Fast, dependency-free JavaScript drop-in replacement for `makefun-site` 1.0.0.

## Architecture

- `solve.js`: ES module exporting `run(input)` with exact byte-for-byte parity with `makefun-site` 1.0.0:
  - Single-pass offset-based front matter parsing without splitting the full Markdown body.
  - Single-pass Markdown block + inline renderer with lookup-table scanning, slice-based HTML escaping, and cached relative link resolution.
  - ASCII fast-path slugification with per-case memoization and zero-allocation word counting.
  - Static template include inlining, site-wide partial evaluation / constant folding, text coalescing, and pure loop-include memoization (e.g. post cards shared across index and tag pages).
- `main.ts`: Minimal driver reading `cases.json`, running `run(c.input)` per case, and writing `answers.json`.
