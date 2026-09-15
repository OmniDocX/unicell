// OpenCell 前端 — 纯 HTML/DOM 虚拟化电子表格（无 Canvas、无 WASM）
// 所有计算与数据由 Rust 服务（默认 127.0.0.1:8143）提供，本文件只做渲染与交互。
'use strict';

/* ================= 全局状态 ================= */
const S = {
  sheet: 0,               // 当前 sheet 索引
  sheets: [],             // sheet 名列表
  defW: 100, defH: 21,    // 默认列宽/行高（初始化后按服务端校准）
  zoom: 1,                // 视觉缩放因子（Ctrl+滚轮，Excel 式）
  colW: new Map(),        // 非默认列宽覆盖 col -> px（真实值，不含缩放）
  rowH: new Map(),        // 非默认行高覆盖 row -> px（真实值）
  vRows: 300, vCols: 40,  // 当前虚拟区行列数（滚动接近边缘时扩展）
  cur: { r: 1, c: 1 },    // 光标单元格
  sel: { r0: 1, c0: 1, r1: 1, c1: 1 }, // 选区
  anchor: { r: 1, c: 1 },
  editing: false,
  editCell: null,
  editOriginal: '',       // 编辑开始时的真实内容；富文本无变化提交必须是 no-op
  editHasRichText: false,
  editRichRuns: [],       // 当前富文本草稿；公式栏按字符分段编辑，提交时保持 runs
  editRichOriginal: [],
  editBaseRichStyle: {},  // 普通常量首次做字符格式时，继承所在单元格字体作为首个 run
  editFrozen: null,       // 编辑器当前是否锚定在冻结行/列（{rows, cols}）
  richSelection: null,    // 工具栏抢焦点时保留 contenteditable 的字符选区
  richTypingStyle: null,  // 折叠光标处后续输入采用的 run 样式
  richEditSurface: null,  // 当前字符选区所在编辑面：公式栏或原单元格覆盖层
  plainFormulaSelection: null,
  formulaCellStyle: null, // 公式栏最近一次已解析的单元格样式（带坐标，避免异步串格）
  editDirty: false,
  editLoadPromise: null,
  refMode: null,          // 公式点选引用状态 {start,len,anchor,cur}
  cellsCache: new Map(),  // "r,c" -> {v,t,s} 视口缓存
  cutPending: null,       // 剪切待粘贴标记（源清除由服务端剪切语义完成）
  showFormulas: false,    // 显示公式模式（Excel「显示公式」）
  maxUsedR: 1, maxUsedC: 1,
  viewSeq: 0,
  merges: [],             // 全工作表合并区域；命中/编辑统一归一到左上角
  excelExtension: 'xlsx', // macro workbooks remain .xlsm after round-trip
  storageScope: '',       // 服务端会话命名空间；隔离自动恢复和最近文件
  fileHandle: null,       // File System Access API handle；Save 复用，Save As/打开时替换
  localFileName: '',      // 带扩展名的本地文件名（服务端只保存工作簿 basename）
  dirty: false,
  dirtyRevision: 0,
  saveBusy: false,
  autosaveTimer: 0,
  autosaveWriting: null,
  lastAutosaveAt: 0,
  recoveryRecord: null,
};

const $ = (id) => document.getElementById(id);
const gridScroll = $('grid-scroll');
const cellsLayer = $('cells-layer');
const colHdrInner = $('col-headers-inner');
const rowHdrInner = $('row-headers-inner');
const editor = $('cell-editor');
const formulaInput = $('formula-input');
const richFormulaInput = $('rich-formula-input');
const richCellEditor = document.createElement('div');
richCellEditor.id = 'rich-cell-editor';
richCellEditor.className = 'rich-cell-editor';
richCellEditor.contentEditable = 'false';
richCellEditor.setAttribute('role', 'textbox');
richCellEditor.setAttribute('aria-multiline', 'true');
richCellEditor.setAttribute('aria-label', '单元格富文本编辑器');
richCellEditor.spellcheck = false;
gridScroll.appendChild(richCellEditor);
const nameBox = $('name-box');
const formulaRow = $('formula-row');

gridScroll.tabIndex = 0; // 接收键盘

/* ================= 服务端通信 ================= */
async function api(path, opts) {
  try {
    const requestPath = String(path).split('?')[0];
    const requestMethod = String(opts?.method || 'GET').toUpperCase();
    const res = await fetch(path, opts);
    const rawMutation = requestMethod === 'POST'
      && ['/api/undo', '/api/redo', '/api/calc'].includes(requestPath);
    const ct = res.headers.get('Content-Type') || '';
    if (!ct.includes('json')) {
      if (res.ok && rawMutation) markWorkbookDirty(requestPath);
      return res;
    }
    const j = await res.json();
    if (j.ok === false) throw new Error(j.error || 'server error');
    if (rawMutation) markWorkbookDirty(requestPath);
    return j;
  } catch (e) {
    setStatus('错误: ' + e.message);
    throw e;
  }
}
function apiPostMutates(path, obj) {
  if (['/api/copy', '/api/find', '/api/native-drawing/validate', '/api/power-query/execute'].includes(path)) return false;
  if (path === '/api/ai/apply') return obj?.dryRun === false;
  const readOnlyOps = new Set(['list', 'get', 'inspect', 'preview', 'validate']);
  if (['/api/cf', '/api/dv', '/api/names', '/api/pivot-caches', '/api/pivot-tables',
    '/api/pivot-local-refresh', '/api/slicers', '/api/timelines', '/api/tables',
    '/api/native-data', '/api/page-review', '/api/what-if', '/api/calcmode'].includes(path)
    && readOnlyOps.has(String(obj?.op || ''))) return false;
  return true;
}
const apiPost = async (path, obj) => {
  const result = await api(path, { method: 'POST', body: JSON.stringify(obj) });
  if (path === '/api/dv' && !['list', 'validate'].includes(obj?.op)) window.invalidateDvRules?.(obj?.sheet);
  else if (['/api/rows', '/api/cols'].includes(path)) window.invalidateDvRules?.(obj?.sheet);
  else if (path === '/api/sheet') window.invalidateAllDvRules?.();
  if (apiPostMutates(path, obj)) markWorkbookDirty(path);
  return result;
};

// All object mutations share one ordered queue.  Payloads are snapshotted when queued so a
// subsequent drag/edit cannot mutate a request that is still waiting behind an earlier write.
let pendingObjectWrites = Promise.resolve();
function queueObjectMutation(mutation) {
  const result = pendingObjectWrites.then(mutation);
  pendingObjectWrites = result.catch(() => undefined);
  return result;
}
function queueObjectWrite(payload) {
  const snapshot = JSON.parse(JSON.stringify(payload));
  return queueObjectMutation(() => apiPost('/api/objects', snapshot));
}
async function flushObjectWrites() { await pendingObjectWrites; }
window.queueObjectMutation = queueObjectMutation;
window.queueObjectWrite = queueObjectWrite;
window.flushObjectWrites = flushObjectWrites;

function setStatus(msg) { $('status-msg').textContent = msg; }

/* ================= 几何计算（稀疏覆盖 + 默认值 + 缩放） =================
 * colX/rowY/colWidth/rowHeight 输出缩放后像素（渲染友好）；
 * colAtX/rowAtY 接收缩放后坐标反向换算。S.colW/S.defW 存真实值。 */
function zRawColW(c) { return S.colW.get(c) ?? S.defW; } // 真实列宽（无缩放）
function zRawRowH(r) { return S.rowH.get(r) ?? S.defH; }
function colX(c) { // 第 c 列左边缘 x（1-based，缩放后）
  let x = (c - 1) * S.defW;
  for (const [k, w] of S.colW) if (k < c) x += w - S.defW;
  return x * S.zoom;
}
function rowY(r) {
  let y = (r - 1) * S.defH;
  for (const [k, h] of S.rowH) if (k < r) y += h - S.defH;
  return y * S.zoom;
}
function colWidth(c) { return zRawColW(c) * S.zoom; } // 缩放后列宽
function rowHeight(r) { return zRawRowH(r) * S.zoom; }
function colAtX(x) { // 缩放后 x 坐标落在哪一列
  const dw = S.defW * S.zoom;
  let c = Math.max(1, Math.floor(x / dw) + 1);
  for (let i = 0; i < 30; i++) {
    const left = colX(c);
    if (x < left) { c = Math.max(1, c - Math.max(1, Math.ceil((left - x) / dw))); continue; }
    if (x >= left + colWidth(c)) { c += Math.max(1, Math.floor((x - left - colWidth(c)) / dw)); c++; continue; }
    return c;
  }
  return c;
}
function rowAtY(y) {
  const dh = S.defH * S.zoom;
  let r = Math.max(1, Math.floor(y / dh) + 1);
  for (let i = 0; i < 30; i++) {
    const top = rowY(r);
    if (y < top) { r = Math.max(1, r - Math.max(1, Math.ceil((top - y) / dh))); continue; }
    if (y >= top + rowHeight(r)) { r += Math.max(1, Math.floor((y - top - rowHeight(r)) / dh)); r++; continue; }
    return r;
  }
  return r;
}
function colName(c) {
  let s = '';
  while (c > 0) { s = String.fromCharCode(65 + ((c - 1) % 26)) + s; c = Math.floor((c - 1) / 26); }
  return s;
}
function cellRef(r, c) { return colName(c) + r; }
function parseRef(txt) {
  const m = /^([A-Za-z]{1,3})(\d{1,7})$/.exec(txt.trim());
  if (!m) return null;
  let c = 0;
  for (const ch of m[1].toUpperCase()) c = c * 26 + ch.charCodeAt(0) - 64;
  return { r: parseInt(m[2], 10), c };
}

/* ================= 视口渲染 ================= */
function updateSpacer() {
  $('grid-spacer').style.width = colX(S.vCols + 1) + 'px';
  $('grid-spacer').style.height = rowY(S.vRows + 1) + 'px';
}

// 网格线层绘制：按 colX/rowY 精确画 1px 边框线（主网格与冻结 pane 复用）
const gridLines = $('grid-lines');
function renderLines(container, j) {
  const frag = document.createDocumentFragment();
  const x0 = colX(j.c0), x1 = colX(j.c1) + colWidth(j.c1);
  const y0 = rowY(j.r0), y1 = rowY(j.r1) + rowHeight(j.r1);
  for (let c = j.c0; c <= j.c1 + 1; c++) {
    const d = document.createElement('div');
    d.className = 'gline-v';
    d.style.left = colX(c) + 'px';
    d.style.top = y0 + 'px';
    d.style.height = (y1 - y0) + 'px';
    frag.appendChild(d);
  }
  for (let r = j.r0; r <= j.r1 + 1; r++) {
    const d = document.createElement('div');
    d.className = 'gline-h';
    d.style.left = x0 + 'px';
    d.style.top = rowY(r) + 'px';
    d.style.width = (x1 - x0) + 'px';
    frag.appendChild(d);
  }
  container.textContent = '';
  container.appendChild(frag);
}

function visibleRange() {
  const x0 = gridScroll.scrollLeft, y0 = gridScroll.scrollTop;
  const x1 = x0 + gridScroll.clientWidth, y1 = y0 + gridScroll.clientHeight;
  return {
    r0: Math.max(1, rowAtY(y0) - 3), r1: rowAtY(y1) + 3,
    c0: Math.max(1, colAtX(x0) - 2), c1: colAtX(x1) + 2,
  };
}

let refreshTimer = null;
function scheduleRefresh(immediate) {
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(refreshView, immediate ? 0 : 60);
}

let serverCanUndo = false;
let serverCanRedo = false;
function updateUndoRedoButtons() {
  // The server owns one workbook transaction timeline (cells, rich text and
  // native OOXML objects).  Consulting the legacy drawing-only stack here
  // would make one Ctrl+Z follow a different order from Excel.
  $('btn-undo').disabled = !serverCanUndo;
  $('btn-redo').disabled = !serverCanRedo;
}
window.updateUndoRedoButtons = updateUndoRedoButtons;

async function refreshView() {
  const vr = visibleRange();
  const seq = ++S.viewSeq;
  const j = await api(`/api/view?sheet=${S.sheet}&r0=${vr.r0}&c0=${vr.c0}&r1=${vr.r1}&c1=${vr.c1}`);
  if (seq !== S.viewSeq) return; // 过期响应
  // 校准几何覆盖
  j.colWidths.forEach((w, i) => {
    const c = j.c0 + i;
    if (Math.abs(w - S.defW) > 0.5) S.colW.set(c, w); else S.colW.delete(c);
  });
  j.rowHeights.forEach((h, i) => {
    const r = j.r0 + i;
    if (Math.abs(h - S.defH) > 0.5) S.rowH.set(r, h); else S.rowH.delete(r);
  });
  S.cellsCache.clear();
  const observedFonts = new Set();
  for (const cell of j.cells) {
    S.cellsCache.set(cell.r + ',' + cell.c, cell);
    S.maxUsedR = Math.max(S.maxUsedR, cell.r);
    S.maxUsedC = Math.max(S.maxUsedC, cell.c);
    if (cell.s?.fn) observedFonts.add(cell.s.fn);
    for (const run of Array.isArray(cell.rt) ? cell.rt : []) if (run.font) observedFonts.add(run.font);
  }
  window.UniCellFonts?.requestFamilies?.([...observedFonts]);
  serverCanUndo = !!j.canUndo;
  serverCanRedo = !!j.canRedo;
  updateUndoRedoButtons();
  renderCells(j);
  renderHeaders(vr);
  renderSelection();
  updateSpacer();
  renderFreeze(j, vr);
  updateDimension();
  updateObjPositions(); // 对象位置随缩放/行列变化重算（不重建 DOM）
  renderPageBreaks();  // 分页预览分页线（开启分页视图时）
}

// 状态栏显示当前表有效数据行列数（节流 1s，数据变化后自动刷新）
let dimTimer = 0;
let dimDeferredTimer = 0;
let dimRequestSeq = 0;
function updateDimension() {
  const now = Date.now();
  const remaining = 1000 - (now - dimTimer);
  if (remaining > 0) {
    // A workbook mutation can arrive inside the throttle window.  Dropping that
    // refresh forever leaves the status bar showing the previous used range;
    // keep exactly one trailing refresh, like Excel's eventually-consistent
    // status calculation.
    if (!dimDeferredTimer) {
      dimDeferredTimer = setTimeout(() => {
        dimDeferredTimer = 0;
        updateDimension();
      }, remaining);
    }
    return;
  }
  if (dimDeferredTimer) {
    clearTimeout(dimDeferredTimer);
    dimDeferredTimer = 0;
  }
  dimTimer = now;
  const sheet = S.sheet;
  const requestSeq = ++dimRequestSeq;
  api(`/api/dimension?sheet=${sheet}`).then((d) => {
    if (requestSeq !== dimRequestSeq || sheet !== S.sheet) return;
    const el = $('status-dim');
    if (el && d) el.textContent = `${d.maxRow} 行 × ${d.maxCol} 列`;
  }).catch(() => {});
}

function renderCells(j) {
  const frag = document.createDocumentFragment();
  S.merges = j.merges || [];
  normalizeCurrentForMerges();
  appendCellsWithMerges(frag, j);
  cellsLayer.textContent = '';
  cellsLayer.appendChild(frag);
  cellsLayer.style.transform = '';
  cellsLayer.style.fontSize = 13 * S.zoom + 'px'; // 无样式单元格基准字号随缩放
  renderLines(gridLines, j); // 默认格线层（DOM 边框，清晰且对齐自定义尺寸）
}

// 单元格 DOM 构建（主网格与冻结窗格共用）
const BORDER_CSS = {
  thin: '1px solid ', medium: '2px solid ', thick: '3px solid ',
  double: '3px double ', dotted: '1px dotted ',
};
function borderCss(it) {
  if (!it) return '';
  return (BORDER_CSS[it.s] || '1px dashed ') + (it.c || '#000');
}
function makeCellDiv(cell) {
  const d = document.createElement('div');
  d.className = 'cell';
  // Frozen panes reuse the same cell renderer.  Keeping the logical address on
  // the element lets the interaction layer distinguish a fixed copy from the
  // scrolled body when a user clicks or double-clicks it.
  d.dataset.row = String(cell.r);
  d.dataset.col = String(cell.c);
  const st = cell.s || {};
  const isNum = cell.t === 'Number';
  if (st.ha === 'general') { if (isNum) d.classList.add('num'); }
  else if (st.ha === 'right') d.classList.add('num');
  else if (st.ha === 'center') d.style.justifyContent = 'center';
  if (st.va === 'top') d.style.alignItems = 'flex-start';
  else if (st.va === 'center') d.style.alignItems = 'center';
  if (st.wr) d.classList.add('wrap');
  if (st.b) d.style.fontWeight = 'bold';
  if (st.i) d.style.fontStyle = 'italic';
  let deco = '';
  if (st.u) deco += ' underline';
  if (st.st) deco += ' line-through';
  if (deco) d.style.textDecoration = deco.trim();
  if (st.fc) d.style.color = st.fc;
  if (st.bg) d.style.background = st.bg;
  // 字号随缩放（默认 12px）
  const sz = (st.sz || 12) * S.zoom;
  if (Math.abs(sz - 12) > 0.01) d.style.fontSize = sz + 'px';
  if (st.fn && st.fn !== 'Inter') d.style.fontFamily = `"${st.fn}", sans-serif`;
  const br = st.br || {};
  if (br.t) d.style.borderTop = borderCss(br.t);
  if (br.b) d.style.borderBottom = borderCss(br.b);
  if (br.l) d.style.borderLeft = borderCss(br.l);
  if (br.r) d.style.borderRight = borderCss(br.r);
  d.style.left = colX(cell.c) + 'px';
  d.style.top = rowY(cell.r) + 'px';
  d.style.width = colWidth(cell.c) + 1 + 'px';
  d.style.height = rowHeight(cell.r) + 1 + 'px';
  if (S.showFormulas && cell.f) {
    // 显示公式模式：呈现公式原文而非计算结果（Excel 式）
    d.textContent = cell.f;
    d.classList.add('formula-view');
  } else if (!renderRichTextInto(d, cell.rt) && !renderLatexInto(d, cell.v)) {
    d.textContent = cell.v;
  }
  return d;
}

function renderRichTextInto(el, runs) {
  if (!Array.isArray(runs) || runs.length === 0) return false;
  const wrapper = document.createElement('span');
  wrapper.className = 'cell-rich-text';
  for (const run of runs) {
    const span = document.createElement('span');
    span.className = 'rich-run';
    span.textContent = run.text || '';
    if (run.bold) span.style.fontWeight = 'bold';
    if (run.italic) span.style.fontStyle = 'italic';
    const decorations = [];
    if (run.underline) decorations.push('underline');
    if (run.strike) decorations.push('line-through');
    if (decorations.length) span.style.textDecoration = decorations.join(' ');
    if (Number.isFinite(run.size)) span.style.fontSize = run.size * S.zoom + 'px';
    if (run.font) span.style.fontFamily = `"${run.font}", sans-serif`;
    const runColor = run.resolvedColor || (typeof run.color === 'string' ? run.color : '');
    if (runColor) span.style.color = runColor;
    wrapper.appendChild(span);
  }
  el.appendChild(wrapper);
  return true;
}

// 合并区只渲染一个覆盖单元格。原先同时保留左上角普通单元格，会在透明背景或冻结窗格中
// 把同一段文字显示两次；冻结层还会把它限制在单列宽度内，形成导入后的重复换行叠影。
function appendCellsWithMerges(frag, data) {
  const merges = data.merges || [];
  const covered = (cell) => merges.some((m) => cell.r >= m.r0 && cell.r <= m.r1 && cell.c >= m.c0 && cell.c <= m.c1);
  for (const cell of data.cells || []) {
    if (!covered(cell)) frag.appendChild(makeCellDiv(cell));
  }
  for (const m of merges) {
    const intersects = m.r1 >= data.r0 && m.r0 <= data.r1 && m.c1 >= data.c0 && m.c0 <= data.c1;
    if (!intersects) continue;
    const tl = (data.cells || []).find((c) => c.r === m.r0 && c.c === m.c0)
      || { r: m.r0, c: m.c0, v: '', t: 'Text', s: null };
    const d = makeCellDiv(tl);
    d.classList.add('merged');
    d.style.width = colX(m.c1) + colWidth(m.c1) - colX(m.c0) + 1 + 'px';
    d.style.height = rowY(m.r1) + rowHeight(m.r1) - rowY(m.r0) + 1 + 'px';
    if (!(tl.s && tl.s.bg)) d.style.background = '#fff';
    frag.appendChild(d);
  }
}

/* ================= LaTeX 公式渲染（移植自母项目 UniDoc，无损） =================
 * 单元格存的永远是 $...$ 源码（编辑/复制/导出均为源文）；
 * 渲染只发生在显示层，失败回退原文。 */
const LATEX_RE = /\$\$([^$]+?)\$\$|\$([^$\n]+?)\$/g;
function renderLatexInto(el, text) {
  if (typeof katex === 'undefined' || !text || text.indexOf('$') < 0) return false;
  LATEX_RE.lastIndex = 0;
  if (!LATEX_RE.test(text)) return false;
  LATEX_RE.lastIndex = 0;
  const frag = document.createDocumentFragment();
  let last = 0, m;
  while ((m = LATEX_RE.exec(text)) !== null) {
    if (m.index > last) frag.appendChild(document.createTextNode(text.slice(last, m.index)));
    const raw = m[0];
    const tex = raw.replace(/^\$\$?|\$\$?$/g, '').trim();
    const span = document.createElement('span');
    span.className = 'latex-src';
    span.dataset.src = raw;
    try {
      span.innerHTML = katex.renderToString(tex, { displayMode: false, throwOnError: false });
    } catch (e) {
      span.textContent = raw;
    }
    frag.appendChild(span);
    last = m.index + raw.length;
  }
  if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
  el.appendChild(frag);
  return true;
}

function renderHeaders(vr) {
  const cf = document.createDocumentFragment();
  for (let c = vr.c0; c <= vr.c1; c++) {
    const d = document.createElement('div');
    d.className = 'col-hdr';
    if (c >= S.sel.c0 && c <= S.sel.c1) d.classList.add('sel');
    d.style.left = colX(c) + 'px';
    d.style.width = colWidth(c) + 'px';
    d.textContent = colName(c);
    d.dataset.col = c;
    const rz = document.createElement('div');
    rz.className = 'col-resizer';
    rz.dataset.col = c;
    d.appendChild(rz);
    cf.appendChild(d);
  }
  colHdrInner.textContent = '';
  colHdrInner.appendChild(cf);
  colHdrInner.style.transform = `translateX(${-gridScroll.scrollLeft}px)`;

  const rf = document.createDocumentFragment();
  for (let r = vr.r0; r <= vr.r1; r++) {
    const d = document.createElement('div');
    d.className = 'row-hdr';
    if (r >= S.sel.r0 && r <= S.sel.r1) d.classList.add('sel');
    d.style.top = rowY(r) + 'px';
    d.style.height = rowHeight(r) + 'px';
    d.textContent = r;
    d.dataset.row = r;
    const rz = document.createElement('div');
    rz.className = 'row-resizer';
    rz.dataset.row = r;
    d.appendChild(rz);
    rf.appendChild(d);
  }
  rowHdrInner.textContent = '';
  rowHdrInner.appendChild(rf);
  rowHdrInner.style.transform = `translateY(${-gridScroll.scrollTop}px)`;
}

/* ================= 冻结窗格（视觉层：三块覆盖 pane，随滚动反向补偿） ================= */
let freezeEls = null;
function ensureFreezeEls() {
  if (freezeEls) return freezeEls;
  const wrap = document.createElement('div');
  wrap.id = 'freeze-layer';
  const mk = (id) => {
    const pane = document.createElement('div');
    pane.id = id;
    pane.className = 'freeze-pane';
    const inner = document.createElement('div');
    inner.className = 'freeze-inner';
    pane.appendChild(inner);
    wrap.appendChild(pane);
    return { pane, inner };
  };
  const rows = mk('freeze-rows');
  const cols = mk('freeze-cols');
  const corner = mk('freeze-corner');
  // A frozen cell is a visual copy of the body cell.  Route pointer events back
  // through #grid-scroll with an explicit logical address; otherwise the event
  // falls through to the row currently underneath after a vertical scroll.
  const forwardGridEvent = (e) => {
    const pane = e.target.closest?.('.freeze-pane');
    if (!pane || !wrap.contains(pane)) return;
    const gridRect = gridScroll.getBoundingClientRect();
    const paneId = pane.id;
    const addX = paneId === 'freeze-rows' ? gridScroll.scrollLeft : 0;
    const addY = paneId === 'freeze-cols' ? gridScroll.scrollTop : 0;
    const localX = e.clientX - gridRect.left + addX;
    const localY = e.clientY - gridRect.top + addY;
    if (!Number.isFinite(localX) || !Number.isFinite(localY)) return;
    const hit = canonicalCell(rowAtY(Math.max(0, localY)), colAtX(Math.max(0, localX)));
    const forwarded = new MouseEvent(e.type, {
      bubbles: true,
      cancelable: true,
      view: e.view,
      detail: e.detail,
      screenX: e.screenX,
      screenY: e.screenY,
      clientX: e.clientX,
      clientY: e.clientY,
      ctrlKey: e.ctrlKey,
      shiftKey: e.shiftKey,
      altKey: e.altKey,
      metaKey: e.metaKey,
      button: e.button,
      buttons: e.buttons,
      relatedTarget: e.relatedTarget,
    });
    Object.defineProperty(forwarded, '__unicellFrozenCell', {
      configurable: false,
      enumerable: false,
      value: { r: hit.r, c: hit.c, merge: hit.merge },
    });
    Object.defineProperty(forwarded, '__unicellFrozenPane', {
      configurable: false,
      enumerable: false,
      value: {
        rows: paneId === 'freeze-rows' || paneId === 'freeze-corner',
        cols: paneId === 'freeze-cols' || paneId === 'freeze-corner',
      },
    });
    e.preventDefault();
    e.stopPropagation();
    gridScroll.dispatchEvent(forwarded);
  };
  ['mousedown', 'dblclick', 'contextmenu'].forEach((type) => wrap.addEventListener(type, forwardGridEvent));
  // Keep wheel scrolling natural while the fixed surface is under the pointer.
  wrap.addEventListener('wheel', (e) => {
    const pane = e.target.closest?.('.freeze-pane');
    if (!pane || !wrap.contains(pane)) return;
    gridScroll.scrollLeft += e.deltaX;
    gridScroll.scrollTop += e.deltaY;
    e.preventDefault();
    e.stopPropagation();
  }, { passive: false });
  // Row and column headers live outside #grid-scroll.  They therefore need a
  // small companion layer of their own: translating the normal header inner
  // element while scrolling otherwise makes the frozen row numbers disappear
  // (and makes frozen-column letters scroll away).
  const headerLayer = document.createElement('div');
  headerLayer.id = 'freeze-header-layer';
  const mkHeader = (id) => {
    const pane = document.createElement('div');
    pane.id = id;
    pane.className = 'freeze-header-pane';
    const inner = document.createElement('div');
    inner.className = 'freeze-header-inner';
    pane.appendChild(inner);
    headerLayer.appendChild(pane);
    return { pane, inner };
  };
  const rowHeaders = mkHeader('freeze-row-headers');
  const colHeaders = mkHeader('freeze-col-headers');
  // Keep the frozen labels interactive.  The overlay is visually above the
  // scrolling headers, so a click would otherwise fall through to whichever
  // (scrolled) row/column happens to be underneath.  Forward header events to
  // the canonical header element; this preserves the existing selection,
  // context-menu and resize handlers in one place.
  const forwardHeaderEvent = (e) => {
    const source = e.target.closest?.('.row-hdr, .col-hdr');
    if (!source || !headerLayer.contains(source)) return;
    const axis = source.classList.contains('row-hdr') ? 'row' : 'col';
    const index = Number(source.dataset[axis]);
    if (!Number.isFinite(index) || index < 1) return;
    const root = axis === 'row' ? rowHdrInner : colHdrInner;
    const selector = `.${axis}-hdr[data-${axis}="${index}"]`;
    // Virtualized headers may have discarded the first frozen rows/columns
    // after scrolling.  Dispatch through a short-lived off-screen shim when
    // the canonical element is not in the current virtual range.
    const canonical = root.querySelector(selector);
    const shim = canonical || document.createElement('div');
    if (!canonical) {
      shim.className = `${axis}-hdr`;
      shim.dataset[axis] = String(index);
      shim.style.cssText = 'position:absolute;left:-10000px;top:-10000px;width:1px;height:1px;';
      root.appendChild(shim);
    }
    const isResizer = !!e.target.closest?.(`.${axis}-resizer`);
    const target = isResizer
      ? shim.querySelector(`.${axis}-resizer`) || (() => {
        const rz = document.createElement('div');
        rz.className = `${axis}-resizer`;
        rz.dataset[axis] = String(index);
        shim.appendChild(rz);
        return rz;
      })()
      : shim;
    const forwarded = new MouseEvent(e.type, {
      bubbles: true,
      cancelable: true,
      view: e.view,
      detail: e.detail,
      screenX: e.screenX,
      screenY: e.screenY,
      clientX: e.clientX,
      clientY: e.clientY,
      ctrlKey: e.ctrlKey,
      shiftKey: e.shiftKey,
      altKey: e.altKey,
      metaKey: e.metaKey,
      button: e.button,
      buttons: e.buttons,
      relatedTarget: e.relatedTarget,
    });
    e.preventDefault();
    e.stopPropagation();
    try { target.dispatchEvent(forwarded); }
    finally { if (!canonical) shim.remove(); }
  };
  headerLayer.addEventListener('mousedown', forwardHeaderEvent);
  headerLayer.addEventListener('contextmenu', forwardHeaderEvent);
  $('grid-wrap').appendChild(headerLayer);
  $('grid-wrap').appendChild(wrap);
  freezeEls = { wrap, rows, cols, corner, headerLayer, rowHeaders, colHeaders };
  return freezeEls;
}
let freezeSeq = 0;
async function renderFreeze(j, vr) {
  const fr = j.frozenRows || 0, fc = j.frozenCols || 0;
  const freezeChanged = !S.frozen || S.frozen.rows !== fr || S.frozen.cols !== fc;
  S.frozen = { rows: fr, cols: fc };
  if (freezeChanged) renderObjects();
  const els = ensureFreezeEls();
  if (fr <= 0 && fc <= 0) {
    els.wrap.style.display = 'none';
    els.headerLayer.style.display = 'none';
    return;
  }
  els.wrap.style.display = 'block';
  els.headerLayer.style.display = 'block';
  const seq = ++freezeSeq;
  const reqs = [];
  reqs.push(fr > 0 ? api(`/api/view?sheet=${S.sheet}&r0=1&c0=${Math.max(1, vr.c0)}&r1=${fr}&c1=${vr.c1}`) : null);
  reqs.push(fc > 0 ? api(`/api/view?sheet=${S.sheet}&r0=${Math.max(1, vr.r0)}&c0=1&r1=${vr.r1}&c1=${fc}`) : null);
  reqs.push(fr > 0 && fc > 0 ? api(`/api/view?sheet=${S.sheet}&r0=1&c0=1&r1=${fr}&c1=${fc}`) : null);
  const [jr, jc, jx] = await Promise.all(reqs.map((p) => p || Promise.resolve(null)));
  if (seq !== freezeSeq) return;
  // Re-read geometry after the requests complete.  Font loading, a scrollbar
  // appearing, or a window resize during the fetch must not leave the panes a
  // few pixels wider/taller than the actual scrolling viewport.
  const offX = gridScroll.offsetLeft, offY = gridScroll.offsetTop;
  const fh = fr > 0 ? rowY(fr + 1) : 0;   // 冻结行总高
  const fw = fc > 0 ? colX(fc + 1) : 0;   // 冻结列总宽
  const fill = (slot, data, w, h, x, y) => {
    slot.pane.style.display = data ? 'block' : 'none';
    if (!data) return;
    Object.assign(slot.pane.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
    const frag = document.createDocumentFragment();
    appendCellsWithMerges(frag, data);
    slot.inner.textContent = '';
    slot.inner.appendChild(frag);
  };
  // clientWidth/clientHeight exclude the native scrollbars and are the exact
  // visible area that Excel keeps uncovered by a frozen pane.
  const vw = gridScroll.clientWidth, vh = gridScroll.clientHeight;
  fill(els.rows, jr, vw, fh, offX, offY);
  fill(els.cols, jc, fw, vh, offX, offY);
  fill(els.corner, jx, fw, fh, offX, offY);
  const renderHeaderPane = (slot, axisCount, axis, x, y, w, h) => {
    slot.pane.style.display = axisCount > 0 ? 'block' : 'none';
    if (axisCount <= 0) return;
    Object.assign(slot.pane.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
    // The inner layer is absolutely positioned, so percentage dimensions on
    // .row-hdr/.col-hdr have no containing block unless we size it explicitly.
    // Without this, the frozen header labels collapse to zero width/height.
    Object.assign(slot.inner.style, { width: w + 'px', height: h + 'px' });
    const frag = document.createDocumentFragment();
    const sel = normSel();
    for (let n = 1; n <= axisCount; n++) {
      const d = document.createElement('div');
      if (axis === 'row') {
        d.className = 'row-hdr';
        d.style.top = rowY(n) + 'px';
        d.style.width = w + 'px';
        d.style.height = rowHeight(n) + 'px';
        d.textContent = n;
        d.dataset.row = n;
        if (n >= sel.r0 && n <= sel.r1) d.classList.add('sel');
        const rz = document.createElement('div');
        rz.className = 'row-resizer';
        rz.dataset.row = n;
        d.appendChild(rz);
      } else {
        d.className = 'col-hdr';
        d.style.left = colX(n) + 'px';
        d.style.width = colWidth(n) + 'px';
        d.style.height = h + 'px';
        d.textContent = colName(n);
        d.dataset.col = n;
        if (n >= sel.c0 && n <= sel.c1) d.classList.add('sel');
        const rz = document.createElement('div');
        rz.className = 'col-resizer';
        rz.dataset.col = n;
        d.appendChild(rz);
      }
      frag.appendChild(d);
    }
    slot.inner.textContent = '';
    slot.inner.appendChild(frag);
  };
  renderHeaderPane(els.rowHeaders, fr, 'row', 0, offY, offX, fh);
  renderHeaderPane(els.colHeaders, fc, 'col', offX, 0, fw, offY);
  syncFreezeScroll();
}
function syncFreezeScroll() {
  if (!freezeEls || !S.frozen || (S.frozen.rows <= 0 && S.frozen.cols <= 0)) return;
  freezeEls.rows.inner.style.transform = `translateX(${-gridScroll.scrollLeft}px)`;
  freezeEls.cols.inner.style.transform = `translateY(${-gridScroll.scrollTop}px)`;
}

/* ================= 选区渲染与操作 ================= */
function mergeAt(r, c) {
  return (S.merges || []).find((merge) => r >= merge.r0 && r <= merge.r1 && c >= merge.c0 && c <= merge.c1) || null;
}
function canonicalCell(r, c) {
  const merge = mergeAt(r, c);
  return merge ? { r: merge.r0, c: merge.c0, merge } : { r, c, merge: null };
}
function normalizeCurrentForMerges() {
  const current = canonicalCell(S.cur.r, S.cur.c);
  const anchor = canonicalCell(S.anchor.r, S.anchor.c);
  S.cur = { r: current.r, c: current.c };
  S.anchor = { r: anchor.r, c: anchor.c };
  S.sel = expandRangeForMerges({
    r0: Math.min(S.sel.r0, S.sel.r1), c0: Math.min(S.sel.c0, S.sel.c1),
    r1: Math.max(S.sel.r0, S.sel.r1), c1: Math.max(S.sel.c0, S.sel.c1),
  });
}
function expandRangeForMerges(range) {
  const expanded = { ...range };
  // A merge pulled into the range can intersect another merge after expansion. Iterate to a
  // fixed point so all selection/edit operations address complete merged regions only.
  let changed = true;
  while (changed) {
    changed = false;
    for (const merge of S.merges || []) {
      const intersects = merge.r1 >= expanded.r0 && merge.r0 <= expanded.r1
        && merge.c1 >= expanded.c0 && merge.c0 <= expanded.c1;
      if (!intersects) continue;
      const next = {
        r0: Math.min(expanded.r0, merge.r0), c0: Math.min(expanded.c0, merge.c0),
        r1: Math.max(expanded.r1, merge.r1), c1: Math.max(expanded.c1, merge.c1),
      };
      if (next.r0 !== expanded.r0 || next.c0 !== expanded.c0 || next.r1 !== expanded.r1 || next.c1 !== expanded.c1) {
        Object.assign(expanded, next); changed = true;
      }
    }
  }
  return expanded;
}
function mergedNavigationTarget(r, c, dr, dc) {
  const merge = mergeAt(r, c);
  if (!merge) return { r: Math.max(1, r + dr), c: Math.max(1, c + dc) };
  if (dr > 0) return { r: merge.r1 + 1, c: merge.c0 };
  if (dr < 0) return { r: Math.max(1, merge.r0 - 1), c: merge.c0 };
  if (dc > 0) return { r: merge.r0, c: merge.c1 + 1 };
  if (dc < 0) return { r: merge.r0, c: Math.max(1, merge.c0 - 1) };
  return { r: merge.r0, c: merge.c0 };
}
function mergedCellRect(r, c) {
  const merge = mergeAt(r, c);
  const range = merge || { r0: r, c0: c, r1: r, c1: c };
  const left = colX(range.c0), top = rowY(range.r0);
  return {
    ...range,
    left,
    top,
    width: colX(range.c1) + colWidth(range.c1) - left,
    height: rowY(range.r1) + rowHeight(range.r1) - top,
  };
}
function normSel() {
  const s = S.sel;
  return {
    r0: Math.min(s.r0, s.r1), r1: Math.max(s.r0, s.r1),
    c0: Math.min(s.c0, s.c1), c1: Math.max(s.c0, s.c1),
  };
}
function renderSelection() {
  const n = normSel();
  const selR = $('sel-range'), cur = $('sel-cursor'), fh = $('fill-handle');
  const x = colX(n.c0), y = rowY(n.r0);
  const w = colX(n.c1) + colWidth(n.c1) - x, h = rowY(n.r1) + rowHeight(n.r1) - y;
  const currentMerge = mergeAt(S.cur.r, S.cur.c);
  const cursorRange = currentMerge || { r0: S.cur.r, c0: S.cur.c, r1: S.cur.r, c1: S.cur.c };
  const selectionIsCursor = n.r0 === cursorRange.r0 && n.c0 === cursorRange.c0
    && n.r1 === cursorRange.r1 && n.c1 === cursorRange.c1;
  const multi = n.r0 !== n.r1 || n.c0 !== n.c1;
  selR.style.display = multi && !selectionIsCursor ? 'block' : 'none';
  Object.assign(selR.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
  const cx = colX(cursorRange.c0), cy = rowY(cursorRange.r0);
  const cw = colX(cursorRange.c1) + colWidth(cursorRange.c1) - cx;
  const ch = rowY(cursorRange.r1) + rowHeight(cursorRange.r1) - cy;
  Object.assign(cur.style, {
    display: 'block', left: cx - 1 + 'px', top: cy - 1 + 'px',
    width: cw + 1 + 'px', height: ch + 1 + 'px',
  });
  Object.assign(fh.style, {
    display: 'block',
    left: x + w - 4 + 'px', top: y + h - 4 + 'px',
  });
  nameBox.value = cellRef(S.cur.r, S.cur.c);
  updateFormulaBar();
  updateStats();
  window.updateDvDropdown?.();
}

async function updateFormulaBar() {
  const request = { sheet: S.sheet, row: S.cur.r, col: S.cur.c };
  const j = await api(`/api/cell?sheet=${request.sheet}&row=${request.row}&col=${request.col}`);
  if (request.sheet !== S.sheet || request.row !== S.cur.r || request.col !== S.cur.c) return;
  S.formulaCellStyle = { ...request, style: j.style };
  if (!S.editing) {
    formulaInput.value = j.content;
    if (j.hasRichText || (Array.isArray(j.richText) && j.richText.length)) showRichFormulaView(j.richText, j.content);
    else setRichFormulaMode(false, false);
    syncToolbarFromStyle(j.style);
  } else if (S.editHasRichText) syncToolbarFromRichSelection();
  else syncToolbarFromStyle(j.style);
}

function syncToolbarFromStyle(st) {
  $('btn-bold').classList.toggle('active', !!st.b);
  $('btn-italic').classList.toggle('active', !!st.i);
  $('btn-underline').classList.toggle('active', !!st.u);
  $('btn-strike').classList.toggle('active', !!st.st);
  $('btn-wrap').classList.toggle('active', !!st.wr);
  if (st.sz) $('sel-fontsize').value = st.sz;
  // 字体回显（Word/Excel 行为：光标在哪，下拉显示谁）
  const ff = $('sel-fontfamily');
  if (ff) ff.value = [...ff.options].some((o) => o.value === st.fn) ? st.fn : '';
}

let statsTimer = null;
function updateStats() {
  if (statsTimer) clearTimeout(statsTimer);
  statsTimer = setTimeout(async () => {
    const n = normSel();
    const j = await api(`/api/stats?sheet=${S.sheet}&r0=${n.r0}&c0=${n.c0}&r1=${n.r1}&c1=${n.c1}`);
    $('status-stats').textContent = j.numbers > 0
      ? `求和: ${fmtNum(j.sum)}  计数: ${j.count}  平均值: ${fmtNum(j.avg)}`
      : (j.count > 1 ? `计数: ${j.count}` : '');
  }, 120);
}
const fmtNum = (x) => Math.abs(x) < 1e12 ? +x.toFixed(6) + '' : x.toExponential(4);

function setCursor(r, c, extend, keepScroll = false) {
  r = Math.max(1, r); c = Math.max(1, c);
  const target = canonicalCell(r, c);
  r = target.r; c = target.c;
  ensureVirtual(target.merge?.r1 || r, target.merge?.c1 || c);
  if (extend) {
    const raw = {
      r0: Math.min(S.anchor.r, r), c0: Math.min(S.anchor.c, c),
      r1: Math.max(S.anchor.r, target.merge?.r1 || r), c1: Math.max(S.anchor.c, target.merge?.c1 || c),
    };
    S.sel = expandRangeForMerges(raw);
    S.cur = { r, c };
  } else {
    S.cur = { r, c }; S.anchor = { r, c };
    S.sel = target.merge
      ? { r0: target.merge.r0, c0: target.merge.c0, r1: target.merge.r1, c1: target.merge.c1 }
      : { r0: r, c0: c, r1: r, c1: c };
  }
  // A click routed from a frozen pane already points at a cell that is visible
  // in the fixed surface.  Scrolling that logical row/column into the regular
  // viewport would jump the workbook back to row 1 (or the left edge), which
  // is not how Excel behaves while frozen panes are active.
  if (!keepScroll) scrollCursorIntoView();
  renderSelection();
  renderHeadersSelOnly();
}
function renderHeadersSelOnly() {
  const n = normSel();
  const colHeaders = [colHdrInner, freezeEls?.colHeaders?.inner].filter(Boolean);
  const rowHeaders = [rowHdrInner, freezeEls?.rowHeaders?.inner].filter(Boolean);
  colHeaders.forEach((root) => root.querySelectorAll('.col-hdr').forEach((d) => {
    const c = +d.dataset.col;
    d.classList.toggle('sel', c >= n.c0 && c <= n.c1);
  }));
  rowHeaders.forEach((root) => root.querySelectorAll('.row-hdr').forEach((d) => {
    const r = +d.dataset.row;
    d.classList.toggle('sel', r >= n.r0 && r <= n.r1);
  }));
}
function ensureVirtual(r, c) {
  let changed = false;
  while (r > S.vRows - 20) { S.vRows += 200; changed = true; }
  while (c > S.vCols - 5) { S.vCols += 20; changed = true; }
  if (changed) updateSpacer();
}
function scrollCursorIntoView() {
  const rect = mergedCellRect(S.cur.r, S.cur.c);
  const x = rect.left, y = rect.top;
  const w = rect.width, h = rect.height;
  const el = gridScroll;
  if (x < el.scrollLeft) el.scrollLeft = x;
  else if (x + w > el.scrollLeft + el.clientWidth) el.scrollLeft = x + w - el.clientWidth;
  if (y < el.scrollTop) el.scrollTop = y;
  else if (y + h > el.scrollTop + el.clientHeight) el.scrollTop = y + h - el.clientHeight;
}

/* ================= 原生富文本公式栏 =================
 * Excel 的公式栏允许在同一单元格内选择字符并修改字体。DOM 只是编辑表面；提交前会重新
 * 归一化为 OOXML 可往返的 runs，普通文本和公式仍完全沿用原 textarea/输入接口。 */
const RICH_STYLE_KEYS = ['bold', 'italic', 'underline', 'strike', 'size', 'font', 'color'];
function normalizeRichRun(run) {
  run = run && typeof run === 'object' ? run : {};
  const size = Number(run.size);
  const rawColor = Object.prototype.hasOwnProperty.call(run, 'color')
    && ((typeof run.color === 'string' && !!run.color) || Array.isArray(run.color) || run.color === null)
    ? (Array.isArray(run.color) ? [...run.color] : run.color) : null;
  const resolvedColor = typeof run.resolvedColor === 'string' && run.resolvedColor
    ? run.resolvedColor : (typeof rawColor === 'string' ? rawColor : '');
  return {
    text: String(run.text ?? ''),
    bold: !!run.bold,
    italic: !!run.italic,
    underline: run.underline === true || (typeof run.underline === 'string' && run.underline !== 'none'),
    strike: !!run.strike,
    ...(Number.isFinite(size) && size > 0 ? { size } : {}),
    ...(typeof run.font === 'string' && run.font ? { font: run.font } : {}),
    color: rawColor,
    ...(resolvedColor ? { resolvedColor } : {}),
  };
}
function richStylesEqual(a, b) {
  return RICH_STYLE_KEYS.every((key) => {
    if (key === 'color') return JSON.stringify(a?.color ?? null) === JSON.stringify(b?.color ?? null);
    return (a?.[key] ?? false) === (b?.[key] ?? false);
  });
}
function mergeRichRuns(runs, keepEmpty = false) {
  const out = [];
  for (const raw of runs || []) {
    const run = normalizeRichRun(raw);
    if (!run.text && !keepEmpty) continue;
    const prev = out[out.length - 1];
    if (prev && richStylesEqual(prev, run)) {
      prev.text += run.text;
      if (!prev.resolvedColor && run.resolvedColor) prev.resolvedColor = run.resolvedColor;
    }
    else out.push(run);
  }
  return out.length ? out : (keepEmpty ? [normalizeRichRun({ text: '' })] : []);
}
function normalizeRichRuns(runs, fallback = '') {
  const normalized = mergeRichRuns(Array.isArray(runs) ? runs : []);
  return normalized.length ? normalized : [normalizeRichRun({ text: fallback })];
}
function richStyleFromCellStyle(st) {
  st = st && typeof st === 'object' ? st : {};
  return normalizeRichRun({
    text: '', bold: !!st.b, italic: !!st.i, underline: !!st.u, strike: !!(st.st || st.strike),
    size: st.sz, font: st.fn, color: st.fc,
  });
}
function richRunsText(runs) { return (runs || []).map((run) => run.text || '').join(''); }
function richRunsEqual(a, b) {
  const aa = mergeRichRuns(a, true), bb = mergeRichRuns(b, true);
  return aa.length === bb.length && aa.every((run, i) => run.text === bb[i].text && richStylesEqual(run, bb[i]));
}
function richRunsForApi(runs) {
  return mergeRichRuns(runs, true).map((raw) => {
    const run = normalizeRichRun(raw);
    return {
      text: run.text, bold: run.bold, italic: run.italic, underline: run.underline, strike: run.strike,
      ...(run.size ? { size: run.size } : {}),
      ...(run.font ? { font: run.font } : {}),
      color: Array.isArray(run.color) ? [...run.color] : run.color,
    };
  });
}
function setRichRunElementStyle(span, raw) {
  const run = normalizeRichRun(raw);
  span.className = 'rich-run';
  span.dataset.richRun = '1';
  for (const key of RICH_STYLE_KEYS.filter((item) => item !== 'color')) {
    if (run[key] !== undefined && run[key] !== false && run[key] !== '') span.dataset[key] = String(run[key]);
  }
  span.dataset.colorToken = JSON.stringify(run.color ?? null);
  if (run.resolvedColor) span.dataset.resolvedColor = run.resolvedColor;
  if (run.bold) span.style.fontWeight = 'bold';
  if (run.italic) span.style.fontStyle = 'italic';
  const decorations = [];
  if (run.underline) decorations.push('underline');
  if (run.strike) decorations.push('line-through');
  if (decorations.length) span.style.textDecoration = decorations.join(' ');
  if (run.size) span.style.fontSize = run.size + 'pt';
  if (run.font) span.style.fontFamily = `"${run.font}", sans-serif`;
  if (run.resolvedColor) span.style.color = run.resolvedColor;
}
function renderRichRunsInto(root, runs) {
  root.textContent = '';
  const frag = document.createDocumentFragment();
  for (const run of normalizeRichRuns(runs)) {
    const span = document.createElement('span');
    setRichRunElementStyle(span, run);
    span.textContent = run.text;
    frag.appendChild(span);
  }
  root.appendChild(frag);
}
function renderRichFormulaRuns(runs) {
  renderRichRunsInto(richFormulaInput, runs);
}
function renderRichEditSurfaces(runs, preserveRoot = null) {
  if (preserveRoot !== richFormulaInput) renderRichRunsInto(richFormulaInput, runs);
  if (preserveRoot !== richCellEditor) renderRichRunsInto(richCellEditor, runs);
}
function richStyleFromElement(el, inherited) {
  const style = { ...(inherited || {}) };
  if (!(el instanceof HTMLElement)) return style;
  if (el.dataset.richRun) {
    for (const key of ['bold', 'italic', 'underline', 'strike']) style[key] = el.dataset[key] === 'true';
    if (el.dataset.size) style.size = Number(el.dataset.size);
    if (el.dataset.font) style.font = el.dataset.font;
    if (el.dataset.colorToken) {
      try { style.color = JSON.parse(el.dataset.colorToken); } catch { style.color = null; }
    }
    if (el.dataset.resolvedColor) style.resolvedColor = el.dataset.resolvedColor;
  }
  const tag = el.tagName;
  if (tag === 'B' || tag === 'STRONG') style.bold = true;
  if (tag === 'I' || tag === 'EM') style.italic = true;
  if (tag === 'U') style.underline = true;
  if (tag === 'S' || tag === 'STRIKE' || tag === 'DEL') style.strike = true;
  return style;
}
function richRunsFromDom(root) {
  const runs = [];
  const push = (text, style) => {
    if (!text) return;
    runs.push({ text, ...style });
  };
  const walk = (node, inherited) => {
    if (node.nodeType === Node.TEXT_NODE) { push(node.nodeValue || '', inherited); return; }
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const el = node;
    if (el.tagName === 'BR') { push('\n', inherited); return; }
    const style = richStyleFromElement(el, inherited);
    const block = el !== root && /^(DIV|P|LI)$/.test(el.tagName);
    if (block && runs.length && !richRunsText(runs).endsWith('\n')) push('\n', inherited);
    for (const child of el.childNodes) walk(child, style);
  };
  for (const child of root.childNodes) walk(child, {});
  return mergeRichRuns(runs, true);
}
function richFormulaRunsFromDom() { return richRunsFromDom(richFormulaInput); }
function activeRichSurface() {
  const selection = window.getSelection();
  if (selection?.rangeCount) {
    const range = selection.getRangeAt(0);
    if (richCellEditor.contains(range.startContainer) && richCellEditor.contains(range.endContainer)) return richCellEditor;
    if (richFormulaInput.contains(range.startContainer) && richFormulaInput.contains(range.endContainer)) return richFormulaInput;
  }
  if (document.activeElement === richCellEditor || document.activeElement === richFormulaInput) {
    return document.activeElement;
  }
  return S.richEditSurface || richFormulaInput;
}
function richEditRunsFromDom() { return richRunsFromDom(activeRichSurface()); }
function isCellEditTarget(target) {
  return [editor, formulaInput, richCellEditor, richFormulaInput].some((root) => root.contains(target));
}
function isEditFormatTarget(target) {
  if (target?.closest?.('#selection-toolbar, .color-pop, .cp-pop')) return true;
  return !!target?.closest?.('.rb-group')?.querySelector('#sel-fontfamily, #btn-align-left, #sel-numfmt');
}
function rememberEditSelection() {
  if (!S.editing) return;
  if (!S.editHasRichText) {
    if (document.activeElement === formulaInput) rememberPlainFormulaSelection();
    else if (document.activeElement === editor) S.plainFormulaSelection = null;
    return;
  }
  // Native selects/color inputs may collapse the DOM range after taking focus.
  if (isEditFormatTarget(document.activeElement) && S.richSelection) return;
  const surface = activeRichSurface();
  const offsets = richSelectionOffsets(surface);
  if (offsets) { S.richSelection = offsets; S.richEditSurface = surface; }
}
let editComposing = false;
for (const surface of [editor, formulaInput, richCellEditor, richFormulaInput]) {
  surface.addEventListener('compositionstart', () => { editComposing = true; });
  surface.addEventListener('compositionend', () => { editComposing = false; });
}
document.addEventListener('mousedown', (event) => {
  if (!S.editing || editComposing || event.isComposing || event.button !== 0) return;
  if (isEditFormatTarget(event.target)) { rememberEditSelection(); return; }
  if (isCellEditTarget(event.target)
      || event.target.closest?.('#formula-cancel, #formula-accept, #formula-expand, #fn-hint')) return;
  // Grid clicks retain their existing selection and formula-reference handling.
  if (!gridScroll.contains(event.target)) void commitEdit().catch(() => undefined);
}, true);
function richSelectionOffsets(root = activeRichSurface()) {
  const sel = window.getSelection();
  if (!sel || !sel.rangeCount) return null;
  const range = sel.getRangeAt(0);
  if (!root.contains(range.startContainer) || !root.contains(range.endContainer)) return null;
  try {
    const beforeStart = range.cloneRange();
    beforeStart.selectNodeContents(root);
    beforeStart.setEnd(range.startContainer, range.startOffset);
    const beforeEnd = range.cloneRange();
    beforeEnd.selectNodeContents(root);
    beforeEnd.setEnd(range.endContainer, range.endOffset);
    return { start: beforeStart.toString().length, end: beforeEnd.toString().length };
  } catch { return null; }
}
function restoreRichSelection(offsets, focus = true, root = activeRichSurface()) {
  if (!offsets) return;
  const total = root.textContent.length;
  const start = Math.max(0, Math.min(total, offsets.start));
  const end = Math.max(start, Math.min(total, offsets.end));
  const locate = (wanted) => {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let seen = 0, node;
    while ((node = walker.nextNode())) {
      const next = seen + node.nodeValue.length;
      if (wanted <= next) return [node, wanted - seen];
      seen = next;
    }
    return [root, root.childNodes.length];
  };
  const [sn, so] = locate(start), [en, eo] = locate(end);
  const range = document.createRange();
  range.setStart(sn, so); range.setEnd(en, eo);
  if (focus) root.focus({ preventScroll: true });
  const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(range);
  S.richSelection = { start, end };
  S.richEditSurface = root;
}
function richStyleAtOffset(runs, offset) {
  let pos = 0, previous = normalizeRichRun({ text: '' });
  for (const run of runs) {
    const next = pos + run.text.length;
    if (offset < next || (offset === next && next === richRunsText(runs).length)) return { ...run, text: undefined };
    previous = run; pos = next;
  }
  return { ...previous, text: undefined };
}
function syncToolbarFromRichStyle(style) {
  $('btn-bold').classList.toggle('active', !!style.bold);
  $('btn-italic').classList.toggle('active', !!style.italic);
  $('btn-underline').classList.toggle('active', !!style.underline);
  $('btn-strike').classList.toggle('active', !!style.strike);
  if (style.size) $('sel-fontsize').value = String(style.size);
  const ff = $('sel-fontfamily');
  if (ff && style.font) ff.value = [...ff.options].some((o) => o.value === style.font) ? style.font : '';
  const visualColor = style.resolvedColor || (typeof style.color === 'string' ? style.color : '');
  if (visualColor) syncColorBtn('font-color-btn', visualColor);
}
function syncToolbarFromRichSelection() {
  if (!S.editing || !S.editHasRichText) return;
  const root = activeRichSurface();
  const offsets = richSelectionOffsets(root) || S.richSelection || { start: 0, end: 0 };
  const runs = richRunsFromDom(root);
  syncToolbarFromRichStyle(S.richTypingStyle || richStyleAtOffset(runs, offsets.start));
}
function activeEditRect() {
  const base = mergedCellRect(S.editCell?.r || S.cur.r, S.editCell?.c || S.cur.c);
  const frozen = S.editFrozen;
  if (!frozen) return base;
  // #cell-editor and #rich-cell-editor are children of the scrolling grid.  A
  // frozen row/column therefore needs the corresponding scroll offset added
  // back to its content coordinate so the editor stays over the fixed copy.
  return {
    ...base,
    left: base.left + (frozen.cols ? gridScroll.scrollLeft : 0),
    top: base.top + (frozen.rows ? gridScroll.scrollTop : 0),
  };
}
function positionPlainCellEditor(editRect = activeEditRect()) {
  Object.assign(editor.style, {
    display: 'block',
    left: editRect.left - 1 + 'px',
    top: editRect.top - 1 + 'px',
    width: Math.max(editRect.width + 1, 60) + 'px',
    height: editRect.height + 1 + 'px',
  });
}
function positionRichCellEditor(editRect = activeEditRect()) {
  const editCell = S.editCell ? { ...S.editCell } : null;
  const base = S.editBaseRichStyle || {};
  const cachedStyle = S.cellsCache.get(`${S.editCell?.r},${S.editCell?.c}`)?.s;
  const formulaStyle = S.formulaCellStyle
    && S.formulaCellStyle.sheet === S.editCell?.sheet
    && S.formulaCellStyle.row === S.editCell?.r
    && S.formulaCellStyle.col === S.editCell?.c
    ? S.formulaCellStyle.style : null;
  const cellStyle = cachedStyle || formulaStyle || {};
  const wrap = !!cellStyle.wr;
  const visualColor = base.resolvedColor || (typeof base.color === 'string' ? base.color : '#000000');
  Object.assign(richCellEditor.style, {
    display: 'block',
    left: editRect.left - 1 + 'px',
    top: editRect.top - 1 + 'px',
    width: Math.max(editRect.width + 1, 60) + 'px',
    height: Math.max(editRect.height + 1, 22) + 'px',
    minWidth: Math.max(editRect.width + 1, 60) + 'px',
    minHeight: Math.max(editRect.height + 1, 22) + 'px',
    whiteSpace: wrap ? 'pre-wrap' : 'pre',
    overflowWrap: wrap ? 'anywhere' : 'normal',
    wordBreak: wrap ? 'break-all' : 'normal',
    fontFamily: base.font ? `"${base.font}", sans-serif` : '',
    fontSize: base.size ? base.size + 'pt' : '',
    fontWeight: base.bold ? 'bold' : '',
    fontStyle: base.italic ? 'italic' : '',
    color: visualColor,
    background: cellStyle.bg || '#ffffff',
  });
  // Non-wrapped Excel cell editing expands over empty neighbours. Measure once synchronously
  // (the runs are already in the DOM) and once on the next frame. The synchronous pass prevents
  // an async /api/cell response from briefly restoring the narrow merged-cell width.
  const fitRichEditorToContent = () => {
    if (!S.editing || !S.editHasRichText || richCellEditor.style.display === 'none') return;
    if (!editCell || !S.editCell || editCell.sheet !== S.editCell.sheet
        || editCell.r !== S.editCell.r || editCell.c !== S.editCell.c) return;
    if (!wrap) richCellEditor.style.width = Math.max(editRect.width + 1, richCellEditor.scrollWidth + 4, 60) + 'px';
    richCellEditor.style.height = Math.max(editRect.height + 1, richCellEditor.scrollHeight + 4, 22) + 'px';
  };
  fitRichEditorToContent();
  requestAnimationFrame(fitRichEditorToContent);
}
function setRichFormulaMode(active, editable = false) {
  formulaRow.classList.toggle('rich-text-mode', !!active);
  formulaRow.classList.toggle('rich-text-edit', !!active && !!editable);
  richFormulaInput.contentEditable = editable ? 'true' : 'false';
  richFormulaInput.setAttribute('aria-readonly', editable ? 'false' : 'true');
  richCellEditor.contentEditable = editable ? 'true' : 'false';
  richCellEditor.setAttribute('aria-readonly', editable ? 'false' : 'true');
  if (!editable) richCellEditor.style.display = 'none';
}
function showRichFormulaView(runs, fallback = '') {
  setRichFormulaMode(true, false);
  renderRichFormulaRuns(normalizeRichRuns(runs, fallback));
}
function enterRichFormulaEdit(runs, fallback, {
  focus = true, selectAll = false, draftRuns = null, selection = null, surface = null, editRect = null,
} = {}) {
  const original = normalizeRichRuns(runs, fallback);
  const normalized = draftRuns ? normalizeRichRuns(draftRuns, fallback) : original;
  S.editHasRichText = true;
  S.editRichRuns = normalized;
  S.editRichOriginal = original.map((run) => ({ ...run }));
  S.richTypingStyle = null;
  setRichFormulaMode(true, true);
  renderRichEditSurfaces(normalized);
  editor.style.display = 'none';
  positionRichCellEditor(editRect || mergedCellRect(S.editCell?.r || S.cur.r, S.editCell?.c || S.cur.c));
  const end = richRunsText(normalized).length;
  const offsets = selection || (selectAll ? { start: 0, end } : { start: end, end });
  S.richSelection = offsets;
  const targetSurface = surface || S.richEditSurface || (focus ? richCellEditor : richFormulaInput);
  S.richEditSurface = targetSurface;
  const editCell = S.editCell;
  requestAnimationFrame(() => {
    if (!S.editing || S.editCell !== editCell || !S.editHasRichText) return;
    restoreRichSelection(S.richSelection || offsets, focus && !isEditFormatTarget(document.activeElement), S.richEditSurface || targetSurface);
  });
}
function promotePlainConstantToRichEdit() {
  if (!S.editing || S.editHasRichText) return S.editHasRichText;
  const formulaWasActive = document.activeElement === formulaInput || !!S.plainFormulaSelection;
  const text = formulaWasActive ? formulaInput.value : editor.value;
  if (String(text).startsWith('=')) return false;
  const liveStart = Number.isInteger(formulaInput.selectionStart) ? formulaInput.selectionStart : text.length;
  const liveEnd = Number.isInteger(formulaInput.selectionEnd) ? formulaInput.selectionEnd : liveStart;
  const editorSelection = {
    start: Number.isInteger(editor.selectionStart) ? editor.selectionStart : text.length,
    end: Number.isInteger(editor.selectionEnd) ? editor.selectionEnd : text.length,
  };
  const selection = formulaWasActive
    ? (S.plainFormulaSelection || { start: liveStart, end: liveEnd }) : editorSelection;
  const base = normalizeRichRun({ ...S.editBaseRichStyle, text });
  const surface = formulaWasActive ? richFormulaInput : richCellEditor;
  enterRichFormulaEdit([base], text, { focus: true, selection, surface });
  restoreRichSelection(selection, true, surface);
  S.editDirty = false;
  return true;
}
function applyRichFormat(prop, value, toggle = false) {
  if (!S.editing) return false;
  if (!S.editHasRichText && !promotePlainConstantToRichEdit()) return false;
  const surface = isEditFormatTarget(document.activeElement) && S.richEditSurface
    ? S.richEditSurface : activeRichSurface();
  const offsets = (isEditFormatTarget(document.activeElement) && S.richSelection)
    || richSelectionOffsets(surface) || S.richSelection || { start: 0, end: 0 };
  const runs = richRunsFromDom(surface);
  const formatted = (run, desired) => prop === 'color'
    ? { ...run, color: desired, resolvedColor: desired }
    : { ...run, [prop]: desired };
  if (offsets.start === offsets.end) {
    const base = S.richTypingStyle || richStyleAtOffset(runs, offsets.start);
    S.richTypingStyle = formatted(base, toggle ? !base[prop] : value);
    syncToolbarFromRichStyle(S.richTypingStyle);
    restoreRichSelection(offsets, true, surface);
    return true;
  }
  let desired = value;
  if (toggle) {
    let pos = 0, allEnabled = true;
    for (const run of runs) {
      const end = pos + run.text.length;
      if (end > offsets.start && pos < offsets.end && !run[prop]) allEnabled = false;
      pos = end;
    }
    desired = !allEnabled;
  }
  const nextRuns = [];
  let pos = 0;
  for (const run of runs) {
    const end = pos + run.text.length;
    const left = Math.max(pos, offsets.start), right = Math.min(end, offsets.end);
    if (left >= right) nextRuns.push(run);
    else {
      if (left > pos) nextRuns.push({ ...run, text: run.text.slice(0, left - pos) });
      nextRuns.push({ ...formatted(run, desired), text: run.text.slice(left - pos, right - pos) });
      if (right < end) nextRuns.push({ ...run, text: run.text.slice(right - pos) });
    }
    pos = end;
  }
  S.editRichRuns = mergeRichRuns(nextRuns, true);
  S.editDirty = true;
  S.richTypingStyle = null;
  renderRichEditSurfaces(S.editRichRuns);
  editor.value = formulaInput.value = richRunsText(S.editRichRuns);
  positionRichCellEditor();
  restoreRichSelection(offsets, true, surface);
  syncToolbarFromRichSelection();
  return true;
}
function insertRichText(text, explicitStyle) {
  const surface = activeRichSurface();
  const offsets = richSelectionOffsets(surface) || S.richSelection;
  const sel = window.getSelection();
  if (!offsets || !sel || !sel.rangeCount) return;
  const range = sel.getRangeAt(0);
  if (!surface.contains(range.startContainer)) return;
  const runs = richRunsFromDom(surface);
  const style = explicitStyle || S.richTypingStyle || richStyleAtOffset(runs, offsets.start);
  range.deleteContents();
  const span = document.createElement('span');
  setRichRunElementStyle(span, { ...style, text });
  span.textContent = text;
  range.insertNode(span);
  range.setStartAfter(span); range.collapse(true);
  sel.removeAllRanges(); sel.addRange(range);
  S.editDirty = true;
  S.richSelection = { start: offsets.start + text.length, end: offsets.start + text.length };
  S.editRichRuns = richRunsFromDom(surface);
  editor.value = formulaInput.value = richRunsText(S.editRichRuns);
  renderRichEditSurfaces(S.editRichRuns, surface);
  positionRichCellEditor();
}

/* ================= 编辑器 ================= */
function setFormulaEditing(active) {
  formulaRow.classList.toggle('editing', !!active);
  $('formula-cancel').disabled = !active;
  $('formula-accept').disabled = !active;
}
function startEdit(initial, selectAll, focusEditor = true, keepScroll = false, frozenPane = null) {
  if (S.editing) return;
  // Every point inside a merged region is the same logical Excel cell.  Keep this guard here as
  // well as in pointer hit-testing because F2/formula-bar callers can enter through other paths.
  const target = canonicalCell(S.cur.r, S.cur.c);
  if (target.r !== S.cur.r || target.c !== S.cur.c) {
    S.cur = { r: target.r, c: target.c };
    S.anchor = { ...S.cur };
    S.sel = target.merge
      ? { r0: target.merge.r0, c0: target.merge.c0, r1: target.merge.r1, c1: target.merge.c1 }
      : { r0: target.r, c0: target.c, r1: target.r, c1: target.c };
  }
  S.editing = true;
  S.editCell = { sheet: S.sheet, ...S.cur };
  S.editFrozen = frozenPane ? { rows: !!frozenPane.rows, cols: !!frozenPane.cols } : null;
  // formulaInput focus passes its current value as `initial`; that is not a user modification.
  S.editDirty = initial !== undefined && focusEditor;
  const cached = S.cellsCache.get(S.cur.r + ',' + S.cur.c);
  S.editHasRichText = Array.isArray(cached?.rt) && cached.rt.length > 0;
  const formulaStyle = S.formulaCellStyle
    && S.formulaCellStyle.sheet === S.sheet
    && S.formulaCellStyle.row === S.cur.r
    && S.formulaCellStyle.col === S.cur.c
    ? S.formulaCellStyle.style : null;
  S.editBaseRichStyle = richStyleFromCellStyle(cached?.s || formulaStyle);
  S.plainFormulaSelection = null;
  S.editOriginal = cached ? (cached.f || cached.v) : (initial === undefined ? '' : String(initial));
  const editRect = activeEditRect();
  const requestedRichSurface = focusEditor ? richCellEditor : richFormulaInput;
  S.richEditSurface = requestedRichSurface;
  editor.value = initial !== undefined ? initial : (cached ? (cached.f || cached.v) : '');
  if (S.editHasRichText) {
    const explicitReplacement = initial !== undefined && focusEditor;
    const base = normalizeRichRuns(cached.rt, S.editOriginal)[0] || {};
    enterRichFormulaEdit(cached.rt, S.editOriginal, {
      focus: true,
      selectAll,
      draftRuns: explicitReplacement ? [{ ...base, text: String(initial) }] : null,
      surface: requestedRichSurface,
      editRect,
    });
    if (explicitReplacement) S.editDirty = true;
  } else {
    setRichFormulaMode(false, false);
    positionPlainCellEditor(editRect);
  }
  const editCell = { ...S.editCell };
  S.editLoadPromise = api(`/api/cell?sheet=${editCell.sheet}&row=${editCell.r}&col=${editCell.c}`).then((j) => {
    if (!S.editing || !S.editCell || S.editCell.sheet !== editCell.sheet
        || S.editCell.r !== editCell.r || S.editCell.c !== editCell.c) return;
    const existingRichDraft = S.editHasRichText ? richEditRunsFromDom() : null;
    const locallyPromoted = S.editHasRichText && !!S.richEditSurface;
    S.editOriginal = j.content;
    S.editBaseRichStyle = richStyleFromCellStyle(j.style);
    const serverHasRichText = !!j.hasRichText || (Array.isArray(j.richText) && j.richText.length > 0);
    S.editHasRichText = serverHasRichText || locallyPromoted;
    if (serverHasRichText) {
      const dirtyText = editor.value;
      const base = normalizeRichRuns(j.richText, j.content)[0] || {};
      enterRichFormulaEdit(j.richText, j.content, {
        focus: true,
        selectAll: !S.editDirty && selectAll,
        selection: existingRichDraft ? S.richSelection : null,
        draftRuns: S.editDirty ? (existingRichDraft || [{ ...base, text: dirtyText }]) : null,
        surface: S.richEditSurface || requestedRichSurface,
        editRect: activeEditRect(),
      });
      if (dirtyText !== j.content) S.editDirty = true;
      formulaInput.value = richFormulaInput.textContent;
    } else if (!locallyPromoted && !S.editDirty) {
      setRichFormulaMode(false, false);
      editor.value = j.content;
      formulaInput.value = j.content;
      if (selectAll) editor.select();
      positionPlainCellEditor();
    }
  }).catch(() => undefined);
  setFormulaEditing(true);
  if (focusEditor && !S.editHasRichText) editor.focus({ preventScroll: keepScroll });
  formulaInput.value = editor.value;
}
function startEditAtCell(r, c, initial, selectAll = false, focusEditor = true, keepScroll = false, frozenPane = null) {
  // A double-click edits the cell under the pointer, never the previously selected cell. Resolve
  // the merge before changing selection so subordinate points always address the top-left anchor.
  const target = canonicalCell(r, c);
  setCursor(target.r, target.c, false, keepScroll);
  startEdit(initial, selectAll, focusEditor, keepScroll, frozenPane);
  return target;
}
let pendingEditCommit = null;
function commitEdit(move) {
  if (pendingEditCommit) return pendingEditCommit;
  const task = commitEditNow(move);
  const tracked = task.finally(() => {
    if (pendingEditCommit === tracked) pendingEditCommit = null;
  });
  pendingEditCommit = tracked;
  // Event handlers can fire-and-forget; awaiting callers still receive the rejection.
  tracked.catch((error) => setStatus('Save failed; draft retained: ' + error.message));
  return tracked;
}
function finishEditCommit(cell, value, runs = null) {
  if (!S.editing || S.editCell !== cell) return false;
  const changed = runs ? !richRunsEqual(richEditRunsFromDom(), runs) : S.editHasRichText || editor.value !== value;
  if (changed) {
    // A slow acknowledgement must not discard text typed while the request was pending.
    S.editOriginal = value;
    if (runs) S.editRichOriginal = runs;
    scheduleRefresh(true);
    return false;
  }
  cancelEditUI(isCellEditTarget(document.activeElement) || document.activeElement === gridScroll);
  return true;
}
async function commitEditNow(move) {
  if (!S.editing || editComposing) return;
  const cell = S.editCell;
  const pending = S.editLoadPromise;
  if (pending) await pending;
  if (!S.editing || !cell || S.editCell !== cell || editComposing) return;
  if (S.editHasRichText) {
    const editSurface = activeRichSurface();
    const runs = richRunsFromDom(editSurface);
    const val = richRunsText(runs);
    const unchanged = !S.editDirty || richRunsEqual(runs, S.editRichOriginal);
    if (!await validateDvCellInput(cell.sheet, cell.r, cell.c, val)) {
      editSurface.focus();
      return;
    }
    if (!S.editing || S.editCell !== cell || editComposing) return;
    if (!unchanged) {
      await apiPost('/api/rich-text', { sheet: cell.sheet, row: cell.r, col: cell.c, runs: richRunsForApi(runs) });
      S.cellsCache.delete(`${cell.r},${cell.c}`);
      setStatus('富文本分段与字符级样式已保存');
    } else setStatus('富文本内容未改变，所有分段样式已保留');
    if (!finishEditCommit(cell, val, runs)) return;
    const delta = { down: [1, 0], up: [-1, 0], right: [0, 1], left: [0, -1] }[move];
    if (delta) {
      const next = mergedNavigationTarget(cell.r, cell.c, delta[0], delta[1]);
      setCursor(next.r, next.c);
    }
    if (!unchanged) scheduleRefresh(true);
    return;
  }
  const val = editor.value;
  if (!await validateDvCellInput(cell.sheet, cell.r, cell.c, val)) {
    editor.focus(); editor.select();
    return;
  }
  if (!S.editing || S.editCell !== cell || editComposing) return;
  const j = await apiPost('/api/input', { sheet: cell.sheet, row: cell.r, col: cell.c, value: val });
  if (!finishEditCommit(cell, val)) return;
  if (j && typeof j.v === 'string' && j.v.includes('#CIRC')) setStatus('警告: 检测到循环引用 (#CIRC!)');
  const delta = { down: [1, 0], up: [-1, 0], right: [0, 1], left: [0, -1] }[move];
  if (delta) {
    const next = mergedNavigationTarget(cell.r, cell.c, delta[0], delta[1]);
    setCursor(next.r, next.c);
  }
  scheduleRefresh(true);
}
// Ctrl+Enter：把编辑内容写入整个选区（公式相对引用随锚点平移，Excel 语义）
function commitEditRange() {
  if (pendingEditCommit) return pendingEditCommit;
  const task = commitEditRangeNow();
  const tracked = task.finally(() => {
    if (pendingEditCommit === tracked) pendingEditCommit = null;
  });
  pendingEditCommit = tracked;
  // Event handlers can fire-and-forget; awaiting callers still receive the rejection.
  tracked.catch((error) => setStatus('Save failed; draft retained: ' + error.message));
  return tracked;
}
async function commitEditRangeNow() {
  if (!S.editing || editComposing) return;
  const cell = S.editCell;
  const pending = S.editLoadPromise;
  if (pending) await pending;
  if (!S.editing || !cell || S.editCell !== cell || editComposing) return;
  const n = normSel();
  if (S.editHasRichText) {
    const editSurface = activeRichSurface();
    const runs = richRunsFromDom(editSurface);
    const val = richRunsText(runs);
    const unchanged = !S.editDirty || richRunsEqual(runs, S.editRichOriginal);
    if (!await validateDvCellInput(cell.sheet, cell.r, cell.c, val)) {
      editSurface.focus();
      return;
    }
    if (!S.editing || S.editCell !== cell || editComposing) return;
    if (!unchanged) {
      await apiPost('/api/rich-text', { sheet: cell.sheet, row: cell.r, col: cell.c, runs: richRunsForApi(runs) });
      S.cellsCache.delete(`${cell.r},${cell.c}`);
      scheduleRefresh(true);
      setStatus('已保存活动单元格的富文本；分段样式未被扁平化');
    } else setStatus('富文本内容未改变，所有分段样式已保留');
    finishEditCommit(cell, val, runs);
    return;
  }
  const val = editor.value;
  if (!await validateDvCellInput(cell.sheet, cell.r, cell.c, val)) {
    editor.focus(); editor.select();
    return;
  }
  if (!S.editing || S.editCell !== cell || editComposing) return;
  await apiPost('/api/inputrange', { sheet: cell.sheet, ...n, row: cell.r, col: cell.c, value: val });
  finishEditCommit(cell, val);
  scheduleRefresh(true);
}
function cancelEditUI(focusGrid = true) {
  S.editing = false;
  S.editLoadPromise = null;
  S.editDirty = false;
  S.editHasRichText = false;
  S.editRichRuns = [];
  S.editRichOriginal = [];
  S.editBaseRichStyle = {};
  S.editFrozen = null;
  S.richSelection = null;
  S.richTypingStyle = null;
  S.richEditSurface = null;
  S.plainFormulaSelection = null;
  S.editOriginal = '';
  S.editCell = null;
  S.refMode = null;
  editor.style.display = 'none';
  richCellEditor.style.display = 'none';
  richCellEditor.contentEditable = 'false';
  setRichFormulaMode(false, false);
  fnHintHide();
  renderRefHighlights();
  setFormulaEditing(false);
  editComposing = false;
  const selectionToolbar = $('selection-toolbar');
  if (selectionToolbar) selectionToolbar.hidden = true;
  if (focusGrid) gridScroll.focus();
}

editor.addEventListener('keydown', (e) => {
  e.stopPropagation(); // 防止冒泡到网格层导致光标双跳
  if (e.isComposing || e.keyCode === 229 || editComposing) return;
  if (fnHintActive()) { // 公式补全优先拦截
    if (e.key === 'ArrowDown') { e.preventDefault(); fnHintMove(1); return; }
    if (e.key === 'ArrowUp') { e.preventDefault(); fnHintMove(-1); return; }
    if (e.key === 'Tab' || e.key === 'Enter') { e.preventDefault(); fnHintAccept(); return; }
    if (e.key === 'Escape') { e.preventDefault(); fnHintHide(); return; }
  }
  if (e.key === 'F4' && formulaEditActive()) { // F4 循环绝对/相对引用
    if (f4ToggleRef()) { e.preventDefault(); return; }
  }
  const nav = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };
  if (nav[e.key] && formulaEditActive() && (S.refMode || refInsertReady())) {
    e.preventDefault(); // 方向键点选引用（Excel point mode）
    refArrow(nav[e.key], e.shiftKey);
    return;
  }
  if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); commitEditRange(); return; }
  if (e.key === 'Enter') { e.preventDefault(); commitEdit(e.shiftKey ? 'up' : 'down'); }
  else if (e.key === 'Tab') { e.preventDefault(); commitEdit(e.shiftKey ? 'left' : 'right'); }
  else if (e.key === 'Escape') { cancelEditUI(); updateFormulaBar(); }
});
editor.addEventListener('focus', () => {
  if (!S.editHasRichText) S.plainFormulaSelection = null;
});
editor.addEventListener('input', () => {
  S.editDirty = true;
  S.refMode = null; // 手动输入后退出点选状态
  formulaInput.value = editor.value;
  fnHintUpdate();
  renderRefHighlights();
});

/* 公式函数自动补全（=开头时匹配 FN_LIST） */
const fnHint = $('fn-hint');
let fnHintIdx = 0, fnHintItems = [], fnHintPrefix = '';
function fnHintActive() { return !fnHint.hidden; }
function fnHintHide() { fnHint.hidden = true; fnHintItems = []; }
function fnHintUpdate() {
  const v = editor.value;
  if (!S.editing || !v.startsWith('=') || typeof FN_LIST === 'undefined') { fnHintHide(); return; }
  // 取光标前最后一个标识符片段（函数名前缀）
  const upToCaret = v.slice(0, editor.selectionStart ?? v.length);
  const m = /([A-Za-z][A-Za-z0-9.]*)$/.exec(upToCaret);
  if (!m || m[1].length < 1) { fnHintHide(); return; }
  // 前面一个字符不能是引用结尾（避免 A1 被当作前缀）
  const prefix = m[1].toUpperCase();
  if (/^[A-Z]{1,3}[0-9]+$/.test(prefix)) { fnHintHide(); return; }
  fnHintItems = FN_LIST.filter((f) => f.startsWith(prefix)).slice(0, 12);
  if (!fnHintItems.length) { fnHintHide(); return; }
  fnHintPrefix = m[1];
  fnHintIdx = 0;
  fnHintRender();
  const er = editor.getBoundingClientRect();
  fnHint.style.left = er.left + 'px';
  fnHint.style.top = er.bottom + 2 + 'px';
  fnHint.hidden = false;
}
function fnHintRender() {
  fnHint.textContent = '';
  fnHintItems.forEach((f, i) => {
    const d = document.createElement('div');
    d.className = 'fi' + (i === fnHintIdx ? ' on' : '');
    d.textContent = f;
    d.onmousedown = (e) => { e.preventDefault(); fnHintIdx = i; fnHintAccept(); };
    fnHint.appendChild(d);
  });
}
function fnHintMove(d) {
  fnHintIdx = (fnHintIdx + d + fnHintItems.length) % fnHintItems.length;
  fnHintRender();
}
function fnHintAccept() {
  const fn = fnHintItems[fnHintIdx];
  if (!fn) { fnHintHide(); return; }
  const caret = editor.selectionStart ?? editor.value.length;
  const before = editor.value.slice(0, caret - fnHintPrefix.length);
  const after = editor.value.slice(caret);
  editor.value = before + fn + '(' + after;
  const pos = (before + fn).length + 1;
  editor.setSelectionRange(pos, pos);
  formulaInput.value = editor.value;
  fnHintHide();
  editor.focus();
}

/* ================= 公式点选引用（point mode）/ F4 / 引用高亮 ================= */
function formulaEditActive() { return S.editing && editor.value.startsWith('='); }
// 光标前一字符是运算符/分隔符 → 可插入引用（Excel 进入 point mode 的条件）
function refInsertReady() {
  const caret = editor.selectionStart ?? editor.value.length;
  if (caret === 0) return false;
  return /[=+\-*\/,(&<>:^%;{ ]$/.test(editor.value.slice(0, caret));
}
function refText(a, b) {
  const r0 = Math.min(a.r, b.r), r1 = Math.max(a.r, b.r);
  const c0 = Math.min(a.c, b.c), c1 = Math.max(a.c, b.c);
  return (r0 === r1 && c0 === c1) ? cellRef(r0, c0) : cellRef(r0, c0) + ':' + cellRef(r1, c1);
}
function startRefMode(anchor, cur) {
  const caret = editor.selectionStart ?? editor.value.length;
  S.refMode = { start: caret, len: 0, anchor, cur };
  refWriteToken();
}
function refWriteToken() {
  const m = S.refMode;
  if (!m) return;
  const txt = refText(m.anchor, m.cur);
  editor.value = editor.value.slice(0, m.start) + txt + editor.value.slice(m.start + m.len);
  m.len = txt.length;
  const pos = m.start + txt.length;
  editor.setSelectionRange(pos, pos);
  formulaInput.value = editor.value;
  renderRefHighlights();
}
// 方向键点选：从编辑单元格出发移动幽灵引用光标；Shift 拓展为区域
function refArrow(d, extend) {
  if (!S.refMode) {
    const t = mergedNavigationTarget(S.editCell.r, S.editCell.c, d[0], d[1]);
    startRefMode(t, t);
    return;
  }
  const m = S.refMode;
  const t = mergedNavigationTarget(m.cur.r, m.cur.c, d[0], d[1]);
  m.cur = t;
  if (!extend) m.anchor = t;
  refWriteToken();
}
// 提取公式中的单元格/区域引用（跳过字符串字面量）
function parseFormulaRefs(text) {
  const out = [];
  const clean = text.replace(/"[^"]*"?/g, (s) => ' '.repeat(s.length));
  const re = /(\$?)([A-Za-z]{1,3})(\$?)(\d{1,7})(?::(\$?)([A-Za-z]{1,3})(\$?)(\d{1,7}))?/g;
  let m;
  while ((m = re.exec(clean))) {
    const prev = clean[m.index - 1];
    const next = clean[m.index + m[0].length];
    if (prev && /[A-Za-z0-9_.$]/.test(prev)) continue; // 函数名/命名区域一部分
    if (next && /[A-Za-z0-9_(]/.test(next)) continue;
    const p = parseRef(m[2] + m[4]);
    if (!p) continue;
    let r0 = p.r, c0 = p.c, r1 = p.r, c1 = p.c;
    if (m[6]) {
      const q = parseRef(m[6] + m[8]);
      if (q) { r1 = q.r; c1 = q.c; }
    }
    out.push({
      r0: Math.min(r0, r1), r1: Math.max(r0, r1),
      c0: Math.min(c0, c1), c1: Math.max(c0, c1),
      key: m[0].toUpperCase().replace(/\$/g, ''),
      start: m.index, len: m[0].length,
    });
  }
  return out;
}
// F4：循环 A1 → $A$1 → A$1 → $A1 → A1（区域两端同步）
function cycleAbs(atom) {
  const m = /^(\$?)([A-Za-z]{1,3})(\$?)(\d+)$/.exec(atom);
  if (!m) return atom;
  const state = (m[1] ? 2 : 0) + (m[3] ? 1 : 0);
  const next = { 0: 3, 3: 1, 1: 2, 2: 0 }[state];
  return (next & 2 ? '$' : '') + m[2] + (next & 1 ? '$' : '') + m[4];
}
function f4ToggleRef() {
  const caret = editor.selectionStart ?? editor.value.length;
  let hit = null;
  for (const ref of parseFormulaRefs(editor.value)) {
    if (caret >= ref.start && caret <= ref.start + ref.len) { hit = ref; break; }
    if (ref.start + ref.len <= caret) hit = ref; // 光标左侧最近的引用
  }
  if (!hit) return false;
  const token = editor.value.slice(hit.start, hit.start + hit.len);
  const toggled = token.split(':').map(cycleAbs).join(':');
  editor.value = editor.value.slice(0, hit.start) + toggled + editor.value.slice(hit.start + hit.len);
  const pos = hit.start + toggled.length;
  editor.setSelectionRange(pos, pos);
  formulaInput.value = editor.value;
  renderRefHighlights();
  return true;
}
// 编辑公式时用彩色框高亮每个引用（Excel 同款体验）
const REF_COLORS = ['#1a73e8', '#e8710a', '#9334e6', '#137333', '#c5221f', '#007b83', '#b06000', '#5f6368'];
let refLayer = null;
function renderRefHighlights() {
  if (!refLayer) {
    refLayer = document.createElement('div');
    refLayer.id = 'ref-layer';
    $('selection-layer').appendChild(refLayer);
  }
  refLayer.textContent = '';
  if (!formulaEditActive()) return;
  const seen = new Map();
  for (const ref of parseFormulaRefs(editor.value)) {
    if (!seen.has(ref.key)) seen.set(ref.key, seen.size);
    const color = REF_COLORS[seen.get(ref.key) % REF_COLORS.length];
    const x = colX(ref.c0), y = rowY(ref.r0);
    const d = document.createElement('div');
    d.className = 'ref-box';
    d.style.left = x + 'px';
    d.style.top = y + 'px';
    d.style.width = colX(ref.c1) + colWidth(ref.c1) - x + 'px';
    d.style.height = rowY(ref.r1) + rowHeight(ref.r1) - y + 'px';
    d.style.borderColor = color;
    d.style.background = color + '14';
    refLayer.appendChild(d);
  }
}
// Alt+= 自动求和：优先上方连续数字块，其次左侧（Excel 语义）
async function autoSum() {
  const { r, c } = S.cur;
  const numeric = (j) => {
    if (!j || j.formatted === '') return false;
    const cleaned = j.formatted.replace(/[^0-9.\-]/g, '');
    return cleaned !== '' && !isNaN(+cleaned);
  };
  const cellInfo = (rr, cc) => api(`/api/cell?sheet=${S.sheet}&row=${rr}&col=${cc}`);
  let range = null;
  if (r > 1 && numeric(await cellInfo(r - 1, c))) {
    let top = r - 1;
    if (r > 2 && numeric(await cellInfo(r - 2, c))) {
      const j = await api(`/api/edge?sheet=${S.sheet}&row=${r - 1}&col=${c}&dir=up`);
      top = Math.max(1, Math.min(j.row, r - 1));
    }
    range = cellRef(top, c) + ':' + cellRef(r - 1, c);
  } else if (c > 1 && numeric(await cellInfo(r, c - 1))) {
    let left = c - 1;
    if (c > 2 && numeric(await cellInfo(r, c - 2))) {
      const j = await api(`/api/edge?sheet=${S.sheet}&row=${r}&col=${c - 1}&dir=left`);
      left = Math.max(1, Math.min(j.col, c - 1));
    }
    range = cellRef(r, left) + ':' + cellRef(r, c - 1);
  }
  startEdit(range ? `=SUM(${range})` : '=SUM(');
  if (!range) {
    editor.value = '=SUM()';
    editor.setSelectionRange(5, 5);
    formulaInput.value = editor.value;
  }
  renderRefHighlights();
}

/* 公式栏编辑 */
formulaInput.addEventListener('focus', () => {
  if (!S.editing) startEdit(formulaInput.value, false, false);
});
function rememberPlainFormulaSelection() {
  if (!S.editing || S.editHasRichText) return;
  S.plainFormulaSelection = { start: formulaInput.selectionStart ?? 0, end: formulaInput.selectionEnd ?? 0 };
}
formulaInput.addEventListener('select', rememberPlainFormulaSelection);
formulaInput.addEventListener('keyup', rememberPlainFormulaSelection);
formulaInput.addEventListener('mouseup', rememberPlainFormulaSelection);
formulaInput.addEventListener('input', () => {
  if (S.editing) {
    S.editDirty = true; editor.value = formulaInput.value;
    rememberPlainFormulaSelection();
  }
});
formulaInput.addEventListener('keydown', (e) => {
  e.stopPropagation();
  if (e.isComposing || e.keyCode === 229 || editComposing) return;
  if (e.key === 'Enter') { e.preventDefault(); if (S.editing) editor.value = formulaInput.value; commitEdit('down'); }
  else if (e.key === 'Escape') { cancelEditUI(); updateFormulaBar(); }
});
richFormulaInput.addEventListener('mousedown', (e) => {
  S.richEditSurface = richFormulaInput;
  if (S.editing) { S.richTypingStyle = null; return; }
  e.preventDefault();
  startEdit(undefined, false, false);
  requestAnimationFrame(() => {
    if (!S.editing || !S.editHasRichText) return;
    const pointRange = document.caretRangeFromPoint?.(e.clientX, e.clientY);
    if (pointRange && richFormulaInput.contains(pointRange.startContainer)) {
      const sel = window.getSelection(); sel.removeAllRanges(); sel.addRange(pointRange);
      S.richSelection = richSelectionOffsets(richFormulaInput);
    } else restoreRichSelection(
      S.richSelection || { start: richFormulaInput.textContent.length, end: richFormulaInput.textContent.length },
      true,
      richFormulaInput,
    );
  });
});
richFormulaInput.addEventListener('beforeinput', (e) => {
  if (!S.editing || !S.editHasRichText) return;
  if (e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak') {
    e.preventDefault(); insertRichText('\n'); return;
  }
  if (S.richTypingStyle && e.inputType === 'insertText' && typeof e.data === 'string' && !e.isComposing) {
    e.preventDefault(); insertRichText(e.data, S.richTypingStyle);
  }
});
richFormulaInput.addEventListener('paste', (e) => {
  if (!S.editing || !S.editHasRichText) return;
  e.preventDefault();
  insertRichText(e.clipboardData?.getData('text/plain') || '');
});
richFormulaInput.addEventListener('input', () => {
  if (!S.editing || !S.editHasRichText) return;
  S.editDirty = true;
  S.editRichRuns = richFormulaRunsFromDom();
  editor.value = formulaInput.value = richRunsText(S.editRichRuns);
  renderRichRunsInto(richCellEditor, S.editRichRuns);
  positionRichCellEditor();
  requestAnimationFrame(() => {
    S.richSelection = richSelectionOffsets() || S.richSelection;
    syncToolbarFromRichSelection();
  });
});
richFormulaInput.addEventListener('keydown', (e) => {
  S.richEditSurface = richFormulaInput;
  e.stopPropagation();
  if (e.isComposing || e.keyCode === 229 || editComposing) return;
  const ctrl = e.ctrlKey || e.metaKey;
  if (/^(Arrow|Home|End|Page)/.test(e.key)) S.richTypingStyle = null;
  if (ctrl && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); applyRichFormat('bold', true, true); return; }
  if (ctrl && (e.key === 'i' || e.key === 'I')) { e.preventDefault(); applyRichFormat('italic', true, true); return; }
  if (ctrl && (e.key === 'u' || e.key === 'U')) { e.preventDefault(); applyRichFormat('underline', true, true); return; }
  if (e.key === 'Enter' && e.altKey) { e.preventDefault(); insertRichText('\n'); return; }
  if (e.key === 'Enter' && ctrl) { e.preventDefault(); commitEditRange(); return; }
  if (e.key === 'Enter') { e.preventDefault(); commitEdit(e.shiftKey ? 'up' : 'down'); return; }
  if (e.key === 'Tab') { e.preventDefault(); commitEdit(e.shiftKey ? 'left' : 'right'); return; }
  if (e.key === 'Escape') { e.preventDefault(); cancelEditUI(); updateFormulaBar(); }
});
richCellEditor.addEventListener('mousedown', (e) => {
  e.stopPropagation();
  S.richEditSurface = richCellEditor;
  S.richTypingStyle = null;
});
richCellEditor.addEventListener('dblclick', (e) => e.stopPropagation());
richCellEditor.addEventListener('beforeinput', (e) => {
  if (!S.editing || !S.editHasRichText) return;
  S.richEditSurface = richCellEditor;
  if (e.inputType === 'insertParagraph' || e.inputType === 'insertLineBreak') {
    e.preventDefault(); insertRichText('\n'); return;
  }
  if (S.richTypingStyle && e.inputType === 'insertText' && typeof e.data === 'string' && !e.isComposing) {
    e.preventDefault(); insertRichText(e.data, S.richTypingStyle);
  }
});
richCellEditor.addEventListener('paste', (e) => {
  if (!S.editing || !S.editHasRichText) return;
  e.preventDefault();
  S.richEditSurface = richCellEditor;
  insertRichText(e.clipboardData?.getData('text/plain') || '');
});
richCellEditor.addEventListener('input', () => {
  if (!S.editing || !S.editHasRichText) return;
  S.richEditSurface = richCellEditor;
  S.editDirty = true;
  S.editRichRuns = richRunsFromDom(richCellEditor);
  editor.value = formulaInput.value = richRunsText(S.editRichRuns);
  renderRichRunsInto(richFormulaInput, S.editRichRuns);
  positionRichCellEditor();
  requestAnimationFrame(() => {
    S.richSelection = richSelectionOffsets(richCellEditor) || S.richSelection;
    syncToolbarFromRichSelection();
  });
});
richCellEditor.addEventListener('keydown', (e) => {
  e.stopPropagation();
  S.richEditSurface = richCellEditor;
  const ctrl = e.ctrlKey || e.metaKey;
  if (/^(Arrow|Home|End|Page)/.test(e.key)) S.richTypingStyle = null;
  if (ctrl && (e.key === 'b' || e.key === 'B')) { e.preventDefault(); applyRichFormat('bold', true, true); return; }
  if (ctrl && (e.key === 'i' || e.key === 'I')) { e.preventDefault(); applyRichFormat('italic', true, true); return; }
  if (ctrl && (e.key === 'u' || e.key === 'U')) { e.preventDefault(); applyRichFormat('underline', true, true); return; }
  if (e.key === 'Enter' && e.altKey) { e.preventDefault(); insertRichText('\n'); return; }
  if (e.key === 'Enter' && ctrl) { e.preventDefault(); commitEditRange(); return; }
  if (e.key === 'Enter') { e.preventDefault(); commitEdit(e.shiftKey ? 'up' : 'down'); return; }
  if (e.key === 'Tab') { e.preventDefault(); commitEdit(e.shiftKey ? 'left' : 'right'); return; }
  if (e.key === 'Escape') { e.preventDefault(); cancelEditUI(); updateFormulaBar(); }
});
document.addEventListener('selectionchange', () => {
  if (!S.editing || !S.editHasRichText) return;
  const offsets = richSelectionOffsets();
  if (offsets) {
    S.richSelection = offsets;
    syncToolbarFromRichSelection();
  }
});
$('formula-cancel').addEventListener('click', () => {
  if (!S.editing) return;
  cancelEditUI();
  updateFormulaBar();
});
$('formula-accept').addEventListener('click', () => {
  if (!S.editing) return;
  if (!S.editHasRichText) editor.value = formulaInput.value;
  commitEdit();
});
$('formula-expand').addEventListener('click', () => {
  const expanded = formulaRow.classList.toggle('expanded');
  $('formula-expand').setAttribute('aria-expanded', String(expanded));
  $('formula-expand').title = expanded ? '折叠公式栏' : '展开公式栏';
});
$('name-box-drop').addEventListener('click', () => { nameBox.focus(); nameBox.select(); });
nameBox.addEventListener('keydown', (e) => {
  e.stopPropagation();
  if (e.key === 'Enter') {
    const ref = parseRef(nameBox.value);
    if (ref) { setCursor(ref.r, ref.c); scheduleRefresh(true); }
    gridScroll.focus();
  }
});
setFormulaEditing(false);

/* ================= 鼠标交互 ================= */
let dragMode = null; // 'select' | 'fill' | {col} | {row}
gridScroll.addEventListener('mousedown', (e) => {
  if (e.target === editor || e.target.closest?.('#rich-cell-editor')) return;
  if (e.target.id === 'fill-handle') {
    dragMode = 'fill';
    e.preventDefault();
    return;
  }
  const pt = evtCell(e);
  if (!pt) return;
  if (S.editing) {
    // 公式编辑中点选单元格 → 插入引用而非提交（Excel point mode）
    if (formulaEditActive() && (S.refMode || refInsertReady())) {
      if (!S.refMode) startRefMode(pt, pt);
      else { S.refMode.anchor = pt; S.refMode.cur = pt; refWriteToken(); }
      dragMode = 'ref';
      e.preventDefault();
      return;
    }
    commitEdit();
  }
  const keepScroll = !!e.__unicellFrozenCell;
  if (e.shiftKey) setCursor(pt.r, pt.c, true, keepScroll);
  else { setCursor(pt.r, pt.c, false, keepScroll); dragMode = 'select'; }
  gridScroll.focus();
  e.preventDefault();
});
gridScroll.addEventListener('dblclick', async (e) => {
  if (e.target.closest?.('#rich-cell-editor')) return;
  if (e.target.id === 'fill-handle') {
    // Excel：双击填充柄 → 向下填充到相邻列数据尽头
    fillDownToAdjacent();
    return;
  }
  const pt = evtCell(e);
  if (!pt) return;
  try {
    if (pendingEditCommit) await pendingEditCommit;
    else if (S.editing) await commitEdit();
  } catch { return; }
  if (S.editing) return; // 数据验证拒绝了上一次提交，保留原编辑器和错误提示。
  const keepScroll = !!e.__unicellFrozenCell;
  setCursor(pt.r, pt.c, false, keepScroll);
  startEditAtCell(pt.r, pt.c, undefined, false, true, keepScroll, e.__unicellFrozenPane || null);
});
async function fillDownToAdjacent() {
  const n = normSel();
  // 参照左列（或右列）的连续数据长度
  const probeCol = n.c0 > 1 ? n.c0 - 1 : n.c1 + 1;
  const j = await api(`/api/edge?sheet=${S.sheet}&row=${n.r1}&col=${probeCol}&dir=down`);
  if (j.row > n.r1) {
    await apiPost('/api/autofill', { sheet: S.sheet, ...n, toRow: j.row });
    S.sel = { r0: n.r0, c0: n.c0, r1: j.row, c1: n.c1 };
    scheduleRefresh(true);
  }
}
document.addEventListener('mousemove', (e) => {
  if (dragMode === 'ref') {
    const pt = evtCell(e);
    if (pt && S.refMode) { S.refMode.cur = pt; refWriteToken(); }
  } else if (dragMode === 'select') {
    const pt = evtCell(e);
    if (pt) setCursor(pt.r, pt.c, true);
  } else if (dragMode === 'fill') {
    const pt = evtCell(e);
    if (!pt) return;
    const n = normSel();
    const fp = $('fill-preview');
    // 只允许纵向或横向扩展（取位移大的方向）
    const dR = pt.r > n.r1 ? pt.r - n.r1 : (pt.r < n.r0 ? pt.r - n.r0 : 0);
    const dC = pt.c > n.c1 ? pt.c - n.c1 : (pt.c < n.c0 ? pt.c - n.c0 : 0);
    let tr0 = n.r0, tr1 = n.r1, tc0 = n.c0, tc1 = n.c1;
    if (Math.abs(dR) >= Math.abs(dC)) { if (dR > 0) tr1 = pt.r; else if (dR < 0) tr0 = pt.r; }
    else { if (dC > 0) tc1 = pt.c; else if (dC < 0) tc0 = pt.c; }
    fp.dataset.target = JSON.stringify({ tr0, tr1, tc0, tc1 });
    const x = colX(tc0), y = rowY(tr0);
    Object.assign(fp.style, {
      display: 'block', left: x + 'px', top: y + 'px',
      width: colX(tc1) + colWidth(tc1) - x + 'px',
      height: rowY(tr1) + rowHeight(tr1) - y + 'px',
    });
  }
});
document.addEventListener('mouseup', async () => {
  if (dragMode === 'ref') {
    dragMode = null;
    if (S.editing) editor.focus();
    return;
  }
  if (dragMode === 'select') {
    dragMode = null;
    applyPainter(); // 格式刷激活时：选区完成即刷格式
    return;
  }
  if (dragMode === 'fill') {
    const fp = $('fill-preview');
    fp.style.display = 'none';
    const t = fp.dataset.target ? JSON.parse(fp.dataset.target) : null;
    fp.dataset.target = '';
    const n = normSel();
    if (t) {
      try {
        if (t.tr1 > n.r1) await apiPost('/api/autofill', { sheet: S.sheet, r0: n.r0, c0: n.c0, r1: n.r1, c1: n.c1, toRow: t.tr1 });
        else if (t.tr0 < n.r0) await apiPost('/api/autofill', { sheet: S.sheet, r0: n.r0, c0: n.c0, r1: n.r1, c1: n.c1, toRow: t.tr0 });
        else if (t.tc1 > n.c1) await apiPost('/api/autofill', { sheet: S.sheet, r0: n.r0, c0: n.c0, r1: n.r1, c1: n.c1, toCol: t.tc1 });
        else if (t.tc0 < n.c0) await apiPost('/api/autofill', { sheet: S.sheet, r0: n.r0, c0: n.c0, r1: n.r1, c1: n.c1, toCol: t.tc0 });
        S.sel = { r0: Math.min(n.r0, t.tr0), c0: Math.min(n.c0, t.tc0), r1: Math.max(n.r1, t.tr1), c1: Math.max(n.c1, t.tc1) };
        scheduleRefresh(true);
      } catch {}
    }
  }
  dragMode = null;
});
function evtCell(e) {
  const frozen = e.__unicellFrozenCell;
  if (frozen && Number.isFinite(frozen.r) && Number.isFinite(frozen.c)) return frozen;
  const rect = gridScroll.getBoundingClientRect();
  const x = e.clientX - rect.left + gridScroll.scrollLeft;
  const y = e.clientY - rect.top + gridScroll.scrollTop;
  if (x < 0 || y < 0) return null;
  const hit = canonicalCell(rowAtY(y), colAtX(x));
  return { r: hit.r, c: hit.c, merge: hit.merge };
}

/* 列头/行头点击选择整列整行、拖拽调宽高 */
$('col-headers').addEventListener('mousedown', (e) => {
  const col = +e.target.dataset.col;
  if (!col) return;
  if (e.target.classList.contains('col-resizer')) {
    startResize('col', col, e);
  } else {
    // 列头右键在已选列上：保留多列选区（Excel 语义）
    const inSel = e.button === 2 && col >= S.sel.c0 && col <= S.sel.c1 && S.sel.r0 <= 1;
    if (!inSel) {
      S.cur = { r: 1, c: col }; S.anchor = { r: 1, c: col };
      S.sel = { r0: 1, c0: col, r1: Math.max(S.maxUsedR, 200), c1: col };
      renderSelection(); renderHeadersSelOnly();
    }
  }
  if (e.button !== 2) e.preventDefault();
});
$('col-headers').addEventListener('contextmenu', (e) => {
  e.preventDefault();
  const col = +e.target.dataset.col || S.sel.c0;
  const n = normSel();
  const nCols = n.c1 - n.c0 + 1;
  showCtxMenu(e.clientX, e.clientY, [
    { label: '剪切', hint: 'Ctrl+X', fn: () => doCopy(true) },
    { label: '复制', hint: 'Ctrl+C', fn: () => doCopy(false) },
    { label: '粘贴', hint: 'Ctrl+V', fn: pasteFromClipboard },
    { label: '选择性粘贴…', hint: 'Ctrl+Alt+V', fn: openPasteSpecialDialog },
    'sep',
    { label: `在左侧插入 ${nCols} 列`, fn: () => colOp('insert', n.c0, nCols) },
    { label: `在右侧插入 ${nCols} 列`, fn: () => colOp('insert', n.c1 + 1, nCols) },
    { label: `删除 ${nCols} 列`, fn: () => colOp('delete', n.c0, nCols) },
    'sep',
    { label: '列宽…', fn: () => promptColWidth(n.c0, n.c1) },
    { label: '隐藏', fn: () => hideCols(n.c0, n.c1) },
    { label: '取消隐藏', fn: () => unhideCols(n.c0, n.c1) },
    'sep',
    { label: '设置单元格格式…', fn: openCellFormatDialog },
    { label: '清除内容', fn: async () => { await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'contents' }); scheduleRefresh(true); } },
  ]);
});
$('row-headers').addEventListener('mousedown', (e) => {
  const row = +e.target.dataset.row;
  if (!row) return;
  if (e.target.classList.contains('row-resizer')) {
    startResize('row', row, e);
  } else {
    const inSel = e.button === 2 && row >= S.sel.r0 && row <= S.sel.r1 && S.sel.c0 <= 1;
    if (!inSel) {
      S.cur = { r: row, c: 1 }; S.anchor = { r: row, c: 1 };
      S.sel = { r0: row, c0: 1, r1: row, c1: Math.max(S.maxUsedC, 50) };
      renderSelection(); renderHeadersSelOnly();
    }
  }
  if (e.button !== 2) e.preventDefault();
});
$('row-headers').addEventListener('contextmenu', (e) => {
  e.preventDefault();
  const n = normSel();
  const nRows = n.r1 - n.r0 + 1;
  showCtxMenu(e.clientX, e.clientY, [
    { label: '剪切', hint: 'Ctrl+X', fn: () => doCopy(true) },
    { label: '复制', hint: 'Ctrl+C', fn: () => doCopy(false) },
    { label: '粘贴', hint: 'Ctrl+V', fn: pasteFromClipboard },
    { label: '选择性粘贴…', hint: 'Ctrl+Alt+V', fn: openPasteSpecialDialog },
    'sep',
    { label: `在上方插入 ${nRows} 行`, fn: () => rowColOp('/api/rows', 'insert', n.r0, nRows) },
    { label: `在下方插入 ${nRows} 行`, fn: () => rowColOp('/api/rows', 'insert', n.r1 + 1, nRows) },
    { label: `删除 ${nRows} 行`, fn: () => rowColOp('/api/rows', 'delete', n.r0, nRows) },
    'sep',
    { label: '行高…', fn: () => promptRowHeight(n.r0, n.r1) },
    { label: '隐藏', fn: () => hideRows(n.r0, n.r1) },
    { label: '取消隐藏', fn: () => unhideRows(n.r0, n.r1) },
    'sep',
    { label: '设置单元格格式…', fn: openCellFormatDialog },
    { label: '清除内容', fn: async () => { await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'contents' }); scheduleRefresh(true); } },
  ]);
});
// 列宽/行高/隐藏辅助
async function promptColWidth(c0, c1) {
  const cur = Math.round(zRawColW(c0));
  const v = prompt('列宽（像素）', cur);
  if (v == null) return;
  const w = parseFloat(v);
  if (!(w >= 0)) return;
  await apiPost('/api/colwidth', { sheet: S.sheet, c0, c1, width: w });
  S.colW.clear(); scheduleRefresh(true);
}
async function promptRowHeight(r0, r1) {
  const cur = Math.round(zRawRowH(r0));
  const v = prompt('行高（像素）', cur);
  if (v == null) return;
  const h = parseFloat(v);
  if (!(h >= 0)) return;
  await apiPost('/api/rowheight', { sheet: S.sheet, r0, r1, height: h });
  S.rowH.clear(); scheduleRefresh(true);
}
async function hideCols(c0, c1) { await apiPost('/api/colwidth', { sheet: S.sheet, c0, c1, width: 0 }); S.colW.clear(); scheduleRefresh(true); }
async function unhideCols(c0, c1) { await apiPost('/api/colwidth', { sheet: S.sheet, c0: Math.max(1, c0 - 1), c1: c1 + 1, width: S.defW }); S.colW.clear(); scheduleRefresh(true); }
async function hideRows(r0, r1) { await apiPost('/api/rowheight', { sheet: S.sheet, r0, r1, height: 0 }); S.rowH.clear(); scheduleRefresh(true); }
async function unhideRows(r0, r1) { await apiPost('/api/rowheight', { sheet: S.sheet, r0: Math.max(1, r0 - 1), r1: r1 + 1, height: S.defH }); S.rowH.clear(); scheduleRefresh(true); }
function startResize(kind, idx, e0) {
  const startPos = kind === 'col' ? e0.clientX : e0.clientY;
  const startSize = kind === 'col' ? zRawColW(idx) : zRawRowH(idx); // 真实尺寸
  const move = (e) => {
    const d = ((kind === 'col' ? e.clientX : e.clientY) - startPos) / S.zoom; // 屏幕 delta 还原
    const size = Math.max(8, startSize + d);
    if (kind === 'col') S.colW.set(idx, size); else S.rowH.set(idx, size);
    scheduleRefresh(true); renderSelection();
  };
  const up = async (e) => {
    document.removeEventListener('mousemove', move);
    document.removeEventListener('mouseup', up);
    const d = ((kind === 'col' ? e.clientX : e.clientY) - startPos) / S.zoom;
    const size = Math.max(8, startSize + d);
    if (kind === 'col') await apiPost('/api/colwidth', { sheet: S.sheet, c0: idx, c1: idx, width: size });
    else await apiPost('/api/rowheight', { sheet: S.sheet, r0: idx, r1: idx, height: size });
    scheduleRefresh(true);
  };
  document.addEventListener('mousemove', move);
  document.addEventListener('mouseup', up);
}

/* Ctrl+滚轮缩放（Excel 标准档位，格线落在整数像素不模糊） */
const ZOOM_STEPS = [0.5, 0.6, 0.7, 0.8, 0.9, 1, 1.1, 1.25, 1.5, 1.75, 2, 2.5, 3, 4];
function setZoom(z) {
  S.zoom = Math.max(0.5, Math.min(4, z));
  updateSpacer();
  scheduleRefresh(true);
  setStatus(`缩放 ${Math.round(S.zoom * 100)}%`);
  syncZoomLevel();
}
gridScroll.addEventListener('wheel', (e) => {
  if (!(e.ctrlKey || e.metaKey)) return;
  e.preventDefault();
  const dir = e.deltaY < 0 ? 1 : -1;
  let i = ZOOM_STEPS.findIndex((s) => Math.abs(s - S.zoom) < 0.005);
  if (i < 0) {
    i = ZOOM_STEPS.findIndex((s) => s > S.zoom);
    if (i < 0) i = ZOOM_STEPS.length;
    if (dir < 0) i -= 1;
  } else {
    i += dir;
  }
  i = Math.max(0, Math.min(ZOOM_STEPS.length - 1, i));
  setZoom(ZOOM_STEPS[i]);
}, { passive: false });

/* 屏蔽浏览器原生缩放（Ctrl+滚轮 / Ctrl+加减号 / Ctrl+0，否则整个 UI 被浏览器缩放变模糊） */
window.addEventListener('wheel', (e) => {
  if (e.ctrlKey || e.metaKey) e.preventDefault();
}, { passive: false, capture: true });
document.addEventListener('keydown', (e) => {
  if ((e.ctrlKey || e.metaKey) && ['+', '=', '-', '_', '0'].includes(e.key)) e.preventDefault();
});

/* ================= 键盘 ================= */
gridScroll.addEventListener('keydown', async (e) => {
  // 文本框有自己的原生文本编辑语义。不能让表格层截获 Delete、Ctrl+A/C/X/V、
  // 普通字符等按键，否则会误删整个对象或把输入写进当前单元格。
  const objectText = e.target.closest && e.target.closest('.obj-text');
  if (objectText) {
    if (e.key === 'Escape') {
      e.preventDefault();
      objectText.blur();
      selectObject(null);
      gridScroll.focus();
    }
    return;
  }
  if (S.editing) return;
  const k = e.key;
  const ctrl = e.ctrlKey || e.metaKey;
  const nav = {
    ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1],
  };
  if (nav[k]) {
    e.preventDefault();
    if (ctrl) {
      const dir = { ArrowUp: 'up', ArrowDown: 'down', ArrowLeft: 'left', ArrowRight: 'right' }[k];
      const j = await api(`/api/edge?sheet=${S.sheet}&row=${S.cur.r}&col=${S.cur.c}&dir=${dir}`);
      setCursor(j.row, j.col, e.shiftKey);
    } else {
      const next = mergedNavigationTarget(S.cur.r, S.cur.c, nav[k][0], nav[k][1]);
      setCursor(next.r, next.c, e.shiftKey);
    }
    scheduleRefresh();
    return;
  }
  if (k === 'Enter') {
    e.preventDefault();
    const next = mergedNavigationTarget(S.cur.r, S.cur.c, e.shiftKey ? -1 : 1, 0);
    setCursor(next.r, next.c); scheduleRefresh(); return;
  }
  if (k === 'Tab') {
    e.preventDefault();
    const next = mergedNavigationTarget(S.cur.r, S.cur.c, 0, e.shiftKey ? -1 : 1);
    setCursor(next.r, next.c); scheduleRefresh(); return;
  }
  if (k === 'F2') { e.preventDefault(); startEdit(undefined, false); return; }
  if (k === 'F9') { e.preventDefault(); await api('/api/calc', { method: 'POST' }); scheduleRefresh(true); setStatus('已重算'); return; }
  if (k === 'Escape') {
    // Excel：Esc 取消剪切（行军蚁）状态 / 格式刷
    if (S.cutPending) { S.cutPending = null; setStatus('已取消剪切'); }
    if (painter) { painter = null; $('btn-painter').classList.remove('active'); setStatus('已取消格式刷'); }
    return;
  }
  if (e.altKey && (k === '=' || k === '+')) {
    // Excel：Alt+= 自动求和
    e.preventDefault();
    autoSum();
    return;
  }
  if (k === 'Delete') {
    e.preventDefault();
    // 优先删除选中的插入对象（文本框/图片等），否则清单元格内容
    if (S.selObj) { deleteObject(S.selObj); return; }
    // Excel：Delete 清选区内容（保留格式）
    const n = normSel();
    await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'contents' });
    scheduleRefresh(true);
    return;
  }
  if (k === 'Backspace') {
    // Excel：Backspace 清空活动单元格并进入编辑态
    e.preventDefault();
    startEdit('');
    return;
  }
  if (k === 'Home') {
    e.preventDefault();
    setCursor(ctrl ? 1 : S.cur.r, 1); scheduleRefresh(); return;
  }
  if (k === 'PageDown' || k === 'PageUp') {
    e.preventDefault();
    const page = Math.max(1, Math.floor(gridScroll.clientHeight / S.defH) - 2);
    setCursor(Math.max(1, S.cur.r + (k === 'PageDown' ? page : -page)), S.cur.c, e.shiftKey);
    scheduleRefresh(); return;
  }
  if (ctrl && (k === 'z' || k === 'Z')) {
    e.preventDefault();
    await api('/api/undo', { method: 'POST' }); await refreshCalculationSettings(); scheduleRefresh(true); return;
  }
  if (ctrl && (k === 'y' || k === 'Y')) {
    e.preventDefault();
    await api('/api/redo', { method: 'POST' }); await refreshCalculationSettings(); scheduleRefresh(true); return;
  }
  if (ctrl && (k === 'b' || k === 'B')) { e.preventDefault(); toggleStyle('font.b', 'btn-bold'); return; }
  if (ctrl && (k === 'i' || k === 'I')) { e.preventDefault(); toggleStyle('font.i', 'btn-italic'); return; }
  if (ctrl && (k === 'u' || k === 'U')) { e.preventDefault(); toggleStyle('font.u', 'btn-underline'); return; }
  if (ctrl && (k === 'a' || k === 'A')) {
    // Excel：第一次 Ctrl+A 选当前数据区域，再按选整表
    e.preventDefault();
    const j = await api(`/api/dimension?sheet=${S.sheet}`);
    const n = normSel();
    const isDataRegion = n.r0 === j.minRow && n.c0 === j.minCol && n.r1 === j.maxRow && n.c1 === j.maxCol;
    if (isDataRegion) {
      S.sel = { r0: 1, c0: 1, r1: Math.max(j.maxRow, 200), c1: Math.max(j.maxCol, 50) };
    } else {
      S.sel = { r0: j.minRow, c0: j.minCol, r1: j.maxRow, c1: j.maxCol };
      S.cur = { r: j.minRow, c: j.minCol };
    }
    renderSelection(); renderHeadersSelOnly(); return;
  }
  if (ctrl && (k === 'f' || k === 'F')) { e.preventDefault(); openFindDialog(false); return; }
  if (ctrl && (k === 'h' || k === 'H')) { e.preventDefault(); openFindDialog(true); return; }
  if (ctrl && e.altKey && (k === 'v' || k === 'V')) { e.preventDefault(); openPasteSpecialDialog(); return; }
  if (ctrl && (k === 'c' || k === 'C')) { e.preventDefault(); doCopy(false); return; }
  if (ctrl && (k === 'x' || k === 'X')) { e.preventDefault(); doCopy(true); return; }
  // Ctrl+V 交给 paste 事件（可拿系统剪贴板）
  if (!ctrl && k.length === 1) {
    // 直接打字进入编辑
    startEdit(k);
    e.preventDefault();
  }
});

/* ================= 复制/剪切/粘贴 ================= */
async function doCopy(isCut) {
  const n = normSel();
  const j = await apiPost('/api/copy', { sheet: S.sheet, ...n });
  let tier = 'memory';
  try { tier = await writeRichClipboard(j); }
  catch { rememberRichClipboard(j); }
  S.cutPending = isCut ? { sheet: S.sheet, ...n } : null;
  const fidelity = tier === 'custom+html+text' ? '高保真'
    : (tier === 'html+text' ? 'HTML' : (tier === 'text' ? '文本' : '仅当前会话'));
  setStatus(`${isCut ? '已剪切' : '已复制'}（${fidelity}剪贴板）`);
}
document.addEventListener('paste', async (e) => {
  // contenteditable 文本框必须保留浏览器原生粘贴，不能被单元格粘贴逻辑吞掉。
  if ((e.target.closest && e.target.closest('.obj-text'))
      || (document.activeElement && document.activeElement.closest && document.activeElement.closest('.obj-text'))
      || S.editing || document.activeElement === nameBox || document.activeElement === formulaInput) return;
  e.preventDefault();
  await handleRichClipboardPasteEvent(e);
});

/* ================= 工具栏 ================= */
async function styleSel(path, value) {
  const n = normSel();
  await apiPost('/api/style', { sheet: S.sheet, ...n, path, value });
  scheduleRefresh(true);
}
async function toggleStyle(path, btnId) {
  const richProp = {
    'font.b': 'bold', 'font.i': 'italic', 'font.u': 'underline', 'font.strike': 'strike',
  }[path];
  if (richProp && applyRichFormat(richProp, true, true)) return;
  const active = $(btnId).classList.contains('active');
  await styleSel(path, active ? 'false' : 'true');
  $(btnId).classList.toggle('active');
}
$('btn-bold').onclick = () => toggleStyle('font.b', 'btn-bold');
$('btn-italic').onclick = () => toggleStyle('font.i', 'btn-italic');
$('btn-underline').onclick = () => toggleStyle('font.u', 'btn-underline');
$('btn-strike').onclick = () => toggleStyle('font.strike', 'btn-strike');
$('btn-wrap').onclick = () => toggleStyle('alignment.wrap_text', 'btn-wrap');
$('sel-fontsize').onchange = (e) => {
  const size = Number(e.target.value);
  if (!applyRichFormat('size', size, false)) styleSel('font.size', e.target.value);
};
['btn-bold', 'btn-italic', 'btn-underline', 'btn-strike'].forEach((id) => {
  $(id).addEventListener('mousedown', (e) => { if (S.editing && S.editHasRichText) e.preventDefault(); });
});
$('btn-align-left').onclick = () => styleSel('alignment.horizontal', 'left');
$('btn-align-center').onclick = () => styleSel('alignment.horizontal', 'center');
$('btn-align-right').onclick = () => styleSel('alignment.horizontal', 'right');
$('btn-valign-top').onclick = () => styleSel('alignment.vertical', 'top');
$('btn-valign-middle').onclick = () => styleSel('alignment.vertical', 'center');
$('btn-valign-bottom').onclick = () => styleSel('alignment.vertical', 'bottom');
/* 颜色调色板（移植自母项目 UniDoc：主题色×明度梯度 + 标准色 + 高级取色器） */
function cbHexToRgb(h) {
  h = String(h).replace('#', '');
  if (h.length === 3) h = h.split('').map((c) => c + c).join('');
  return [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16)];
}
function cbRgbToHex(r, g, b) {
  const f = (x) => Math.max(0, Math.min(255, Math.round(x))).toString(16).padStart(2, '0');
  return '#' + f(r) + f(g) + f(b);
}
function cbTint(hex, pct) {
  const [r, g, b] = cbHexToRgb(hex);
  if (pct >= 0) return cbRgbToHex(r + (255 - r) * pct, g + (255 - g) * pct, b + (255 - b) * pct);
  return cbRgbToHex(r * (1 + pct), g * (1 + pct), b * (1 + pct));
}
const CB_THEME = ['#FFFFFF', '#000000', '#E7E6E6', '#44546A', '#4472C4', '#ED7D31', '#A5A5A5', '#FFC000', '#5B9BD5', '#70AD47'];
const CB_TINTS = [0.8, 0.6, 0.4, -0.25, -0.5];
const CB_STANDARD = ['#C00000', '#FF0000', '#FFC000', '#FFFF00', '#92D050', '#00B050', '#00B0F0', '#0070C0', '#002060', '#7030A0'];
function cbRgbToHsv(r, g, b) {
  r /= 255; g /= 255; b /= 255;
  const mx = Math.max(r, g, b), mn = Math.min(r, g, b), d = mx - mn;
  let h = 0;
  if (d) {
    if (mx === r) h = ((g - b) / d) % 6;
    else if (mx === g) h = (b - r) / d + 2;
    else h = (r - g) / d + 4;
    h *= 60; if (h < 0) h += 360;
  }
  return [h, mx ? d / mx : 0, mx];
}
function cbHsvToRgb(h, s, v) {
  const c = v * s, x = c * (1 - Math.abs((h / 60) % 2 - 1)), m = v - c;
  let r = 0, g = 0, b = 0;
  if (h < 60) { r = c; g = x; } else if (h < 120) { r = x; g = c; }
  else if (h < 180) { g = c; b = x; } else if (h < 240) { g = x; b = c; }
  else if (h < 300) { r = x; b = c; } else { r = c; b = x; }
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
}
function buildAdvancedPicker(initialHex, onConfirm, onCancel) {
  const wrap = document.createElement('div');
  wrap.className = 'cp-adv';
  wrap.innerHTML =
    '<div class="cpa-sv"><span class="cpa-sv-thumb"></span></div>'
    + '<div class="cpa-hue"><span class="cpa-hue-thumb"></span></div>'
    + '<div class="cpa-row"><span class="cpa-preview"></span>'
    + '<label class="cpa-hexl">HEX<input class="cpa-hex" type="text" maxlength="7"></label></div>'
    + '<div class="cpa-row cpa-rgb"><label>R<input class="cpa-r" type="number" min="0" max="255"></label>'
    + '<label>G<input class="cpa-g" type="number" min="0" max="255"></label>'
    + '<label>B<input class="cpa-b" type="number" min="0" max="255"></label></div>'
    + '<div class="cpa-actions"><button type="button" class="cpa-cancel">取消</button>'
    + '<button type="button" class="cpa-ok">确认</button></div>';
  const init = cbHexToRgb(/^#[0-9a-f]{6}$/i.test(initialHex) ? initialHex : '#ce4b4b');
  let hsv = cbRgbToHsv(init[0], init[1], init[2]);
  let h = hsv[0], s = hsv[1], v = hsv[2];
  let curHex = cbRgbToHex(init[0], init[1], init[2]);
  const q = (c) => wrap.querySelector(c);
  const sv = q('.cpa-sv'), svT = q('.cpa-sv-thumb'), hue = q('.cpa-hue'), hueT = q('.cpa-hue-thumb');
  const prev = q('.cpa-preview'), hexIn = q('.cpa-hex'), rIn = q('.cpa-r'), gIn = q('.cpa-g'), bIn = q('.cpa-b');
  function sync() {
    const rgb = cbHsvToRgb(h, s, v);
    curHex = cbRgbToHex(rgb[0], rgb[1], rgb[2]);
    sv.style.background = 'hsl(' + Math.round(h) + ',100%,50%)';
    svT.style.left = (s * 100) + '%'; svT.style.top = ((1 - v) * 100) + '%';
    hueT.style.left = (h / 360 * 100) + '%';
    prev.style.background = curHex; hexIn.value = curHex;
    rIn.value = Math.round(rgb[0]); gIn.value = Math.round(rgb[1]); bIn.value = Math.round(rgb[2]);
  }
  sync();
  const drag = (el, fn) => el.addEventListener('pointerdown', (ev) => {
    ev.preventDefault();
    const mv = (e) => fn(e);
    fn(ev);
    const up = () => { window.removeEventListener('pointermove', mv); window.removeEventListener('pointerup', up); };
    window.addEventListener('pointermove', mv); window.addEventListener('pointerup', up);
  });
  drag(sv, (ev) => {
    const rc = sv.getBoundingClientRect();
    s = Math.max(0, Math.min(1, (ev.clientX - rc.left) / rc.width));
    v = Math.max(0, Math.min(1, 1 - (ev.clientY - rc.top) / rc.height));
    sync();
  });
  drag(hue, (ev) => {
    const rc = hue.getBoundingClientRect();
    h = Math.max(0, Math.min(359.9, (ev.clientX - rc.left) / rc.width * 360));
    sync();
  });
  hexIn.addEventListener('change', () => {
    let x = hexIn.value.trim(); if (!/^#/.test(x)) x = '#' + x;
    if (/^#[0-9a-f]{6}$/i.test(x)) { const c = cbHexToRgb(x); hsv = cbRgbToHsv(c[0], c[1], c[2]); h = hsv[0]; s = hsv[1]; v = hsv[2]; sync(); }
  });
  [rIn, gIn, bIn].forEach((inp) => inp.addEventListener('change', () => {
    const c = [Math.max(0, Math.min(255, +rIn.value || 0)), Math.max(0, Math.min(255, +gIn.value || 0)), Math.max(0, Math.min(255, +bIn.value || 0))];
    hsv = cbRgbToHsv(c[0], c[1], c[2]); h = hsv[0]; s = hsv[1]; v = hsv[2]; sync();
  }));
  wrap.querySelector('.cpa-ok').addEventListener('click', () => onConfirm(curHex));
  wrap.querySelector('.cpa-cancel').addEventListener('click', () => onCancel());
  return wrap;
}
let colorPop = null, colorPopKind = null;
function closeColorPop() { if (colorPop) { colorPop.remove(); colorPop = null; colorPopKind = null; } }
function openColorPicker(anchorEl, opts) {
  opts = opts || {};
  closeColorPop();
  colorPopKind = opts.kind || 'generic';
  const onPick = opts.onPick || function () {};
  const pop = document.createElement('div');
  pop.className = 'color-pop';
  colorPop = pop;
  pop.addEventListener('mousedown', (e) => e.preventDefault());
  document.body.appendChild(pop);
  function place() {
    const r = anchorEl.getBoundingClientRect();
    pop.style.left = Math.max(8, Math.min(r.left, window.innerWidth - pop.offsetWidth - 8)) + 'px';
    pop.style.top = Math.min(r.bottom + 4, window.innerHeight - pop.offsetHeight - 8) + 'px';
  }
  function mkSw(c) {
    const sw = document.createElement('button');
    sw.type = 'button'; sw.className = 'cp-sw'; sw.style.background = c; sw.title = c;
    sw.addEventListener('click', () => { onPick(c); closeColorPop(); });
    return sw;
  }
  function renderPalette() {
    pop.classList.remove('cp-advanced');
    pop.innerHTML = '';
    if (opts.autoLabel) {
      const top = document.createElement('button');
      top.type = 'button'; top.className = 'cp-auto'; top.textContent = opts.autoLabel;
      top.addEventListener('click', () => { onPick(opts.autoColor); closeColorPop(); });
      pop.appendChild(top);
    }
    const l1 = document.createElement('div'); l1.className = 'cp-label'; l1.textContent = '主题颜色'; pop.appendChild(l1);
    const grid = document.createElement('div'); grid.className = 'cp-grid';
    CB_THEME.forEach((base) => {
      const col = document.createElement('div'); col.className = 'cp-col';
      col.appendChild(mkSw(base));
      CB_TINTS.forEach((t) => col.appendChild(mkSw(cbTint(base, t))));
      grid.appendChild(col);
    });
    pop.appendChild(grid);
    const l2 = document.createElement('div'); l2.className = 'cp-label'; l2.textContent = '标准色'; pop.appendChild(l2);
    const std = document.createElement('div'); std.className = 'cp-std';
    CB_STANDARD.forEach((c) => std.appendChild(mkSw(c)));
    pop.appendChild(std);
    const more = document.createElement('button');
    more.type = 'button'; more.className = 'cp-more'; more.textContent = '🎨 更多颜色…';
    more.addEventListener('click', renderAdvanced);
    pop.appendChild(more);
    place();
  }
  function renderAdvanced() {
    pop.classList.add('cp-advanced');
    pop.innerHTML = '';
    const cur = /^#[0-9a-f]{6}$/i.test(opts.current || '') ? opts.current : '#ce4b4b';
    pop.appendChild(buildAdvancedPicker(cur, (hex) => { onPick(hex); closeColorPop(); }, renderPalette));
    place();
  }
  renderPalette();
}
function syncColorBtn(btnId, hex) {
  const bar = document.querySelector('#' + btnId + ' .cb-bar');
  if (bar && /^#[0-9a-f]{6}$/i.test(hex)) bar.style.background = hex;
}
function openColorPop(kind, anchorEl) {
  const bar = document.querySelector('#' + (kind === 'font' ? 'font-color-btn' : 'bg-color-btn') + ' .cb-bar');
  openColorPicker(anchorEl, {
    kind,
    current: bar ? rgbToHex(bar.style.background) : '#000000',
    autoLabel: kind === 'font' ? '自动（黑色）' : '无填充色',
    autoColor: kind === 'font' ? '#000000' : '',
    onPick: (hex) => {
      if (kind === 'font') {
        const color = hex || '#000000';
        if (!applyRichFormat('color', color, false)) styleSel('font.color', color);
        syncColorBtn('font-color-btn', color);
      }
      else { styleSel('fill.color', hex); if (hex) syncColorBtn('bg-color-btn', hex); }
    },
  });
}
function rgbToHex(rgb) {
  const m = /rgba?\((\d+),\s*(\d+),\s*(\d+)/.exec(rgb || '');
  if (!m) return /^#[0-9a-f]{6}$/i.test(rgb) ? rgb : '#000000';
  return cbRgbToHex(+m[1], +m[2], +m[3]);
}
['font', 'bg'].forEach((kind) => {
  const btn = $(kind === 'font' ? 'font-color-btn' : 'bg-color-btn');
  if (!btn) return;
  btn.addEventListener('mousedown', (e) => e.preventDefault());
  btn.addEventListener('click', () => {
    if (colorPop && colorPopKind === kind) closeColorPop();
    else openColorPop(kind, btn);
  });
});
document.addEventListener('mousedown', (e) => {
  if (colorPop && !colorPop.contains(e.target) && !(e.target.closest && e.target.closest('.color-btn'))) closeColorPop();
});

/* 设置单元格格式对话框（Excel 式：数字/对齐/字体/边框/填充） */
// 通用色块按钮：点击打开调色板，当前色存 dataset.hex（供对话框内使用）
function mkColorSwatch(initialHex, onChange) {
  const sw = document.createElement('button');
  sw.type = 'button';
  sw.className = 'color-swatch';
  const set = (hex) => {
    sw.dataset.hex = hex || '';
    sw.style.background = hex || 'transparent';
    if (onChange) onChange(hex);
  };
  set(initialHex);
  sw.addEventListener('mousedown', (e) => e.preventDefault());
  sw.addEventListener('click', () => {
    openColorPicker(sw, {
      kind: 'dialog',
      current: sw.dataset.hex || '#000000',
      onPick: (hex) => { if (hex) set(hex); },
    });
  });
  return sw;
}
async function openCellFormatDialog() {
  hideCtxMenu();
  const n = normSel();
  const cur = await api(`/api/cell?sheet=${S.sheet}&row=${S.cur.r}&col=${S.cur.c}`);
  const st = cur.style || {};
  let dlg = $('cellfmt-dialog');
  if (dlg) dlg.remove();
  dlg = document.createElement('div');
  dlg.id = 'cellfmt-dialog';
  dlg.innerHTML = `
    <div class="fd-title"><span>设置单元格格式</span><button id="cfmt-close">×</button></div>
    <div class="cfmt-tabs">
      <button class="cfmt-tab active" data-tab="num">数字</button>
      <button class="cfmt-tab" data-tab="align">对齐</button>
      <button class="cfmt-tab" data-tab="font">字体</button>
      <button class="cfmt-tab" data-tab="border">边框</button>
      <button class="cfmt-tab" data-tab="fill">填充</button>
    </div>
    <div class="cfmt-body">
      <div class="cfmt-pane" data-pane="num">
        <div class="fd-row"><label>分类</label>
          <select id="cfmt-num">
            <option value="general">常规</option>
            <option value="0">数值 1235</option>
            <option value="#,##0.00">数值 1,234.56</option>
            <option value="$#,##0.00">货币 $1,234.56</option>
            <option value="¥#,##0.00">货币 ¥1,234.56</option>
            <option value="0.00%">百分比 12.34%</option>
            <option value="0.00E+00">科学计数</option>
            <option value="yyyy-mm-dd">日期 2026-07-31</option>
            <option value="yyyy-mm-dd hh:mm">日期时间</option>
            <option value="hh:mm:ss">时间</option>
            <option value="@">文本</option>
          </select></div>
      </div>
      <div class="cfmt-pane" data-pane="align" hidden>
        <div class="fd-row"><label>水平</label>
          <select id="cfmt-ha"><option value="general">常规</option><option value="left">靠左</option><option value="center">居中</option><option value="right">靠右</option></select></div>
        <div class="fd-row"><label>垂直</label>
          <select id="cfmt-va"><option value="bottom">靠下</option><option value="center">居中</option><option value="top">靠上</option></select></div>
        <div class="fd-row"><label><input type="checkbox" id="cfmt-wrap"> 自动换行</label></div>
      </div>
      <div class="cfmt-pane" data-pane="font" hidden>
        <div class="fd-row"><label>字体</label><input id="cfmt-fn" spellcheck="false" placeholder="Inter"></div>
        <div class="fd-row"><label>字号</label><input id="cfmt-sz" type="number" min="1" max="409"></div>
        <div class="fd-row">
          <label><input type="checkbox" id="cfmt-b"> 加粗</label>
          <label><input type="checkbox" id="cfmt-i"> 斜体</label>
          <label><input type="checkbox" id="cfmt-u"> 下划线</label>
          <label><input type="checkbox" id="cfmt-st"> 删除线</label>
        </div>
        <div class="fd-row"><label>颜色</label><span id="cfmt-fc-wrap"></span></div>
      </div>
      <div class="cfmt-pane" data-pane="border" hidden>
        <div class="fd-row cfmt-brgrid">
          <button data-bt="all">⊞ 所有</button><button data-bt="outer">□ 外框</button>
          <button data-bt="top">▔ 上</button><button data-bt="bottom">▁ 下</button>
          <button data-bt="left">▏ 左</button><button data-bt="right">▕ 右</button>
          <button data-bt="none">✕ 无</button>
        </div>
        <div class="fd-row"><label>样式</label>
          <select id="cfmt-bs"><option value="thin">细</option><option value="medium">中</option><option value="thick">粗</option><option value="double">双线</option><option value="dotted">点线</option></select>
          <label>颜色</label><span id="cfmt-bc-wrap"></span></div>
      </div>
      <div class="cfmt-pane" data-pane="fill" hidden>
        <div class="fd-row"><label>背景色</label><span id="cfmt-bg-wrap"></span><button id="cfmt-nofill">无填充</button></div>
      </div>
    </div>
    <div class="fd-row fd-btns"><button id="cfmt-cancel">取消</button><button id="cfmt-ok" class="primary">确定</button></div>`;
  document.body.appendChild(dlg);
  // 回显当前值
  $('cfmt-num').value = [...$('cfmt-num').options].some((o) => o.value === st.nf) ? st.nf : 'general';
  $('cfmt-ha').value = st.ha || 'general'; $('cfmt-va').value = st.va || 'bottom'; $('cfmt-wrap').checked = !!st.wr;
  $('cfmt-fn').value = st.fn && st.fn !== 'Inter' ? st.fn : ''; $('cfmt-sz').value = st.sz || 12;
  $('cfmt-b').checked = !!st.b; $('cfmt-i').checked = !!st.i; $('cfmt-u').checked = !!st.u; $('cfmt-st').checked = !!st.st;
  // 调色板色块（替换原生 color input，Excel 同款取色器）
  const fcSw = mkColorSwatch(/^#[0-9a-f]{6}$/i.test(st.fc) ? st.fc : '#000000');
  $('cfmt-fc-wrap').appendChild(fcSw);
  const bcSw = mkColorSwatch('#000000');
  $('cfmt-bc-wrap').appendChild(bcSw);
  const bgSw = mkColorSwatch(/^#[0-9a-f]{6}$/i.test(st.bg) ? st.bg : '#ffff00',
    () => { $('cfmt-nofill').dataset.none = ''; $('cfmt-nofill').classList.remove('on'); });
  $('cfmt-bg-wrap').appendChild(bgSw);
  dlg.querySelectorAll('.cfmt-tab').forEach((tab) => tab.onclick = () => {
    dlg.querySelectorAll('.cfmt-tab').forEach((t) => t.classList.toggle('active', t === tab));
    dlg.querySelectorAll('.cfmt-pane').forEach((p) => p.hidden = p.dataset.pane !== tab.dataset.tab);
  });
  let pendingBorder = null;
  dlg.querySelectorAll('.cfmt-brgrid button').forEach((b) => b.onclick = () => {
    pendingBorder = b.dataset.bt;
    dlg.querySelectorAll('.cfmt-brgrid button').forEach((x) => x.classList.toggle('on', x === b));
  });
  $('cfmt-nofill').onclick = () => { $('cfmt-nofill').dataset.none = '1'; $('cfmt-nofill').classList.add('on'); };
  const close = () => { closeColorPop(); dlg.remove(); };
  $('cfmt-close').onclick = close; $('cfmt-cancel').onclick = close;
  $('cfmt-ok').onclick = async () => {
    const rng = { sheet: S.sheet, ...n };
    await apiPost('/api/style', { ...rng, path: 'num_fmt', value: $('cfmt-num').value });
    await apiPost('/api/style', { ...rng, path: 'alignment.horizontal', value: $('cfmt-ha').value });
    await apiPost('/api/style', { ...rng, path: 'alignment.vertical', value: $('cfmt-va').value });
    await apiPost('/api/style', { ...rng, path: 'alignment.wrap_text', value: $('cfmt-wrap').checked ? 'true' : 'false' });
    await apiPost('/api/style', { ...rng, path: 'font.b', value: $('cfmt-b').checked ? 'true' : 'false' });
    await apiPost('/api/style', { ...rng, path: 'font.i', value: $('cfmt-i').checked ? 'true' : 'false' });
    await apiPost('/api/style', { ...rng, path: 'font.u', value: $('cfmt-u').checked ? 'true' : 'false' });
    await apiPost('/api/style', { ...rng, path: 'font.strike', value: $('cfmt-st').checked ? 'true' : 'false' });
    await apiPost('/api/style', { ...rng, path: 'font.size', value: String($('cfmt-sz').value || 12) });
    await apiPost('/api/style', { ...rng, path: 'font.color', value: fcSw.dataset.hex || '#000000' });
    if ($('cfmt-fn').value.trim()) await apiPost('/api/fontname', { ...rng, name: $('cfmt-fn').value.trim() });
    if ($('cfmt-nofill').dataset.none !== '1') await apiPost('/api/style', { ...rng, path: 'fill.color', value: bgSw.dataset.hex || '#ffff00' });
    if (pendingBorder) await apiPost('/api/border', { ...rng, type: pendingBorder, style: $('cfmt-bs').value, color: bcSw.dataset.hex || '#000000' });
    close();
    scheduleRefresh(true);
  };
}
$('btn-clearfmt').onclick = async () => {
  const n = normSel();
  await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'formatting' });
  scheduleRefresh(true);
};
/* 字体家族（同母项目 UniDoc 字体清单） */
$('sel-fontfamily').onchange = async (e) => {
  if (!e.target.value) return;
  if (applyRichFormat('font', e.target.value, false)) return;
  const n = normSel();
  await apiPost('/api/fontname', { sheet: S.sheet, ...n, name: e.target.value });
  scheduleRefresh(true);
  gridScroll.focus();
};
/* 边框菜单（Excel 边框下拉语义） */
const borderMenu = $('border-menu');
$('btn-border').onclick = (e) => {
  e.stopPropagation();
  borderMenu.hidden = !borderMenu.hidden;
  if (!borderMenu.hidden) {
    const r = $('btn-border').getBoundingClientRect();
    borderMenu.style.left = r.left + 'px';
    borderMenu.style.top = r.bottom + 2 + 'px';
  }
};
borderMenu.querySelectorAll('.bm-item').forEach((it) => {
  it.onclick = async () => {
    borderMenu.hidden = true;
    const n = normSel();
    await apiPost('/api/border', {
      sheet: S.sheet, ...n,
      type: it.dataset.btype,
      style: it.dataset.bstyle || 'thin',
      color: '#000000',
    });
    scheduleRefresh(true);
    gridScroll.focus();
  };
});
document.addEventListener('mousedown', (e) => {
  if (!borderMenu.hidden && !borderMenu.contains(e.target) && e.target.id !== 'btn-border') borderMenu.hidden = true;
});
/* 格式刷（同母项目 UniDoc：取样→刷到目标选区，Esc 取消） */
let painter = null;
$('btn-painter').onclick = () => {
  if (painter) {
    painter = null;
    $('btn-painter').classList.remove('active');
    setStatus('已取消格式刷');
    return;
  }
  painter = { ...normSel() };
  $('btn-painter').classList.add('active');
  setStatus('格式刷：选择目标区域应用格式（Esc 取消）');
};
async function applyPainter() {
  if (!painter) return;
  const src = painter;
  painter = null;
  $('btn-painter').classList.remove('active');
  const n = normSel();
  await apiPost('/api/copystyle', {
    sheet: S.sheet, sr0: src.r0, sc0: src.c0, sr1: src.r1, sc1: src.c1,
    dstSheet: S.sheet, dr0: n.r0, dc0: n.c0, dr1: n.r1, dc1: n.c1,
  });
  setStatus('已应用格式');
  scheduleRefresh(true);
}
/* 合并单元格（引擎 vendor 分支 API，Excel 语义） */
$('btn-merge').onclick = async () => {
  const n = normSel();
  const merged = (S.merges || []).some((m) => m.r0 === n.r0 && m.c0 === n.c0 && m.r1 === n.r1 && m.c1 === n.c1);
  if (!merged && n.r0 === n.r1 && n.c0 === n.c1) { setStatus('请先选择多个单元格'); return; }
  const result = await apiPost('/api/merge', { sheet: S.sheet, ...n, op: merged ? 'unmerge' : 'merge' });
  S.merges = result.merges || [];
  normalizeCurrentForMerges();
  setStatus(merged ? '已取消合并' : '已合并单元格');
  scheduleRefresh(true);
};
/* 条件格式规则管理器。标准规则本地执行；未知标准/x14 扩展只做无损往返。 */
$('btn-cf').onclick = () => openCfDialog();
const CF_PRESETS = {
  red: { fill: '#FFC7CE', font: '#9C0006', label: '浅红填充色深红色文本' },
  green: { fill: '#C6EFCE', font: '#006100', label: '绿填充色深绿色文本' },
  yellow: { fill: '#FFEB9C', font: '#9C6500', label: '黄填充色深黄色文本' },
};
const CF_RULE_LABELS = {
  CellIs: '单元格值', Text: '特定文本', Formula: '使用公式', TimePeriod: '日期',
  DuplicateValues: '重复值', UniqueValues: '唯一值', Blanks: '空值', NotBlanks: '非空值',
  Errors: '错误', NoErrors: '无错误', AboveAverage: '高于平均值', BelowAverage: '低于平均值',
  Top10: '前若干项', Bottom10: '后若干项', ColorScale: '色阶', DataBar: '数据条',
  IconSet: '图标集', IconRating: '评级图标',
};
let cfRules = [];
let cfListRequest = 0;
function cfEscape(value) {
  return String(value ?? '').replaceAll('&', '&amp;').replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}
function cfSelectionRange() {
  const n = normSel();
  return `${cellRef(n.r0, n.c0)}:${cellRef(n.r1, n.c1)}`;
}
function renderCfRules() {
  const host = $('cf-rule-list');
  if (!host) return;
  if (!cfRules.length) {
    host.innerHTML = '<div class="cf-empty">当前工作表没有可执行的标准条件格式规则。</div>';
    return;
  }
  host.innerHTML = cfRules.map((rule, position) => {
    const type = CF_RULE_LABELS[rule.ruleType] || rule.ruleType || '未知规则';
    const stopControl = rule.canStopIfTrue
      ? `<label class="cf-stop"><input type="checkbox" ${rule.stopIfTrue ? 'checked' : ''}> 为真则停止</label>`
      : '<span class="cf-stop cf-muted">此视觉规则无“为真则停止”运行语义</span>';
    return `<section class="cf-rule" data-cf-index="${rule.index}">
      <div class="cf-rule-head">
        <span class="cf-priority">${position + 1}</span>
        <span class="cf-rule-type">${cfEscape(type)}</span>
        <span class="cf-summary" title="${cfEscape(rule.summary)}">${cfEscape(rule.summary)}</span>
        <button class="cf-up" title="提高优先级" ${position === 0 ? 'disabled' : ''}>↑</button>
        <button class="cf-down" title="降低优先级" ${position + 1 === cfRules.length ? 'disabled' : ''}>↓</button>
      </div>
      <div class="cf-rule-edit">
        <label>应用于</label><input class="cf-range" value="${cfEscape(rule.range)}" spellcheck="false">
        ${stopControl}
      </div>
      <div class="cf-rule-actions">
        <button class="cf-copy">复制规则</button><button class="cf-delete danger">删除</button>
        <button class="cf-save primary">应用修改</button>
      </div>
    </section>`;
  }).join('');
}
async function refreshCfRules() {
  const request = ++cfListRequest;
  $('cf-info').textContent = '正在读取规则…';
  try {
    const result = await apiPost('/api/cf', { sheet: S.sheet, op: 'list' });
    if (request !== cfListRequest) return;
    cfRules = result.rules || [];
    renderCfRules();
    $('cf-info').textContent = `${cfRules.length} 条标准规则`;
  } catch (error) {
    if (request === cfListRequest) $('cf-info').textContent = `读取失败：${error.message}`;
  }
}
async function cfMutate(payload, message) {
  const result = await apiPost('/api/cf', { sheet: S.sheet, ...payload });
  cfRules = result.rules || [];
  renderCfRules();
  $('cf-info').textContent = message;
  scheduleRefresh(true);
  return result;
}
function cfUpdateRequest(rule, index, range, stopValue) {
  const payload = { op: 'update', index: Number(index), range: String(range || '').trim() };
  if (rule?.canStopIfTrue) payload.stopIfTrue = !!stopValue;
  return payload;
}
function cfActionRequest(op, index) { return { op, index: Number(index) }; }
function cfPreserveOnlyBoundary(result) {
  return result?.capabilities?.x14Runtime === false
    && result?.capabilities?.unknownExtensions === 'preserve-only';
}
window.__unicellCfManagerTest = {
  open: openCfDialog,
  updateRequest: cfUpdateRequest,
  actionRequest: cfActionRequest,
  preserveOnlyBoundary: cfPreserveOnlyBoundary,
  selectionRange: cfSelectionRange,
};
async function openCfDialog() {
  let dlg = $('cf-dialog');
  if (!dlg) {
    dlg = document.createElement('div');
    dlg.id = 'cf-dialog';
    dlg.innerHTML = `
      <div class="fd-title"><span>条件格式规则管理器</span><button id="cf-close">×</button></div>
      <div class="cf-toolbar"><span>规则按优先级从高到低排列</span><button id="cf-refresh">刷新</button></div>
      <div id="cf-rule-list"></div>
      <div class="cf-extension-note">x14 与未知扩展：导入和导出时原样保留；这里不虚假宣称已在 UniCell 内执行。</div>
      <details class="cf-create" open>
        <summary>基于当前选区新建规则</summary>
        <div class="fd-row"><label>规则</label>
          <select id="cf-kind">
            <option value="gt">单元格值 大于</option>
            <option value="lt">单元格值 小于</option>
            <option value="eq">单元格值 等于</option>
            <option value="between">单元格值 介于</option>
            <option value="text">文本包含</option>
            <option value="dup">重复值</option>
          </select></div>
        <div class="fd-row"><label>值</label><input id="cf-v1" spellcheck="false"><input id="cf-v2" spellcheck="false" placeholder="上限" hidden></div>
        <div class="fd-row"><label>格式</label>
          <select id="cf-preset">
            <option value="red">浅红填充色深红色文本</option>
            <option value="green">绿填充色深绿色文本</option>
            <option value="yellow">黄填充色深黄色文本</option>
          </select></div>
        <div class="cf-create-actions"><span>应用于：<b id="cf-new-range"></b></span><button id="cf-add" class="primary">新建规则</button></div>
      </details>
      <div class="fd-row fd-btns"><span id="cf-info"></span><button id="cf-clear">清除本表规则</button></div>`;
    document.body.appendChild(dlg);
    $('cf-close').onclick = () => { dlg.hidden = true; gridScroll.focus(); };
    $('cf-refresh').onclick = refreshCfRules;
    $('cf-kind').onchange = () => {
      $('cf-v2').hidden = $('cf-kind').value !== 'between';
      $('cf-v1').hidden = $('cf-kind').value === 'dup';
    };
    $('cf-add').onclick = addCfRule;
    $('cf-clear').onclick = async () => {
      if (!cfRules.length) { $('cf-info').textContent = '没有可清除的标准规则'; return; }
      if (!confirm(`确定清除本工作表的 ${cfRules.length} 条标准规则？未知扩展仍会无损保留。`)) return;
      try { await cfMutate({ op: 'clear' }, `已清除 ${cfRules.length} 条规则`); }
      catch (error) { $('cf-info').textContent = `清除失败：${error.message}`; }
    };
    $('cf-rule-list').addEventListener('click', async (event) => {
      const button = event.target.closest('button');
      const card = event.target.closest('.cf-rule');
      if (!button || !card || button.disabled) return;
      const index = Number(card.dataset.cfIndex);
      button.disabled = true;
      try {
        if (button.classList.contains('cf-up')) await cfMutate(cfActionRequest('raise', index), '已提高规则优先级');
        else if (button.classList.contains('cf-down')) await cfMutate(cfActionRequest('lower', index), '已降低规则优先级');
        else if (button.classList.contains('cf-copy')) await cfMutate(cfActionRequest('duplicate', index), '已复制规则（未知扩展 ID 不会被重复）');
        else if (button.classList.contains('cf-delete')) await cfMutate(cfActionRequest('delete', index), '已删除规则');
        else if (button.classList.contains('cf-save')) {
          const rule = cfRules.find((item) => Number(item.index) === index);
          const payload = cfUpdateRequest(rule, index, card.querySelector('.cf-range').value,
            card.querySelector('.cf-stop input')?.checked);
          await cfMutate(payload, '已更新“应用于”和停止条件；原 DXF 与未知 XML 保持不变');
        }
      } catch (error) {
        $('cf-info').textContent = `操作失败：${error.message}`;
        button.disabled = false;
      }
    });
    $('cf-rule-list').addEventListener('keydown', (event) => {
      if (event.key === 'Enter' && event.target.classList.contains('cf-range')) {
        event.preventDefault();
        event.target.closest('.cf-rule').querySelector('.cf-save').click();
      }
    });
    dlg.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') $('cf-close').click();
    });
  }
  dlg.hidden = false;
  $('cf-new-range').textContent = cfSelectionRange();
  await refreshCfRules();
  return dlg;
}
async function addCfRule() {
  const n = normSel();
  const kind = $('cf-kind').value;
  const v1 = $('cf-v1').value.trim();
  const v2 = $('cf-v2').value.trim();
  const preset = CF_PRESETS[$('cf-preset').value];
  if (kind !== 'dup' && !v1) { $('cf-info').textContent = '请输入值'; return; }
  if (kind === 'between' && !v2) { $('cf-info').textContent = '请输入上限'; return; }
  const dxf = {
    font: { color: preset.font },
    fill: { color: preset.fill },
    border: null, num_fmt: null, alignment: null,
  };
  let rule;
  if (kind === 'dup') {
    rule = { type: 'DuplicateValues', format: dxf, stop_if_true: false };
  } else if (kind === 'text') {
    rule = { type: 'Text', operator: 'Contains', value: v1, format: dxf, stop_if_true: false };
  } else {
    const opMap = { gt: 'GreaterThan', lt: 'LessThan', eq: 'Equal', between: 'Between' };
    rule = { type: 'CellIs', operator: opMap[kind], formula: v1, formula2: kind === 'between' ? v2 : null, format: dxf, stop_if_true: false };
  }
  try {
    await cfMutate({ ...n, op: 'add', rule }, `已对 ${cfSelectionRange()} 添加规则`);
  } catch (error) { $('cf-info').textContent = `新建失败：${error.message}`; }
}

/* Excel 数据验证：typed OOXML 规则管理（未知扩展由服务端差量保留） */
$('btn-dv').onclick = () => openDvDialog();
$('btn-data-dv').onclick = () => openDvDialog();
let dvEditingId = null;
let dvRules = [];
const dvRuleCache = new Map();
let dvDropdownRequest = 0;
let dvListRequest = 0;
const DV_TYPE_LABELS = {
  any: '任何值', whole: '整数', decimal: '小数', list: '序列', date: '日期',
  time: '时间', textLength: '文本长度', custom: '自定义公式',
};
const DV_OPERATOR_LABELS = {
  between: '介于', notBetween: '不介于', equal: '等于', notEqual: '不等于',
  greaterThan: '大于', lessThan: '小于', greaterThanOrEqual: '大于或等于',
  lessThanOrEqual: '小于或等于',
};
function dvColumnNumber(text) {
  let value = 0;
  for (const ch of text.toUpperCase()) {
    if (ch < 'A' || ch > 'Z') return 0;
    value = value * 26 + ch.charCodeAt(0) - 64;
  }
  return value;
}
function dvParseEndpoint(text) {
  const value = text.replaceAll('$', '');
  let match = /^([A-Za-z]+)(\d+)$/.exec(value);
  if (match) return { c: dvColumnNumber(match[1]), r: +match[2] };
  match = /^([A-Za-z]+)$/.exec(value);
  if (match) return { c: dvColumnNumber(match[1]), r: null };
  match = /^(\d+)$/.exec(value);
  if (match) return { c: null, r: +match[1] };
  return null;
}
function dvRuleContainsCell(rule, row, col) {
  return String(rule.sqref || '').split(/\s+/).some((token) => {
    if (!token) return false;
    const pair = token.split(':');
    const first = dvParseEndpoint(pair[0]);
    const second = dvParseEndpoint(pair[1] || pair[0]);
    if (!first || !second) return false;
    const rows = first.r == null ? true : row >= Math.min(first.r, second.r) && row <= Math.max(first.r, second.r);
    const cols = first.c == null ? true : col >= Math.min(first.c, second.c) && col <= Math.max(first.c, second.c);
    return rows && cols;
  });
}
async function cachedDvRules(sheet) {
  if (!dvRuleCache.has(sheet)) {
    const pending = apiPost('/api/dv', { sheet, op: 'list' })
      .then((result) => result.rules || [])
      .catch((error) => {
        // A transient transport failure must not poison this sheet's cache forever.
        if (dvRuleCache.get(sheet) === pending) dvRuleCache.delete(sheet);
        throw error;
      });
    dvRuleCache.set(sheet, pending);
  }
  return dvRuleCache.get(sheet);
}
function invalidateDvRules(sheet = S.sheet) {
  dvRuleCache.delete(sheet);
}
window.invalidateDvRules = invalidateDvRules;
window.invalidateAllDvRules = () => dvRuleCache.clear();
function removeDvCellList() {
  $('dv-cell-list')?.remove();
}
function hideDvCellList() {
  dvListRequest++;
  removeDvCellList();
}
async function resolveDvListValues(rule, defaultSheet = S.sheet) {
  let formula = String(rule.formula1 || '').trim();
  if (formula.startsWith('=')) formula = formula.slice(1);
  if (formula.length >= 2 && formula.startsWith('"') && formula.endsWith('"')) {
    return formula.slice(1, -1).replaceAll('""', '"').split(',');
  }
  let sourceSheet = defaultSheet;
  const bang = formula.lastIndexOf('!');
  if (bang >= 0) {
    let name = formula.slice(0, bang);
    if (name.startsWith("'") && name.endsWith("'")) name = name.slice(1, -1).replaceAll("''", "'");
    const found = S.sheets.findIndex((sheet) => sheet.toLocaleLowerCase() === name.toLocaleLowerCase());
    if (found < 0) return [];
    sourceSheet = found;
    formula = formula.slice(bang + 1);
  }
  const match = /^\$?([A-Za-z]+)\$?(\d+)(?::\$?([A-Za-z]+)\$?(\d+))?$/.exec(formula);
  if (!match) return [];
  const c0 = dvColumnNumber(match[1]), r0 = +match[2];
  const c1 = match[3] ? dvColumnNumber(match[3]) : c0, r1 = match[4] ? +match[4] : r0;
  const cells = [];
  for (let row = Math.min(r0, r1); row <= Math.max(r0, r1); row++) {
    for (let col = Math.min(c0, c1); col <= Math.max(c0, c1); col++) {
      if (cells.length >= 500) break;
      cells.push(api(`/api/cell?sheet=${sourceSheet}&row=${row}&col=${col}`).then((cell) => cell.formatted));
    }
  }
  return (await Promise.all(cells)).filter((value) => value !== '');
}
function dvDateSerial(value) {
  const numeric = Number(value);
  if (value !== '' && Number.isFinite(numeric)) return numeric;
  const match = /^(\d{4})[-\/]([01]?\d)[-\/]([0-3]?\d)$/.exec(String(value).trim());
  if (!match) return NaN;
  const utc = Date.UTC(+match[1], +match[2] - 1, +match[3]);
  const date = new Date(utc);
  if (date.getUTCFullYear() !== +match[1] || date.getUTCMonth() !== +match[2] - 1 || date.getUTCDate() !== +match[3]) return NaN;
  return utc / 86400000 + 25569;
}
function dvTimeSerial(value) {
  const numeric = Number(value);
  if (value !== '' && Number.isFinite(numeric)) return numeric;
  const match = /^([01]?\d|2[0-3]):([0-5]\d)(?::([0-5]\d(?:\.\d+)?))?$/.exec(String(value).trim());
  return match ? (+match[1] * 3600 + +match[2] * 60 + +(match[3] || 0)) / 86400 : NaN;
}
function dvComparable(type, value) {
  if (type === 'date') return dvDateSerial(value);
  if (type === 'time') return dvTimeSerial(value);
  if (type === 'textLength') return Array.from(String(value)).length;
  return Number(value);
}
function dvCompare(operator, value, first, second) {
  switch (operator || 'between') {
    case 'between': return value >= first && value <= second;
    case 'notBetween': return value < first || value > second;
    case 'equal': return value === first;
    case 'notEqual': return value !== first;
    case 'greaterThan': return value > first;
    case 'lessThan': return value < first;
    case 'greaterThanOrEqual': return value >= first;
    case 'lessThanOrEqual': return value <= first;
    default: return true;
  }
}
function dvValidationRequest(sheet, row, col, rule, value) {
  return {
    url: '/api/dv',
    body: { sheet, op: 'validate', row, col, id: rule?.id, value },
  };
}
async function requestDvRuntimeValidation(sheet, row, col, rule, value, post = apiPost) {
  const request = dvValidationRequest(sheet, row, col, rule, value);
  return post(request.url, request.body);
}
window.__dvRuntimeTestHooks = {
  request: dvValidationRequest,
  evaluate: requestDvRuntimeValidation,
};
async function validateDvCellInput(sheet, row, col, value) {
  const rules = await cachedDvRules(sheet);
  const rule = rules.find((candidate) => candidate.type !== 'any' && dvRuleContainsCell(candidate, row, col));
  if (!rule || (value === '' && rule.allowBlank)) return true;
  let outcome;
  try {
    outcome = await requestDvRuntimeValidation(sheet, row, col, rule, value);
  } catch (error) {
    setStatus(`数据验证求值失败：${error.message || error}`);
    return false;
  }
  if (outcome?.valid || !rule.showErrorMessage) return true;
  const title = rule.errorTitle || '输入值无效';
  const message = rule.error || '此值与为该单元格定义的数据验证限制不匹配。';
  const text = `${title}\n\n${message}`;
  if ((rule.errorStyle || 'stop') === 'stop') {
    alert(text);
    return false;
  }
  return confirm(`${text}\n\n是否仍要继续？`);
}
function positionDvCellList(popup, arrow, col) {
  const rect = arrow.getBoundingClientRect();
  Object.assign(popup.style, {
    left: `${rect.left}px`, top: `${rect.bottom + 1}px`,
    minWidth: `${Math.max(120, colWidth(col))}px`,
  });
}
async function showDvCellList(rule, arrow, context) {
  const sheet = context?.sheet ?? S.sheet;
  const row = context?.row ?? S.cur.r;
  const col = context?.col ?? S.cur.c;
  const contextKey = `${sheet}:${row}:${col}`;
  const request = ++dvListRequest;
  removeDvCellList();
  const values = await resolveDvListValues(rule, sheet);
  if (request !== dvListRequest || sheet !== S.sheet || row !== S.cur.r || col !== S.cur.c
    || arrow.dataset.context !== contextKey) return;
  if (!values.length) { setStatus('数据验证列表来源暂时没有可显示的值'); return; }
  const popup = document.createElement('div');
  popup.id = 'dv-cell-list';
  popup.dataset.context = contextKey;
  positionDvCellList(popup, arrow, col);
  for (const value of values) {
    const option = document.createElement('button');
    option.type = 'button';
    option.textContent = value;
    option.onclick = async (event) => {
      event.stopPropagation();
      await apiPost('/api/input', { sheet, row, col, value });
      hideDvCellList();
      scheduleRefresh(true);
      gridScroll.focus();
    };
    popup.appendChild(option);
  }
  document.body.appendChild(popup);
}
function hideDvInputMessage() {
  $('dv-input-message')?.remove();
}
function showDvInputMessage(rule, row, col) {
  hideDvInputMessage();
  if (!rule?.showInputMessage || (!rule.promptTitle && !rule.prompt)) return;
  const tip = document.createElement('div');
  tip.id = 'dv-input-message';
  if (rule.promptTitle) {
    const title = document.createElement('strong');
    title.textContent = rule.promptTitle;
    tip.appendChild(title);
  }
  if (rule.prompt) {
    const message = document.createElement('span');
    message.textContent = rule.prompt;
    tip.appendChild(message);
  }
  Object.assign(tip.style, {
    left: `${colX(col) + 4}px`, top: `${rowY(row) + rowHeight(row) + 3}px`,
  });
  $('selection-layer').appendChild(tip);
}
window.updateDvDropdown = async () => {
  const request = ++dvDropdownRequest;
  const sheet = S.sheet, row = S.cur.r, col = S.cur.c;
  const contextKey = `${sheet}:${row}:${col}`;
  let arrow = $('dv-cell-dropdown');
  if (!arrow) {
    arrow = document.createElement('button');
    arrow.id = 'dv-cell-dropdown';
    arrow.type = 'button';
    arrow.textContent = '▼';
    arrow.onmousedown = (event) => { event.preventDefault(); event.stopPropagation(); };
    arrow.onclick = (event) => {
      event.stopPropagation();
      if (arrow.dvRule && arrow.dataset.context) {
        showDvCellList(arrow.dvRule, arrow, {
          sheet: Number(arrow.dataset.sheet), row: Number(arrow.dataset.row), col: Number(arrow.dataset.col),
        });
      }
    };
    $('selection-layer').appendChild(arrow);
  }
  if (arrow.dataset.context !== contextKey) {
    arrow.hidden = true;
    arrow.dvRule = null;
    hideDvCellList();
  }
  hideDvInputMessage();
  let rules;
  try {
    rules = await cachedDvRules(sheet);
  } catch (error) {
    if (request === dvDropdownRequest) {
      arrow.hidden = true;
      arrow.dvRule = null;
      hideDvCellList();
    }
    return;
  }
  if (request !== dvDropdownRequest || sheet !== S.sheet || row !== S.cur.r || col !== S.cur.c) return;
  showDvInputMessage(rules.find((candidate) => dvRuleContainsCell(candidate, row, col)), row, col);
  const rule = rules.find((candidate) => candidate.type === 'list'
    && candidate.inCellDropdown !== false && dvRuleContainsCell(candidate, row, col));
  if (!rule) {
    arrow.hidden = true;
    arrow.dvRule = null;
    hideDvCellList();
    return;
  }
  Object.assign(arrow.style, {
    left: `${colX(col) + colWidth(col) - 19}px`, top: `${rowY(row) + 2}px`,
    width: '17px', height: `${Math.max(15, rowHeight(row) - 4)}px`,
  });
  arrow.dataset.context = contextKey;
  arrow.dataset.sheet = String(sheet);
  arrow.dataset.row = String(row);
  arrow.dataset.col = String(col);
  arrow.dvRule = rule;
  arrow.hidden = false;
  const popup = $('dv-cell-list');
  if (popup?.dataset.context === contextKey) positionDvCellList(popup, arrow, col);
};
document.addEventListener('mousedown', (event) => {
  if (!event.target.closest?.('#dv-cell-list, #dv-cell-dropdown')) hideDvCellList();
});
function selectionSqref() {
  const n = normSel();
  const first = cellRef(n.r0, n.c0);
  const last = cellRef(n.r1, n.c1);
  return first === last ? first : `${first}:${last}`;
}
function openDvDialog() {
  let dlg = $('dv-dialog');
  if (!dlg) {
    dlg = document.createElement('div');
    dlg.id = 'dv-dialog';
    dlg.innerHTML = `
      <div class="fd-title"><span>数据验证</span><button id="dv-close">×</button></div>
      <div class="dv-layout">
        <div class="dv-rules-pane">
          <div class="dv-pane-title">本工作表中的规则</div>
          <div id="dv-rule-list"></div>
          <div class="dv-rule-actions"><button id="dv-new">新建</button><button id="dv-delete">删除</button><button id="dv-clear">全部清除</button></div>
        </div>
        <div class="dv-editor-pane">
          <div class="fd-row"><label>应用区域</label><input id="dv-sqref" spellcheck="false"></div>
          <div class="fd-row"><label>允许</label><select id="dv-type">
            <option value="any">任何值</option><option value="whole">整数</option>
            <option value="decimal">小数</option><option value="list">序列</option>
            <option value="date">日期</option><option value="time">时间</option>
            <option value="textLength">文本长度</option><option value="custom">自定义公式</option>
          </select></div>
          <div class="fd-row" id="dv-operator-row"><label>数据</label><select id="dv-operator">
            <option value="between">介于</option><option value="notBetween">不介于</option>
            <option value="equal">等于</option><option value="notEqual">不等于</option>
            <option value="greaterThan">大于</option><option value="lessThan">小于</option>
            <option value="greaterThanOrEqual">大于或等于</option><option value="lessThanOrEqual">小于或等于</option>
          </select></div>
          <div class="fd-row" id="dv-formula1-row"><label id="dv-formula1-label">最小值</label><input id="dv-formula1" spellcheck="false"></div>
          <div class="fd-row" id="dv-formula2-row"><label>最大值</label><input id="dv-formula2" spellcheck="false"></div>
          <div class="dv-checks">
            <label><input type="checkbox" id="dv-allow-blank"> 忽略空值</label>
            <label id="dv-dropdown-wrap"><input type="checkbox" id="dv-dropdown" checked> 提供下拉箭头</label>
          </div>
          <details><summary>输入信息</summary>
            <label class="dv-toggle"><input type="checkbox" id="dv-show-input"> 选中单元格时显示输入信息</label>
            <div class="fd-row"><label>标题</label><input id="dv-prompt-title" maxlength="32"></div>
            <div class="fd-row"><label>信息</label><textarea id="dv-prompt" maxlength="255"></textarea></div>
          </details>
          <details><summary>出错警告</summary>
            <label class="dv-toggle"><input type="checkbox" id="dv-show-error"> 输入无效数据时显示警告</label>
            <div class="fd-row"><label>样式</label><select id="dv-error-style"><option value="stop">停止</option><option value="warning">警告</option><option value="information">信息</option></select></div>
            <div class="fd-row"><label>标题</label><input id="dv-error-title" maxlength="32"></div>
            <div class="fd-row"><label>消息</label><textarea id="dv-error" maxlength="255"></textarea></div>
          </details>
        </div>
      </div>
      <div class="fd-row fd-btns"><span id="dv-info"></span><button id="dv-cancel">取消</button><button id="dv-save" class="primary">确定</button></div>`;
    document.body.appendChild(dlg);
    $('dv-close').onclick = $('dv-cancel').onclick = () => { dlg.hidden = true; gridScroll.focus(); };
    $('dv-type').onchange = updateDvFieldVisibility;
    $('dv-operator').onchange = updateDvFieldVisibility;
    $('dv-new').onclick = () => resetDvEditor();
    $('dv-save').onclick = saveDvRule;
    $('dv-delete').onclick = deleteDvRule;
    $('dv-clear').onclick = clearDvRules;
    dlg.addEventListener('keydown', (e) => e.stopPropagation());
  }
  dlg.hidden = false;
  resetDvEditor();
  loadDvRules();
}
function updateDvFieldVisibility() {
  const type = $('dv-type').value;
  const operator = $('dv-operator').value;
  const any = type === 'any';
  const list = type === 'list';
  const custom = type === 'custom';
  $('dv-operator-row').hidden = any || list || custom;
  $('dv-formula1-row').hidden = any;
  $('dv-formula2-row').hidden = any || list || custom || !['between', 'notBetween'].includes(operator);
  $('dv-dropdown-wrap').hidden = !list;
  $('dv-formula1-label').textContent = list ? '来源' : custom ? '公式' : ['between', 'notBetween'].includes(operator) ? '最小值' : '值';
  $('dv-formula1').placeholder = list ? '例如 "是,否" 或 Sheet2!$A$1:$A$10' : custom ? '例如 ISNUMBER(A1)' : '';
}
function resetDvEditor() {
  dvEditingId = null;
  $('dv-sqref').value = selectionSqref();
  $('dv-type').value = 'any';
  $('dv-operator').value = 'between';
  $('dv-formula1').value = '';
  $('dv-formula2').value = '';
  $('dv-allow-blank').checked = false;
  $('dv-dropdown').checked = true;
  $('dv-show-input').checked = false;
  $('dv-show-error').checked = true;
  $('dv-error-style').value = 'stop';
  $('dv-prompt-title').value = '';
  $('dv-prompt').value = '';
  $('dv-error-title').value = '';
  $('dv-error').value = '';
  $('dv-info').textContent = '新建规则';
  updateDvFieldVisibility();
  renderDvRuleList();
}
async function loadDvRules() {
  try {
    const result = await apiPost('/api/dv', { sheet: S.sheet, op: 'list' });
    dvRules = result.rules || [];
    dvRuleCache.set(S.sheet, Promise.resolve(dvRules));
    renderDvRuleList();
    window.updateDvDropdown?.();
    $('dv-info').textContent = dvRules.length ? `${dvRules.length} 条规则` : '本表暂无规则';
  } catch (error) {
    $('dv-info').textContent = error.message || String(error);
  }
}
function renderDvRuleList() {
  const list = $('dv-rule-list');
  if (!list) return;
  list.replaceChildren();
  if (!dvRules.length) {
    const empty = document.createElement('div');
    empty.className = 'dv-empty';
    empty.textContent = '暂无数据验证规则';
    list.appendChild(empty);
    return;
  }
  for (const rule of dvRules) {
    const item = document.createElement('button');
    item.className = `dv-rule-item${rule.id === dvEditingId ? ' active' : ''}`;
    const title = document.createElement('strong');
    title.textContent = rule.sqref;
    const detail = document.createElement('span');
    const op = rule.operator ? ` · ${DV_OPERATOR_LABELS[rule.operator] || rule.operator}` : '';
    detail.textContent = `${DV_TYPE_LABELS[rule.type] || rule.type}${op}`;
    item.append(title, detail);
    item.onclick = () => editDvRule(rule);
    list.appendChild(item);
  }
}
function editDvRule(rule) {
  dvEditingId = rule.id;
  $('dv-sqref').value = rule.sqref || selectionSqref();
  $('dv-type').value = rule.type || 'any';
  $('dv-operator').value = rule.operator || 'between';
  $('dv-formula1').value = rule.formula1 || '';
  $('dv-formula2').value = rule.formula2 || '';
  $('dv-allow-blank').checked = !!rule.allowBlank;
  $('dv-dropdown').checked = rule.inCellDropdown !== false;
  $('dv-show-input').checked = !!rule.showInputMessage;
  $('dv-show-error').checked = !!rule.showErrorMessage;
  $('dv-error-style').value = rule.errorStyle || 'stop';
  $('dv-prompt-title').value = rule.promptTitle || '';
  $('dv-prompt').value = rule.prompt || '';
  $('dv-error-title').value = rule.errorTitle || '';
  $('dv-error').value = rule.error || '';
  $('dv-info').textContent = `正在编辑 ${rule.sqref}`;
  updateDvFieldVisibility();
  renderDvRuleList();
}
function dvOptional(id) {
  const value = $(id).value;
  return value === '' ? null : value;
}
function collectDvRule() {
  const type = $('dv-type').value;
  const usesOperator = !['any', 'list', 'custom'].includes(type);
  const operator = usesOperator ? $('dv-operator').value : null;
  const usesFormula = type !== 'any';
  const usesFormula2 = usesOperator && ['between', 'notBetween'].includes(operator);
  return {
    id: dvEditingId || '', sqref: $('dv-sqref').value.trim(), type,
    operator, allowBlank: $('dv-allow-blank').checked,
    inCellDropdown: $('dv-dropdown').checked,
    showInputMessage: $('dv-show-input').checked,
    showErrorMessage: $('dv-show-error').checked,
    errorStyle: $('dv-error-style').value,
    promptTitle: dvOptional('dv-prompt-title'), prompt: dvOptional('dv-prompt'),
    errorTitle: dvOptional('dv-error-title'), error: dvOptional('dv-error'),
    formula1: usesFormula ? dvOptional('dv-formula1') : null,
    formula2: usesFormula2 ? dvOptional('dv-formula2') : null,
  };
}
async function saveDvRule() {
  const rule = collectDvRule();
  if (!rule.sqref) { $('dv-info').textContent = '请输入应用区域'; return; }
  if (rule.type !== 'any' && !rule.formula1) { $('dv-info').textContent = '请输入条件或来源'; return; }
  if (rule.formula2 === null && ['between', 'notBetween'].includes(rule.operator) && rule.type !== 'any') {
    $('dv-info').textContent = '请输入第二个边界值'; return;
  }
  try {
    const op = dvEditingId ? 'update' : 'add';
    const result = await apiPost('/api/dv', { sheet: S.sheet, op, id: dvEditingId, rule });
    dvEditingId = result.rule.id;
    await loadDvRules();
    $('dv-info').textContent = op === 'add' ? '规则已添加' : '规则已更新';
  } catch (error) {
    $('dv-info').textContent = error.message || String(error);
  }
}
async function deleteDvRule() {
  if (!dvEditingId) { $('dv-info').textContent = '请先选择规则'; return; }
  try {
    await apiPost('/api/dv', { sheet: S.sheet, op: 'delete', id: dvEditingId });
    resetDvEditor();
    await loadDvRules();
    $('dv-info').textContent = '规则已删除';
  } catch (error) {
    $('dv-info').textContent = error.message || String(error);
  }
}
async function clearDvRules() {
  if (!dvRules.length) return;
  if (!confirm(`确定清除本工作表的 ${dvRules.length} 条数据验证规则？`)) return;
  try {
    await apiPost('/api/dv', { sheet: S.sheet, op: 'clear' });
    dvRules = [];
    resetDvEditor();
    $('dv-info').textContent = '已清除全部规则';
  } catch (error) {
    $('dv-info').textContent = error.message || String(error);
  }
}

/* 原生 PivotCache：只编辑刷新策略，PivotTable/缓存字段/记录/切片器继续保持 OOXML 原生 */
$('btn-pivot-caches').onclick = () => openPivotCacheDialog();
let pivotCaches = [];
let pivotSelectedPart = null;
const PIVOT_BOOL_FIELDS = [
  ['refreshOnLoad', '打开文件时刷新'], ['enableRefresh', '允许刷新'],
  ['backgroundQuery', '允许后台刷新'], ['saveData', '随文件保存缓存数据'],
  ['upgradeOnRefresh', '刷新时升级缓存'],
];
function openPivotCacheDialog() {
  let dlg = $('pivot-cache-dialog');
  if (!dlg) {
    dlg = document.createElement('div');
    dlg.id = 'pivot-cache-dialog';
    dlg.innerHTML = `
      <div class="fd-title"><span>原生透视表缓存</span><button id="pivot-close">×</button></div>
      <div class="pivot-note">仅差量修改 Excel 原生 PivotCache 的刷新属性；透视表、缓存字段与记录、切片器和时间线不会被重建或栅格化。</div>
      <div class="pivot-layout">
        <div class="pivot-list-pane"><div id="pivot-cache-list"></div><button id="pivot-refresh-all">全部设为打开时刷新</button></div>
        <div id="pivot-cache-editor" class="pivot-editor-pane">
          <div id="pivot-empty">当前工作簿没有原生透视缓存。</div>
          <div id="pivot-editor-fields" hidden>
            <div class="pivot-heading"><strong id="pivot-cache-title"></strong><span id="pivot-edited-badge" hidden>已修改</span></div>
            <dl id="pivot-cache-meta"></dl>
            <div id="pivot-shared-warning" class="pivot-warning" hidden></div>
            <div id="pivot-bool-fields"></div>
            <div class="fd-row"><label>保留项目上限</label><select id="pivot-missing-mode"><option value="keep">保持当前值</option><option value="value">设置为</option><option value="remove">删除显式设置</option></select><input id="pivot-missing-value" type="number" min="0" step="1" disabled></div>
            <div class="fd-row fd-btns"><span id="pivot-info"></span><button id="pivot-reset">恢复原设置</button><button id="pivot-save" class="primary">应用</button></div>
          </div>
        </div>
      </div>`;
    document.body.appendChild(dlg);
    $('pivot-close').onclick = () => { dlg.hidden = true; gridScroll.focus(); };
    $('pivot-missing-mode').onchange = () => { $('pivot-missing-value').disabled = $('pivot-missing-mode').value !== 'value'; };
    $('pivot-save').onclick = savePivotCacheRefresh;
    $('pivot-reset').onclick = resetPivotCacheRefresh;
    $('pivot-refresh-all').onclick = refreshAllPivotCachesOnOpen;
    dlg.addEventListener('keydown', (event) => event.stopPropagation());
  }
  dlg.hidden = false;
  loadPivotCaches();
}
async function loadPivotCaches(preferredPart = pivotSelectedPart) {
  $('pivot-cache-list').innerHTML = '<div class="pivot-loading">正在读取原生 OPC 关系图…</div>';
  try {
    const model = await apiPost('/api/pivot-caches', { op: 'list' });
    pivotCaches = model.caches || [];
    pivotSelectedPart = pivotCaches.some((cache) => cache.part === preferredPart)
      ? preferredPart : pivotCaches[0]?.part || null;
    renderPivotCacheList();
    renderPivotCacheEditor();
  } catch (error) {
    pivotCaches = [];
    $('pivot-cache-list').textContent = '';
    $('pivot-empty').hidden = false;
    $('pivot-empty').textContent = error.message || String(error);
  }
}
function pivotSourceLabel(cache) {
  const source = cache.source || {};
  if (source.type === 'worksheet') return source.name || [source.sheet, source.ref].filter(Boolean).join('!') || '工作表区域';
  if (source.connectionId != null) return `外部连接 #${source.connectionId}`;
  return source.type || '未知来源';
}
function renderPivotCacheList() {
  const list = $('pivot-cache-list');
  list.replaceChildren();
  if (!pivotCaches.length) {
    const empty = document.createElement('div');
    empty.className = 'pivot-loading'; empty.textContent = '没有透视缓存'; list.appendChild(empty); return;
  }
  for (const cache of pivotCaches) {
    const button = document.createElement('button');
    button.className = `pivot-cache-item${cache.part === pivotSelectedPart ? ' active' : ''}`;
    const name = document.createElement('strong');
    name.textContent = `缓存 ${cache.cacheId}${cache.shared ? '（共享）' : ''}`;
    const source = document.createElement('span'); source.textContent = pivotSourceLabel(cache);
    const consumers = document.createElement('small'); consumers.textContent = `${cache.pivotTables?.length || 0} 个透视表${cache.edited ? ' · 已修改' : ''}`;
    button.append(name, source, consumers);
    button.onclick = () => { pivotSelectedPart = cache.part; renderPivotCacheList(); renderPivotCacheEditor(); };
    list.appendChild(button);
  }
}
function pivotCurrentCache() {
  return pivotCaches.find((cache) => cache.part === pivotSelectedPart) || null;
}
function renderPivotCacheEditor() {
  const cache = pivotCurrentCache();
  $('pivot-empty').hidden = !!cache;
  $('pivot-editor-fields').hidden = !cache;
  if (!cache) return;
  $('pivot-cache-title').textContent = `缓存 ${cache.cacheId}`;
  $('pivot-edited-badge').hidden = !cache.edited;
  const source = cache.source || {};
  const meta = $('pivot-cache-meta');
  meta.replaceChildren();
  for (const [term, value] of [
    ['原生部件', cache.part], ['来源', pivotSourceLabel(cache)],
    ['字段数', cache.fieldCount ?? '未知'], ['记录部件', cache.recordsPart || '未保存'],
  ]) {
    const dt = document.createElement('dt'); dt.textContent = term;
    const dd = document.createElement('dd'); dd.textContent = value;
    meta.append(dt, dd);
  }
  const warning = $('pivot-shared-warning');
  warning.hidden = !cache.shared;
  warning.textContent = cache.shared ? `这是共享缓存；修改刷新策略会同时影响 ${cache.pivotTables.length} 个透视表。` : '';
  const fields = $('pivot-bool-fields');
  fields.replaceChildren();
  for (const [name, label] of PIVOT_BOOL_FIELDS) {
    const row = document.createElement('div'); row.className = 'fd-row';
    const caption = document.createElement('label'); caption.textContent = label;
    const select = document.createElement('select'); select.dataset.pivotField = name;
    const current = cache.refresh?.[name];
    select.innerHTML = `<option value="keep">保持当前值（${current == null ? '未显式设置' : current ? '是' : '否'}）</option><option value="true">是</option><option value="false">否</option><option value="null">删除显式设置</option>`;
    row.append(caption, select); fields.appendChild(row);
  }
  $('pivot-missing-mode').value = 'keep';
  $('pivot-missing-value').value = cache.refresh?.missingItemsLimit ?? '';
  $('pivot-missing-value').disabled = true;
  $('pivot-info').textContent = `${cache.pivotTables?.map((pivot) => pivot.name || pivot.part).join('、') || '尚无引用者'}`;
}
function collectPivotRefreshPatch() {
  const patch = {};
  document.querySelectorAll('#pivot-bool-fields [data-pivot-field]').forEach((select) => {
    if (select.value === 'true') patch[select.dataset.pivotField] = true;
    else if (select.value === 'false') patch[select.dataset.pivotField] = false;
    else if (select.value === 'null') patch[select.dataset.pivotField] = null;
  });
  const missingMode = $('pivot-missing-mode').value;
  if (missingMode === 'remove') patch.missingItemsLimit = null;
  else if (missingMode === 'value') patch.missingItemsLimit = Math.max(0, Number.parseInt($('pivot-missing-value').value, 10) || 0);
  return patch;
}
async function savePivotCacheRefresh() {
  const cache = pivotCurrentCache();
  if (!cache) return;
  const patch = collectPivotRefreshPatch();
  if (!Object.keys(patch).length) { $('pivot-info').textContent = '没有需要应用的更改'; return; }
  try {
    const model = await apiPost('/api/pivot-caches', { op: 'update', cacheId: cache.cacheId, part: cache.part, patch });
    pivotCaches = model.caches || [];
    renderPivotCacheList(); renderPivotCacheEditor();
    $('pivot-info').textContent = '刷新策略已差量写入；Excel 打开/刷新时仍使用原生透视缓存。';
  } catch (error) { $('pivot-info').textContent = error.message || String(error); }
}
async function resetPivotCacheRefresh() {
  const cache = pivotCurrentCache();
  if (!cache) return;
  const model = await apiPost('/api/pivot-caches', { op: 'reset', part: cache.part });
  pivotCaches = model.caches || [];
  renderPivotCacheList(); renderPivotCacheEditor();
  $('pivot-info').textContent = '已恢复导入文件中的原始刷新设置';
}
async function refreshAllPivotCachesOnOpen() {
  if (!pivotCaches.length) return;
  for (const cache of pivotCaches) {
    await apiPost('/api/pivot-caches', {
      op: 'update', cacheId: cache.cacheId, part: cache.part,
      patch: { refreshOnLoad: true, enableRefresh: true },
    });
  }
  await loadPivotCaches(pivotSelectedPart);
  $('pivot-info').textContent = `已将 ${pivotCaches.length} 个缓存设为打开文件时刷新`;
}
$('sel-numfmt').onchange = (e) => styleSel('num_fmt', e.target.value);
$('btn-dec-more').onclick = () => adjustDecimals(1);
$('btn-dec-less').onclick = () => adjustDecimals(-1);
async function adjustDecimals(delta) {
  const j = await api(`/api/cell?sheet=${S.sheet}&row=${S.cur.r}&col=${S.cur.c}`);
  let nf = j.style.nf || 'general';
  if (nf === 'general' || nf === '') nf = delta > 0 ? '0.0' : '0';
  else {
    const m = /\.(0+)/.exec(nf);
    if (m) {
      const count = m[1].length + delta;
      nf = count <= 0 ? nf.replace(/\.0+/, '') : nf.replace(/\.0+/, '.' + '0'.repeat(count));
    } else if (delta > 0) {
      nf = nf.replace(/0(?![\d.])/, '0.0');
      if (!/\.0/.test(nf)) nf += '.0';
    }
  }
  await styleSel('num_fmt', nf);
}
$('btn-undo').onclick = async () => {
  await api('/api/undo', { method: 'POST' }); await refreshCalculationSettings(); scheduleRefresh(true);
};
$('btn-redo').onclick = async () => {
  await api('/api/redo', { method: 'POST' }); await refreshCalculationSettings(); scheduleRefresh(true);
};
$('btn-freeze').onclick = async () => {
  await apiPost('/api/freeze', { sheet: S.sheet, rows: S.cur.r - 1, cols: S.cur.c - 1 });
  setStatus(`已冻结 ${S.cur.r - 1} 行 ${S.cur.c - 1} 列`);
  scheduleRefresh(true);
};
$('btn-unfreeze').onclick = async () => {
  await apiPost('/api/freeze', { sheet: S.sheet, rows: 0, cols: 0 });
  setStatus('已取消冻结');
  scheduleRefresh(true);
};

/* ================= Excel 式本地文件生命周期 =================
 * showOpenFilePicker/showSaveFilePicker 可用时保留 FileSystemFileHandle，Ctrl+S 原位覆盖；
 * 其余浏览器回退为 input/a[download]。IndexedDB 只保存本地恢复副本与最近文件，不上传云端。
 */
const LOCAL_FILE_DB = 'unicell-local-files';
const LOCAL_FILE_DB_VERSION = 1;
const LOCAL_AUTOSAVE_KEY = 'current';
const MAX_RECENT_FILES = 10;
let localFileDbPromise = null;

function setStorageScope(value) {
  const scope = String(value || '').trim();
  if (/^[A-Za-z0-9._-]{8,96}$/.test(scope)) {
    S.storageScope = scope;
    return scope;
  }
  // Updated servers always provide a scope. A random page-only fallback keeps
  // recovery private even when a stale frontend is accidentally paired with an
  // older server; it deliberately cannot discover legacy unscoped records.
  S.storageScope = `ephemeral-${crypto.randomUUID?.() || `${Date.now()}-${Math.random()}`}`;
  return S.storageScope;
}
function currentStorageScope() { return setStorageScope(S.storageScope); }
function scopedLocalRecordId(kind, key) {
  return `${currentStorageScope()}:${kind}:${String(key)}`;
}
function currentAutosaveKey() {
  return scopedLocalRecordId('autosave', LOCAL_AUTOSAVE_KEY);
}
function isCurrentStorageRecord(record) {
  return !!record && record.storageScope === currentStorageScope();
}

function excelExtension() { return String(S.excelExtension || 'xlsx').toLowerCase() === 'xlsm' ? 'xlsm' : 'xlsx'; }
function excelMime(ext = excelExtension()) {
  return ext === 'xlsm'
    ? 'application/vnd.ms-excel.sheet.macroEnabled.12'
    : 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
}
function stripWorkbookExtension(name) {
  return String(name || '').replace(/\.(xlsx|xlsm|udoc|html?|csv)$/i, '') || 'workbook';
}
function normalizedWorkbookName(name, ext = excelExtension()) {
  return `${stripWorkbookExtension(name)}.${ext}`;
}
function workbookFormatFromName(name, fallback = null) {
  const match = /\.(xlsx|xlsm|udoc|html|csv)$/i.exec(String(name || '').trim());
  return match ? match[1].toLowerCase() : fallback;
}
function workbookMime(format) {
  if (format === 'udoc') return UDOC_MIME;
  if (format === 'html') return LOSSLESS_HTML_MIME;
  if (format === 'csv') return CSV_MIME;
  return excelMime(format);
}
function workbookFormatDescription(format) {
  if (format === 'udoc') return 'UniCell udoc';
  if (format === 'html') return 'UniCell 无损 HTML';
  if (format === 'csv') return 'CSV 当前工作表（仅值）';
  return format === 'xlsm' ? 'Excel 宏工作簿' : 'Excel 工作簿';
}
function currentBasename() {
  if (S.localFileName) return stripWorkbookExtension(S.localFileName);
  const t = (document.title || '').split(' — ')[0];
  return t && t !== 'UniCell' ? stripWorkbookExtension(t) : 'workbook';
}
function suggestedWorkbookName() {
  const source = S.localFileName || currentBasename();
  return normalizedWorkbookName(source, workbookFormatFromName(source, excelExtension()));
}
function setLocalFileIdentity(name, handle = null) {
  const source = name || currentBasename();
  S.localFileName = normalizedWorkbookName(source, workbookFormatFromName(source, excelExtension()));
  S.fileHandle = handle;
  if (!location.search.includes('test=auto')) document.title = `${stripWorkbookExtension(S.localFileName)} — UniCell`;
  updateFileLifecycleUI();
}
function updateFileLifecycleUI() {
  const state = $('file-save-state');
  const save = $('btn-file-save');
  const saveAs = $('btn-file-save-as');
  if (save) save.disabled = !!S.saveBusy;
  if (saveAs) saveAs.disabled = !!S.saveBusy;
  if (state) {
    state.classList.toggle('dirty', !!S.dirty && !S.saveBusy);
    state.classList.toggle('saving', !!S.saveBusy);
    if (S.saveBusy) state.textContent = '保存中…';
    else if (!S.dirty) state.textContent = '已保存';
    else if (S.lastAutosaveAt) state.textContent = `未保存 · 自动恢复 ${new Date(S.lastAutosaveAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}`;
    else state.textContent = '未保存';
  }
  const recover = $('btn-file-recover');
  if (recover) recover.hidden = !S.recoveryRecord;
}
function hasUnsavedWorkbookChanges() { return !!S.dirty || (!!S.editing && !!S.editDirty); }
function markWorkbookDirty(reason = '') {
  S.dirty = true;
  S.dirtyRevision += 1;
  S.lastAutosaveAt = 0;
  updateFileLifecycleUI();
  // 自动化回归会进行大量短间隔写入，测试自身在 T72 显式验证自动保存，避免后台导出干扰时序。
  if (!location.search.includes('test=auto')) scheduleWorkbookAutosave();
  return reason;
}
async function markWorkbookClean(options = {}) {
  clearTimeout(S.autosaveTimer);
  S.autosaveTimer = 0;
  S.dirty = false;
  S.lastAutosaveAt = 0;
  if (options.name || options.handle !== undefined) {
    setLocalFileIdentity(options.name || S.localFileName || suggestedWorkbookName(), options.handle ?? null);
  }
  updateFileLifecycleUI();
  if (!options.clearRecovery) return;
  // 若空闲自动保存正处于导出/IDB 写入阶段，先等它收尾再删除，避免“保存成功后旧恢复副本复活”。
  const pendingAutosave = S.autosaveWriting;
  if (pendingAutosave) await pendingAutosave.catch(() => undefined);
  S.recoveryRecord = null;
  updateFileLifecycleUI();
  await localDbDelete('autosaves', currentAutosaveKey()).catch(() => undefined);
}
function scheduleWorkbookAutosave(delay = 2200) {
  clearTimeout(S.autosaveTimer);
  S.autosaveTimer = setTimeout(() => { void writeAutosaveSnapshot(); }, delay);
}

function openLocalFileDb() {
  if (!('indexedDB' in window)) return Promise.reject(new Error('IndexedDB unavailable'));
  if (localFileDbPromise) return localFileDbPromise;
  localFileDbPromise = new Promise((resolve, reject) => {
    const request = indexedDB.open(LOCAL_FILE_DB, LOCAL_FILE_DB_VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains('autosaves')) db.createObjectStore('autosaves', { keyPath: 'id' });
      if (!db.objectStoreNames.contains('recent')) db.createObjectStore('recent', { keyPath: 'id' });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error || new Error('IndexedDB open failed'));
    request.onblocked = () => reject(new Error('IndexedDB upgrade blocked'));
  });
  return localFileDbPromise;
}
async function localDbRequest(storeName, mode, operation) {
  const db = await openLocalFileDb();
  return new Promise((resolve, reject) => {
    const tx = db.transaction(storeName, mode);
    let request;
    let result;
    let settled = false;
    const fail = (error) => { if (!settled) { settled = true; reject(error); } };
    try { request = operation(tx.objectStore(storeName)); }
    catch (err) { fail(err); return; }
    request.onsuccess = () => { result = request.result; };
    request.onerror = () => fail(request.error || tx.error || new Error('IndexedDB request failed'));
    tx.oncomplete = () => { if (!settled) { settled = true; resolve(result); } };
    tx.onerror = () => fail(tx.error || new Error('IndexedDB transaction failed'));
    tx.onabort = () => fail(tx.error || new Error('IndexedDB transaction aborted'));
  });
}
const localDbGet = (store, key) => localDbRequest(store, 'readonly', (s) => s.get(key));
const localDbGetAll = (store) => localDbRequest(store, 'readonly', (s) => s.getAll());
const localDbPut = (store, value) => localDbRequest(store, 'readwrite', (s) => s.put(value));
const localDbDelete = (store, key) => localDbRequest(store, 'readwrite', (s) => s.delete(key));
const localDbClear = (store) => localDbRequest(store, 'readwrite', (s) => s.clear());

function buildSavePickerOptions(name = suggestedWorkbookName()) {
  const excelExt = excelExtension();
  const namedFormat = workbookFormatFromName(name, excelExt);
  // xlsx/xlsm 之间不能只靠改扩展名转换；宏工作簿必须继续建议 .xlsm。
  // udoc/HTML 则是完整可回读格式，已经是当前文件时应继续建议原格式。
  const preferred = ['udoc', 'html', 'csv'].includes(namedFormat) ? namedFormat : excelExt;
  const formats = [preferred, excelExt, 'udoc', 'html', 'csv'].filter((format, index, all) => all.indexOf(format) === index);
  return {
    id: 'unicell-workbook-save',
    suggestedName: normalizedWorkbookName(name, preferred),
    excludeAcceptAllOption: true,
    types: formats.map((format) => ({
      description: workbookFormatDescription(format),
      accept: { [workbookMime(format)]: [`.${format}`] },
    })),
  };
}
/* 可打开的四种扩展名。HTML 只认 .html：能再导入的只有 UniCell 导出的无损 HTML，
 * 服务端 /api/import-html 会校验内嵌载荷，普通网页会被明确拒绝。 */
const OPENABLE_EXTENSIONS = ['.xlsx', '.xlsm', '.udoc', '.html', '.csv'];
const OPEN_ACCEPT_ATTR = OPENABLE_EXTENSIONS.join(',');
/* 注意 MIME 的选法：Windows 的文件对话框会把 accept 里的 MIME 反查注册表，
 * 把该 MIME 关联的所有扩展名一并塞进筛选器。用 application/octet-stream 会带出
 * .com/.exe/.bin，用 text/html 会带出 .htm/.shtml/.ehtml。所以自有格式一律用
 * 私有 MIME，注册表查不到就只剩这里显式列出的扩展名。 */
const UDOC_MIME = 'application/x-unicell-udoc';
const LOSSLESS_HTML_MIME = 'application/x-unicell-html';
const XLSX_MIME = 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet';
const XLSM_MIME = 'application/vnd.ms-excel.sheet.macroEnabled.12';
const CSV_MIME = 'text/csv';

function buildOpenPickerOptions() {
  return {
    id: 'unicell-workbook-open', multiple: false, excludeAcceptAllOption: true,
    types: [
      // 第一项是默认筛选器：四种格式一起显示，省得为了看到 .udoc 去切换下拉框
      { description: 'UniCell 支持的工作簿', accept: {
        [XLSX_MIME]: ['.xlsx'], [XLSM_MIME]: ['.xlsm'],
        [UDOC_MIME]: ['.udoc'], [LOSSLESS_HTML_MIME]: ['.html'], [CSV_MIME]: ['.csv'],
      } },
      { description: 'Excel 工作簿', accept: { [XLSX_MIME]: ['.xlsx'], [XLSM_MIME]: ['.xlsm'] } },
      { description: 'UniCell udoc', accept: { [UDOC_MIME]: ['.udoc'] } },
      { description: 'UniCell 无损 HTML', accept: { [LOSSLESS_HTML_MIME]: ['.html'] } },
      { description: 'CSV 当前工作表', accept: { [CSV_MIME]: ['.csv'] } },
    ],
  };
}
async function ensureHandlePermission(handle, mode = 'readwrite') {
  if (!handle) return false;
  const descriptor = { mode };
  try {
    if (typeof handle.queryPermission !== 'function') return true;
    if (await handle.queryPermission(descriptor) === 'granted') return true;
    return typeof handle.requestPermission === 'function'
      && await handle.requestPermission(descriptor) === 'granted';
  } catch { return false; }
}
async function writeWorkbookToHandle(handle, blob) {
  if (!await ensureHandlePermission(handle, 'readwrite')) throw new Error('没有该文件的写入权限');
  const writable = await handle.createWritable();
  try {
    await writable.write(blob);
    await writable.close();
  } catch (err) {
    try { await writable.abort?.(); } catch {}
    throw err;
  }
}
function triggerBlobDownload(blob, fileName) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1500);
}
async function prepareWorkbookForPersistence() {
  if (pendingEditCommit) await pendingEditCommit;
  else if (S.editing && !S.editDirty) cancelEditUI();
  else if (S.editing) await commitEdit();
  // 数据验证拒绝提交时 commitEdit 会保留编辑器；绝不能把旧值保存后反而清掉 dirty。
  if (S.editing) throw new Error('当前单元格尚未通过数据验证，无法保存');
  await flushObjectWrites();
}
async function createWorkbookBlob(options = {}) {
  const quiet = options.quiet === true;
  if (options.skipActiveEdit) await flushObjectWrites();
  else await prepareWorkbookForPersistence();
  const objects = await collectObjectsForExport({ quiet });
  const response = await fetch('/api/export?name=' + encodeURIComponent(currentBasename()), {
    method: 'POST', body: JSON.stringify({ objects }),
  });
  if (!response.ok) throw new Error(`导出失败：HTTP ${response.status}`);
  const source = await response.blob();
  return source.type === excelMime() ? source : new Blob([source], { type: excelMime() });
}
async function createPersistenceBlob(format) {
  const normalized = String(format || '').toLowerCase();
  if (normalized === 'xlsx' || normalized === 'xlsm') {
    if (normalized !== excelExtension()) {
      throw new Error(`当前工作簿的 Excel 格式是 .${excelExtension()}，不能直接另存为 .${normalized}`);
    }
    return createWorkbookBlob();
  }
  if (normalized !== 'udoc' && normalized !== 'html' && normalized !== 'csv') {
    throw new Error(`不支持的保存格式：${format || '未知'}`);
  }
  await prepareWorkbookForPersistence();
  const sheetQuery = normalized === 'csv' ? `&sheet=${encodeURIComponent(S.sheet)}` : normalized === 'udoc' ? '&derived=0' : '';
  const response = await fetch(`/api/export-${normalized}?name=${encodeURIComponent(currentBasename())}${sheetQuery}`);
  if (!response.ok) throw new Error(`导出 ${normalized} 失败：HTTP ${response.status}`);
  const source = await response.blob();
  const mime = workbookMime(normalized);
  return source.type === mime ? source : new Blob([source], { type: mime });
}
async function addRecentFile(name, blob, handle = null) {
  if (!blob) return;
  const storageScope = currentStorageScope();
  const record = {
    id: scopedLocalRecordId('file', String(name).toLowerCase()),
    storageScope,
    name, blob, handle, updatedAt: Date.now(), extension: String(name).split('.').pop().toLowerCase(),
  };
  try {
    await localDbPut('recent', record);
  } catch (err) {
    // Safari/Firefox 不允许结构化克隆 FileSystemHandle 时，仍保留可恢复的 Blob。
    if (handle) await localDbPut('recent', { ...record, handle: null });
    else throw err;
  }
  const all = (await localDbGetAll('recent'))
    .filter(isCurrentStorageRecord)
    .sort((a, b) => b.updatedAt - a.updatedAt);
  await Promise.all(all.slice(MAX_RECENT_FILES).map((item) => localDbDelete('recent', item.id)));
}
async function writeAutosaveSnapshot() {
  clearTimeout(S.autosaveTimer);
  S.autosaveTimer = 0;
  if (!S.dirty) return null;
  // contenteditable/公式栏草稿尚未形成模型事务时，不擅自提交用户正在输入的内容。
  if ((S.editing && S.editDirty) || pendingEditCommit) { scheduleWorkbookAutosave(1200); return null; }
  if (S.autosaveWriting) return S.autosaveWriting;
  const revision = S.dirtyRevision;
  const task = (async () => {
    try {
      const blob = await createWorkbookBlob({ quiet: true, skipActiveEdit: true });
      const record = {
        id: currentAutosaveKey(), storageScope: currentStorageScope(),
        // 自动恢复统一保存权威 Excel 快照；当前文件可能是 udoc/HTML，不能沿用它的扩展名。
        name: normalizedWorkbookName(S.localFileName || currentBasename(), excelExtension()), extension: excelExtension(),
        updatedAt: Date.now(), revision, blob,
      };
      if (!S.dirty || S.dirtyRevision !== revision) return null;
      await localDbPut('autosaves', record);
      S.recoveryRecord = record;
      S.lastAutosaveAt = record.updatedAt;
      updateFileLifecycleUI();
      if (S.dirtyRevision !== revision && S.dirty) scheduleWorkbookAutosave(800);
      return record;
    } catch (err) {
      console.warn('UniCell autosave failed:', err);
      if (err?.name === 'QuotaExceededError') setStatus('自动恢复空间不足；请立即保存到本地文件');
      else if (S.dirty) scheduleWorkbookAutosave(5000);
      return null;
    }
  })();
  S.autosaveWriting = task;
  try { return await task; }
  finally { if (S.autosaveWriting === task) S.autosaveWriting = null; }
}
async function refreshRecoveryState() {
  try {
    const record = await localDbGet('autosaves', currentAutosaveKey());
    S.recoveryRecord = isCurrentStorageRecord(record) ? record : null;
  }
  catch { S.recoveryRecord = null; }
  updateFileLifecycleUI();
  if (S.recoveryRecord) setStatus(`发现 ${new Date(S.recoveryRecord.updatedAt).toLocaleString()} 的自动恢复副本`);
  return S.recoveryRecord;
}
async function importWorkbookFile(file, handle = null, options = {}) {
  if (!file) return false;
  if (options.confirmDiscard !== false && hasUnsavedWorkbookChanges()
      && !confirm('当前工作簿尚未保存，仍要打开其他文件吗？')) return false;
  if (S.editing) cancelEditUI();
  const lowerName = String(file.name || '').toLowerCase();
  // 拖拽和"最近文件"也会走到这里，扩展名白名单不能只靠选择器把关：
  // 否则无法识别的文件会被当成 xlsx 送进解压器，报出难懂的 zip 错误。
  if (!OPENABLE_EXTENSIONS.some((ext) => lowerName.endsWith(ext))) {
    throw new Error(`不支持的文件类型：${file.name}\n只能打开 ${OPENABLE_EXTENSIONS.join('、')}（HTML 需为 UniCell 导出的无损 HTML）`);
  }
  let endpoint = '/api/import';
  if (lowerName.endsWith('.udoc')) endpoint = '/api/import-udoc';
  else if (lowerName.endsWith('.html')) endpoint = '/api/import-html';
  else if (lowerName.endsWith('.csv')) endpoint = '/api/import-csv';
  const buffer = await file.arrayBuffer();
  await flushObjectWrites();
  await api(endpoint, { method: 'POST', body: buffer });
  await loadWorkbook();
  const targetName = normalizedWorkbookName(file.name, workbookFormatFromName(file.name, excelExtension()));
  if (options.recovered) {
    setLocalFileIdentity(targetName, null);
    S.recoveryRecord = null;
    markWorkbookDirty('recovery');
    setStatus(`已恢复 ${file.name}；请保存以确认恢复结果`);
  } else {
    await markWorkbookClean({ name: targetName, handle, clearRecovery: true });
    if (!options.skipRecent) await addRecentFile(file.name, file, handle).catch(() => undefined);
    setStatus(`已打开 ${file.name}`);
  }
  return true;
}
async function openLocalWorkbook() {
  if (typeof window.showOpenFilePicker !== 'function') { openFile(OPEN_ACCEPT_ATTR); return false; }
  try {
    const [handle] = await window.showOpenFilePicker(buildOpenPickerOptions());
    if (!handle) return false;
    return await importWorkbookFile(await handle.getFile(), handle);
  } catch (err) {
    if (err?.name === 'AbortError') return false;
    console.warn('Native open picker unavailable, falling back to file input:', err);
    openFile(OPEN_ACCEPT_ATTR);
    return false;
  }
}
function openFile(accept) {
  const input = $('file-input');
  input.accept = accept;
  input.click();
}
$('file-input').onchange = async (event) => {
  const file = event.target.files[0];
  try { if (file) await importWorkbookFile(file, null); }
  catch (err) { alert('导入失败：' + (err?.message || err)); }
  event.target.value = '';
};
async function runSaveAction(action) {
  if (S.saveBusy) return false;
  S.saveBusy = true;
  updateFileLifecycleUI();
  try { return await action(); }
  catch (err) {
    setStatus('保存失败');
    alert('保存失败：' + (err?.message || err));
    return false;
  } finally {
    S.saveBusy = false;
    updateFileLifecycleUI();
  }
}
async function saveWorkbook() {
  if (!S.fileHandle) return saveWorkbookAs();
  if (!await ensureHandlePermission(S.fileHandle, 'readwrite')) {
    S.fileHandle = null;
    updateFileLifecycleUI();
    return saveWorkbookAs();
  }
  return runSaveAction(async () => {
    const format = workbookFormatFromName(S.fileHandle.name);
    if (!format) throw new Error('当前文件扩展名不受支持，请使用“另存为”选择 Excel、udoc、无损 HTML 或 CSV');
    const blob = await createPersistenceBlob(format);
    await writeWorkbookToHandle(S.fileHandle, blob);
    const name = S.fileHandle.name || suggestedWorkbookName();
    await addRecentFile(name, blob, S.fileHandle).catch(() => undefined);
    await markWorkbookClean({ name, handle: S.fileHandle, clearRecovery: true });
    setStatus(`已保存 ${name}`);
    return true;
  });
}
async function saveWorkbookAs() {
  return runSaveAction(async () => {
    const name = suggestedWorkbookName();
    let handle = null;
    if (typeof window.showSaveFilePicker === 'function') {
      try { handle = await window.showSaveFilePicker(buildSavePickerOptions(name)); }
      catch (err) {
        if (err?.name === 'AbortError') return false;
        console.warn('Native save picker unavailable, falling back to download:', err);
      }
    }
    const format = workbookFormatFromName(handle?.name || name);
    if (!format) throw new Error('文件名必须以 .xlsx、.xlsm、.udoc、.html 或 .csv 结尾');
    const blob = await createPersistenceBlob(format);
    if (handle) await writeWorkbookToHandle(handle, blob);
    else triggerBlobDownload(blob, name);
    const savedName = handle?.name || name;
    await addRecentFile(savedName, blob, handle).catch(() => undefined);
    await markWorkbookClean({ name: savedName, handle, clearRecovery: true });
    setStatus(handle ? `已另存为 ${savedName}` : `已下载 ${savedName}`);
    return true;
  });
}
async function newWorkbook() {
  if (hasUnsavedWorkbookChanges() && !confirm('新建工作簿？未保存内容将丢失。')) return false;
  if (S.editing) cancelEditUI();
  await flushObjectWrites();
  await api('/api/new', { method: 'POST' });
  S.localFileName = '';
  S.fileHandle = null;
  await loadWorkbook();
  await markWorkbookClean({ name: `工作簿1.${excelExtension()}`, handle: null, clearRecovery: true });
  setStatus('已新建空白工作簿');
  return true;
}
async function openRecentRecord(record) {
  if (!record) return false;
  let file = null;
  let handle = record.handle || null;
  if (handle) {
    try {
      if (await ensureHandlePermission(handle, 'read')) file = await handle.getFile();
      else handle = null;
    } catch { handle = null; }
  }
  if (!file && record.blob) file = new File([record.blob], record.name, {
    type: record.blob.type || workbookMime(record.extension || workbookFormatFromName(record.name, excelExtension())),
  });
  if (!file) throw new Error('最近文件不可访问，也没有可恢复副本');
  return importWorkbookFile(file, handle);
}
async function restoreAutosaveSnapshot() {
  const record = S.recoveryRecord || await refreshRecoveryState();
  if (!record?.blob) { setStatus('没有可恢复的自动保存副本'); return false; }
  const file = new File([record.blob], record.name || `恢复的工作簿.${record.extension || 'xlsx'}`, {
    type: record.blob.type || excelMime(record.extension),
  });
  return importWorkbookFile(file, null, { recovered: true, confirmDiscard: true, skipRecent: true });
}
async function renderRecentFilesMenu() {
  const list = $('recent-files-list');
  let records = [];
  try {
    records = (await localDbGetAll('recent'))
      .filter(isCurrentStorageRecord)
      .sort((a, b) => b.updatedAt - a.updatedAt);
  }
  catch {}
  list.replaceChildren();
  if (!records.length) {
    const empty = document.createElement('div'); empty.className = 'recent-files-empty'; empty.textContent = '暂无最近文件'; list.appendChild(empty);
  }
  for (const record of records) {
    const button = document.createElement('button'); button.type = 'button'; button.className = 'recent-file-item'; button.setAttribute('role', 'menuitem');
    const name = document.createElement('span'); name.className = 'recent-file-name'; name.textContent = record.name;
    const time = document.createElement('span'); time.className = 'recent-file-time'; time.textContent = new Date(record.updatedAt).toLocaleString();
    const source = document.createElement('span'); source.className = 'recent-file-source'; source.textContent = record.handle ? '本地文件（允许时原位保存）' : '浏览器恢复副本';
    button.append(name, time, source);
    button.onclick = async () => {
      $('recent-files-menu').hidden = true;
      try { await openRecentRecord(record); } catch (err) { alert('打开最近文件失败：' + (err?.message || err)); }
    };
    list.appendChild(button);
  }
  $('btn-clear-recent').disabled = !records.length;
  return records;
}
async function toggleRecentFilesMenu() {
  const menu = $('recent-files-menu');
  if (!menu.hidden) { menu.hidden = true; return; }
  await renderRecentFilesMenu();
  const anchor = $('btn-file-recent').getBoundingClientRect();
  menu.style.top = `${Math.min(window.innerHeight - 80, anchor.bottom + 4)}px`;
  menu.style.left = `${Math.max(6, Math.min(window.innerWidth - 316, anchor.left))}px`;
  menu.hidden = false;
}
async function initLocalFileLifecycle() {
  if (!S.localFileName) setLocalFileIdentity(`${currentBasename()}.${excelExtension()}`, null);
  updateFileLifecycleUI();
  await refreshRecoveryState();
}
function handleSaveShortcut(event) {
  if (!(event.ctrlKey || event.metaKey) || String(event.key).toLowerCase() !== 's') return;
  event.preventDefault();
  event.stopImmediatePropagation();
  if (event.repeat) return;
  void (event.shiftKey ? saveWorkbookAs() : saveWorkbook());
}
function handleDirtyBeforeUnload(event) {
  if (!hasUnsavedWorkbookChanges()) return;
  event.preventDefault();
  event.returnValue = '';
}
window.addEventListener('keydown', handleSaveShortcut, true);
window.addEventListener('beforeunload', handleDirtyBeforeUnload);
document.addEventListener('pointerdown', (event) => {
  const menu = $('recent-files-menu');
  if (!menu.hidden && !menu.contains(event.target) && !$('btn-file-recent').contains(event.target)) menu.hidden = true;
});
async function clearCurrentRecentFiles() {
  try {
    const records = (await localDbGetAll('recent')).filter(isCurrentStorageRecord);
    await Promise.all(records.map((record) => localDbDelete('recent', record.id)));
  } catch {}
}
$('btn-clear-recent').onclick = async () => {
  await clearCurrentRecentFiles();
  await renderRecentFilesMenu();
};

// 下载 Response（含 Content-Disposition 的附件）为本地文件
async function downloadResp(resp, fallbackName) { triggerBlobDownload(await resp.blob(), fallbackName); }
// 收集插入对象用于 xlsx 嵌图（SVG→EMF、图片→原图、文本框/沙盒HTML/视频→PNG 截图）
async function collectObjectsForExport(options = {}) {
  const quiet = options.quiet === true;
  const objs = [];
  const sourceObjects = [];
  for (let sheet = 0; sheet < S.sheets.length; sheet++) {
    if (sheet === S.sheet) {
      sourceObjects.push(...(S.objects || []));
      continue;
    }
    try {
      const response = await api(`/api/objects?sheet=${sheet}`);
      sourceObjects.push(...(response.objects || []));
    } catch (err) {
      console.warn(`Unable to collect drawing objects from sheet ${sheet}:`, err);
    }
  }
  const htmlTotal = sourceObjects.filter((o) => o.type === 'html').length;
  let htmlDone = 0;
  for (const o of sourceObjects) {
    const item = { id: o.id, type: o.type, mode: o.mode, sheet: o.sheet, r: o.r, c: o.c, x: o.x, y: o.y, w: o.w, h: o.h };
    // Imported DrawingML objects keep their native anchor/relationship identity.  Their DOM
    // representation is only a preview; exporting that preview would flatten editable Excel
    // charts, shapes and SmartArt into a bitmap.
    if (o.config && o.config.nativeDrawing) {
      item.nativeDrawing = o.config.nativeDrawing;
      objs.push(item);
      continue;
    }
    if (o.type === 'svg') item.svg = o.config.svg;
    else if (o.type === 'image') item.png = o.config.src;
    else if (o.type === 'html') {
      if (!quiet) setStatus(`正在渲染网页对象 ${htmlDone + 1}/${htmlTotal}（3× PNG）…`);
      try {
        item.png = await htmlObjectToPng(o, 3);
      } catch (err) {
        // 单个复杂页面、跨域媒体或 Chromium 超时不能拖垮整份工作簿导出。
        console.warn('HTML object capture fell back to placeholder:', err);
        item.png = await objToPng(o);
      }
      htmlDone += 1;
    } else item.png = await objToPng(o);
    objs.push(item);
  }
  return objs;
}
// 导出为指定格式（xlsx / html / udoc / csv）：统一走原生下载流（blob + <a download>），
// 不弹 JS prompt、不依赖 showSaveFilePicker（避免不支持/卡对话框），文件名用当前工作簿名。
async function exportAs(fmt) {
  await prepareWorkbookForPersistence();
  const base = currentBasename();
  const ext = fmt === 'xlsx' ? '.' + (S.excelExtension || 'xlsx') : `.${fmt}`;
  const q = '?name=' + encodeURIComponent(base);
  setStatus('正在导出…');
  let resp;
  let blob;
  if (fmt === 'xlsx') {
    blob = await createWorkbookBlob();
  } else {
    const sheetQuery = fmt === 'csv' ? `&sheet=${encodeURIComponent(S.sheet)}` : '';
    resp = await fetch(`/api/export-${fmt}${q}${sheetQuery}`);
  }
  if (resp && !resp.ok) { setStatus('导出失败'); alert('导出失败：' + resp.status); return; }
  if (!blob) blob = await resp.blob();
  // 原生下载流：创建 a[download] 触发浏览器下载（最可靠，与“打开”的原生文件流对称）
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url; a.download = base + ext;
  document.body.appendChild(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1500);
  const label = fmt === 'xlsx' ? 'Excel' : fmt === 'html' ? '无损 HTML' : fmt === 'csv' ? 'CSV 当前工作表' : 'udoc';
  setStatus(`已导出 ${label}：${base}${ext}`);
}
// 当前工作表的完整实际区域由 Rust 端按真实行高/列宽重建。必须在专用 iframe
// 的 Window 上调用 print()；若在应用主 Window 上调用，Chromium 会把 Ribbon、公式栏、
// 状态栏和滚动条一起缩放进 PDF（这正是不能接受的错误效果）。
async function printCurrentSheetPdf(options = {}) {
  await flushObjectWrites();
  const suppressPrint = options && options.suppressPrint === true;
  const scope = $('print-scope').value;
  const paper = $('print-paper-size').value;
  const orientation = $('print-orientation').value;
  const scaling = $('print-scaling').value;
  if (!suppressPrint) {
    // Chromium 会把隐藏 iframe.contentWindow.print() 归到顶层页面，导致整套 UniCell
    // 外壳进入 PDF。当前标签先进入纯打印文档，再由该文档自己的 window.print() 发起；
    // 保存或取消后，打印文档通过 afterprint + history.back() 返回应用。
    setStatus('正在进入当前工作表打印页面…');
    window.location.assign(`/api/print-html?sheet=${S.sheet}&scope=${encodeURIComponent(scope)}&paper=${encodeURIComponent(paper)}&orientation=${encodeURIComponent(orientation)}&scaling=${encodeURIComponent(scaling)}&autoprint=1&return=1&ts=${Date.now()}`);
    return Promise.resolve(null);
  }
  const oldFrame = document.getElementById('unicell-native-print-frame');
  if (oldFrame) oldFrame.remove();
  const frame = document.createElement('iframe');
  frame.id = 'unicell-native-print-frame';
  frame.title = '当前工作表打印文档';
  frame.setAttribute('aria-hidden', 'true');
  // 不能 display:none，否则部分 Chromium 版本不会排版 iframe；放到视口外仍会完整渲染。
  frame.style.cssText = 'position:fixed;left:-100000px;top:0;width:1px;height:1px;border:0;pointer-events:none;';
  let cleanupTimer = 0;
  const cleanup = () => {
    clearTimeout(cleanupTimer);
    if (frame.isConnected) frame.remove();
  };
  return new Promise((resolve, reject) => {
    frame.onload = async () => {
      try {
        const printWindow = frame.contentWindow;
        const printDocument = frame.contentDocument;
        if (!printWindow || !printDocument || !printDocument.querySelector('main.print-document > .print-page')) {
          throw new Error('打印文档未正确载入');
        }
        if (printDocument.fonts) await printDocument.fonts.ready;
        await Promise.all(Array.from(printDocument.images).map((img) => img.complete
          ? Promise.resolve()
          : new Promise((done) => {
            img.addEventListener('load', done, { once: true });
            img.addEventListener('error', done, { once: true });
          })));
        // 自动化隔离检查只载入纯打印文档，不触发系统打印面板。
        await new Promise((done) => setTimeout(done, 350));
        frame.dataset.printReady = 'true';
        resolve(frame);
      } catch (err) {
        cleanup();
        setStatus('打印失败');
        alert('打印失败：' + (err && err.message || err));
        reject(err);
      }
    };
    frame.onerror = () => {
      const err = new Error('打印文档网络加载失败');
      cleanup();
      setStatus('打印失败');
      reject(err);
    };
    frame.src = `/api/print-html?sheet=${S.sheet}&scope=${encodeURIComponent(scope)}&paper=${encodeURIComponent(paper)}&orientation=${encodeURIComponent(orientation)}&scaling=${encodeURIComponent(scaling)}&autoprint=0&ts=${Date.now()}`;
    document.body.appendChild(frame);
    setStatus('正在生成当前工作表打印区域…');
  });
}
function loadPrintSetting(key, fallback) {
  try { return localStorage.getItem(key) || fallback; } catch (e) { return fallback; }
}
function savePrintSetting(key, value) {
  try { localStorage.setItem(key, value); } catch (e) {}
}
const printPaperSize = $('print-paper-size');
const printScope = $('print-scope');
const printOrientation = $('print-orientation');
const printScaling = $('print-scaling');
const savedPaper = loadPrintSetting('unicell.print.paper', 'actual');
const savedScope = loadPrintSetting('unicell.print.scope', 'sheet');
const savedOrientation = loadPrintSetting('unicell.print.orientation', 'portrait');
const savedScaling = loadPrintSetting('unicell.print.scaling', 'none');
if (Array.from(printPaperSize.options).some((o) => o.value === savedPaper)) printPaperSize.value = savedPaper;
if (Array.from(printScope.options).some((o) => o.value === savedScope)) printScope.value = savedScope;
if (Array.from(printOrientation.options).some((o) => o.value === savedOrientation)) printOrientation.value = savedOrientation;
if (Array.from(printScaling.options).some((o) => o.value === savedScaling)) printScaling.value = savedScaling;
function syncPrintPageControls() {
  const actual = printPaperSize.value === 'actual';
  printOrientation.disabled = actual;
  printScaling.disabled = actual;
}
printPaperSize.addEventListener('change', () => {
  savePrintSetting('unicell.print.paper', printPaperSize.value);
  syncPrintPageControls();
});
printScope.addEventListener('change', () => savePrintSetting('unicell.print.scope', printScope.value));
printOrientation.addEventListener('change', () => savePrintSetting('unicell.print.orientation', printOrientation.value));
printScaling.addEventListener('change', () => savePrintSetting('unicell.print.scaling', printScaling.value));
syncPrintPageControls();
function handlePrintShortcut(e) {
  if (!(e.ctrlKey || e.metaKey) || String(e.key).toLowerCase() !== 'p') return;
  e.preventDefault();
  e.stopImmediatePropagation();
  if (e.repeat) return;
  const options = e.__unicellSuppressPrint === true ? { suppressPrint: true } : undefined;
  void printCurrentSheetPdf(options);
}
// 捕获阶段接管 Ctrl+P，确保编辑器、公式栏、对象文本框获得焦点时也走纯工作表打印页。
window.addEventListener('keydown', handlePrintShortcut, true);
// 「文件」选项卡按钮（放在“开始”前，对齐母项目 unidoc）
$('btn-file-new').onclick = newWorkbook;
$('btn-file-open').onclick = openLocalWorkbook;
$('btn-file-save').onclick = saveWorkbook;
$('btn-file-save-as').onclick = saveWorkbookAs;
$('btn-file-recent').onclick = toggleRecentFilesMenu;
$('btn-file-recover').onclick = restoreAutosaveSnapshot;
$('btn-file-exp-xlsx').onclick = () => exportAs('xlsx');
$('btn-file-exp-csv').onclick = () => exportAs('csv');
$('btn-file-exp-html').onclick = () => exportAs('html');
$('btn-file-exp-udoc').onclick = () => exportAs('udoc');
$('btn-file-print-pdf').onclick = printCurrentSheetPdf;
// 导出 xlsx：把插入对象以 drawing 嵌入（SVG→EMF、图片→原图、文本框/沙盒HTML/视频→PNG 截图）
async function exportXlsx() {
  await exportAs('xlsx');
}
// 将文本框/沙盒HTML/视频对象截图为高 DPI PNG DataURL（scale 倍率位图，嵌入 Excel 保持清晰）
async function objToPng(o, scale = 3) {
  const w = Math.max(1, Math.round(o.w)), h = Math.max(1, Math.round(o.h));
  const cv = document.createElement('canvas');
  cv.width = w * scale; cv.height = h * scale;
  const ctx = cv.getContext('2d');
  ctx.scale(scale, scale); // 以逻辑坐标绘制，输出高分辨率位图
  ctx.imageSmoothingEnabled = true; ctx.imageSmoothingQuality = 'high';
  ctx.fillStyle = '#fff'; ctx.fillRect(0, 0, w, h);
  ctx.strokeStyle = '#c3c9d2'; ctx.strokeRect(0.5, 0.5, w - 1, h - 1);
  if (o.type === 'text') {
    ctx.fillStyle = '#222'; ctx.font = '13px sans-serif';
    const txt = (o.config.html || '').replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
    wrapCanvasText(ctx, txt, 8, 18, w - 16, 16, h - 4);
  } else if (o.type === 'video') {
    const v = document.querySelector(`.cell-obj[data-id="${o.id}"] video`);
    if (v) { try { ctx.drawImage(v, 0, 0, w, h); } catch (e) {} }
    ctx.fillStyle = 'rgba(0,0,0,.45)';
    ctx.beginPath(); ctx.moveTo(w / 2 - 10, h / 2 - 14);
    ctx.lineTo(w / 2 + 12, h / 2); ctx.lineTo(w / 2 - 10, h / 2 + 14); ctx.fill();
  } else if (o.type === 'html') {
    ctx.fillStyle = '#217346'; ctx.font = 'bold 13px sans-serif';
    ctx.fillText('沙盒 HTML', 8, 20);
    ctx.fillStyle = '#888'; ctx.font = '11px sans-serif';
    ctx.fillText('交互内容需在 UniCell 中查看', 8, 38);
  }
  return cv.toDataURL('image/png');
}

// 运行在 sandbox iframe 内的只读快照桥。它只响应父页面的一次性请求，不授予
// allow-same-origin，因此用户 HTML 仍不能读取 UniCell 或本机文件。
function sandboxSnapshotBridgeRuntime(objectId) {
  const responseType = 'unicell-html-snapshot-response';
  function copyAttributes(from, to) {
    for (const attr of Array.from(from.attributes || [])) to.setAttribute(attr.name, attr.value);
  }
  function snapshotDocument() {
    const sourceRoot = document.documentElement;
    if (!sourceRoot) return '';
    const cloneRoot = sourceRoot.cloneNode(true);
    const sourceControls = Array.from(sourceRoot.querySelectorAll('input,textarea,select,option,details'));
    const cloneControls = Array.from(cloneRoot.querySelectorAll('input,textarea,select,option,details'));
    sourceControls.forEach((source, index) => {
      const clone = cloneControls[index];
      if (!clone) return;
      const tag = source.tagName;
      if (tag === 'INPUT') {
        if (source.type !== 'file') clone.setAttribute('value', source.value || '');
        if (source.checked) clone.setAttribute('checked', ''); else clone.removeAttribute('checked');
      } else if (tag === 'TEXTAREA') {
        clone.textContent = source.value || '';
      } else if (tag === 'OPTION') {
        if (source.selected) clone.setAttribute('selected', ''); else clone.removeAttribute('selected');
      } else if (tag === 'DETAILS') {
        if (source.open) clone.setAttribute('open', ''); else clone.removeAttribute('open');
      }
    });
    // Canvas/动态图表必须先转成像素，否则克隆 DOM 后 canvas 会变成空白。
    const sourceCanvases = Array.from(sourceRoot.querySelectorAll('canvas'));
    const cloneCanvases = Array.from(cloneRoot.querySelectorAll('canvas'));
    sourceCanvases.forEach((source, index) => {
      const clone = cloneCanvases[index];
      if (!clone) return;
      try {
        const image = document.createElement('img');
        copyAttributes(clone, image);
        image.src = source.toDataURL('image/png');
        image.width = source.width;
        image.height = source.height;
        clone.replaceWith(image);
      } catch (e) { /* cross-origin tainted canvas: leave original element as fallback */ }
    });
    // 对已经解码的视频尽量固化当前帧；跨域视频失败时保留 video 标签让 Chromium 重载。
    const sourceVideos = Array.from(sourceRoot.querySelectorAll('video'));
    const cloneVideos = Array.from(cloneRoot.querySelectorAll('video'));
    sourceVideos.forEach((source, index) => {
      const clone = cloneVideos[index];
      if (!clone || !source.videoWidth || !source.videoHeight) return;
      try {
        const canvas = document.createElement('canvas');
        canvas.width = source.videoWidth; canvas.height = source.videoHeight;
        canvas.getContext('2d').drawImage(source, 0, 0);
        const image = document.createElement('img');
        copyAttributes(clone, image);
        image.src = canvas.toDataURL('image/png');
        clone.replaceWith(image);
      } catch (e) {}
    });
    // 保存页面及内部滚动容器的位置；重放脚本只设置位置，不执行用户脚本。
    const sourceElements = [sourceRoot, ...Array.from(sourceRoot.querySelectorAll('*'))];
    const cloneElements = [cloneRoot, ...Array.from(cloneRoot.querySelectorAll('*'))];
    sourceElements.forEach((source, index) => {
      if (!source.scrollLeft && !source.scrollTop) return;
      const clone = cloneElements[index];
      if (clone) clone.setAttribute('data-unicell-scroll', `${source.scrollLeft},${source.scrollTop}`);
    });
    cloneRoot.querySelectorAll('script,base').forEach((node) => node.remove());
    cloneRoot.querySelectorAll('meta[http-equiv]').forEach((node) => {
      if ((node.getAttribute('http-equiv') || '').toLowerCase() === 'content-security-policy') node.remove();
    });
    let head = cloneRoot.querySelector('head');
    if (!head) {
      head = document.createElement('head');
      cloneRoot.insertBefore(head, cloneRoot.firstChild);
    }
    const base = document.createElement('base');
    base.href = document.baseURI;
    head.insertBefore(base, head.firstChild);
    const freeze = document.createElement('style');
    freeze.textContent = '*,*::before,*::after{animation-play-state:paused!important;transition:none!important;caret-color:transparent!important}html{scroll-behavior:auto!important}';
    head.appendChild(freeze);
    let body = cloneRoot.querySelector('body');
    if (!body) { body = document.createElement('body'); cloneRoot.appendChild(body); }
    const restore = document.createElement('script');
    restore.textContent = `addEventListener('load',()=>{scrollTo(${window.scrollX},${window.scrollY});document.querySelectorAll('[data-unicell-scroll]').forEach((el)=>{const p=el.getAttribute('data-unicell-scroll').split(',');el.scrollLeft=+p[0]||0;el.scrollTop=+p[1]||0;});});`;
    body.appendChild(restore);
    const doctype = document.doctype
      ? `<!DOCTYPE ${document.doctype.name || 'html'}>`
      : '<!doctype html>';
    return doctype + cloneRoot.outerHTML;
  }
  addEventListener('message', (event) => {
    const data = event.data || {};
    if (data.type !== 'unicell-html-snapshot-request' || data.objectId !== objectId) return;
    let html = '';
    try { html = snapshotDocument(); } catch (e) {}
    parent.postMessage({ type: responseType, objectId, requestId: data.requestId, html }, '*');
  });
  parent.postMessage({ type: 'unicell-html-snapshot-ready', objectId }, '*');
}

function htmlWithSnapshotBridge(html, objectId) {
  const bridge = `;(${sandboxSnapshotBridgeRuntime.toString()})(${JSON.stringify(String(objectId))});`;
  const source = String(html || DEFAULT_HTML);
  const script = `\n<script>${bridge}<\/script>`;
  const lower = source.toLowerCase();
  const bodyClose = lower.lastIndexOf('</body>');
  if (bodyClose >= 0) return source.slice(0, bodyClose) + script + source.slice(bodyClose);
  const htmlClose = lower.lastIndexOf('</html>');
  if (htmlClose >= 0) return source.slice(0, htmlClose) + script + source.slice(htmlClose);
  return source + script;
}

function htmlObjectIframe(o) {
  const frames = Array.from(document.querySelectorAll('iframe[data-unicell-object-id]'));
  return frames.find((frame) => frame.dataset.unicellObjectId === String(o.id) && frame.closest('#obj-fs'))
    || frames.find((frame) => frame.dataset.unicellObjectId === String(o.id))
    || null;
}

function requestHtmlObjectSnapshot(o, timeout = 2500) {
  const frame = htmlObjectIframe(o);
  const target = frame && frame.contentWindow;
  if (!target) return Promise.resolve(null);
  const requestId = `snap-${Date.now()}-${Math.random().toString(36).slice(2)}`;
  return new Promise((resolve) => {
    let timer = 0, retryTimer = 0;
    const finish = (value) => {
      clearTimeout(timer);
      clearInterval(retryTimer);
      window.removeEventListener('message', onMessage);
      resolve(value || null);
    };
    const onMessage = (event) => {
      const data = event.data || {};
      if (event.source !== target || data.type !== 'unicell-html-snapshot-response'
          || data.objectId !== String(o.id) || data.requestId !== requestId) return;
      finish(typeof data.html === 'string' && data.html ? data.html : null);
    };
    window.addEventListener('message', onMessage);
    timer = setTimeout(() => finish(null), timeout);
    const request = () => {
      // A sheet/object rerender can replace this iframe while the snapshot is
      // pending. Stop instead of repeatedly posting to a detached/crashed frame.
      if (!frame.isConnected || frame.contentWindow !== target) return finish(null);
      try {
        target.postMessage({ type: 'unicell-html-snapshot-request', objectId: String(o.id), requestId }, '*');
      } catch (e) {
        finish(null);
      }
    };
    request();
    // 首次冷加载时 iframe 的脚本可能尚未安装监听器；短间隔重发避免一次消息丢失后
    // 整个导出退回占位图，收到匹配响应即立即停止。
    retryTimer = setInterval(request, 100);
  });
}

function rememberHtmlObjectSnapshot(o, html) {
  if (!html) return;
  // 仅用于当前浏览器会话的导出，不序列化进工作簿对象 JSON，避免把巨大的 Canvas
  // DataURL 写入服务端模型。
  Object.defineProperty(o, '_lastSnapshotHtml', {
    value: html, writable: true, configurable: true, enumerable: false,
  });
}

function blobToDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(reader.error || new Error('读取 PNG 失败'));
    reader.readAsDataURL(blob);
  });
}

const htmlPngExportCache = new Map();
let htmlPngExportCacheBytes = 0;
async function htmlPngCacheKey(html, width, height, scale) {
  const bytes = new TextEncoder().encode(html);
  if (globalThis.crypto && globalThis.crypto.subtle) {
    const digest = new Uint8Array(await globalThis.crypto.subtle.digest('SHA-256', bytes));
    return `${width}x${height}@${scale}:` + Array.from(digest, (v) => v.toString(16).padStart(2, '0')).join('');
  }
  // localhost Chromium always exposes SubtleCrypto；这里只为非安全上下文保留确定性回退。
  let hash = 2166136261;
  for (const value of bytes) { hash ^= value; hash = Math.imul(hash, 16777619); }
  return `${width}x${height}@${scale}:${bytes.length}:${hash >>> 0}`;
}

function cacheHtmlObjectPng(cacheKey, dataUrl) {
  if (!cacheKey || dataUrl.length > 12 * 1024 * 1024) return;
  while (htmlPngExportCache.size && (htmlPngExportCache.size >= 8
      || htmlPngExportCacheBytes + dataUrl.length > 32 * 1024 * 1024)) {
    const oldestKey = htmlPngExportCache.keys().next().value;
    htmlPngExportCacheBytes -= htmlPngExportCache.get(oldestKey).length;
    htmlPngExportCache.delete(oldestKey);
  }
  htmlPngExportCache.set(cacheKey, dataUrl);
  htmlPngExportCacheBytes += dataUrl.length;
}

async function fetchHtmlCaptureBlob(requestBody, signal) {
  let lastError = null;
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const response = await fetch('/api/render-html-png', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: requestBody,
        signal,
      });
      if (!response.ok) {
        let reason = `HTTP ${response.status}`;
        try { reason = (await response.json()).error || reason; } catch (e) {}
        const error = new Error(reason);
        error.htmlCaptureTransient = reason.startsWith('Chromium capture failed')
          || reason.startsWith('start Chromium capture:')
          || reason.startsWith('wait for Chromium capture:');
        throw error;
      }
      return await response.blob();
    } catch (error) {
      lastError = error;
      const aborted = signal.aborted || error?.name === 'AbortError';
      const transient = error?.htmlCaptureTransient || error instanceof TypeError;
      if (aborted || !transient || attempt === 1) throw error;
      // This is a single bounded recovery for a broken local HTTP connection or
      // a Chromium process restart. It yields to the WebView between attempts.
      await new Promise((resolve) => setTimeout(resolve, 125));
    }
  }
  throw lastError || new Error('Chromium capture failed');
}

async function htmlObjectToPng(o, scale = 3) {
  const snapshot = await requestHtmlObjectSnapshot(o);
  if (snapshot) rememberHtmlObjectSnapshot(o, snapshot);
  const rememberedSnapshot = o._lastSnapshotHtml || '';
  const liveHtml = snapshot || rememberedSnapshot || o.config.code || DEFAULT_HTML;
  const width = Math.max(1, Math.round(o.w));
  const height = Math.max(1, Math.round(o.h));
  const captureScale = Math.max(1, Math.min(4, Math.round(scale)));
  // 非当前工作表没有活动 iframe，其保存源码就是确定输入，可以安全缓存；当前表仅缓存
  // 已拿到实时快照的对象，避免 iframe 暂时不可用时错误复用旧的动态画面。
  const cacheable = !!snapshot || !!rememberedSnapshot || Number(o.sheet) !== Number(S.sheet);
  const cacheKey = cacheable ? await htmlPngCacheKey(liveHtml, width, height, captureScale) : '';
  if (cacheKey && htmlPngExportCache.has(cacheKey)) {
    const cached = htmlPngExportCache.get(cacheKey);
    // 读取即提升到 LRU 尾部。
    htmlPngExportCache.delete(cacheKey);
    htmlPngExportCache.set(cacheKey, cached);
    return cached;
  }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 15000);
  try {
    const blob = await fetchHtmlCaptureBlob(JSON.stringify({
      html: liveHtml,
      width,
      height,
      scale: captureScale,
    }), controller.signal);
    if (!blob.size || (blob.type && blob.type !== 'image/png')) throw new Error('Chromium 未返回 PNG');
    const dataUrl = await blobToDataUrl(blob);
    cacheHtmlObjectPng(cacheKey, dataUrl);
    return dataUrl;
  } finally {
    clearTimeout(timer);
  }
}
function wrapCanvasText(ctx, text, x, y, maxW, lh, maxY) {
  const limit = maxY || (ctx.canvas.height - 4);
  let line = '', yy = y;
  for (const ch of text) {
    if (ctx.measureText(line + ch).width > maxW || ch === '\n') { ctx.fillText(line, x, yy); line = ch === '\n' ? '' : ch; yy += lh; if (yy > limit) return; }
    else line += ch;
  }
  if (line) ctx.fillText(line, x, yy);
}

/* ================= Sheet 标签 ================= */
function renderSheetTabs() {
  const box = $('sheet-tabs');
  box.textContent = '';
  S.sheets.forEach((name, i) => {
    const t = document.createElement('div');
    t.className = 'sheet-tab' + (i === S.sheet ? ' active' : '');
    t.textContent = name;
    t.onclick = () => { void switchSheet(i); };
    t.ondblclick = () => startRenameSheet(t, i);
    t.oncontextmenu = (e) => {
      e.preventDefault();
      showCtxMenu(e.clientX, e.clientY, [
        { label: '重命名', fn: () => startRenameSheet(t, i) },
        { label: '复制工作表', fn: async () => { await apiPost('/api/sheet', { op: 'duplicate', sheet: i }).then(updateSheets); } },
        { label: '删除', fn: async () => {
            if (S.sheets.length <= 1) { setStatus('至少保留一个工作表'); return; }
            if (!confirm(`删除工作表 "${name}"？`)) return;
            await flushObjectWrites();
            await apiPost('/api/sheet', { op: 'delete', sheet: i });
            window.clearObjectHistory?.();
            if (S.sheet >= S.sheets.length - 1) S.sheet = Math.max(0, S.sheets.length - 2);
            await updateSheets(); await switchSheet(Math.min(S.sheet, S.sheets.length - 1));
          } },
      ]);
    };
    box.appendChild(t);
  });
}
function startRenameSheet(tab, i) {
  const input = document.createElement('input');
  input.value = S.sheets[i];
  tab.textContent = '';
  tab.appendChild(input);
  input.focus(); input.select();
  const done = async (commit) => {
    if (commit && input.value.trim() && input.value !== S.sheets[i]) {
      await apiPost('/api/sheet', { op: 'rename', sheet: i, name: input.value.trim() });
    }
    await updateSheets();
  };
  input.onkeydown = (e) => {
    if (e.key === 'Enter') done(true);
    else if (e.key === 'Escape') done(false);
  };
  input.onblur = () => done(true);
}
async function updateSheets() {
  const j = await api('/api/info');
  S.sheets = j.sheets;
  renderSheetTabs();
}
let sheetSwitchSeq = 0;
async function switchSheet(i) {
  const seq = ++sheetSwitchSeq;
  await flushObjectWrites();
  // Resolve the target sheet's complete merge map before exposing it.  Otherwise a fast click
  // after switching can be canonicalized with the previous sheet's ranges (or no ranges at all).
  const bootstrap = await api(`/api/view?sheet=${i}&r0=1&c0=1&r1=2&c1=2`);
  if (seq !== sheetSwitchSeq) return;
  S.sheet = i;
  S.cellsCache.clear(); S.colW.clear(); S.rowH.clear();
  S.merges = bootstrap.merges || [];
  if (bootstrap.colWidths.length) S.defW = bootstrap.colWidths[0];
  if (bootstrap.rowHeights.length) S.defH = bootstrap.rowHeights[0];
  S.maxUsedR = 1; S.maxUsedC = 1;
  S.objects = [];
  renderObjects();
  setCursor(1, 1);
  renderSheetTabs();
  await Promise.all([fitVirtualToData(), loadObjects()]);
  scheduleRefresh(true);
}
// 滚动范围覆盖实际数据：虚拟区行列数 = max(30, 当前表行列数 + 缓冲)
// 让右侧滚动条可直接拉到数据末尾（如 10000 行），无需逐步扩展
async function fitVirtualToData() {
  try {
    const d = await api(`/api/dimension?sheet=${S.sheet}`);
    S.vRows = Math.max(30, (d.maxRow || 1) + 50);
    S.vCols = Math.max(30, (d.maxCol || 1) + 10);
  } catch {}
  updateSpacer();
}
$('btn-addsheet').onclick = async () => {
  await apiPost('/api/sheet', { op: 'new' });
  await updateSheets();
  await switchSheet(S.sheets.length - 1);
};

/* ================= 右键菜单 ================= */
const ctxMenu = $('ctx-menu');
function showCtxMenu(x, y, items) {
  ctxMenu.textContent = '';
  for (const it of items) {
    if (it === 'sep') {
      const d = document.createElement('div'); d.className = 'sep'; ctxMenu.appendChild(d); continue;
    }
    const d = document.createElement('div');
    d.className = 'mi';
    d.innerHTML = `<span>${it.label}</span>` + (it.hint ? `<span class="hint">${it.hint}</span>` : '');
    d.onclick = () => { hideCtxMenu(); it.fn(); };
    ctxMenu.appendChild(d);
  }
  ctxMenu.hidden = false;
  const mw = ctxMenu.offsetWidth, mh = ctxMenu.offsetHeight;
  ctxMenu.style.left = Math.min(x, innerWidth - mw - 4) + 'px';
  ctxMenu.style.top = Math.min(y, innerHeight - mh - 4) + 'px';
}
function hideCtxMenu() { ctxMenu.hidden = true; }
document.addEventListener('mousedown', (e) => { if (!ctxMenu.contains(e.target)) hideCtxMenu(); });
// 全局屏蔽浏览器原生右键菜单（除输入框，保留文本粘贴体验）
document.addEventListener('contextmenu', (e) => {
  const t = e.target;
  if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
  e.preventDefault();
});

gridScroll.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  e.stopPropagation();
  const pt = evtCell(e);
  if (pt) {
    const n = normSel();
    if (pt.r < n.r0 || pt.r > n.r1 || pt.c < n.c0 || pt.c > n.c1) {
      setCursor(pt.r, pt.c, false, !!e.__unicellFrozenCell);
    }
  }
  const n = normSel();
  const nRows = n.r1 - n.r0 + 1, nCols = n.c1 - n.c0 + 1;
  const merged = (S.merges || []).some((m) => m.r0 === n.r0 && m.c0 === n.c0 && m.r1 === n.r1 && m.c1 === n.c1);
  showCtxMenu(e.clientX, e.clientY, [
    { label: '剪切', hint: 'Ctrl+X', fn: () => doCopy(true) },
    { label: '复制', hint: 'Ctrl+C', fn: () => doCopy(false) },
    { label: '粘贴', hint: 'Ctrl+V', fn: pasteFromClipboard },
    { label: '选择性粘贴…', hint: 'Ctrl+Alt+V', fn: openPasteSpecialDialog },
    'sep',
    { label: `插入 ${nRows} 行（上方）`, fn: () => rowColOp('/api/rows', 'insert', n.r0, nRows) },
    { label: `删除 ${nRows} 行`, fn: () => rowColOp('/api/rows', 'delete', n.r0, nRows) },
    { label: `插入 ${nCols} 列（左侧）`, fn: () => colOp('insert', n.c0, nCols) },
    { label: `删除 ${nCols} 列`, fn: () => colOp('delete', n.c0, nCols) },
    'sep',
    { label: merged ? '取消合并单元格' : '合并单元格', fn: () => $('btn-merge').onclick() },
    { label: '设置单元格格式…', fn: openCellFormatDialog },
    'sep',
    { label: '清除内容', hint: 'Del', fn: async () => { await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'contents' }); scheduleRefresh(true); } },
    { label: '清除格式', fn: async () => { await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'formatting' }); scheduleRefresh(true); } },
    { label: '全部清除', fn: async () => { await apiPost('/api/clear', { sheet: S.sheet, ...n, what: 'all' }); scheduleRefresh(true); } },
  ]);
});
async function rowColOp(url, op, row, count) {
  await apiPost(url, { sheet: S.sheet, op, row, count });
  S.rowH.clear();
  adjustObjectsForRowCol('row', row, count, op === 'delete');
  scheduleRefresh(true);
}
async function colOp(op, col, count) {
  await apiPost('/api/cols', { sheet: S.sheet, op, col, count });
  S.colW.clear();
  adjustObjectsForRowCol('col', col, count, op === 'delete');
  scheduleRefresh(true);
}
// 行列增删时让锚定单元格的对象跟随锚定行/列移动（Excel 嵌入单元格语义）
function adjustObjectsForRowCol(kind, index, count, isDelete) {
  let changed = false;
  for (const o of S.objects || []) {
    if (o.mode !== 'cell') continue;
    const key = kind === 'row' ? 'r' : 'c';
    if (isDelete) { if (o[key] > index) { o[key] = Math.max(1, o[key] - count); changed = true; } }
    else if (o[key] >= index) { o[key] += count; changed = true; }
  }
  if (changed) renderObjects();
}
async function pasteFromClipboard() {
  return pasteRichFromClipboard('all');
}

/* ================= 查找替换（Ctrl+F / Ctrl+H，Excel 语义） ================= */
let findState = { matches: [], idx: -1, lastQuery: '' };
function openFindDialog(withReplace) {
  let dlg = $('find-dialog');
  if (!dlg) {
    dlg = document.createElement('div');
    dlg.id = 'find-dialog';
    dlg.innerHTML = `
      <div class="fd-title"><span id="fd-caption">查找</span><button id="fd-close">×</button></div>
      <div class="fd-row"><label>查找内容</label><input id="fd-find" spellcheck="false"></div>
      <div class="fd-row" id="fd-replace-row"><label>替换为</label><input id="fd-replace" spellcheck="false"></div>
      <div class="fd-row fd-opts">
        <label><input type="checkbox" id="fd-case">区分大小写</label>
        <label><input type="checkbox" id="fd-whole">单元格匹配</label>
      </div>
      <div class="fd-row fd-btns">
        <span id="fd-info"></span>
        <button id="fd-replace-one">替换</button>
        <button id="fd-replace-all">全部替换</button>
        <button id="fd-next" class="primary">查找下一个</button>
      </div>`;
    document.body.appendChild(dlg);
    $('fd-close').onclick = () => { dlg.hidden = true; gridScroll.focus(); };
    $('fd-next').onclick = findNext;
    $('fd-replace-one').onclick = replaceOne;
    $('fd-replace-all').onclick = replaceAll;
    dlg.addEventListener('keydown', (e) => {
      e.stopPropagation();
      if (e.key === 'Enter') { e.preventDefault(); findNext(); }
      else if (e.key === 'Escape') { dlg.hidden = true; gridScroll.focus(); }
    });
    $('fd-find').addEventListener('input', () => { findState.lastQuery = ''; $('fd-info').textContent = ''; });
  }
  $('fd-replace-row').style.display = withReplace ? '' : 'none';
  $('fd-replace-one').style.display = withReplace ? '' : 'none';
  $('fd-replace-all').style.display = withReplace ? '' : 'none';
  $('fd-caption').textContent = withReplace ? '替换' : '查找';
  dlg.hidden = false;
  $('fd-find').focus();
  $('fd-find').select();
}
async function ensureMatches() {
  const text = $('fd-find').value;
  if (!text) return false;
  const key = JSON.stringify([text, $('fd-case').checked, $('fd-whole').checked, S.sheet]);
  if (findState.lastQuery !== key) {
    const j = await apiPost('/api/find', {
      sheet: S.sheet, text,
      matchCase: $('fd-case').checked, wholeCell: $('fd-whole').checked,
    });
    findState = { matches: j.matches, idx: -1, lastQuery: key };
  }
  if (!findState.matches.length) {
    $('fd-info').textContent = '找不到匹配项';
    return false;
  }
  return true;
}
async function findNext() {
  if (!(await ensureMatches())) return;
  const m = findState.matches;
  // Excel 语义：从当前光标之后的第一个匹配开始，循环推进
  if (findState.idx < 0) {
    findState.idx = m.findIndex((x) => x.r > S.cur.r || (x.r === S.cur.r && x.c > S.cur.c));
    if (findState.idx < 0) findState.idx = 0;
  } else {
    findState.idx = (findState.idx + 1) % m.length;
  }
  const hit = m[findState.idx];
  setCursor(hit.r, hit.c);
  scheduleRefresh(true);
  $('fd-info').textContent = `第 ${findState.idx + 1}/${m.length} 个`;
}
async function replaceOne() {
  if (!(await ensureMatches())) return;
  if (findState.idx < 0) { await findNext(); return; }
  const hit = findState.matches[findState.idx];
  const j = await apiPost('/api/replace', {
    sheet: S.sheet, find: $('fd-find').value, replace: $('fd-replace').value,
    matchCase: $('fd-case').checked, all: false, row: hit.r, col: hit.c,
  });
  findState.lastQuery = ''; // 失效缓存
  scheduleRefresh(true);
  if (j.replaced > 0) await findNext();
}
async function replaceAll() {
  const text = $('fd-find').value;
  if (!text) return;
  const j = await apiPost('/api/replace', {
    sheet: S.sheet, find: text, replace: $('fd-replace').value,
    matchCase: $('fd-case').checked, all: true,
  });
  findState.lastQuery = '';
  $('fd-info').textContent = `已替换 ${j.replaced} 处`;
  scheduleRefresh(true);
}

/* ================= 滚动 ================= */
gridScroll.addEventListener('scroll', () => {
  colHdrInner.style.transform = `translateX(${-gridScroll.scrollLeft}px)`;
  rowHdrInner.style.transform = `translateY(${-gridScroll.scrollTop}px)`;
  syncFreezeScroll();
  // Keep an editor opened from a frozen copy attached to that copy while the
  // user scrolls the other axis.  The editor nodes live inside #grid-scroll,
  // so their content coordinates must be recomputed after every scroll.
  if (S.editing && S.editFrozen) {
    const editRect = activeEditRect();
    if (S.editHasRichText) positionRichCellEditor(editRect);
    else positionPlainCellEditor(editRect);
  }
  // 冻结区内的对象必须与冻结单元格同步抵消滚动。若只等待异步视口刷新，
  // Excel 导入图片会在滚动过程中产生明显的延迟和晃动。
  updateObjPositions();
  // 接近底部/右侧时扩展虚拟区
  const nearBottom = gridScroll.scrollTop + gridScroll.clientHeight > rowY(S.vRows - 30);
  const nearRight = gridScroll.scrollLeft + gridScroll.clientWidth > colX(S.vCols - 8);
  if (nearBottom) { S.vRows += 200; updateSpacer(); }
  if (nearRight) { S.vCols += 20; updateSpacer(); }
  scheduleRefresh();
});

/* ================= 公式板块（Excel 公式选项卡） ================= */
// 函数库分类（只显示引擎实际支持的函数）
const FN_GROUPS = {
  '常用': ['SUM', 'AVERAGE', 'COUNT', 'COUNTA', 'MAX', 'MIN', 'IF', 'SUMIF', 'COUNTIF', 'VLOOKUP', 'ROUND', 'TODAY'],
  '财务': ['FV', 'PV', 'PMT', 'PPMT', 'IPMT', 'NPV', 'IRR', 'XIRR', 'RATE', 'NPER', 'SLN', 'SYD', 'DB', 'DDB', 'EFFECT', 'NOMINAL'],
  '逻辑': ['AND', 'OR', 'NOT', 'XOR', 'IF', 'IFS', 'IFERROR', 'IFNA', 'SWITCH', 'TRUE', 'FALSE'],
  '文本': ['LEFT', 'RIGHT', 'MID', 'LEN', 'CONCAT', 'CONCATENATE', 'TEXTJOIN', 'TEXT', 'UPPER', 'LOWER', 'PROPER', 'TRIM', 'SUBSTITUTE', 'REPLACE', 'FIND', 'SEARCH', 'REPT', 'EXACT', 'VALUE', 'CHAR', 'CODE', 'UNICHAR', 'UNICODE'],
  '日期时间': ['DATE', 'DATEVALUE', 'TODAY', 'NOW', 'YEAR', 'MONTH', 'DAY', 'HOUR', 'MINUTE', 'SECOND', 'WEEKDAY', 'WEEKNUM', 'EDATE', 'EOMONTH', 'DAYS', 'DAYS360', 'DATEDIF', 'NETWORKDAYS', 'WORKDAY', 'TIME', 'TIMEVALUE'],
  '查找引用': ['VLOOKUP', 'HLOOKUP', 'XLOOKUP', 'LOOKUP', 'INDEX', 'MATCH', 'XMATCH', 'CHOOSE', 'OFFSET', 'INDIRECT', 'ROW', 'COLUMN', 'ROWS', 'COLUMNS', 'TRANSPOSE', 'FORMULATEXT', 'SHEET', 'SHEETS'],
  '数学三角': ['ABS', 'SIGN', 'ROUND', 'ROUNDUP', 'ROUNDDOWN', 'MROUND', 'CEILING', 'FLOOR', 'INT', 'TRUNC', 'MOD', 'POWER', 'SQRT', 'EXP', 'LN', 'LOG', 'LOG10', 'SUM', 'SUMIF', 'SUMIFS', 'SUMPRODUCT', 'SUMSQ', 'PRODUCT', 'QUOTIENT', 'GCD', 'LCM', 'FACT', 'RAND', 'RANDBETWEEN', 'PI', 'SIN', 'COS', 'TAN', 'DEGREES', 'RADIANS', 'SUBTOTAL'],
  '统计': ['AVERAGE', 'AVERAGEA', 'AVERAGEIF', 'AVERAGEIFS', 'MEDIAN', 'STDEV', 'STDEVP', 'VAR', 'VARP', 'MAX', 'MAXA', 'MIN', 'MINA', 'MAXIFS', 'MINIFS', 'COUNT', 'COUNTA', 'COUNTBLANK', 'COUNTIF', 'COUNTIFS', 'LARGE', 'SMALL', 'CORREL', 'SLOPE', 'INTERCEPT'],
};
// 插入函数到当前单元格（编辑中则在光标处）
function insertFunction(fn) {
  if (S.editing) {
    const caret = editor.selectionStart ?? editor.value.length;
    editor.value = editor.value.slice(0, caret) + fn + '(' + editor.value.slice(caret);
    const pos = caret + fn.length + 1;
    editor.setSelectionRange(pos, pos);
    formulaInput.value = editor.value;
    fnHintUpdate();
    editor.focus();
  } else {
    startEdit('=' + fn + '(');
    editor.setSelectionRange(editor.value.length, editor.value.length);
  }
}
// 函数库分类下拉 → 弹出函数菜单
$('fr-lib').onchange = (e) => {
  const grp = e.target.value;
  e.target.value = '';
  if (!grp || !FN_GROUPS[grp]) return;
  const fns = FN_GROUPS[grp].filter((f) => typeof FN_LIST === 'undefined' || FN_LIST.includes(f));
  const r = $('fr-lib').getBoundingClientRect();
  showCtxMenu(r.left, r.bottom + 2, fns.map((fn) => ({ label: fn, fn: () => insertFunction(fn) })));
};
// 插入函数对话框（搜索 + 全量函数）
function openInsertFnDialog() {
  let dlg = $('fnx-dialog');
  if (dlg) dlg.remove();
  dlg = document.createElement('div');
  dlg.id = 'fnx-dialog';
  dlg.innerHTML = `
    <div class="fd-title"><span>插入函数</span><button id="fnx-close">×</button></div>
    <div class="fd-row"><input id="fnx-search" placeholder="搜索函数（如 SUM）" spellcheck="false"></div>
    <div class="fd-row"><div id="fnx-list"></div></div>`;
  document.body.appendChild(dlg);
  const list = dlg.querySelector('#fnx-list');
  const search = dlg.querySelector('#fnx-search');
  const render = () => {
    const q = search.value.trim().toUpperCase();
    const all = (typeof FN_LIST !== 'undefined' ? FN_LIST : []);
    const fns = (q ? all.filter((f) => f.includes(q)) : all).slice(0, 200);
    list.textContent = '';
    for (const fn of fns) {
      const d = document.createElement('div');
      d.className = 'fnx-item';
      d.textContent = fn;
      d.ondblclick = () => { insertFunction(fn); dlg.remove(); };
      d.onclick = () => { dlg.querySelectorAll('.fnx-item').forEach((x) => x.classList.remove('on')); d.classList.add('on'); dlg.dataset.pick = fn; };
      list.appendChild(d);
    }
    if (!fns.length) list.innerHTML = '<div class="fnx-empty">无匹配函数</div>';
  };
  search.oninput = render;
  search.onkeydown = (e) => {
    e.stopPropagation();
    if (e.key === 'Enter' && dlg.dataset.pick) { insertFunction(dlg.dataset.pick); dlg.remove(); }
    else if (e.key === 'Escape') dlg.remove();
  };
  dlg.querySelector('#fnx-close').onclick = () => dlg.remove();
  render();
  search.focus();
}
$('fr-fx').onclick = openInsertFnDialog;
$('fr-autosum').onclick = () => autoSum();
// 显示公式 toggle（公式栏按钮与状态栏视图切换统一）
function setShowFormulas(on) {
  S.showFormulas = !!on;
  $('fr-showformulas').classList.toggle('on', S.showFormulas);
  $('st-view-normal').classList.toggle('on', !S.showFormulas);
  $('st-view-formulas').classList.toggle('on', S.showFormulas);
  scheduleRefresh(true);
}
$('fr-showformulas').onclick = () => setShowFormulas(!S.showFormulas);
// 计算选项 + 迭代计算。服务端是唯一状态源，打开工作簿、撤销/重做及协作回放后均回读。
function syncCalculationControls(state) {
  if (!state) return;
  $('fr-calcmode').value = state.mode === 'manual' ? 'manual' : 'auto';
  const panel = $('calc-iteration-panel');
  if (!panel) return;
  panel.querySelector('[data-calc="enabled"]').checked = !!state.enabled;
  panel.querySelector('[data-calc="maxIterations"]').value = String(state.maxIterations ?? 100);
  panel.querySelector('[data-calc="maxChange"]').value = String(state.maxChange ?? 0.001);
}
async function refreshCalculationSettings() {
  const state = await apiPost('/api/calcmode', { op: 'get' });
  syncCalculationControls(state);
  return state;
}
function calculationSettingsPayload(panel = $('calc-iteration-panel')) {
  const maxIterations = Number(panel.querySelector('[data-calc="maxIterations"]').value);
  const maxChange = Number(panel.querySelector('[data-calc="maxChange"]').value);
  if (!Number.isInteger(maxIterations) || maxIterations < 1 || maxIterations > 32767) {
    throw new Error('最大迭代次数必须是 1 到 32767 的整数');
  }
  if (!Number.isFinite(maxChange) || maxChange < 0) {
    throw new Error('最大误差必须是非负有限数值');
  }
  return {
    op: 'set',
    mode: $('fr-calcmode').value,
    enabled: panel.querySelector('[data-calc="enabled"]').checked,
    maxIterations,
    maxChange,
  };
}
async function openCalculationSettingsPanel() {
  $('calc-iteration-panel')?.remove();
  const panel = document.createElement('div');
  panel.id = 'calc-iteration-panel';
  panel.className = 'calc-iteration-panel';
  panel.setAttribute('role', 'dialog');
  panel.setAttribute('aria-labelledby', 'calc-iteration-title');
  panel.innerHTML = `
    <h3 id="calc-iteration-title">迭代计算设置</h3>
    <label><input type="checkbox" data-calc="enabled"> 启用迭代计算</label>
    <label>最大迭代次数<input type="number" min="1" max="32767" step="1" data-calc="maxIterations"></label>
    <label>最大误差<input type="number" min="0" step="any" data-calc="maxChange"></label>
    <p class="calc-iteration-help">用于求解循环引用；达到最大次数或相邻两次结果变化小于最大误差时停止。</p>
    <div class="calc-iteration-actions"><button type="button" data-action="cancel">取消</button><button type="button" class="primary" data-action="apply">确定</button></div>`;
  document.body.appendChild(panel);
  const anchor = $('fr-iteration').getBoundingClientRect();
  panel.style.top = `${Math.min(innerHeight - panel.offsetHeight - 6, anchor.bottom + 4)}px`;
  panel.style.left = `${Math.max(6, Math.min(innerWidth - panel.offsetWidth - 6, anchor.right - panel.offsetWidth))}px`;
  const close = () => {
    document.removeEventListener('mousedown', closeOutside, true);
    panel.remove();
  };
  const closeOutside = (event) => { if (!panel.contains(event.target) && event.target !== $('fr-iteration')) close(); };
  panel.querySelector('[data-action="cancel"]').onclick = close;
  panel.querySelector('[data-action="apply"]').onclick = async () => {
    try {
      const state = await apiPost('/api/calcmode', calculationSettingsPayload(panel));
      syncCalculationControls(state);
      close();
      scheduleRefresh(true);
      setStatus(state.enabled ? `已启用迭代计算（最多 ${state.maxIterations} 次）` : '已关闭迭代计算');
    } catch (error) { setStatus('迭代计算设置错误: ' + error.message); }
  };
  panel.onkeydown = (event) => { if (event.key === 'Escape') close(); };
  try { syncCalculationControls(await refreshCalculationSettings()); }
  catch (error) { close(); throw error; }
  setTimeout(() => document.addEventListener('mousedown', closeOutside, true));
  panel.querySelector('[data-calc="enabled"]').focus();
  return panel;
}
$('fr-calcmode').onchange = async (e) => {
  const state = await apiPost('/api/calcmode', { op: 'set', mode: e.target.value });
  syncCalculationControls(state);
  setStatus(state.mode === 'manual' ? '手动计算：按 F9 重算' : '自动计算');
  scheduleRefresh(true);
};
$('fr-iteration').onclick = () => { void openCalculationSettingsPanel(); };
$('fr-calc').onclick = async () => { await api('/api/calc', { method: 'POST' }); scheduleRefresh(true); setStatus('已重算'); };
window.__unicellCalculationTest = {
  sync: syncCalculationControls,
  payload: calculationSettingsPayload,
  refresh: refreshCalculationSettings,
  open: openCalculationSettingsPanel,
};

/* 名称管理器 */
async function openNamesDialog() {
  let dlg = $('names-dialog');
  if (dlg) dlg.remove();
  dlg = document.createElement('div');
  dlg.id = 'names-dialog';
  dlg.innerHTML = `
    <div class="fd-title"><span>名称管理器</span><button id="names-close">×</button></div>
    <div class="fd-row"><div id="names-list"></div></div>
    <div class="fd-row"><input id="names-new-name" placeholder="名称（如 销售额）" spellcheck="false">
      <input id="names-new-ref" placeholder="引用位置（如 =Sheet1!$A$1:$A$10）" spellcheck="false"></div>
    <div class="fd-row fd-btns"><span id="names-info"></span>
      <button id="names-del">删除选中</button><button id="names-add" class="primary">新建</button></div>`;
  document.body.appendChild(dlg);
  const list = dlg.querySelector('#names-list');
  const refresh = async () => {
    const j = await apiPost('/api/names', { op: 'list' });
    list.textContent = '';
    for (const nm of j.names || []) {
      const d = document.createElement('div');
      d.className = 'fnx-item';
      d.innerHTML = `<b>${nm.name}</b> <span class="hint">${nm.formula}</span>`;
      d.onclick = () => { list.querySelectorAll('.fnx-item').forEach((x) => x.classList.remove('on')); d.classList.add('on'); d.dataset.name = nm.name; dlg.dataset.pick = nm.name; };
      list.appendChild(d);
    }
    if (!(j.names || []).length) list.innerHTML = '<div class="fnx-empty">暂无定义名称</div>';
  };
  await refresh();
  dlg.querySelector('#names-add').onclick = async () => {
    const name = dlg.querySelector('#names-new-name').value.trim();
    const formula = dlg.querySelector('#names-new-ref').value.trim();
    if (!name || !formula) { dlg.querySelector('#names-info').textContent = '请填名称和引用'; return; }
    try {
      await apiPost('/api/names', { op: 'add', name, formula });
      dlg.querySelector('#names-new-name').value = ''; dlg.querySelector('#names-new-ref').value = '';
      dlg.querySelector('#names-info').textContent = '已新建';
      await refresh();
    } catch (e) { dlg.querySelector('#names-info').textContent = e.message; }
  };
  dlg.querySelector('#names-del').onclick = async () => {
    if (!dlg.dataset.pick) { dlg.querySelector('#names-info').textContent = '请先选中'; return; }
    await apiPost('/api/names', { op: 'del', name: dlg.dataset.pick });
    dlg.dataset.pick = '';
    await refresh();
  };
  dlg.querySelector('#names-close').onclick = () => dlg.remove();
}
$('fr-names').onclick = openNamesDialog;

/* 追踪引用/从属箭头（SVG 层） */
function traceLayerEl() {
  let svg = $('trace-layer');
  if (!svg) {
    svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.id = 'trace-layer';
    $('selection-layer').appendChild(svg);
  }
  return svg;
}
function clearTrace() { const l = $('trace-layer'); if (l) l.innerHTML = ''; }
function cellCenter(r, c) {
  return { x: colX(c) + colWidth(c) / 2, y: rowY(r) + rowHeight(r) / 2 };
}
function drawTrace(targets, kind) {
  const svg = traceLayerEl();
  const dst = cellCenter(S.cur.r, S.cur.c);
  const ns = 'http://www.w3.org/2000/svg';
  for (const t of targets) {
    const src = cellCenter(t.r0, t.c0);
    const line = document.createElementNS(ns, 'line');
    line.setAttribute('x1', src.x); line.setAttribute('y1', src.y);
    line.setAttribute('x2', dst.x); line.setAttribute('y2', dst.y);
    line.setAttribute('class', kind === 'dep' ? 't-line-dep' : 't-line');
    line.setAttribute('marker-end', 'url(#trace-arrow)');
    svg.appendChild(line);
    const dot = document.createElementNS(ns, 'circle');
    dot.setAttribute('cx', src.x); dot.setAttribute('cy', src.y); dot.setAttribute('r', 3);
    dot.setAttribute('class', kind === 'dep' ? 't-dot-dep' : 't-dot');
    svg.appendChild(dot);
  }
  // 箭头标记
  if (!svg.querySelector('defs')) {
    const defs = document.createElementNS(ns, 'defs');
    defs.innerHTML = '<marker id="trace-arrow" markerWidth="8" markerHeight="8" refX="6" refY="3" orient="auto"><path d="M0,0 L6,3 L0,6 z" fill="#1a73e8"/></marker>';
    svg.insertBefore(defs, svg.firstChild);
  }
  const w = colX(S.vCols + 1), h = rowY(S.vRows + 1);
  svg.setAttribute('width', w); svg.setAttribute('height', h);
  svg.style.width = w + 'px'; svg.style.height = h + 'px';
}
$('fr-trace-prec').onclick = async () => {
  clearTrace();
  const j = await api(`/api/cell?sheet=${S.sheet}&row=${S.cur.r}&col=${S.cur.c}`);
  if (!(j.content || '').startsWith('=')) { setStatus('当前单元格不是公式'); return; }
  const refs = parseFormulaRefs(j.content);
  if (!refs.length) { setStatus('公式中无引用'); return; }
  drawTrace(refs, 'prec');
  setStatus(`追踪引用单元格 ${refs.length} 处`);
};
$('fr-trace-dep').onclick = async () => {
  clearTrace();
  const j = await api(`/api/dependents?sheet=${S.sheet}&row=${S.cur.r}&col=${S.cur.c}`);
  const deps = j.dependents || [];
  if (!deps.length) { setStatus('没有从属单元格'); return; }
  drawTrace(deps.map((d) => ({ r0: d.r, c0: d.c })), 'dep');
  setStatus(`追踪从属单元格 ${deps.length} 处`);
};
$('fr-trace-clear').onclick = () => { clearTrace(); setStatus('已删除箭头'); };

/* ================= 初始化 ================= */
async function loadWorkbook() {
  window.UniCellFonts?.beginWorkbook?.();
  const j = await api('/api/info');
  const previousStorageScope = S.storageScope;
  const storageScope = setStorageScope(j.storageScope);
  if (previousStorageScope && previousStorageScope !== storageScope) {
    clearTimeout(S.autosaveTimer);
    S.autosaveTimer = 0;
    S.autosaveWriting = null;
    S.recoveryRecord = null;
    S.localFileName = '';
    S.fileHandle = null;
    S.dirty = false;
    S.lastAutosaveAt = 0;
  }
  await refreshCalculationSettings();
  window.clearObjectHistory?.();
  window.invalidateAllDvRules?.();
  S.sheets = j.sheets;
  S.excelExtension = j.excelExtension || 'xlsx';
  if (!S.localFileName) S.localFileName = normalizedWorkbookName(j.fileName || '工作簿1', excelExtension());
  S.sheet = 0;
  S.cellsCache.clear(); S.colW.clear(); S.rowH.clear();
  // 校准默认几何
  const v = await api(`/api/view?sheet=0&r0=1&c0=1&r1=2&c1=2`);
  if (v.colWidths.length) S.defW = v.colWidths[0];
  if (v.rowHeights.length) S.defH = v.rowHeights[0];
  // The first hit can happen before the deferred viewport refresh.  Load the complete merge map
  // now so an imported workbook can never briefly expose a merged region's subordinate cells.
  S.merges = v.merges || [];
  document.title = `${S.localFileName ? stripWorkbookExtension(S.localFileName) : j.fileName} — UniCell`;
  renderSheetTabs();
  setCursor(1, 1);
  await fitVirtualToData(); // 导入后滚动范围直接覆盖数据行数
  updateSpacer();
  await loadObjects();
  scheduleRefresh(true);
}
loadWorkbook().then(async () => {
  await initLocalFileLifecycle();
  gridScroll.focus();
  if (!S.recoveryRecord) setStatus('就绪 — UniCell 公益版电子表格（Rust 引擎驱动）');
});
window.addEventListener('pageshow', (event) => {
  if (!event.persisted) return;
  // A page restored from the back/forward cache may outlive a rotated server
  // cookie. Re-read the session namespace before exposing workbook or recovery UI.
  void loadWorkbook().then(refreshRecoveryState).catch(() => undefined);
});
window.addEventListener('unicell-fonts-ready', () => scheduleRefresh(true));

/* ================= 插入对象系统（参考母项目 unidoc：配置存模型、DOM 只是渲染装饰） =================
 * 对象模型 {id, sheet, type, mode:'cell'|'abs', r, c, x, y, w, h, config}
 * cell 模式锚定单元格（x,y 为相对偏移，随滚动/缩放/行列变化）；abs 模式绝对位置浮动。 */
let objSeq = 0;
const objectsLayer = $('objects-layer');
const frozenObjectsLayer = $('frozen-objects-layer');
S.objects = [];
S.selObj = null;
S.editTextObj = null;
let objectsLoadSeq = 0;
const DEFAULT_SVG = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100"><circle cx="50" cy="50" r="40" fill="#217346"/><text x="50" y="56" font-size="20" text-anchor="middle" fill="#fff">SVG</text></svg>';
const DEFAULT_HTML = '<!doctype html><html><body style="font:13px sans-serif;padding:8px"><b>沙盒 HTML</b><p>可运行脚本（iframe sandbox）</p><button onclick="this.textContent=Date.now()">点我</button></body></html>';

async function loadObjects() {
  const seq = ++objectsLoadSeq;
  const sheet = S.sheet;
  try {
    const j = await api(`/api/objects?sheet=${sheet}`);
    if (seq !== objectsLoadSeq || sheet !== S.sheet) return false;
    S.objects = j.objects || [];
  } catch {
    if (seq !== objectsLoadSeq || sheet !== S.sheet) return false;
    S.objects = [];
  }
  // 切换工作表/重新导入后，旧工作表的对象 id 不得泄漏为新工作表的选中态。
  S.selObj = null;
  S.editTextObj = null;
  renderObjects();
  return true;
}
function objLogicalPosition(o) {
  // 缩放：o.x/o.y（格内偏移）与 o.w/o.h（尺寸）恒为逻辑像素（无缩放），渲染统一乘 S.zoom；
  // colX/rowY 已是缩放后坐标，故 cell 模式只需再缩放格内偏移。
  if (o.mode === 'abs') return { x: o.x * S.zoom, y: o.y * S.zoom };
  return { x: colX(o.c) + o.x * S.zoom, y: rowY(o.r) + o.y * S.zoom };
}
function objFrozenAxes(o) {
  const frozenRows = Number(S.frozen && S.frozen.rows) || 0;
  const frozenCols = Number(S.frozen && S.frozen.cols) || 0;
  const anchorRow = Number(o.r) || 0;
  const anchorCol = Number(o.c) || 0;
  return {
    x: frozenCols > 0 && anchorCol > 0 && anchorCol <= frozenCols,
    y: frozenRows > 0 && anchorRow > 0 && anchorRow <= frozenRows,
  };
}
function objPosition(o) {
  const p = objLogicalPosition(o);
  const frozen = objFrozenAxes(o);
  // 冻结对象位于滚动容器外的独立视口：冻结轴完全不参与滚动，非冻结轴按当前
  // scroll offset 定位。这样纵向滚动时浏览器合成线程不会先移动图片、再等 JS 补偿。
  return {
    x: p.x - (frozen.y && !frozen.x ? gridScroll.scrollLeft : 0),
    y: p.y - (frozen.x && !frozen.y ? gridScroll.scrollTop : 0),
  };
}
function objectElement(id) {
  return document.querySelector(`.cell-obj[data-id="${id}"]`);
}
function allObjectElements() {
  return document.querySelectorAll('#objects-layer .cell-obj, #frozen-objects-layer .cell-obj');
}
function buildObjContent(o) {
  if (o.type === 'text') {
    const t = document.createElement('div');
    t.className = 'obj-text';
    const editing = S.editTextObj === o.id;
    t.contentEditable = editing ? 'true' : 'false';
    t.classList.toggle('editing', editing);
    t.tabIndex = editing ? 0 : -1;
    t.spellcheck = false;
    t.innerHTML = o.config.html || '文本框';
    // 普通状态下整块文本框都可拖动；双击进入编辑态后才保留原生光标与文字选择。
    t.addEventListener('pointerdown', (e) => {
      if (S.editTextObj === o.id && !e.ctrlKey) {
        selectObject(o.id);
        e.stopPropagation();
      }
    });
    t.addEventListener('blur', () => {
      if (o.config.html !== t.innerHTML) {
        const before = JSON.parse(JSON.stringify(o));
        o.config.html = t.innerHTML;
        void saveObject(o, before, '编辑文本框');
      }
      if (S.editTextObj === o.id) {
        S.editTextObj = null;
        t.contentEditable = 'false';
        t.classList.remove('editing');
      }
    });
    return t;
  }
  if (o.type === 'image') {
    const img = document.createElement('img');
    img.src = o.config.src; img.draggable = false; img.alt = '';
    return img;
  }
  if (o.type === 'svg') {
    const box = document.createElement('div');
    box.className = 'obj-svg';
    box.innerHTML = o.config.svg || '';
    return box;
  }
  if (o.type === 'video') {
    const v = document.createElement('video');
    v.src = o.config.src; v.controls = true;
    return v;
  }
  if (o.type === 'html') {
    const f = document.createElement('iframe');
    f.setAttribute('sandbox', 'allow-scripts');
    f.dataset.unicellObjectId = String(o.id);
    f.srcdoc = htmlWithSnapshotBridge(o.config.code || DEFAULT_HTML, o.id);
    // 拖拽/选择/右键由上层透明护盾接管（见 renderObjects）；实时交互走右键「全屏」。
    return f;
  }
  return document.createElement('div');
}
function renderObjects() {
  objectsLayer.textContent = '';
  frozenObjectsLayer.textContent = '';
  for (const o of S.objects || []) {
    const d = document.createElement('div');
    d.className = 'cell-obj obj-' + o.type + (S.selObj === o.id ? ' sel' : '');
    d.dataset.id = o.id;
    const p = objPosition(o);
    Object.assign(d.style, { left: p.x + 'px', top: p.y + 'px', width: o.w * S.zoom + 'px', height: o.h * S.zoom + 'px' });
    // 内容单独裁剪，外层保持 overflow:visible，确保 8 个缩放手柄不会被切掉。
    const viewport = document.createElement('div');
    viewport.className = 'obj-viewport';
    viewport.appendChild(buildObjContent(o));
    d.appendChild(viewport);
    // iframe/video 会吞掉指针事件（导致无法拖拽/选择/右键）：盖一层透明护盾统一交给对象层处理；
    // 需要与沙盒内容实时交互时，右键「全屏查看 / 交互」在全屏层里操作。
    if (o.type === 'html' || o.type === 'video') {
      const shield = document.createElement('div');
      shield.className = 'obj-shield';
      viewport.appendChild(shield);
    }
    // 8 方向缩放手柄（nw/n/ne/e/se/s/sw/w，对齐母项目 unidoc 多边角拉伸）
    for (const h of ['nw','n','ne','e','se','s','sw','w']) {
      const hd = document.createElement('div');
      hd.className = 'obj-handle';
      hd.dataset.h = h;
      d.appendChild(hd);
    }
    const frozen = objFrozenAxes(o);
    (frozen.x || frozen.y ? frozenObjectsLayer : objectsLayer).appendChild(d);
  }
}
// 仅更新位置（滚动/缩放/行列变化时，不重建 DOM 避免视频/iframe 中断）
function updateObjPositions() {
  for (const o of S.objects || []) {
    const d = objectElement(o.id);
    if (!d) continue;
    const p = objPosition(o);
    Object.assign(d.style, { left: p.x + 'px', top: p.y + 'px', width: o.w * S.zoom + 'px', height: o.h * S.zoom + 'px' });
  }
}
async function insertObject(type, config) {
  const o = {
    id: 'o' + (++objSeq) + Date.now().toString(36),
    sheet: S.sheet, type, mode: 'cell',
    r: S.cur.r, c: S.cur.c,
    x: 8, y: 8, w: 240, h: 140,
    config: config || {},
  };
  (S.objects = S.objects || []).push(o);
  try {
    await queueObjectWrite({ op: 'add', sheet: S.sheet, object: o });
  } catch (error) {
    S.objects = (S.objects || []).filter((item) => item.id !== o.id);
    renderObjects();
    setStatus(`插入对象失败：${error?.message || error}`);
    return null;
  }
  window.recordObjectHistory?.({ id: o.id, sheet: o.sheet, before: null, after: o, label: '插入对象' });
  renderObjects();
  selectObject(o.id);
  setStatus(`已插入${{ text: '文本框', image: '图片', svg: 'SVG', video: '视频', html: '沙盒HTML' }[type] || '对象'}`);
}
async function saveObject(o, before = undefined, label = '编辑对象') {
  const wasNew = !!o._isNew;
  const prior = before === undefined ? undefined : JSON.parse(JSON.stringify(before));
  if (o._isNew) {
    delete o._isNew;
  }
  const attempted = JSON.parse(JSON.stringify(o));
  try {
    await queueObjectWrite({ op: wasNew ? 'add' : 'update', sheet: attempted.sheet, id: attempted.id, object: attempted });
  } catch (error) {
    const index = (S.objects || []).findIndex((item) => item.id === attempted.id);
    if (wasNew) {
      if (index >= 0) S.objects.splice(index, 1);
    } else if (prior !== undefined && index >= 0 && JSON.stringify(S.objects[index]) === JSON.stringify(attempted)) {
      S.objects[index] = prior;
    } else if (prior === undefined && Number(attempted.sheet) === Number(S.sheet)) {
      await loadObjects();
    }
    renderObjects();
    setStatus(`${label}失败：${error?.message || error}`);
    return false;
  }
  if (prior !== undefined && JSON.stringify(prior) !== JSON.stringify(attempted)) {
    window.recordObjectHistory?.({ id: attempted.id, sheet: attempted.sheet, before: wasNew ? null : prior, after: attempted, label: wasNew ? '复制对象' : label });
  }
  return true;
}
async function deleteObject(id) {
  const before = JSON.parse(JSON.stringify((S.objects || []).find((x) => x.id === id) || null));
  if (!before) return false;
  try {
    await queueObjectWrite({ op: 'delete', sheet: before.sheet, id });
  } catch (error) {
    setStatus(`删除对象失败：${error?.message || error}`);
    return false;
  }
  S.objects = (S.objects || []).filter((x) => x.id !== id);
  if (S.selObj === id) S.selObj = null;
  if (S.editTextObj === id) S.editTextObj = null;
  if (before) window.recordObjectHistory?.({ id, sheet: before.sheet, before, after: null, label: '删除对象' });
  renderObjects();
  return true;
}
function selectObject(id) {
  if (S.editTextObj && S.editTextObj !== id) {
    const editingId = S.editTextObj;
    const t = objectElement(editingId)?.querySelector('.obj-text');
    const o = (S.objects || []).find((x) => x.id === editingId);
    if (t && o && o.config.html !== t.innerHTML) {
      const before = JSON.parse(JSON.stringify(o));
      o.config.html = t.innerHTML;
      void saveObject(o, before, '编辑文本框');
    }
    S.editTextObj = null;
    if (t) { t.contentEditable = 'false'; t.classList.remove('editing'); }
  }
  S.selObj = id || null;
  if (id) window.armObjectHistory?.();
  allObjectElements().forEach((d) => d.classList.toggle('sel', d.dataset.id === id));
}

// 对象选择是临时 UI 状态：点击对象外部必须立刻失焦并去掉绿色边框。
// 使用 capture 可覆盖网格、冻结窗格等会自行 stopPropagation 的区域；对象命令按钮
// 被保留，因为它们需要在 click 阶段读取当前对象。
document.addEventListener('pointerdown', (e) => {
  if (!S.selObj || !(e.target instanceof Element)) return;
  if (e.target.closest('.cell-obj, #ctx-menu, #objsrc-dialog, #native-drawing-dialog, #obj-fs, #btn-ins-cell, #btn-ins-abs, #btn-ins-del, #btn-undo, #btn-redo')) return;
  selectObject(null);
  window.disarmObjectHistory?.();
}, true);
function selObjRef() { return (S.objects || []).find((x) => x.id === S.selObj); }
// 复制对象（Ctrl+拖动或右键菜单）
async function duplicateObject(o) {
  const duplicated = JSON.parse(JSON.stringify(o));
  duplicated.id = 'o' + (++objSeq) + Date.now().toString(36);
  duplicated.x = (o.x || 0) + 16;
  duplicated.y = (o.y || 0) + 16;
  if (o.config?.nativeDrawing) window.prepareNativeDrawingClone?.(o, duplicated);
  (S.objects = S.objects || []).push(duplicated);
  try {
    await queueObjectWrite({ op: 'add', sheet: duplicated.sheet, object: duplicated });
  } catch (error) {
    S.objects = (S.objects || []).filter((item) => item.id !== duplicated.id);
    renderObjects();
    setStatus(`复制对象失败：${error?.message || error}`);
    return null;
  }
  window.recordObjectHistory?.({ id: duplicated.id, sheet: duplicated.sheet, before: null, after: duplicated, label: '复制对象' });
  renderObjects();
  selectObject(duplicated.id);
  return duplicated;
}
// 对象右键菜单（屏蔽浏览器原生菜单）
function showObjMenu(o, x, y) {
  const isNativeDrawing = !!(o.config && o.config.nativeDrawing);
  const items = isNativeDrawing
    ? [{ label: '编辑原生对象…', fn: () => window.openNativeDrawingEditor?.(o) }]
    : [{ label: '编辑源码', fn: () => openObjSourceEditor(o) }];
  // 沙盒 HTML / 视频等“活内容”内联时被护盾罩住不可交互，改由全屏层里实时交互
  if (o.type === 'html' || o.type === 'video') {
    items.push({ label: '全屏查看 / 交互', fn: () => openObjFullscreen(o) });
  }
  items.push(
    { label: '复制对象', fn: () => duplicateObject(o) },
    { label: o.mode === 'cell' ? '改为绝对位置浮动' : '锚定到当前单元格', fn: () => toggleObjMode(o) },
    'sep',
    { label: '删除对象', fn: () => deleteObject(o.id) },
  );
  showCtxMenu(x, y, items);
}
// 全屏查看/交互：沙盒 HTML 在全屏层里可运行脚本、点击按钮；视频全屏播放
function openObjFullscreen(o) {
  const prev = document.getElementById('obj-fs');
  if (prev) prev.remove();
  const fs = document.createElement('div');
  fs.id = 'obj-fs';
  fs.className = 'obj-fs';
  const bar = document.createElement('div');
  bar.className = 'obj-fs-bar';
  const title = document.createElement('span');
  title.textContent = (o.type === 'html' ? '沙盒 HTML' : '视频') + ' — 全屏（Esc 退出）';
  const close = document.createElement('button');
  close.textContent = '关闭 ✕';
  bar.appendChild(title); bar.appendChild(close);
  const stage = document.createElement('div');
  stage.className = 'obj-fs-stage';
  let inner;
  if (o.type === 'html') {
    inner = document.createElement('iframe');
    inner.setAttribute('sandbox', 'allow-scripts allow-forms allow-modals allow-popups');
    inner.dataset.unicellObjectId = String(o.id);
    inner.srcdoc = htmlWithSnapshotBridge(o.config.code || DEFAULT_HTML, o.id);
  } else {
    inner = document.createElement('video');
    inner.src = o.config.src || ''; inner.controls = true; inner.autoplay = true;
  }
  stage.appendChild(inner);
  fs.appendChild(bar); fs.appendChild(stage);
  document.body.appendChild(fs);
  let closing = false;
  const closeFn = async () => {
    if (closing) return;
    closing = true; close.disabled = true;
    if (o.type === 'html') {
      const snapshot = await requestHtmlObjectSnapshot(o, 700);
      if (snapshot) rememberHtmlObjectSnapshot(o, snapshot);
    }
    document.removeEventListener('keydown', onKey, true);
    fs.remove();
  };
  const onKey = (e) => { if (e.key === 'Escape') { e.stopPropagation(); void closeFn(); } };
  close.onclick = () => { void closeFn(); };
  document.addEventListener('keydown', onKey, true);
}
$('grid-wrap').addEventListener('contextmenu', (e) => {
  const objEl = e.target.closest('.cell-obj');
  if (!objEl) return;
  e.preventDefault();
  e.stopPropagation();
  const o = (S.objects || []).find((x) => x.id === objEl.dataset.id);
  if (o) { selectObject(o.id); showObjMenu(o, e.clientX, e.clientY); }
});
// 双击对象进入源码编辑
$('grid-wrap').addEventListener('dblclick', (e) => {
  const objEl = e.target.closest('.cell-obj');
  if (!objEl) return;
  const o = (S.objects || []).find((x) => x.id === objEl.dataset.id);
  if (!o) return;
  if (o.type === 'text') {
    e.preventDefault();
    e.stopPropagation();
    selectObject(o.id);
    S.editTextObj = o.id;
    const t = objEl.querySelector('.obj-text');
    if (t) {
      t.contentEditable = 'true';
      t.classList.add('editing');
      t.tabIndex = 0;
      t.focus();
      const sel = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(t);
      range.collapse(false);
      sel.removeAllRanges();
      sel.addRange(range);
    }
    return;
  }
  openObjSourceEditor(o);
});
// 锚定/浮动切换（锚定到单元格时让对象跳到该单元格位置）
function toggleObjMode(o) {
  const before = JSON.parse(JSON.stringify(o));
  if (o.mode === 'cell') {
    const p = objLogicalPosition(o);
    o.mode = 'abs'; o.x = p.x / S.zoom; o.y = p.y / S.zoom;
  } else {
    o.mode = 'cell'; o.r = S.cur.r; o.c = S.cur.c; o.x = 8; o.y = 8;
  }
  void saveObject(o, before, '更改对象定位方式');
  renderObjects();
  selectObject(o.id);
}
// 编辑对象源码对话框（SVG/沙盒HTML/文本框）
function openObjSourceEditor(o) {
  if (o.config && o.config.nativeDrawing) {
    window.openNativeDrawingEditor?.(o);
    return;
  }
  let dlg = $('objsrc-dialog');
  if (dlg) dlg.remove();
  dlg = document.createElement('div');
  dlg.id = 'objsrc-dialog';
  const typeName = { text: '文本框', svg: 'SVG', html: '沙盒HTML', image: '图片', video: '视频' }[o.type] || o.type;
  const src = o.type === 'svg' ? (o.config.svg || '') : o.type === 'html' ? (o.config.code || '') : (o.config.html || o.config.src || '');
  dlg.innerHTML = `
    <div class="fd-title"><span>编辑源码 — ${typeName}</span><button id="objsrc-close">×</button></div>
    <textarea id="objsrc-code" spellcheck="false"></textarea>
    <div class="fd-btns"><button id="objsrc-ok" class="primary">确定</button></div>`;
  dlg.className = 'obj-dialog';
  document.body.appendChild(dlg);
  const ta = dlg.querySelector('#objsrc-code');
  ta.value = src;
  ta.addEventListener('keydown', (e) => e.stopPropagation());
  ta.addEventListener('pointerdown', (e) => e.stopPropagation());
  dlg.querySelector('#objsrc-close').onclick = () => dlg.remove();
  dlg.querySelector('#objsrc-ok').onclick = () => {
    const before = JSON.parse(JSON.stringify(o));
    const v = ta.value;
    if (o.type === 'svg') o.config.svg = v;
    else if (o.type === 'html') { o.config.code = v; delete o._lastSnapshotHtml; }
    else if (o.type === 'image' || o.type === 'video') o.config.src = v;
    else o.config.html = v;
    void saveObject(o, before, '编辑对象源码');
    renderObjects();
    selectObject(o.id);
    dlg.remove();
    setStatus('已保存源码');
  };
  ta.focus();
}
// 拖拽移动 / 8方向缩放手柄（Ctrl+拖动 = 复制对象）
$('grid-wrap').addEventListener('pointerdown', (e) => {
  let objEl = e.target.closest('.cell-obj');
  if (!objEl) { selectObject(null); return; }
  let o = (S.objects || []).find((x) => x.id === objEl.dataset.id);
  if (!o) return;
  // 检测是否点击了缩放手柄
  const handleEl = e.target.closest('.obj-handle');
  const resizeDir = handleEl ? handleEl.dataset.h : null;
  // 文本编辑态保留原生光标；普通状态下文本框与其他对象一样可直接拖动。
  if (S.editTextObj === o.id && e.target.closest('.obj-text') && !e.ctrlKey && !resizeDir) { selectObject(o.id); return; }
  e.preventDefault();
  // Ctrl+拖动：先复制出副本再拖动副本（Excel/Word 语义）
  let historyBefore = JSON.parse(JSON.stringify(o));
  if (e.ctrlKey && !resizeDir) {
    const source = o;
    o = JSON.parse(JSON.stringify(o));
    o.id = 'o' + (++objSeq) + Date.now().toString(36);
    if (source.config?.nativeDrawing) window.prepareNativeDrawingClone?.(source, o);
    o._isNew = true;
    historyBefore = null;
    (S.objects = S.objects || []).push(o);
    renderObjects();
    objEl = objectElement(o.id);
    if (!objEl) return;
  }
  selectObject(o.id);
  // 指针捕获（对齐母项目 unidoc）：拖拽期间即便光标经过 iframe/图片/SVG，事件也不会被吞
  try { objEl.setPointerCapture(e.pointerId); } catch (err) { /* 合成事件可能无活动指针 */ }
  const startX = e.clientX, startY = e.clientY;
  const w0 = o.w, h0 = o.h, x0 = o.x, y0 = o.y;
  const MIN_W = 30, MIN_H = 20;
  const move = (ev) => {
    const dx = (ev.clientX - startX) / S.zoom, dy = (ev.clientY - startY) / S.zoom;
    if (resizeDir) {
      // 8 方向缩放：根据方向调整 w/h 和 x/y 偏移
      let nw = w0, nh = h0, nx = x0, ny = y0;
      if (resizeDir.includes('e')) nw = Math.max(MIN_W, w0 + dx);
      if (resizeDir.includes('w')) { nw = Math.max(MIN_W, w0 - dx); nx = x0 + (w0 - nw); }
      if (resizeDir.includes('s')) nh = Math.max(MIN_H, h0 + dy);
      if (resizeDir.includes('n')) { nh = Math.max(MIN_H, h0 - dy); ny = y0 + (h0 - nh); }
      o.w = nw; o.h = nh; o.x = nx; o.y = ny;
    } else {
      o.x = x0 + dx; o.y = y0 + dy;
    }
    const p = objPosition(o);
    Object.assign(objEl.style, { left: p.x + 'px', top: p.y + 'px', width: o.w * S.zoom + 'px', height: o.h * S.zoom + 'px' });
  };
  const up = () => {
    document.removeEventListener('pointermove', move);
    document.removeEventListener('pointerup', up);
    document.removeEventListener('pointercancel', up);
    try { objEl.releasePointerCapture(e.pointerId); } catch (err) { /* 已自动释放 */ }
    void saveObject(o, historyBefore, resizeDir ? '调整对象大小' : '移动对象');
  };
  document.addEventListener('pointermove', move);
  document.addEventListener('pointerup', up);
  document.addEventListener('pointercancel', up);
});
/* 插入按钮 */
$('btn-ins-textbox').onclick = () => insertObject('text', { html: '文本框' });
$('btn-ins-image').onclick = () => $('ins-image-input').click();
$('ins-image-input').onchange = (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const isSvg = /\.svg$/i.test(f.name);
  const rd = new FileReader();
  rd.onload = () => {
    if (isSvg) insertObject('svg', { svg: rd.result });
    else insertObject('image', { src: rd.result });
    e.target.value = '';
  };
  if (isSvg) rd.readAsText(f); else rd.readAsDataURL(f);
};
$('btn-ins-svg').onclick = () => insertObject('svg', { svg: DEFAULT_SVG });
$('btn-ins-video').onclick = () => $('ins-video-input').click();
$('ins-video-input').onchange = (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const rd = new FileReader();
  rd.onload = () => { insertObject('video', { src: rd.result }); e.target.value = ''; };
  rd.readAsDataURL(f);
};
$('btn-ins-html').onclick = () => insertObject('html', { code: DEFAULT_HTML });
$('btn-ins-latex').onclick = () => startEdit('$E=mc^2$');
$('btn-ins-link').onclick = () => {
  const u = prompt('链接地址', 'https://');
  if (u) insertObject('text', { html: `<a href="${u}" target="_blank" rel="noopener">${u}</a>` });
};
/* 摆放方式切换 + 删除 */
$('btn-ins-cell').onclick = () => {
  const o = selObjRef(); if (!o) { setStatus('请先选中对象'); return; }
  const before = JSON.parse(JSON.stringify(o));
  // 锚定到当前单元格：对象跳到该单元格位置并随其移动
  o.mode = 'cell'; o.r = S.cur.r; o.c = S.cur.c; o.x = 8; o.y = 8;
  void saveObject(o, before, '锚定对象'); renderObjects(); selectObject(o.id); setStatus(`已锚定到 ${cellRef(o.r, o.c)}`);
};
$('btn-ins-abs').onclick = () => {
  const o = selObjRef(); if (!o) { setStatus('请先选中对象'); return; }
  const before = JSON.parse(JSON.stringify(o));
  const p = objLogicalPosition(o);
  // objPosition 返回渲染像素；模型始终保存未缩放逻辑像素，避免 200% 下切换
  // 绝对定位后坐标再次乘 zoom 而瞬间跳位。
  o.mode = 'abs'; o.x = p.x / S.zoom; o.y = p.y / S.zoom;
  void saveObject(o, before, '更改对象定位方式'); renderObjects(); selectObject(o.id); setStatus('已改为绝对位置浮动');
};
$('btn-ins-del').onclick = () => { if (S.selObj) deleteObject(S.selObj); };
document.querySelectorAll('.rb-tab').forEach((tab) => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.rb-tab').forEach((t) => t.classList.toggle('active', t === tab));
    document.querySelectorAll('.rb-panel').forEach((p) => p.classList.toggle('active', p.dataset.tab === tab.dataset.tab));
  });
});
/* 剪切板按钮 */
$('btn-paste').onclick = () => pasteFromClipboard();
$('btn-cut').onclick = () => doCopy(true);
$('btn-copy').onclick = () => doCopy(false);
/* 缩放控件 */
function syncZoomLevel() {
  const pct = Math.round(S.zoom * 100) + '%';
  const v = Math.round(S.zoom * 100);
  const a = $('zoom-level'); if (a) a.textContent = pct;
  const b = $('st-zoom-level'); if (b) b.textContent = pct;
  const s = $('st-zoom-slider'); if (s && document.activeElement !== s) s.value = v;
}
function stepZoom(dir) {
  let i = ZOOM_STEPS.findIndex((s) => Math.abs(s - S.zoom) < 0.005);
  if (i < 0) { i = ZOOM_STEPS.findIndex((s) => s > S.zoom); if (i < 0) i = ZOOM_STEPS.length; if (dir < 0) i -= 1; }
  else i += dir;
  setZoom(ZOOM_STEPS[Math.max(0, Math.min(ZOOM_STEPS.length - 1, i))]);
  syncZoomLevel();
}
$('btn-zoom-in').onclick = () => stepZoom(1);
$('btn-zoom-out').onclick = () => stepZoom(-1);
$('btn-zoom-reset').onclick = () => { setZoom(1); syncZoomLevel(); };
/* 状态栏缩放滑块 + 按钮 */
$('st-zoom-slider').addEventListener('input', (e) => { setZoom(+e.target.value / 100); });
$('st-zoom-in').onclick = () => stepZoom(1);
$('st-zoom-out').onclick = () => stepZoom(-1);
$('st-zoom-level').onclick = () => setZoom(1);
/* 状态栏视图切换（与公式栏「显示公式」联动） */
$('st-view-normal').onclick = () => { setShowFormulas(false); setPageView(false); };
$('st-view-pages').onclick = () => setPageView(!S.pageView);
$('st-view-formulas').onclick = () => setShowFormulas(true);
// 分页预览（A4 分页线，Excel/unidoc 同款）
function setPageView(on) {
  S.pageView = !!on;
  $('st-view-pages').classList.toggle('on', S.pageView);
  renderPageBreaks();
}
let pageBreakLayer = null;
function renderPageBreaks() {
  if (!pageBreakLayer) {
    pageBreakLayer = document.createElement('div');
    pageBreakLayer.id = 'pagebreak-layer';
    $('grid-scroll').appendChild(pageBreakLayer);
  }
  pageBreakLayer.textContent = '';
  if (!S.pageView) { pageBreakLayer.style.display = 'none'; return; }
  pageBreakLayer.style.display = 'block';
  const PW = 794 * S.zoom, PH = 1123 * S.zoom; // A4 @96dpi 随缩放
  const frag = document.createDocumentFragment();
  const maxX = colX(S.vCols + 1), maxY = rowY(S.vRows + 1);
  for (let x = PW; x < maxX; x += PW) {
    const d = document.createElement('div');
    d.className = 'pbreak-v';
    d.style.left = x + 'px'; d.style.top = '0'; d.style.height = maxY + 'px';
    frag.appendChild(d);
  }
  for (let y = PH; y < maxY; y += PH) {
    const d = document.createElement('div');
    d.className = 'pbreak-h';
    d.style.top = y + 'px'; d.style.left = '0'; d.style.width = maxX + 'px';
    frag.appendChild(d);
  }
  pageBreakLayer.appendChild(frag);
}
// 查看.udoc：展示当前文档的 udoc 结构 JSON（参考母项目 unidoc 面板）
$('btn-view-udoc').onclick = async () => {
  let dlg = $('udoc-dialog');
  if (dlg) dlg.remove();
  dlg = document.createElement('div');
  dlg.id = 'udoc-dialog';
  dlg.innerHTML = `<div class="fd-title"><span>udoc 文档结构（unidoc_type:cell）</span><button id="udoc-close">×</button></div><pre id="udoc-pre">加载中…</pre>`;
  document.body.appendChild(dlg);
  dlg.querySelector('#udoc-close').onclick = () => dlg.remove();
  try {
    const doc = await api('/api/udoc-json');
    dlg.querySelector('#udoc-pre').textContent = JSON.stringify(doc, null, 2);
  } catch (e) {
    dlg.querySelector('#udoc-pre').textContent = '加载失败: ' + e.message;
  }
};
/* 显示开关：网格线 / 公式栏 */
$('btn-toggle-gridlines').onclick = () => {
  const el = $('grid-lines');
  const show = el.style.display === 'none';
  el.style.display = show ? '' : 'none';
  $('btn-toggle-gridlines').classList.toggle('on', show);
};
$('btn-toggle-formulabar').onclick = () => {
  const el = $('formula-row');
  const show = el.style.display === 'none';
  el.style.display = show ? '' : 'none';
  $('btn-toggle-formulabar').classList.toggle('on', show);
};

/* ================= Excel 假设分析：目标求解 / 数据表 / 方案管理器 ================= */
function whatIfAddress(text) {
  const ref = parseRef(String(text || '').replaceAll('$', ''));
  if (!ref || ref.r < 1 || ref.r > 1048576 || ref.c < 1 || ref.c > 16384) {
    throw new Error(`无效单元格引用：${text || '（空）'}`);
  }
  return { sheet: S.sheet, row: ref.r, column: ref.c };
}

function whatIfScalar(text) {
  const value = String(text ?? '').trim();
  if (!value) return '';
  if (/^(true|false)$/i.test(value)) return value.toLowerCase() === 'true';
  const number = Number(value);
  return Number.isFinite(number) ? number : value;
}

function whatIfValues(text) {
  const values = String(text || '').split(/[\n\t,;]+/).map((value) => value.trim()).filter(Boolean);
  if (!values.length) throw new Error('至少输入一个假设值');
  return values.map(whatIfScalar);
}

function whatIfScenarioCells(text) {
  const cells = [];
  const seen = new Set();
  for (const token of String(text || '').split(/[\s,;]+/).filter(Boolean)) {
    const [startText, endText = startText] = token.split(':');
    const start = parseRef(startText.replaceAll('$', ''));
    const end = parseRef(endText.replaceAll('$', ''));
    if (!start || !end) throw new Error(`无效方案区域：${token}`);
    const r0 = Math.min(start.r, end.r), r1 = Math.max(start.r, end.r);
    const c0 = Math.min(start.c, end.c), c1 = Math.max(start.c, end.c);
    for (let row = r0; row <= r1; row++) for (let column = c0; column <= c1; column++) {
      const key = `${row},${column}`;
      if (!seen.has(key)) {
        seen.add(key);
        cells.push({ sheet: S.sheet, row, column });
      }
      if (cells.length > 32) throw new Error('Excel 方案最多包含 32 个可变单元格');
    }
  }
  if (!cells.length) throw new Error('方案至少需要一个可变单元格');
  return cells;
}

function whatIfNumber(id, options = {}) {
  const raw = $(id).value.trim();
  if (!raw && options.optional) return null;
  const value = Number(raw);
  if (!Number.isFinite(value)) throw new Error(`${options.label || id} 必须是有限数字`);
  return value;
}

function whatIfGoalRequest() {
  const request = {
    target: whatIfAddress($('wi-goal-target').value),
    changing: whatIfAddress($('wi-goal-changing').value),
    targetValue: whatIfNumber('wi-goal-value', { label: '目标值' }),
    maxIterations: Math.trunc(whatIfNumber('wi-goal-iterations', { label: '最大迭代次数' })),
    tolerance: whatIfNumber('wi-goal-tolerance', { label: '容差' }),
  };
  const lower = whatIfNumber('wi-goal-lower', { optional: true, label: '下界' });
  const upper = whatIfNumber('wi-goal-upper', { optional: true, label: '上界' });
  const initial = whatIfNumber('wi-goal-initial', { optional: true, label: '初始值' });
  if (lower != null) request.lowerBound = lower;
  if (upper != null) request.upperBound = upper;
  if (initial != null) request.initialValue = initial;
  return request;
}

function whatIfDataTableRequest() {
  const kind = $('wi-table-kind').value;
  const common = {
    formulaCell: whatIfAddress($('wi-table-formula').value),
    output: whatIfAddress($('wi-table-output').value),
  };
  if (kind === 'oneVariable') {
    return {
      kind,
      ...common,
      inputCell: whatIfAddress($('wi-table-input').value),
      values: whatIfValues($('wi-table-values').value),
      orientation: $('wi-table-orientation').value,
    };
  }
  return {
    kind,
    ...common,
    rowInputCell: whatIfAddress($('wi-table-row-input').value),
    columnInputCell: whatIfAddress($('wi-table-column-input').value),
    rowValues: whatIfValues($('wi-table-row-values').value),
    columnValues: whatIfValues($('wi-table-column-values').value),
  };
}

function whatIfSetBusy(dialog, busy) {
  dialog.classList.toggle('busy', busy);
  dialog.querySelectorAll('button').forEach((button) => {
    if (!button.classList.contains('wi-close')) button.disabled = !!busy;
  });
}

function whatIfResultText(result) {
  if (result?.status) {
    const achieved = result.achievedValue == null ? '—' : Number(result.achievedValue).toPrecision(12);
    const residual = result.residual == null ? '—' : Number(result.residual).toExponential(5);
    return `${result.converged ? '已收敛' : '未收敛'} · 可变值 ${Number(result.resultValue).toPrecision(12)} · 结果 ${achieved} · 残差 ${residual} · ${result.evaluations} 次求值\n${result.message}`;
  }
  return JSON.stringify(result, null, 2);
}

function whatIfRenderMatrix(container, result) {
  container.textContent = '';
  const note = document.createElement('div');
  note.className = 'wi-note';
  note.textContent = `${result.rows} 行 × ${result.columns} 列，${result.evaluations} 次隔离求值。${result.dynamic ? '动态数据表' : '应用后写入静态结果快照'}`;
  container.appendChild(note);
  const wrap = document.createElement('div');
  wrap.className = 'wi-matrix-wrap';
  const table = document.createElement('table');
  table.className = 'wi-matrix';
  for (const row of result.matrix || []) {
    const tr = document.createElement('tr');
    for (const value of row) {
      const td = document.createElement('td');
      td.textContent = value == null ? '' : String(value);
      tr.appendChild(td);
    }
    table.appendChild(tr);
  }
  wrap.appendChild(table);
  container.appendChild(wrap);
}

async function whatIfRunGoal(dialog, op) {
  const output = $('wi-goal-result');
  whatIfSetBusy(dialog, true);
  try {
    const result = await apiPost('/api/what-if', { op, kind: 'goalSeek', request: whatIfGoalRequest() });
    output.textContent = whatIfResultText(result);
    output.dataset.status = result.converged ? 'ok' : 'warning';
    if (op === 'apply') {
      scheduleRefresh(true);
      setStatus(`目标求解已写回 ${$('wi-goal-changing').value.toUpperCase()}，可用 Ctrl+Z 整体撤销`);
    }
  } catch (error) {
    output.textContent = error.message;
    output.dataset.status = 'error';
  } finally {
    whatIfSetBusy(dialog, false);
  }
}

async function whatIfRunDataTable(dialog, op) {
  const output = $('wi-table-result');
  whatIfSetBusy(dialog, true);
  try {
    const result = await apiPost('/api/what-if', { op, kind: 'dataTable', request: whatIfDataTableRequest() });
    whatIfRenderMatrix(output, result);
    if (op === 'apply') {
      scheduleRefresh(true);
      setStatus(`数据表已作为一次事务写入 ${$('wi-table-output').value.toUpperCase()}，可用 Ctrl+Z 整体撤销`);
    }
  } catch (error) {
    output.textContent = error.message;
    output.dataset.status = 'error';
  } finally {
    whatIfSetBusy(dialog, false);
  }
}

let whatIfScenarioEditing = null;
let whatIfScenarioRows = [];
async function whatIfLoadScenarios(dialog) {
  const result = await apiPost('/api/what-if', { op: 'list', kind: 'scenarios' });
  whatIfScenarioRows = Array.isArray(result.scenarios) ? result.scenarios : [];
  const list = $('wi-scenario-list');
  list.textContent = '';
  if (!whatIfScenarioRows.length) {
    const empty = document.createElement('div');
    empty.className = 'wi-empty';
    empty.textContent = '当前工作簿还没有方案。';
    list.appendChild(empty);
    return;
  }
  for (const scenario of whatIfScenarioRows) {
    const row = document.createElement('div');
    row.className = 'wi-scenario-row';
    row.dataset.id = scenario.id;
    const description = document.createElement('div');
    description.className = 'wi-scenario-description';
    const name = document.createElement('strong');
    name.textContent = scenario.name;
    const meta = document.createElement('span');
    meta.textContent = `${scenario.changes?.length || 0} 个单元格${scenario.locked ? ' · 锁定' : ''}${scenario.hidden ? ' · 隐藏' : ''}`;
    description.append(name, meta);
    const actions = document.createElement('div');
    actions.className = 'wi-row-actions';
    for (const [action, label] of [['preview', '预览'], ['apply', '应用'], ['edit', '编辑'], ['delete', '删除']]) {
      const button = document.createElement('button');
      button.type = 'button';
      button.dataset.action = action;
      button.textContent = label;
      actions.appendChild(button);
    }
    row.append(description, actions);
    list.appendChild(row);
  }
}

function whatIfScenarioForm(scenario = null) {
  whatIfScenarioEditing = scenario?.id || null;
  $('wi-scenario-name').value = scenario?.name || '';
  $('wi-scenario-comment').value = scenario?.comment || '';
  $('wi-scenario-locked').checked = !!scenario?.locked;
  $('wi-scenario-hidden').checked = !!scenario?.hidden;
  if (scenario) {
    $('wi-scenario-cells').value = scenario.changes.map((change) => cellRef(change.cell.row, change.cell.column)).join(',');
    $('wi-scenario-values').value = scenario.changes.map((change) => change.input).join('\n');
  }
  $('wi-scenario-save').textContent = scenario ? '更新方案' : '创建方案';
  $('wi-scenario-cancel-edit').hidden = !scenario;
}

function whatIfCollectScenario() {
  const cells = whatIfScenarioCells($('wi-scenario-cells').value);
  const values = String($('wi-scenario-values').value).split(/\r?\n|\t/);
  if (values.length !== cells.length) {
    throw new Error(`方案值数量（${values.length}）必须等于可变单元格数量（${cells.length}）`);
  }
  return {
    id: whatIfScenarioEditing || '',
    name: $('wi-scenario-name').value.trim(),
    comment: $('wi-scenario-comment').value,
    locked: $('wi-scenario-locked').checked,
    hidden: $('wi-scenario-hidden').checked,
    changes: cells.map((cell, index) => ({ cell, input: values[index] })),
  };
}

async function whatIfSaveScenario(dialog) {
  const output = $('wi-scenario-result');
  whatIfSetBusy(dialog, true);
  try {
    const scenario = whatIfCollectScenario();
    const body = whatIfScenarioEditing
      ? { op: 'update', kind: 'scenario', sheet: scenario.changes[0].cell.sheet, id: whatIfScenarioEditing, scenario }
      : { op: 'create', kind: 'scenario', sheet: scenario.changes[0].cell.sheet, scenario };
    await apiPost('/api/what-if', body);
    whatIfScenarioForm();
    await whatIfLoadScenarios(dialog);
    output.textContent = '方案已保存；创建/更新同样进入统一撤销历史。';
  } catch (error) {
    output.textContent = error.message;
    output.dataset.status = 'error';
  } finally {
    whatIfSetBusy(dialog, false);
  }
}

async function whatIfScenarioAction(dialog, id, action) {
  const scenario = whatIfScenarioRows.find((item) => item.id === id);
  if (!scenario) return;
  if (action === 'edit') {
    whatIfScenarioForm(scenario);
    return;
  }
  if (action === 'delete' && !confirm(`删除方案“${scenario.name}”？`)) return;
  const output = $('wi-scenario-result');
  whatIfSetBusy(dialog, true);
  try {
    const result = await apiPost('/api/what-if', {
      op: action, kind: 'scenario', id, sheet: scenario.changes?.[0]?.cell?.sheet ?? S.sheet,
    });
    if (action === 'delete') {
      await whatIfLoadScenarios(dialog);
      output.textContent = `已删除方案“${scenario.name}”。`;
    } else if (action === 'preview') {
      output.textContent = scenario.changes.map((change, index) => {
        const before = result.before?.[index] ?? '';
        const after = result.after?.[index] ?? '';
        return `${cellRef(change.cell.row, change.cell.column)}: ${before} → ${after}`;
      }).join('\n');
    } else if (action === 'apply') {
      scheduleRefresh(true);
      output.textContent = `已应用方案“${scenario.name}”；所有可变单元格可用一次 Ctrl+Z 撤销。`;
    }
  } catch (error) {
    output.textContent = error.message;
    output.dataset.status = 'error';
  } finally {
    whatIfSetBusy(dialog, false);
  }
}

async function whatIfCaptureSelection() {
  const n = normSel();
  const count = (n.r1 - n.r0 + 1) * (n.c1 - n.c0 + 1);
  if (count > 32) throw new Error('Excel 方案最多包含 32 个可变单元格');
  const refs = [], requests = [];
  for (let row = n.r0; row <= n.r1; row++) for (let column = n.c0; column <= n.c1; column++) {
    refs.push(cellRef(row, column));
    requests.push(api(`/api/cell?sheet=${S.sheet}&row=${row}&col=${column}`));
  }
  const cells = await Promise.all(requests);
  $('wi-scenario-cells').value = refs.join(',');
  $('wi-scenario-values').value = cells.map((cell) => cell.content).join('\n');
}

function whatIfSyncTableKind() {
  const two = $('wi-table-kind').value === 'twoVariable';
  $('wi-table-one-fields').hidden = two;
  $('wi-table-two-fields').hidden = !two;
}

function openWhatIfDialog() {
  $('what-if-dialog')?.remove();
  const dialog = document.createElement('div');
  dialog.id = 'what-if-dialog';
  dialog.className = 'what-if-dialog';
  dialog.setAttribute('role', 'dialog');
  dialog.setAttribute('aria-modal', 'true');
  dialog.setAttribute('aria-labelledby', 'wi-title');
  const current = cellRef(S.cur.r, S.cur.c);
  const alternativeColumn = S.cur.c > 10 ? S.cur.c - 10 : Math.min(16384, S.cur.c + 10);
  const secondAlternativeColumn = alternativeColumn > 1 ? alternativeColumn - 1 : Math.min(16384, alternativeColumn + 1);
  const alternative = cellRef(S.cur.r, alternativeColumn);
  const alternative2 = cellRef(S.cur.r, secondAlternativeColumn);
  dialog.innerHTML = `
    <div class="wi-window">
      <div class="wi-titlebar"><div><strong id="wi-title">假设分析</strong><span>隔离预览 · 原子写回 · 统一撤销</span></div><button type="button" class="wi-close" aria-label="关闭">×</button></div>
      <div class="wi-tabs" role="tablist">
        <button type="button" class="active" data-wi-tab="goal">目标求解</button>
        <button type="button" data-wi-tab="table">数据表</button>
        <button type="button" data-wi-tab="scenario">方案管理器</button>
      </div>
      <section class="wi-panel active" data-wi-panel="goal">
        <div class="wi-grid">
          <label>目标单元格<input id="wi-goal-target" value="${current}" spellcheck="false"></label>
          <label>目标值<input id="wi-goal-value" value="0" inputmode="decimal"></label>
          <label>可变单元格<input id="wi-goal-changing" value="${alternative}" spellcheck="false"></label>
          <label>初始值（可选）<input id="wi-goal-initial" inputmode="decimal"></label>
          <label>下界（可选）<input id="wi-goal-lower" inputmode="decimal"></label>
          <label>上界（可选）<input id="wi-goal-upper" inputmode="decimal"></label>
          <label>容差<input id="wi-goal-tolerance" value="1e-8" inputmode="decimal"></label>
          <label>最大迭代次数<input id="wi-goal-iterations" value="100" inputmode="numeric"></label>
        </div>
        <p class="wi-help">目标必须是公式，可变单元格必须是数值或空白。预览始终在工作簿克隆中执行；只有收敛后才能写回。</p>
        <div class="wi-actions"><button type="button" id="wi-goal-preview">预览</button><button type="button" id="wi-goal-apply" class="primary">求解并写回</button></div>
        <pre id="wi-goal-result" class="wi-result" aria-live="polite"></pre>
      </section>
      <section class="wi-panel" data-wi-panel="table">
        <div class="wi-grid">
          <label>类型<select id="wi-table-kind"><option value="oneVariable">单变量</option><option value="twoVariable">双变量</option></select></label>
          <label>结果公式单元格<input id="wi-table-formula" value="${current}" spellcheck="false"></label>
          <label>输出左上角<input id="wi-table-output" value="${current}" spellcheck="false"></label>
          <label>排列方向<select id="wi-table-orientation"><option value="column">按列</option><option value="row">按行</option></select></label>
        </div>
        <div id="wi-table-one-fields" class="wi-grid">
          <label>输入单元格<input id="wi-table-input" value="${alternative}" spellcheck="false"></label>
          <label class="wide">假设值（逗号/换行分隔）<textarea id="wi-table-values" rows="3">1,2,3</textarea></label>
        </div>
        <div id="wi-table-two-fields" class="wi-grid" hidden>
          <label>行输入单元格<input id="wi-table-row-input" value="${alternative}" spellcheck="false"></label>
          <label>列输入单元格<input id="wi-table-column-input" value="${alternative2}" spellcheck="false"></label>
          <label>行假设值<textarea id="wi-table-row-values" rows="3">1,2,3</textarea></label>
          <label>列假设值<textarea id="wi-table-column-values" rows="3">10,20,30</textarea></label>
        </div>
        <p class="wi-help">数据表在隔离模型中计算。当前引擎写入公式标题、假设值与静态结果快照，不伪装成 Excel 的隐藏动态 dataTable 公式。</p>
        <div class="wi-actions"><button type="button" id="wi-table-preview">预览</button><button type="button" id="wi-table-apply" class="primary">写入工作表</button></div>
        <div id="wi-table-result" class="wi-result" aria-live="polite"></div>
      </section>
      <section class="wi-panel" data-wi-panel="scenario">
        <div class="wi-scenario-layout">
          <div><h3>工作簿方案</h3><div id="wi-scenario-list" class="wi-scenario-list"></div></div>
          <div><h3>方案定义</h3>
            <div class="wi-grid">
              <label>名称<input id="wi-scenario-name" maxlength="255"></label>
              <label>可变单元格<input id="wi-scenario-cells" value="${current}" spellcheck="false"></label>
              <label class="wide">对应输入（每行一个）<textarea id="wi-scenario-values" rows="4"></textarea></label>
              <label class="wide">备注<textarea id="wi-scenario-comment" rows="2"></textarea></label>
            </div>
            <div class="wi-checks"><label><input type="checkbox" id="wi-scenario-locked"> 锁定</label><label><input type="checkbox" id="wi-scenario-hidden"> 隐藏</label></div>
            <div class="wi-actions"><button type="button" id="wi-scenario-capture">读取当前选区</button><button type="button" id="wi-scenario-cancel-edit" hidden>取消编辑</button><button type="button" id="wi-scenario-save" class="primary">创建方案</button></div>
          </div>
        </div>
        <pre id="wi-scenario-result" class="wi-result" aria-live="polite"></pre>
      </section>
    </div>`;
  document.body.appendChild(dialog);

  const close = () => dialog.remove();
  dialog.querySelector('.wi-close').onclick = close;
  dialog.addEventListener('mousedown', (event) => { if (event.target === dialog) close(); });
  dialog.addEventListener('keydown', (event) => { if (event.key === 'Escape') close(); });
  dialog.querySelectorAll('[data-wi-tab]').forEach((button) => button.onclick = () => {
    dialog.querySelectorAll('[data-wi-tab]').forEach((item) => item.classList.toggle('active', item === button));
    dialog.querySelectorAll('[data-wi-panel]').forEach((panel) => panel.classList.toggle('active', panel.dataset.wiPanel === button.dataset.wiTab));
    if (button.dataset.wiTab === 'scenario') void whatIfLoadScenarios(dialog);
  });
  $('wi-goal-preview').onclick = () => whatIfRunGoal(dialog, 'preview');
  $('wi-goal-apply').onclick = () => whatIfRunGoal(dialog, 'apply');
  $('wi-table-preview').onclick = () => whatIfRunDataTable(dialog, 'preview');
  $('wi-table-apply').onclick = () => whatIfRunDataTable(dialog, 'apply');
  $('wi-table-kind').onchange = whatIfSyncTableKind;
  $('wi-scenario-save').onclick = () => whatIfSaveScenario(dialog);
  $('wi-scenario-cancel-edit').onclick = () => whatIfScenarioForm();
  $('wi-scenario-capture').onclick = async () => {
    try { await whatIfCaptureSelection(); }
    catch (error) { $('wi-scenario-result').textContent = error.message; }
  };
  $('wi-scenario-list').onclick = (event) => {
    const button = event.target.closest('button[data-action]');
    const row = button?.closest('.wi-scenario-row');
    if (button && row) void whatIfScenarioAction(dialog, row.dataset.id, button.dataset.action);
  };
  whatIfScenarioForm();
  dialog.querySelector('input')?.focus();
}

if ($('btn-what-if')) $('btn-what-if').onclick = openWhatIfDialog;
window.__unicellWhatIfTest = {
  address: whatIfAddress,
  values: whatIfValues,
  scenarioCells: whatIfScenarioCells,
  goalRequest: whatIfGoalRequest,
  dataTableRequest: whatIfDataTableRequest,
};
initRichClipboardUi();
syncZoomLevel();

window.UniCellFiles = { export: () => createPersistenceBlob("udoc"), open: importWorkbookFile, basename: currentBasename };
