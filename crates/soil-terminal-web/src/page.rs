//! The one page this host serves.
//!
//! It is deliberately dependency-free: an `EventSource` for frames, `fetch` for
//! keystrokes and resizes, and DOM spans for cells. The server owns the VT; the
//! page is only a presenter.

pub const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>soil terminal</title>
<style>
:root { --bg:#0b0e11; --fg:#dce5ea; --muted:#8395a2; --accent:#7dd3a8; --line:#1c252d; }
* { box-sizing: border-box; }
html, body { height: 100%; margin: 0; }
body {
  background: var(--bg); color: var(--fg); display: flex; flex-direction: column;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
}
header {
  display: flex; align-items: center; gap: 10px; padding: 8px 12px;
  border-bottom: 1px solid var(--line); font-size: 12px; color: var(--muted);
}
header .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--accent); }
#title { color: var(--fg); font-weight: 600; }
#status { margin-left: auto; }
#screen {
  flex: 1; position: relative; overflow: hidden; padding: 10px 12px;
  font-size: 14px; line-height: 1.35; outline: none; cursor: text;
}
#grid { position: relative; }
.row { height: 1.35em; white-space: pre; }
#cursor {
  position: absolute; width: 1ch; height: 1.35em; background: var(--accent);
  opacity: .55; pointer-events: none; animation: blink 1.1s step-end infinite;
}
@keyframes blink { 50% { opacity: 0; } }
#measure { position: absolute; visibility: hidden; white-space: pre; }
#scrollbar {
  position: absolute; top: 0; right: 0; width: 6px; height: 100%;
  pointer-events: none; display: none;
}
#thumb {
  position: absolute; left: 1px; width: 4px; border-radius: 2px;
  background: rgba(220, 229, 234, .28);
}
footer { padding: 6px 12px; border-top: 1px solid var(--line); font-size: 11px; color: var(--muted); }
</style>
</head>
<body>
<header>
  <span class="dot"></span>
  <span id="title">soil terminal</span>
  <span id="status">connecting…</span>
</header>
<main id="screen" tabindex="0">
  <div id="grid"><div id="cursor"></div><span id="measure">0000000000</span></div>
  <div id="scrollbar"><div id="thumb"></div></div>
</main>
<footer>click to focus · ctrl-c sends SIGINT · paste or drop files to upload · resize to reflow</footer>
<script>
const token = new URLSearchParams(location.search).get('token') || '';
const suffix = token ? '?token=' + encodeURIComponent(token) : '';
const screen = document.getElementById('screen');
const grid = document.getElementById('grid');
const cursor = document.getElementById('cursor');
const measure = document.getElementById('measure');
const statusEl = document.getElementById('status');
const titleEl = document.getElementById('title');
const scrollbar = document.getElementById('scrollbar');
const thumb = document.getElementById('thumb');

let cols = 0, rows = 0, cellW = 8, lineH = 18;
const rowEls = [];

function ensureRow(y) {
  while (rowEls.length <= y) {
    const row = document.createElement('div');
    row.className = 'row';
    grid.insertBefore(row, cursor);
    rowEls.push(row);
  }
  return rowEls[y];
}

function clearGrid() {
  for (const row of rowEls) row.remove();
  rowEls.length = 0;
}

function runSpan(run) {
  const [text, fg, bg, flags] = run;
  let color = fg, back = bg;
  if (flags & 16) {
    const swap = color;
    color = back || 'var(--fg)';
    back = swap || 'var(--bg)';
  }
  const span = document.createElement('span');
  span.textContent = text;
  let css = '';
  if (color) css += 'color:' + color + ';';
  if (back) css += 'background:' + back + ';';
  if (flags & 1) css += 'font-weight:700;';
  if (flags & 2) css += 'font-style:italic;';
  if (flags & 4) css += 'text-decoration:underline;';
  if (flags & 8) css += 'opacity:.65;';
  span.style.cssText = css;
  return span;
}

function applyFrame(frame) {
  if (frame.title) {
    titleEl.textContent = frame.title;
    document.title = frame.title + ' — soil terminal';
  }
  if (frame.full) clearGrid();
  for (const row of frame.rows) {
    ensureRow(row.y).replaceChildren(...row.runs.map(runSpan));
  }
  if (frame.full) {
    while (rowEls.length > frame.rows) rowEls.pop().remove();
  }
  if (frame.cursor) {
    cursor.style.display = 'block';
    cursor.style.left = frame.cursor[0] + 'ch';
    cursor.style.top = (frame.cursor[1] * lineH) + 'px';
  } else {
    cursor.style.display = 'none';
  }
  if (frame.scroll && frame.scroll.total > frame.scroll.len) {
    scrollbar.style.display = 'block';
    const track = scrollbar.clientHeight;
    const height = Math.max(24, Math.round(track * (frame.scroll.len / frame.scroll.total)));
    thumb.style.height = height + 'px';
    thumb.style.top = Math.round(track * (frame.scroll.offset / frame.scroll.total)) + 'px';
  } else {
    scrollbar.style.display = 'none';
  }
}

function sendInput(text) {
  if (!text) return;
  fetch('/input' + suffix, { method: 'POST', body: text, keepalive: true }).catch(() => {});
}

function shellQuote(path) {
  if (/^[A-Za-z0-9_./@%+=-]+$/.test(path)) return path;
  return "'" + path.replace(/'/g, "'\\''") + "'";
}

async function upload(blob, name) {
  try {
    const response = await fetch('/paste' + suffix, {
      method: 'POST',
      headers: {
        'content-type': blob.type || 'application/octet-stream',
        'x-filename': encodeURIComponent(name || 'paste'),
      },
      body: blob,
    });
    if (!response.ok) {
      statusEl.textContent = 'paste failed: HTTP ' + response.status;
      return null;
    }
    const info = await response.json();
    statusEl.textContent = 'pasted ' + info.name + ' · ' + info.cid.slice(0, 12) + '…';
    return info;
  } catch (error) {
    statusEl.textContent = 'paste failed';
    return null;
  }
}

async function insertUpload(blob, name) {
  const info = await upload(blob, name);
  if (info) sendInput(shellQuote(info.path) + ' ');
}

document.addEventListener('paste', async (event) => {
  const items = Array.from(event.clipboardData ? event.clipboardData.items : []);
  const files = items
    .filter((item) => item.kind === 'file')
    .map((item) => item.getAsFile())
    .filter(Boolean);
  if (files.length) {
    event.preventDefault();
    for (const file of files) await insertUpload(file, file.name || 'paste');
    return;
  }
  const text = event.clipboardData ? event.clipboardData.getData('text/plain') : '';
  if (!text) return;
  event.preventDefault();
  if (text.length > 64 * 1024) {
    await insertUpload(new Blob([text], { type: 'text/plain' }), 'paste.txt');
  } else {
    sendInput(text);
  }
});

screen.addEventListener('wheel', (event) => {
  event.preventDefault();
  const lines = Math.max(1, Math.round(Math.abs(event.deltaY) / 40));
  const delta = event.deltaY < 0 ? -lines : lines;
  fetch('/scroll' + suffix, {
    method: 'POST',
    body: JSON.stringify({ delta }),
    keepalive: true,
  }).catch(() => {});
}, { passive: false });

screen.addEventListener('dragover', (event) => event.preventDefault());
screen.addEventListener('drop', async (event) => {
  event.preventDefault();
  const files = Array.from(event.dataTransfer ? event.dataTransfer.files : []);
  for (const file of files) await insertUpload(file, file.name || 'paste');
});

function encodeKey(event) {
  if (event.metaKey) return null;
  const key = event.key;
  if (event.ctrlKey && !event.altKey && key.length === 1) {
    const code = key.toUpperCase().charCodeAt(0);
    if (code >= 64 && code <= 95) return String.fromCharCode(code - 64);
    if (key === ' ') return '\x00';
  }
  switch (key) {
    case 'Enter': return '\r';
    case 'Backspace': return '\x7f';
    case 'Tab': return event.shiftKey ? '\x1b[Z' : '\t';
    case 'Escape': return '\x1b';
    case 'ArrowUp': return '\x1b[A';
    case 'ArrowDown': return '\x1b[B';
    case 'ArrowRight': return '\x1b[C';
    case 'ArrowLeft': return '\x1b[D';
    case 'Home': return '\x1b[H';
    case 'End': return '\x1b[F';
    case 'PageUp': return '\x1b[5~';
    case 'PageDown': return '\x1b[6~';
    case 'Insert': return '\x1b[2~';
    case 'Delete': return '\x1b[3~';
  }
  if (key.length === 1) return (event.altKey ? '\x1b' : '') + key;
  return null;
}

document.addEventListener('keydown', (event) => {
  const data = encodeKey(event);
  if (data === null) return;
  event.preventDefault();
  sendInput(data);
});

function fit() {
  const rect = screen.getBoundingClientRect();
  const style = getComputedStyle(screen);
  const width = rect.width - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
  const height = rect.height - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom);
  cellW = measure.getBoundingClientRect().width / 10 || 8;
  lineH = rowEls[0] ? rowEls[0].getBoundingClientRect().height : (parseFloat(style.lineHeight) || 18);
  const nextCols = Math.max(20, Math.floor(width / cellW));
  const nextRows = Math.max(5, Math.floor(height / lineH));
  if (nextCols === cols && nextRows === rows) return;
  cols = nextCols;
  rows = nextRows;
  fetch('/resize' + suffix, {
    method: 'POST',
    body: JSON.stringify({ cols, rows }),
    keepalive: true,
  }).catch(() => {});
}

let fitTimer;
window.addEventListener('resize', () => {
  clearTimeout(fitTimer);
  fitTimer = setTimeout(fit, 80);
});

screen.addEventListener('click', () => screen.focus());
screen.focus();

const source = new EventSource('/events' + suffix);
source.onopen = () => {
  statusEl.textContent = 'connected';
  fit();
};
source.onerror = () => {
  statusEl.textContent = 'reconnecting…';
};
source.onmessage = (event) => {
  const message = JSON.parse(event.data);
  if (message.type === 'exit') {
    statusEl.textContent = 'session ended';
    return;
  }
  if (message.type === 'frame') {
    applyFrame(message);
    if (!cols || !rows) fit();
  }
};
</script>
</body>
</html>
"##;
