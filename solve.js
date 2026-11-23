// Fast exact drop-in implementation of makefun-site 1.0.0.
// Dependency-free ES module exporting run(input).

const REPORT_FILE = 'build-report.txt';
const RESERVED = ['posts', 'tags', 'page'];
const SLUG_MAX = 64;
const INCLUDE_DEPTH = 8;

const MONTHS = [
  'January', 'February', 'March', 'April', 'May', 'June',
  'July', 'August', 'September', 'October', 'November', 'December',
];

const ESC_MAP = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

const IS_ESCAPABLE = new Uint8Array(128);
for (const ch of '\\`*_[]()#+-.!>') IS_ESCAPABLE[ch.charCodeAt(0)] = 1;

const IS_SCHEME_CONT = new Uint8Array(128);
for (let c = 48; c <= 57; c++) IS_SCHEME_CONT[c] = 1;
for (let c = 65; c <= 90; c++) IS_SCHEME_CONT[c] = 1;
for (let c = 97; c <= 122; c++) IS_SCHEME_CONT[c] = 1;
IS_SCHEME_CONT[43] = 1;
IS_SCHEME_CONT[46] = 1;
IS_SCHEME_CONT[45] = 1;

const RE_COMBINING = /\p{M}/gu;
const RE_NON_ASCII = /[^\x00-\x7f]/;
const RE_NON_ALNUM = /[^a-z0-9]+/g;
const RE_EDGE_DASH = /^-|-$/g;
const RE_TRAIL_DASH = /-+$/;
const RE_CRLF = /\r\n?/g;
const RE_HTML = /[&<>"']/g;
const RE_HTML_TEST = /[&<>"']/;
const RE_W200Y = /\s*\S+(?:\s+\S+){199}/y;
const RE_W1Y = /\s*\S+/y;
const RE_FM_LINE = /^([a-z_]+):(?: (.*))?$/;
const RE_BLANK = /^[ \t]*$/;
const RE_DATE = /^(\d{4})-(\d{2})-(\d{2})$/;
const RE_LAYOUT = /^[a-z0-9-]+$/;
const RE_SLUG = /^[a-z0-9]+(?:-[a-z0-9]+)*$/;
const RE_TAG_BAD = /["[\]]/;
const RE_TRAIL_ST = /[ \t]+$/;

// Zero-capture inline Markdown Irregexp (100% equivalent to makefun-site's inline()).
const RE_INL = /[&<>"']|\\[\\`*_[\]()#+\-.!>]|`[^`]+`|\*\*(?:\\[\\`*_[\]()#+\-.!>]|\\(?![\\`*_[\]()#+\-.!>])|(?!\*\*)[^\\])+?\*\*|\*\*|\*(?:\\[\\`*_[\]()#+\-.!>]|\\(?![\\`*_[\]()#+\-.!>])|\*\*|[^*\\])+?\*(?!\*)|\[(?:\\[\\`*_[\]()#+\-.!>]|\\(?![\\`*_[\]()#+\-.!>])|[^\]\\])*\]\([^\s)]+\)/g;

// Block Markdown Irregexp; absorbs trailing blank lines into each block match to halve callback count.
const RE_BLOCK = /(?:^[ \t]*\n)+|^[ \t]+(?![\s\S])|^```([A-Za-z0-9_+-]*)[ \t]*(?:\n(?:```[ \t]*(?:\n(?:[ \t]*\n)*|(?![\s\S]))|([\s\S]*?)(?:\n```[ \t]*(?:\n(?:[ \t]*\n)*|(?![\s\S]))|(?![\s\S])))|(?![\s\S]))|^(#{1,6})[ \t]+([^ \t\n](?:[^\n]*[^ \t\n])?)[ \t]*(?:\n(?:[ \t]*\n)*|(?![\s\S]))|^((?:---|\*\*\*))[ \t]*(?:\n(?:[ \t]*\n)*|(?![\s\S]))|^(>[^\n]*(?:\n>[^\n]*)*)(?:\n(?:[ \t]*\n)*|(?![\s\S]))|^([-*] [^\n]*(?:\n(?:[-*] | {2,}\S)[^\n]*)*)(?:\n(?:[ \t]*\n)*|(?![\s\S]))|^((\d{1,9})\. [^\n]*(?:\n(?:\d{1,9}\. | {2,}\S)[^\n]*)*)(?:\n(?:[ \t]*\n)*|(?![\s\S]))|^([^\n]+(?:\n(?![ \t]*(?:\n|(?![\s\S]))|```[A-Za-z0-9_+-]*[ \t]*(?:\n|(?![\s\S]))|#{1,6}[ \t]+[^ \t\n]|(?:---|\*\*\*)[ \t]*(?:\n|(?![\s\S])))[^\n]+)*)(?:\n(?:[ \t]*\n)*|(?![\s\S]))/gm;

const RE_TRIM_LINE = /^[ \t]+|[ \t]+$/gm;
const RE_QUOTE_PREFIX = /^> ?[ \t]*|[ \t]+$/gm;
const RE_LIST_SPLIT = /\n(?=[-*] )/;
const RE_OLIST_SPLIT = /\n(?=\d{1,9}\. )/;
const RE_ORD_PREFIX = /^\d{1,9}\. /;

function byCodeUnits(a, b) {
  return a < b ? -1 : a > b ? 1 : 0;
}

// ---------------------------------------------------------------- text helpers

function escCb(c) {
  return ESC_MAP[c];
}

function escapeHtml(s) {
  return RE_HTML_TEST.test(s) ? s.replace(RE_HTML, escCb) : s;
}

function slugify(text, cache) {
  let out = cache.get(text);
  if (out !== undefined) return out;
  const plain = RE_NON_ASCII.test(text) ? text.normalize('NFKD').replace(RE_COMBINING, '') : text;
  out = plain.toLowerCase().replace(RE_NON_ALNUM, '-').replace(RE_EDGE_DASH, '');
  if (out.length > SLUG_MAX) out = out.slice(0, SLUG_MAX).replace(RE_TRAIL_DASH, '');
  if (out === '') out = 'untitled';
  cache.set(text, out);
  return out;
}

function uniqueSlug(slug, taken) {
  let out = slug;
  if (taken.has(out)) {
    let n = 2;
    do {
      out = slug + '-' + n;
      n++;
    } while (taken.has(out));
  }
  taken.add(out);
  return out;
}

function longDate(date) {
  const m = (date.charCodeAt(5) - 48) * 10 + (date.charCodeAt(6) - 48);
  const d = (date.charCodeAt(8) - 48) * 10 + (date.charCodeAt(9) - 48);
  return d + ' ' + MONTHS[m - 1] + ' ' + date.slice(0, 4);
}

function readingMinutes(s) {
  if (s.length < 401) return 1;
  let mins = 0;
  let pos = 0;
  RE_W200Y.lastIndex = 0;
  while (RE_W200Y.test(s)) {
    mins++;
    pos = RE_W200Y.lastIndex;
  }
  if (mins === 0) return 1;
  RE_W1Y.lastIndex = pos;
  return RE_W1Y.test(s) ? mins + 1 : mins;
}

// ---------------------------------------------------------------- front matter

function daysIn(y, m) {
  if (m === 2) return (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0 ? 29 : 28;
  return m === 4 || m === 6 || m === 9 || m === 11 ? 30 : 31;
}

function parseFmQuoted(raw, key) {
  const len = raw.length;
  if (len < 2 || raw.charCodeAt(len - 1) !== 34) {
    return { error: `key "${key}": a quoted string is not closed` };
  }
  if (raw.indexOf('\\', 1) === -1 && raw.indexOf('"', 1) === len - 1) {
    return { value: raw.slice(1, len - 1) };
  }
  let out = '';
  for (let i = 1; i < len - 1; i++) {
    const c = raw.charCodeAt(i);
    if (c === 92) {
      const n = raw.charCodeAt(i + 1);
      if ((n !== 34 && n !== 92) || i + 1 === len - 1) {
        return { error: `key "${key}": a quoted string holds an escape other than \\" or \\\\` };
      }
      out += n === 34 ? '"' : '\\';
      i++;
    } else if (c === 34) {
      return { error: `key "${key}": a quoted string holds a stray quote` };
    } else {
      out += raw.charAt(i);
    }
  }
  return { value: out };
}

function parseFmValue(key, raw, data) {
  switch (key) {
    case 'title':
    case 'summary': {
      const c0 = raw.charCodeAt(0);
      if (c0 === 34) {
        const res = parseFmQuoted(raw, key);
        if (res.error !== undefined) return res.error;
        data[key] = res.value;
        return null;
      }
      if (c0 === 91) return `key "${key}": a bare string opens on [`;
      data[key] = raw;
      return null;
    }
    case 'date': {
      const m = RE_DATE.exec(raw);
      if (m === null) return 'key "date": not a date from 1970-01-01 to 2099-12-31 (YYYY-MM-DD)';
      const y = Number(m[1]);
      const mo = Number(m[2]);
      const d = Number(m[3]);
      if (y < 1970 || y > 2099 || mo < 1 || mo > 12 || d < 1 || d > daysIn(y, mo)) {
        return 'key "date": not a date from 1970-01-01 to 2099-12-31 (YYYY-MM-DD)';
      }
      data.date = raw;
      return null;
    }
    case 'tags': {
      if (raw.charCodeAt(0) !== 91 || raw.charCodeAt(raw.length - 1) !== 93) {
        return 'key "tags": not a list [a, b]';
      }
      const inner = raw.slice(1, -1);
      if (inner.trim() === '') {
        data.tags = [];
        return null;
      }
      const parts = inner.split(',');
      for (let j = 0; j < parts.length; j++) {
        const t = parts[j].trim();
        if (t === '' || RE_TAG_BAD.test(t)) return 'key "tags": an empty tag, or one holding " [ or ]';
        parts[j] = t;
      }
      data.tags = parts;
      return null;
    }
    case 'draft':
      if (raw === 'true') { data.draft = true; return null; }
      if (raw === 'false') { data.draft = false; return null; }
      return 'key "draft": true or false';
    case 'layout':
      if (!RE_LAYOUT.test(raw)) return 'key "layout": not a template name';
      data.layout = raw;
      return null;
    case 'slug':
      if (raw.length > SLUG_MAX || !RE_SLUG.test(raw)) return 'key "slug": not a slug';
      data.slug = raw;
      return null;
  }
  return `unknown key "${key}"`;
}

function parseFrontMatter(text) {
  const len = text.length;
  if (len < 3 || text.charCodeAt(0) !== 45 || text.charCodeAt(1) !== 45 || text.charCodeAt(2) !== 45) {
    return { error: 'no front matter' };
  }
  if (len === 3) return { error: 'the front matter is not closed' };
  if (text.charCodeAt(3) !== 10) return { error: 'no front matter' };

  let idx = text.indexOf('\n---', 3);
  while (idx !== -1) {
    const after = idx + 4;
    if (after === len || text.charCodeAt(after) === 10) break;
    idx = text.indexOf('\n---', after);
  }
  if (idx === -1) return { error: 'the front matter is not closed' };

  const data = {};
  if (idx > 3) {
    const lines = text.slice(4, idx).split('\n');
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i];
      if (line.length === 0 || line.charCodeAt(0) === 35 || RE_BLANK.test(line)) continue;
      const m = RE_FM_LINE.exec(line);
      if (m === null) return { error: `line ${i + 2} is not "key: value"` };
      const key = m[1];
      if (
        key !== 'title' && key !== 'date' && key !== 'tags' &&
        key !== 'draft' && key !== 'layout' && key !== 'slug' && key !== 'summary'
      ) {
        return { error: `unknown key "${key}"` };
      }
      if (Object.hasOwn(data, key)) return { error: `key "${key}" is given twice` };
      const raw = m[2] === undefined ? '' : m[2].replace(RE_TRAIL_ST, '');
      if (raw === '') return { error: `key "${key}" has no value` };
      const err = parseFmValue(key, raw, data);
      if (err !== null) return { error: err };
    }
  }
  if (!Object.hasOwn(data, 'title')) return { error: 'no title' };
  const closeEnd = idx + 4;
  return { data, bodyStart: closeEnd < len ? closeEnd + 1 : len };
}

// ---------------------------------------------------------------- path & link helpers

function dirOf(path) {
  const idx = path.lastIndexOf('/');
  return idx === -1 ? '' : path.slice(0, idx);
}

function hasScheme(target) {
  const len = target.length;
  if (len < 2) return false;
  const c0 = target.charCodeAt(0);
  if (!((c0 >= 65 && c0 <= 90) || (c0 >= 97 && c0 <= 122))) return false;
  for (let i = 1; i < len; i++) {
    const c = target.charCodeAt(i);
    if (c === 58) return true;
    if (c >= 128 || !IS_SCHEME_CONT[c]) return false;
  }
  return false;
}

function resolvePath(dirParts, rel) {
  const parts = dirParts.slice();
  const len = rel.length;
  let segStart = 0;
  for (let i = 0; i <= len; i++) {
    if (i === len || rel.charCodeAt(i) === 47) {
      const segLen = i - segStart;
      if (segLen === 2 && rel.charCodeAt(segStart) === 46 && rel.charCodeAt(segStart + 1) === 46) {
        if (parts.length === 0) return null;
        parts.pop();
      } else if (segLen > 0 && !(segLen === 1 && rel.charCodeAt(segStart) === 46)) {
        parts.push(rel.slice(segStart, i));
      }
      segStart = i + 1;
    }
  }
  return parts.join('/');
}

function resolveLink(target, st) {
  const c0 = target.charCodeAt(0);
  if (c0 === 47 || c0 === 35 || hasScheme(target)) return target;
  const hash = target.indexOf('#');
  const rel = hash === -1 ? target : target.slice(0, hash);
  if (rel === '') {
    st.broken.add('broken-link ' + st.sPath + ' -> ' + target);
    return target;
  }
  let resolvedBase = st.relCache.get(rel);
  if (resolvedBase === undefined) {
    const rp = resolvePath(st.dirParts, rel);
    if (rp !== null) {
      const targetSrc = st.bySource.get(rp);
      if (targetSrc !== undefined) resolvedBase = targetSrc.url;
      else if (st.staticPaths.has(rp)) resolvedBase = st.base + '/' + rp;
      else resolvedBase = null;
    } else {
      resolvedBase = null;
    }
    st.relCache.set(rel, resolvedBase);
  }
  if (resolvedBase !== null) {
    return hash === -1 ? resolvedBase : resolvedBase + target.slice(hash);
  }
  st.broken.add('broken-link ' + st.sPath + ' -> ' + target);
  return target;
}

// ---------------------------------------------------------------- markdown (Irregexp single-pass)

const mdCtx = {
  linkState: null,
  slugCache: null,
  ids: null,
  toc: null,
  excerpt: null,
};

function findLinkSplit(m) {
  const sp = m.indexOf('](');
  if (sp === m.lastIndexOf('](')) return sp;
  const len = m.length;
  for (let j = 1; j < len; j++) {
    const ch = m.charCodeAt(j);
    if (ch === 92 && j + 1 < len) {
      const nc = m.charCodeAt(j + 1);
      if (nc < 128 && IS_ESCAPABLE[nc]) { j++; continue; }
    }
    if (ch === 93) return j;
  }
  return sp;
}

function inlineCb(m) {
  if (m.length === 1) return ESC_MAP[m];
  const c = m.charCodeAt(0);
  if (c === 42) {
    if (m.charCodeAt(1) === 42) {
      return m.length === 2 ? '**' : '<strong>' + inlineMd(m.slice(2, -2)) + '</strong>';
    }
    return '<em>' + inlineMd(m.slice(1, -1)) + '</em>';
  }
  if (c === 91) {
    const sp = findLinkSplit(m);
    const href = resolveLink(m.slice(sp + 2, -1), mdCtx.linkState);
    return '<a href="' + escapeHtml(href) + '">' + inlineMd(m.slice(1, sp)) + '</a>';
  }
  if (c === 96) return '<code>' + escapeHtml(m.slice(1, -1)) + '</code>';
  return m.charCodeAt(1) === 62 ? '&gt;' : m.charAt(1);
}

function inlineMd(s) {
  return s.replace(RE_INL, inlineCb);
}

function blockCb(m, fLang, fBody, hHash, hText, hr, bq, ul, ol, olStart, para) {
  if (para !== undefined) {
    const h = inlineMd(para.replace(RE_TRIM_LINE, ''));
    if (mdCtx.excerpt === null) mdCtx.excerpt = h;
    return '<p>' + h + '</p>\n';
  }
  if (hHash !== undefined) {
    const d = hHash.length;
    const id = uniqueSlug(slugify(hText, mdCtx.slugCache), mdCtx.ids);
    const h = inlineMd(hText);
    if (d === 2 || d === 3) mdCtx.toc.push({ level: d, id, html: h });
    return '<h' + d + ' id="' + id + '">' + h + '</h' + d + '>\n';
  }
  if (fLang !== undefined) {
    const cls = fLang === '' ? '' : ' class="language-' + fLang + '"';
    return '<pre><code' + cls + '>' + (fBody === undefined ? '' : escapeHtml(fBody) + '\n') + '</code></pre>\n';
  }
  if (ul !== undefined) {
    const items = ul.split(RE_LIST_SPLIT);
    let s = '<ul>\n';
    for (let i = 0; i < items.length; i++) {
      s += '<li>' + inlineMd(items[i].slice(2).replace(RE_TRIM_LINE, '')) + '</li>\n';
    }
    return s + '</ul>\n';
  }
  if (ol !== undefined) {
    const items = ol.split(RE_OLIST_SPLIT);
    const st = Number(olStart);
    let s = st !== 1 ? '<ol start="' + st + '">\n' : '<ol>\n';
    for (let i = 0; i < items.length; i++) {
      s += '<li>' + inlineMd(items[i].replace(RE_ORD_PREFIX, '').replace(RE_TRIM_LINE, '')) + '</li>\n';
    }
    return s + '</ol>\n';
  }
  if (bq !== undefined) {
    const lines = bq.replace(RE_QUOTE_PREFIX, '').split('\n');
    let joined = '';
    for (let i = 0; i < lines.length; i++) {
      if (lines[i] !== '') joined = joined === '' ? lines[i] : joined + '\n' + lines[i];
    }
    return joined === '' ? '<blockquote>\n</blockquote>\n' : '<blockquote>\n<p>' + inlineMd(joined) + '</p>\n</blockquote>\n';
  }
  if (hr !== undefined) return '<hr>\n';
  return '';
}

function renderMarkdown(body, linkState, slugCache) {
  mdCtx.linkState = linkState;
  mdCtx.slugCache = slugCache;
  mdCtx.ids = new Set();
  mdCtx.toc = [];
  mdCtx.excerpt = null;
  const html = body === '' ? '' : body.replace(RE_BLOCK, blockCb);
  const toc = mdCtx.toc;
  const excerpt = mdCtx.excerpt === null ? '' : mdCtx.excerpt;
  mdCtx.ids = null;
  mdCtx.toc = null;
  return { html, toc, excerpt };
}

// ---------------------------------------------------------------- templates (parse + JIT compile)

const RE_PATH = /^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*$/;
const RE_FOR = /^for ([A-Za-z_][A-Za-z0-9_]*) in (\S+)$/;
const RE_IF = /^if (not )?(\S+)$/;
const RE_INCLUDE = /^include "([a-z0-9-]+)"$/;

class TemplateError extends Error {}

function pathOf(name, text) {
  if (!RE_PATH.test(text)) throw new TemplateError(`template "${name}": ${JSON.stringify(text)} is not a path`);
  return text.split('.');
}

function isSpacesTabs(src, from, to) {
  for (let i = from; i < to; i++) {
    const c = src.charCodeAt(i);
    if (c !== 32 && c !== 9) return false;
  }
  return true;
}

function parseTemplate(name, src) {
  const root = { kind: 'root', body: [] };
  const stack = [root];
  let body = root.body;
  let text = '';
  let i = 0;
  const len = src.length;

  while (i < len) {
    const open = src.indexOf('{', i);
    if (open === -1 || open === len - 1) {
      text += src.slice(i);
      break;
    }
    const next = src.charCodeAt(open + 1);
    if (next !== 123 && next !== 37) {
      text += src.slice(i, open + 1);
      i = open + 1;
      continue;
    }
    text += src.slice(i, open);
    if (next === 123) {
      const raw = open + 2 < len && src.charCodeAt(open + 2) === 123;
      const close = src.indexOf(raw ? '}}}' : '}}', open + (raw ? 3 : 2));
      if (close === -1) throw new TemplateError(`template "${name}": a ${raw ? '{{{' : '{{'} is not closed`);
      if (text !== '') { body.push({ kind: 'text', text }); text = ''; }
      body.push({ kind: raw ? 'raw' : 'var', path: pathOf(name, src.slice(open + (raw ? 3 : 2), close).trim()) });
      i = close + (raw ? 3 : 2);
      continue;
    }
    const close = src.indexOf('%}', open + 2);
    if (close === -1) throw new TemplateError(`template "${name}": a {% is not closed`);
    const end = close + 2;
    const lineStart = open === 0 ? 0 : src.lastIndexOf('\n', open - 1) + 1;
    const nl = src.indexOf('\n', end);
    const lineEnd = nl === -1 ? len : nl;
    const standalone = isSpacesTabs(src, lineStart, open) && isSpacesTabs(src, end, lineEnd);
    if (standalone) text = text.slice(0, text.length - (open - lineStart));
    if (text !== '') { body.push({ kind: 'text', text }); text = ''; }
    const tag = src.slice(open + 2, close).trim();
    const top = stack[stack.length - 1];
    let m;
    if ((m = RE_FOR.exec(tag)) !== null) {
      const node = { kind: 'for', name: m[1], path: pathOf(name, m[2]), body: [] };
      body.push(node); stack.push(node); body = node.body;
    } else if ((m = RE_IF.exec(tag)) !== null) {
      const node = { kind: 'if', not: m[1] !== undefined, path: pathOf(name, m[2]), then: [], else: null };
      body.push(node); stack.push(node); body = node.then;
    } else if (tag === 'else') {
      if (top.kind !== 'if' || top.else !== null) throw new TemplateError(`template "${name}": an else outside an if`);
      top.else = []; body = top.else;
    } else if (tag === 'endfor' || tag === 'endif') {
      if (top.kind !== (tag === 'endfor' ? 'for' : 'if')) {
        throw new TemplateError(`template "${name}": an ${tag} with no open ${tag.slice(3)}`);
      }
      stack.pop();
      const parent = stack[stack.length - 1];
      body = parent.kind === 'root' || parent.kind === 'for' ? parent.body : parent.else ?? parent.then;
    } else if ((m = RE_INCLUDE.exec(tag)) !== null) {
      body.push({ kind: 'include', name: m[1] });
    } else {
      throw new TemplateError(`template "${name}": {% ${tag} %} is not a tag`);
    }
    i = standalone ? (nl === -1 ? len : nl + 1) : end;
  }
  if (text !== '') body.push({ kind: 'text', text });
  if (stack.length > 1) throw new TemplateError(`template "${name}": a ${stack[stack.length - 1].kind} is not closed`);
  return root.body;
}

function textOf(v) {
  if (typeof v === 'string') return v;
  if (typeof v === 'number') return String(v);
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (Array.isArray(v)) {
    const n = v.length;
    if (n === 0) return '';
    let out = textOf(v[0]);
    for (let i = 1; i < n; i++) out += ', ' + textOf(v[i]);
    return out;
  }
  return '';
}

function varOut(v) {
  if (typeof v === 'string') return escapeHtml(v);
  if (typeof v === 'number') return String(v);
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (Array.isArray(v)) return escapeHtml(textOf(v));
  return '';
}

function truthy(v) {
  return Array.isArray(v) ? v.length > 0 : v !== undefined && v !== null && v !== false && v !== '' && v !== 0;
}

function ownProp(o, k) {
  return o !== null && typeof o === 'object' && Object.hasOwn(o, k) ? o[k] : undefined;
}

function staticLookup(staticVars, path) {
  if (!Object.hasOwn(staticVars, path[0])) return undefined;
  let v = staticVars[path[0]];
  for (let k = 1; k < path.length; k++) {
    if (v === null || typeof v !== 'object' || !Object.hasOwn(v, path[k])) return undefined;
    v = v[path[k]];
  }
  return v;
}

function analyzeSubtree(nodes, bound, templates, relDepth, maxAllowed, freeOut) {
  let maxD = relDepth;
  for (let i = 0; i < nodes.length; i++) {
    const n = nodes[i];
    if (n.kind === 'var' || n.kind === 'raw') {
      if (!bound.has(n.path[0])) freeOut.add(n.path[0]);
    } else if (n.kind === 'if') {
      if (!bound.has(n.path[0])) freeOut.add(n.path[0]);
      const d1 = analyzeSubtree(n.then, bound, templates, relDepth, maxAllowed, freeOut);
      if (d1 > maxD) maxD = d1;
      if (n.else !== null) {
        const d2 = analyzeSubtree(n.else, bound, templates, relDepth, maxAllowed, freeOut);
        if (d2 > maxD) maxD = d2;
      }
    } else if (n.kind === 'for') {
      if (!bound.has(n.path[0])) freeOut.add(n.path[0]);
      const inner = new Set(bound);
      inner.add(n.name);
      inner.add('loop');
      const d1 = analyzeSubtree(n.body, inner, templates, relDepth, maxAllowed, freeOut);
      if (d1 > maxD) maxD = d1;
    } else if (n.kind === 'include') {
      if (relDepth < maxAllowed) {
        const t = templates.get(n.name);
        if (t !== undefined) {
          const d1 = analyzeSubtree(t, bound, templates, relDepth + 1, maxAllowed, freeOut);
          if (d1 > maxD) maxD = d1;
        }
      } else {
        maxD = maxAllowed + 1;
      }
    }
  }
  return maxD;
}

function compileTemplate(rootNodes, staticVars, dynParamMap, templates, memoCache) {
  const strings = [];
  const consts = [];
  const memos = [];
  const code = [];
  let pendingText = '';
  let pushArgs = [];
  let loopCounter = 0;

  function flushPush() {
    if (pendingText !== '') {
      const idx = strings.length;
      strings.push(pendingText);
      pushArgs.push('S[' + idx + ']');
      pendingText = '';
    }
    if (pushArgs.length > 0) {
      code.push('O.push(' + pushArgs.join(',') + ');');
      pushArgs = [];
    }
  }

  function emitText(str) {
    if (str !== '') pendingText += str;
  }

  function emitExprPush(expr) {
    if (pendingText !== '') {
      const idx = strings.length;
      strings.push(pendingText);
      pushArgs.push('S[' + idx + ']');
      pendingText = '';
    }
    pushArgs.push(expr);
  }

  function findLoop(loopEnv, name) {
    for (let d = loopEnv.length - 1; d >= 0; d--) {
      if (loopEnv[d].name === name) return d;
    }
    return -1;
  }

  function hasDynamicVar(nodes, localStaticBound, loopEnv, depth) {
    for (let i = 0; i < nodes.length; i++) {
      const n = nodes[i];
      if (n.kind === 'var' || n.kind === 'raw') {
        const p0 = n.path[0];
        if (!localStaticBound.has(p0)) {
          if (p0 === 'loop' && loopEnv.length > 0) return true;
          if (findLoop(loopEnv, p0) !== -1 || dynParamMap.has(p0)) return true;
        }
      } else if (n.kind === 'if') {
        const p0 = n.path[0];
        if (!localStaticBound.has(p0)) {
          if (p0 === 'loop' && loopEnv.length > 0) return true;
          if (findLoop(loopEnv, p0) !== -1 || dynParamMap.has(p0)) return true;
        }
        if (hasDynamicVar(n.then, localStaticBound, loopEnv, depth)) return true;
        if (n.else !== null && hasDynamicVar(n.else, localStaticBound, loopEnv, depth)) return true;
      } else if (n.kind === 'for') {
        const p0 = n.path[0];
        if (!localStaticBound.has(p0)) {
          if (p0 === 'loop' && loopEnv.length > 0) return true;
          if (findLoop(loopEnv, p0) !== -1 || dynParamMap.has(p0)) return true;
        }
        const nextBound = new Set(localStaticBound);
        nextBound.add(n.name);
        nextBound.add('loop');
        if (hasDynamicVar(n.body, nextBound, loopEnv, depth)) return true;
      } else if (n.kind === 'include') {
        if (depth < INCLUDE_DEPTH) {
          const t = templates.get(n.name);
          if (t !== undefined && hasDynamicVar(t, localStaticBound, loopEnv, depth + 1)) return true;
        }
      }
    }
    return false;
  }

  function pathExpr(path, loopEnv) {
    const p0 = path[0];
    let expr = null;
    let startK = 1;
    if (p0 === 'loop' && loopEnv.length > 0) {
      const id = loopEnv[loopEnv.length - 1].id;
      if (path.length === 1) {
        return `({index:i${id}+1,first:i${id}===0,last:i${id}===n${id}-1,length:n${id}})`;
      }
      const p1 = path[1];
      if (p1 === 'index') expr = `(i${id}+1)`;
      else if (p1 === 'first') expr = `(i${id}===0)`;
      else if (p1 === 'last') expr = `(i${id}===n${id}-1)`;
      else if (p1 === 'length') expr = `n${id}`;
      else return 'undefined';
      startK = 2;
    } else {
      const li = findLoop(loopEnv, p0);
      if (li !== -1) {
        expr = 'v' + loopEnv[li].id;
      } else {
        const param = dynParamMap.get(p0);
        if (param !== undefined) expr = param;
        else return null;
      }
    }
    for (let k = startK; k < path.length; k++) {
      expr = `P(${expr},${JSON.stringify(path[k])})`;
    }
    return expr;
  }

  function emitNodes(nodes, loopEnv, depth) {
    for (let i = 0; i < nodes.length; i++) {
      const n = nodes[i];
      if (n.kind === 'text') {
        emitText(n.text);
      } else if (n.kind === 'var' || n.kind === 'raw') {
        const expr = pathExpr(n.path, loopEnv);
        if (expr === null) {
          const val = textOf(staticLookup(staticVars, n.path));
          emitText(n.kind === 'var' ? escapeHtml(val) : val);
        } else {
          emitExprPush((n.kind === 'var' ? 'V(' : 'T(') + expr + ')');
        }
      } else if (n.kind === 'if') {
        const expr = pathExpr(n.path, loopEnv);
        if (expr === null) {
          const take = truthy(staticLookup(staticVars, n.path)) !== n.not;
          const br = take ? n.then : n.else;
          if (br !== null) emitNodes(br, loopEnv, depth);
        } else {
          flushPush();
          code.push(n.not ? `if(!U(${expr})){` : `if(U(${expr})){`);
          emitNodes(n.then, loopEnv, depth);
          flushPush();
          if (n.else !== null) {
            code.push('}else{');
            emitNodes(n.else, loopEnv, depth);
            flushPush();
          }
          code.push('}');
        }
      } else if (n.kind === 'for') {
        const expr = pathExpr(n.path, loopEnv);
        let iterExpr = expr;
        if (expr === null) {
          const xs = staticLookup(staticVars, n.path);
          if (!Array.isArray(xs) || xs.length === 0) continue;
          if (!hasDynamicVar(n.body, new Set([n.name, 'loop']), loopEnv, depth)) {
            const subFn = compileTemplate([n], staticVars, new Map(), templates, null, true, depth);
            emitText(subFn(null, null, []));
            continue;
          }
          const cIdx = consts.length;
          consts.push(xs);
          iterExpr = 'C[' + cIdx + ']';
        }
        flushPush();
        const id = loopCounter++;
        code.push(`{const a${id}=${iterExpr};if(Array.isArray(a${id})){for(let i${id}=0,n${id}=a${id}.length;i${id}<n${id};i${id}++){const v${id}=a${id}[i${id}];`);
        const nextEnv = loopEnv.concat({ name: n.name, id });
        emitNodes(n.body, nextEnv, depth);
        flushPush();
        code.push('}}}');
      } else if (n.kind === 'include') {
        if (depth >= INCLUDE_DEPTH) continue;
        const t = templates.get(n.name);
        if (t === undefined) continue;

        if (loopEnv.length > 0 && memoCache !== null) {
          const free = new Set();
          const maxRel = analyzeSubtree(t, new Set(), templates, 0, INCLUDE_DEPTH - depth - 1, free);
          if (maxRel <= INCLUDE_DEPTH - depth - 1) {
            let dynVar = null;
            let canMemo = true;
            for (const v of free) {
              if (v === 'loop') { canMemo = false; break; }
              const li = findLoop(loopEnv, v);
              if (li !== -1) {
                if (dynVar === null || dynVar === v) dynVar = v;
                else { canMemo = false; break; }
              } else if (dynParamMap.has(v)) {
                canMemo = false; break;
              }
            }
            if (canMemo && dynVar !== null) {
              const li = findLoop(loopEnv, dynVar);
              const loopId = loopEnv[li].id;
              const cacheKey = n.name + '\0' + dynVar;
              let entry = memoCache.get(cacheKey);
              if (entry === undefined) {
                const subFn = compileTemplate(t, staticVars, new Map([[dynVar, 'A']]), templates, null);
                entry = { f: subFn, c: new Map() };
                memoCache.set(cacheKey, entry);
              }
              const mIdx = memos.length;
              memos.push(entry);
              flushPush();
              code.push(`{const _k=v${loopId};let _c=M[${mIdx}].c.get(_k);if(_c===undefined){_c=M[${mIdx}].f(_k,null,[]);M[${mIdx}].c.set(_k,_c);}O.push(_c);}`);
              continue;
            }
          }
        }
        emitNodes(t, loopEnv, depth + 1);
      }
    }
  }

  const forceStaticFor = arguments[5] === true;
  const initDepth = arguments[6] || 0;

  if (forceStaticFor) {
    const n = rootNodes[0];
    const xs = staticLookup(staticVars, n.path);
    consts.push(xs);
    const id = loopCounter++;
    code.push(`{const a${id}=C[0];for(let i${id}=0,n${id}=a${id}.length;i${id}<n${id};i${id}++){const v${id}=a${id}[i${id}];`);
    emitNodes(n.body, [{ name: n.name, id }], initDepth);
    flushPush();
    code.push('}}');
  } else {
    emitNodes(rootNodes, [], initDepth);
    flushPush();
  }

  if (code.length === 1 && strings.length === 1 && consts.length === 0 && memos.length === 0 && code[0] === 'O.push(S[0]);') {
    const constantStr = strings[0];
    return () => constantStr;
  }
  if (code.length === 0) return () => '';

  const rawFn = new Function('V', 'T', 'U', 'P', 'S', 'C', 'M', 'A', 'B', 'O', 'O.length=0;' + code.join('') + 'return O.join("");');
  return (a, b, outParts) => rawFn(varOut, textOf, truthy, ownProp, strings, consts, memos, a, b, outParts);
}

// ---------------------------------------------------------------- main build

export function run(input) {
  const slugCache = new Map();

  // 1. Parse templates in code-unit order
  const templates = new Map();
  const report = [];
  const tplNames = Object.keys(input.templates).sort(byCodeUnits);
  for (let i = 0; i < tplNames.length; i++) {
    const name = tplNames[i];
    try {
      templates.set(name, parseTemplate(name, input.templates[name]));
    } catch (err) {
      if (!(err instanceof TemplateError)) throw err;
      report.push(err.message);
    }
  }

  const base = input.site.base_url.replace(/\/+$/, '');

  // 2. Partition & sort sources and statics
  const rawSources = [];
  const statics = [];
  const inFiles = input.files;
  for (let i = 0; i < inFiles.length; i++) {
    const f = inFiles[i];
    if (f.path.endsWith('.md')) rawSources.push(f);
    else statics.push(f);
  }
  rawSources.sort((x, y) => byCodeUnits(x.path, y.path));
  statics.sort((x, y) => byCodeUnits(x.path, y.path));

  // 3. Front matter
  const sources = [];
  for (let i = 0; i < rawSources.length; i++) {
    const f = rawSources[i];
    const path = f.path;
    const rawContent = f.content;
    const text = rawContent.indexOf('\r') === -1 ? rawContent : rawContent.replace(RE_CRLF, '\n');
    const fm = parseFrontMatter(text);
    if (fm.error !== undefined) {
      report.push(`refused ${path}: ${fm.error}`);
      continue;
    }
    const data = fm.data;
    const post = path.startsWith('posts/');
    if (!post) {
      if (path.startsWith('tags/')) {
        report.push(`refused ${path}: tags/ is the generator's own`);
        continue;
      }
      if (path.startsWith('page/')) {
        report.push(`refused ${path}: page/ is the generator's own`);
        continue;
      }
    }
    if (post && data.date === undefined) {
      report.push(`refused ${path}: a post needs a date`);
      continue;
    }
    const layout = data.layout !== undefined ? data.layout : post ? 'post' : 'page';
    if (!templates.has(layout)) {
      report.push(`refused ${path}: layout "${layout}" is no template that parses`);
      continue;
    }
    if (data.draft === true) {
      report.push(`draft ${path}`);
      continue;
    }
    sources.push({
      path,
      body: fm.bodyStart < text.length ? text.slice(fm.bodyStart) : '',
      data,
      post,
      layout,
      slug: '',
      outDir: '',
      url: '',
      html: '',
      toc: null,
      excerpt: '',
      newer: null,
      older: null,
      tagRefs: null,
      card: null,
      link: null,
      dateLong: data.date === undefined ? '' : longDate(data.date),
      readingMinutes: 0,
    });
  }

  // 4. Permalinks
  const taken = new Map();
  const bySource = new Map();
  for (let i = 0; i < sources.length; i++) {
    const s = sources[i];
    const dir = s.post ? 'posts' : dirOf(s.path);
    let set = taken.get(dir);
    if (set === undefined) {
      set = dir === '' ? new Set(RESERVED) : new Set();
      taken.set(dir, set);
    }
    const baseSlug = s.data.slug !== undefined ? s.data.slug : slugify(s.data.title, slugCache);
    const slug = uniqueSlug(baseSlug, set);
    s.slug = slug;
    s.outDir = dir === '' ? slug : dir + '/' + slug;
    s.url = base + '/' + s.outDir + '/';
    bySource.set(s.path, s);
  }

  // 5. Markdown + reading minutes
  const staticPaths = new Set();
  for (let i = 0; i < statics.length; i++) staticPaths.add(statics[i].path);
  const broken = new Set();
  const dirPartsCache = new Map();
  const relCacheByDir = new Map();
  const linkState = { base, bySource, staticPaths, broken, dirParts: null, relCache: null, sPath: '' };

  for (let i = 0; i < sources.length; i++) {
    const s = sources[i];
    const sPath = s.path;
    const dir = dirOf(sPath);
    let dirParts = dirPartsCache.get(dir);
    if (dirParts === undefined) {
      dirParts = dir === '' ? [] : dir.split('/');
      dirPartsCache.set(dir, dirParts);
    }
    let relCache = relCacheByDir.get(dir);
    if (relCache === undefined) {
      relCache = new Map();
      relCacheByDir.set(dir, relCache);
    }
    linkState.dirParts = dirParts;
    linkState.relCache = relCache;
    linkState.sPath = sPath;

    const body = s.body;
    const md = renderMarkdown(body, linkState, slugCache);
    s.html = md.html;
    s.toc = md.toc;
    s.excerpt = md.excerpt;
    s.readingMinutes = readingMinutes(body);
    s.body = '';
  }
  mdCtx.linkState = null;
  mdCtx.slugCache = null;

  // 6. Collections
  const posts = [];
  for (let i = 0; i < sources.length; i++) {
    if (sources[i].post) posts.push(sources[i]);
  }
  posts.sort((x, y) => byCodeUnits(y.data.date, x.data.date) || byCodeUnits(x.slug, y.slug));
  for (let k = 0; k < posts.length; k++) {
    const p = posts[k];
    p.newer = k === 0 ? null : posts[k - 1];
    p.older = k === posts.length - 1 ? null : posts[k + 1];
    p.link = {
      title: p.data.title,
      url: p.url,
      date: p.data.date !== undefined ? p.data.date : '',
      date_long: p.dateLong,
    };
  }

  // 7. Tags
  const bySlug = new Map();
  for (let i = 0; i < posts.length; i++) {
    const p = posts[i];
    const rawTags = p.data.tags;
    if (rawTags === undefined || rawTags.length === 0) {
      p.tagRefs = [];
      continue;
    }
    const seenSlugs = new Set();
    const tagRefs = [];
    for (let j = 0; j < rawTags.length; j++) {
      const name = rawTags[j];
      const slug = slugify(name, slugCache);
      if (seenSlugs.has(slug)) continue;
      seenSlugs.add(slug);
      let tag = bySlug.get(slug);
      if (tag === undefined) {
        const url = base + '/tags/' + slug + '/';
        tag = { name, slug, url, posts: [], cardTag: { name, slug, url } };
        bySlug.set(slug, tag);
      }
      tag.posts.push(p);
      tagRefs.push(tag.cardTag);
    }
    p.tagRefs = tagRefs;
  }

  for (let i = 0; i < sources.length; i++) {
    const s = sources[i];
    if (s.post) continue;
    const rawTags = s.data.tags;
    if (rawTags === undefined || rawTags.length === 0) {
      s.tagRefs = [];
      continue;
    }
    const seenSlugs = new Set();
    const tagRefs = [];
    for (let j = 0; j < rawTags.length; j++) {
      const name = rawTags[j];
      const slug = slugify(name, slugCache);
      if (seenSlugs.has(slug)) continue;
      seenSlugs.add(slug);
      const tag = bySlug.get(slug);
      tagRefs.push(tag !== undefined ? tag.cardTag : { name, slug, url: '' });
    }
    s.tagRefs = tagRefs;
  }

  const tags = Array.from(bySlug.values());
  tags.sort((x, y) => y.posts.length - x.posts.length || byCodeUnits(x.slug, y.slug));

  for (let i = 0; i < posts.length; i++) {
    const p = posts[i];
    p.card = {
      title: p.data.title,
      url: p.url,
      date: p.data.date !== undefined ? p.data.date : '',
      date_long: p.dateLong,
      summary: p.data.summary !== undefined ? p.data.summary : '',
      excerpt: p.excerpt,
      reading_minutes: p.readingMinutes,
      tags: p.tagRefs,
    };
  }

  // 8. Layouts
  const site = { title: input.site.title, base_url: base };
  const allTags = new Array(tags.length);
  for (let i = 0; i < tags.length; i++) {
    const t = tags[i];
    allTags[i] = { name: t.name, slug: t.slug, url: t.url, count: t.posts.length };
  }
  const recentCount = Math.min(input.site.recent, posts.length);
  const recent = new Array(recentCount);
  for (let i = 0; i < recentCount; i++) recent[i] = posts[i].card;

  const staticVars = { site, tags: allTags, recent };
  const memoCache = new Map();
  const compiledPageLayouts = new Map();
  const pageParamMap = new Map([['page', 'A']]);
  const outParts = [];
  const output = {};

  for (let i = 0; i < sources.length; i++) {
    const s = sources[i];
    let fn = compiledPageLayouts.get(s.layout);
    if (fn === undefined) {
      fn = compileTemplate(templates.get(s.layout), staticVars, pageParamMap, templates, memoCache);
      compiledPageLayouts.set(s.layout, fn);
    }
    const card = s.card;
    const pageObj =
      card !== null
        ? {
            title: card.title,
            url: card.url,
            date: card.date,
            date_long: card.date_long,
            summary: card.summary,
            excerpt: card.excerpt,
            reading_minutes: card.reading_minutes,
            tags: card.tags,
            slug: s.slug,
            content: s.html,
            toc: s.toc,
            newer: s.newer !== null ? s.newer.link : null,
            older: s.older !== null ? s.older.link : null,
            is_post: true,
          }
        : {
            title: s.data.title,
            url: s.url,
            date: s.data.date !== undefined ? s.data.date : '',
            date_long: s.dateLong,
            summary: s.data.summary !== undefined ? s.data.summary : '',
            excerpt: s.excerpt,
            reading_minutes: s.readingMinutes,
            tags: s.tagRefs,
            slug: s.slug,
            content: s.html,
            toc: s.toc,
            newer: null,
            older: null,
            is_post: false,
          };
    output[s.outDir + '/index.html'] = fn(pageObj, null, outParts);
  }

  const indexTpl = templates.get('index');
  if (indexTpl !== undefined) {
    const indexFn = compileTemplate(indexTpl, staticVars, new Map([['posts', 'A'], ['pager', 'B']]), templates, memoCache);
    const per = input.site.per_page;
    const totalPages = Math.max(1, Math.ceil(posts.length / per));
    const homeUrl = base + '/';
    const pagePrefix = base + '/page/';
    for (let k = 1; k <= totalPages; k++) {
      const startIdx = (k - 1) * per;
      const endIdx = Math.min(k * per, posts.length);
      const pagePosts = new Array(endIdx - startIdx);
      for (let j = startIdx; j < endIdx; j++) pagePosts[j - startIdx] = posts[j].card;
      const pager = {
        number: k,
        total: totalPages,
        url: k === 1 ? homeUrl : pagePrefix + k + '/',
        newer_url: k === 1 ? '' : k === 2 ? homeUrl : pagePrefix + (k - 1) + '/',
        older_url: k === totalPages ? '' : pagePrefix + (k + 1) + '/',
      };
      output[k === 1 ? 'index.html' : 'page/' + k + '/index.html'] = indexFn(pagePosts, pager, outParts);
    }
  }

  const tagTpl = templates.get('tag');
  if (tagTpl !== undefined) {
    const tagFn = compileTemplate(tagTpl, staticVars, new Map([['tag', 'A']]), templates, memoCache);
    for (let i = 0; i < tags.length; i++) {
      const t = tags[i];
      const tPosts = t.posts;
      const tCards = new Array(tPosts.length);
      for (let j = 0; j < tPosts.length; j++) tCards[j] = tPosts[j].card;
      const tagObj = { name: t.name, slug: t.slug, url: t.url, count: tPosts.length, posts: tCards };
      output['tags/' + t.slug + '/index.html'] = tagFn(tagObj, null, outParts);
    }
  }

  const tagsTpl = templates.get('tags');
  if (tagsTpl !== undefined) {
    const tagsFn = compileTemplate(tagsTpl, staticVars, new Map(), templates, memoCache);
    output['tags/index.html'] = tagsFn(null, null, outParts);
  }

  for (let i = 0; i < statics.length; i++) {
    const f = statics[i];
    if (Object.hasOwn(output, f.path)) report.push(`shadowed ${f.path}: a built page has its path`);
    else output[f.path] = f.content;
  }

  // 9. Report
  const lines = broken.size === 0 ? report.slice() : report.concat(Array.from(broken));
  lines.sort(byCodeUnits);
  const pageCount = sources.length - posts.length;
  lines.push(`built ${pageCount} pages, ${posts.length} posts, ${tags.length} tags and ${statics.length} static files`);
  output[REPORT_FILE] = lines.join('\n') + '\n';

  return output;
}
