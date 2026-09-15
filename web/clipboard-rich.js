// UniCell high-fidelity clipboard bridge.
//
// Chrome exposes application-specific clipboard formats through the `web ` prefix.
// We also keep an in-memory copy because browser/OS clipboard implementations can strip
// custom formats while keeping text/html and text/plain.
'use strict';

const UNICELL_CLIPBOARD_MIME = 'web application/x-unicell+json';
const UNICELL_CLIPBOARD_BASE_MIME = 'application/x-unicell+json';
let unicellClipboardMemory = null;
let pendingPasteSpecialSource = null;
let pendingPasteSpecialReplacesLast = false;
let lastPasteOptionsSource = null;

function validUniCellClipboard(value) {
  return !!value && value.version === 1 && value.kind === 'unicell-range'
    && value.clip && Number.isFinite(Number(value.height)) && Number.isFinite(Number(value.width));
}

function parseUniCellClipboard(value) {
  try {
    const parsed = typeof value === 'string' ? JSON.parse(value) : value;
    return validUniCellClipboard(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

function rememberRichClipboard(data) {
  const unicell = parseUniCellClipboard(data?.unicell);
  unicellClipboardMemory = {
    text: String(data?.tsv ?? ''),
    html: String(data?.html ?? ''),
    unicell,
    writtenAt: Date.now(),
  };
  return unicellClipboardMemory;
}

function clipboardBlob(text, type) {
  return new Blob([String(text ?? '')], { type });
}

function richClipboardRepresentations(data, includeCustom = true) {
  const result = {
    'text/plain': clipboardBlob(data?.tsv ?? '', 'text/plain'),
    'text/html': clipboardBlob(data?.html ?? '', 'text/html'),
  };
  const unicell = parseUniCellClipboard(data?.unicell);
  if (includeCustom && unicell) {
    result[UNICELL_CLIPBOARD_MIME] = clipboardBlob(
      JSON.stringify(unicell),
      UNICELL_CLIPBOARD_BASE_MIME,
    );
  }
  return result;
}

// Returns the actual fidelity tier used. Dependency injection keeps this independently
// testable without replacing the browser's read-only navigator.clipboard property.
async function writeRichClipboard(data, clipboard = navigator.clipboard,
  ClipboardItemCtor = globalThis.ClipboardItem) {
  rememberRichClipboard(data);
  if (clipboard?.write && ClipboardItemCtor) {
    try {
      await clipboard.write([new ClipboardItemCtor(richClipboardRepresentations(data, true))]);
      return 'custom+html+text';
    } catch {
      // Firefox and older Chromium builds reject web custom formats. HTML remains the
      // cross-application fidelity path in that case.
      try {
        await clipboard.write([new ClipboardItemCtor(richClipboardRepresentations(data, false))]);
        return 'html+text';
      } catch {}
    }
  }
  if (clipboard?.writeText) {
    await clipboard.writeText(String(data?.tsv ?? ''));
    return 'text';
  }
  throw new Error('当前浏览器不支持写入系统剪贴板');
}

async function clipboardItemText(item, type) {
  if (!item?.types?.includes(type)) return '';
  return (await item.getType(type)).text();
}

function memoryMatchesClipboard(text, html) {
  if (!unicellClipboardMemory?.unicell) return false;
  // A matching HTML fragment is strongest. Matching plain text is the deliberate same-tab
  // fallback for browsers that discarded the custom MIME type during the copy operation.
  if (html && unicellClipboardMemory.html && html === unicellClipboardMemory.html) return true;
  return !!text && text === unicellClipboardMemory.text;
}

async function readRichClipboard(clipboard = navigator.clipboard) {
  let text = '';
  let html = '';
  let unicell = null;
  if (clipboard?.read) {
    const items = await clipboard.read();
    for (const item of items) {
      if (!unicell) {
        const customType = item.types?.includes(UNICELL_CLIPBOARD_MIME)
          ? UNICELL_CLIPBOARD_MIME
          : (item.types?.includes(UNICELL_CLIPBOARD_BASE_MIME)
            ? UNICELL_CLIPBOARD_BASE_MIME : '');
        if (customType) unicell = parseUniCellClipboard(await clipboardItemText(item, customType));
      }
      if (!html) html = await clipboardItemText(item, 'text/html');
      if (!text) text = await clipboardItemText(item, 'text/plain');
    }
  }
  if (!text && clipboard?.readText) text = await clipboard.readText();
  if (!unicell && memoryMatchesClipboard(text, html)) unicell = unicellClipboardMemory.unicell;
  return { text, html, unicell, source: unicell ? 'unicell' : (html ? 'html' : 'text') };
}

function clipboardDataTransferPayload(dataTransfer) {
  if (!dataTransfer) return { text: '', html: '', unicell: null, source: 'text' };
  const get = (type) => {
    try { return dataTransfer.getData(type) || ''; }
    catch { return ''; }
  };
  const text = get('text/plain');
  const html = get('text/html');
  const custom = get(UNICELL_CLIPBOARD_MIME) || get(UNICELL_CLIPBOARD_BASE_MIME);
  let unicell = parseUniCellClipboard(custom);
  if (!unicell && memoryMatchesClipboard(text, html)) unicell = unicellClipboardMemory.unicell;
  return { text, html, unicell, source: unicell ? 'unicell' : (html ? 'html' : 'text') };
}

function stripClipboardTransportHeader(html) {
  const value = String(html || '');
  const start = value.search(/<(?:!doctype|html|head|body|table)\b/i);
  return start > 0 ? value.slice(start) : value;
}

function sanitizedClipboardDocument(html) {
  const parsed = new DOMParser().parseFromString(stripClipboardTransportHeader(html), 'text/html');
  parsed.querySelectorAll('script,link,meta[http-equiv],base,object,embed,iframe,frame,form')
    .forEach((node) => node.remove());
  parsed.querySelectorAll('*').forEach((element) => {
    for (const attribute of [...element.attributes]) {
      const name = attribute.name.toLowerCase();
      if (name.startsWith('on') || ['srcdoc', 'formaction', 'action'].includes(name)) {
        element.removeAttribute(attribute.name);
      }
      if (['src', 'href', 'poster', 'background'].includes(name)
          && /^(?:javascript|data|https?|file):/i.test(attribute.value.trim())) {
        element.removeAttribute(attribute.name);
      }
    }
    const inline = element.getAttribute('style');
    if (inline) {
      element.setAttribute('style', inline
        .replace(/expression\s*\([^)]*\)/gi, '')
        .replace(/url\s*\([^)]*\)/gi, 'none')
        .replace(/(?:behavior|-moz-binding)\s*:[^;]*/gi, ''));
    }
  });
  parsed.querySelectorAll('style').forEach((style) => {
    style.textContent = String(style.textContent || '')
      .replace(/@import[^;]+;/gi, '')
      .replace(/url\s*\([^)]*\)/gi, 'none')
      .replace(/expression\s*\([^)]*\)/gi, '');
  });
  return '<!doctype html>' + parsed.documentElement.outerHTML;
}

async function clipboardRenderDocument(html) {
  const iframe = document.createElement('iframe');
  iframe.className = 'clipboard-sandbox';
  iframe.setAttribute('sandbox', 'allow-same-origin'); // no allow-scripts
  iframe.setAttribute('aria-hidden', 'true');
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      iframe.remove();
      reject(new Error('Excel HTML 解析超时'));
    }, 2000);
    iframe.onload = () => {
      clearTimeout(timer);
      try { resolve({ document: iframe.contentDocument, iframe }); }
      catch (error) { iframe.remove(); reject(error); }
    };
    iframe.srcdoc = sanitizedClipboardDocument(html);
    document.body.appendChild(iframe);
  });
}

function cssHexColor(input) {
  const value = String(input || '').trim();
  if (!value || value === 'transparent' || value === 'rgba(0, 0, 0, 0)') return null;
  if (/^#[\da-f]{6}$/i.test(value)) return value.toUpperCase();
  if (/^#[\da-f]{3}$/i.test(value)) {
    return ('#' + [...value.slice(1)].map((part) => part + part).join('')).toUpperCase();
  }
  const match = /^rgba?\(\s*([\d.]+)[, ]+\s*([\d.]+)[, ]+\s*([\d.]+)(?:\s*[,/]\s*([\d.]+))?\s*\)$/i.exec(value);
  if (!match || (match[4] != null && Number(match[4]) <= 0)) return null;
  const hex = (number) => Math.max(0, Math.min(255, Math.round(Number(number))))
    .toString(16).padStart(2, '0');
  return `#${hex(match[1])}${hex(match[2])}${hex(match[3])}`.toUpperCase();
}

function walkCssRules(rules, visit) {
  for (const rule of [...(rules || [])]) {
    if (rule.selectorText && rule.style) visit(rule);
    if (rule.cssRules) walkCssRules(rule.cssRules, visit);
  }
}

function rawCssProperty(declarations, property) {
  const wanted = String(property || '').trim().toLowerCase();
  let quote = '', depth = 0, token = '';
  const entries = [];
  for (const character of `${String(declarations || '')};`) {
    if (quote) {
      token += character;
      if (character === quote && token[token.length - 2] !== '\\') quote = '';
      continue;
    }
    if (character === '"' || character === "'") { quote = character; token += character; continue; }
    if (character === '(') { depth++; token += character; continue; }
    if (character === ')') { depth = Math.max(0, depth - 1); token += character; continue; }
    if (character === ';' && depth === 0) { entries.push(token); token = ''; continue; }
    token += character;
  }
  let result = '';
  for (const entry of entries) {
    const colon = entry.indexOf(':');
    if (colon < 0 || entry.slice(0, colon).trim().toLowerCase() !== wanted) continue;
    result = entry.slice(colon + 1).trim().replace(/\s*!important\s*$/i, '').trim();
  }
  return result;
}

function rawStylesheetProperty(element, property) {
  let result = '';
  for (const style of element.ownerDocument.querySelectorAll('style')) {
    const source = String(style.textContent || '');
    const blocks = /([^{}]+)\{([^{}]*)\}/g;
    let match;
    while ((match = blocks.exec(source))) {
      const selector = match[1].trim();
      if (!selector || selector.startsWith('@')) continue;
      try {
        if (element.matches(selector)) result = rawCssProperty(match[2], property) || result;
      } catch {}
    }
  }
  return result;
}

// Excel HTML relies on non-standard mso-* declarations. Browsers retain them in a rule's
// CSSStyleDeclaration but do not expose them through getComputedStyle, so resolve the simple
// class/table cascade explicitly. Excel-generated selectors are intentionally uncomplicated.
function declaredCssProperty(element, property) {
  // CSSOM drops some Office-only declarations (notably mso-number-format) in
  // Chromium.  Keep a source-text fallback so Excel formats survive HTML
  // clipboard transfer even when the browser does not recognise the property.
  let result = rawStylesheetProperty(element, property);
  for (const sheet of [...element.ownerDocument.styleSheets]) {
    try {
      walkCssRules(sheet.cssRules, (rule) => {
        try {
          if (element.matches(rule.selectorText)) {
            const candidate = rule.style.getPropertyValue(property);
            if (candidate) result = candidate;
          }
        } catch {}
      });
    } catch {}
  }
  const inline = element.style?.getPropertyValue(property)
    || rawCssProperty(element.getAttribute('style'), property);
  return (inline || result || '').trim();
}

function cleanMsoNumberFormat(value) {
  let result = String(value || '').trim();
  if ((result.startsWith('"') && result.endsWith('"'))
      || (result.startsWith("'") && result.endsWith("'"))) result = result.slice(1, -1);
  return result.replace(/\\([@"'])/g, '$1').replace(/\\-/g, '-').trim() || 'general';
}

function borderStyleFromCss(style, width) {
  const pixels = parseFloat(width) || 0;
  if (style === 'none' || pixels <= 0) return null;
  if (style === 'double') return 'double';
  if (style === 'dotted') return 'dotted';
  if (style === 'dashed') return pixels >= 2 ? 'mediumdashed' : 'dotted';
  if (pixels >= 3) return 'thick';
  if (pixels >= 2) return 'medium';
  return 'thin';
}

function engineBorderItem(computed, side) {
  const cap = side[0].toUpperCase() + side.slice(1);
  const style = borderStyleFromCss(computed[`border${cap}Style`], computed[`border${cap}Width`]);
  if (!style) return null;
  const color = cssHexColor(computed[`border${cap}Color`]);
  return color ? { style, color } : { style };
}

function computedPointSize(computed) {
  const value = parseFloat(computed.fontSize) || 16;
  return Math.max(1, Math.round(value * 72 / 96));
}

function engineFont(computed) {
  const color = cssHexColor(computed.color);
  const decoration = `${computed.textDecorationLine || ''} ${computed.textDecoration || ''}`;
  const result = {
    sz: computedPointSize(computed),
    name: String(computed.fontFamily || 'Calibri').split(',')[0].replace(/^['"]|['"]$/g, '').trim() || 'Calibri',
    family: /mono/i.test(computed.fontFamily) ? 3 : 2,
    scheme: 'none',
  };
  if (Number(computed.fontWeight) >= 600 || /bold/i.test(computed.fontWeight)) result.b = true;
  if (/italic|oblique/i.test(computed.fontStyle)) result.i = true;
  if (/underline/i.test(decoration)) result.u = true;
  if (/line-through/i.test(decoration)) result.strike = true;
  if (color) result.color = color;
  return result;
}

function engineCellStyle(cell) {
  const computed = cell.ownerDocument.defaultView.getComputedStyle(cell);
  const horizontal = ({ center: 'center', right: 'right', left: 'left', justify: 'justify' })[computed.textAlign];
  const vertical = ({ middle: 'center', top: 'top', bottom: 'bottom', justify: 'justify' })[computed.verticalAlign];
  const explicitWhiteSpace = declaredCssProperty(cell, 'white-space');
  const msoWrap = declaredCssProperty(cell, 'mso-wrap-text').toLowerCase();
  const alignment = {};
  if (horizontal) alignment.horizontal = horizontal;
  if (vertical && vertical !== 'bottom') alignment.vertical = vertical;
  if (msoWrap === 'yes' || /pre-wrap|break-spaces/.test(explicitWhiteSpace)) alignment.wrap_text = true;
  const fillColor = cssHexColor(computed.backgroundColor);
  const border = {};
  for (const side of ['left', 'right', 'top', 'bottom']) {
    const item = engineBorderItem(computed, side);
    if (item) border[side] = item;
  }
  return {
    ...(Object.keys(alignment).length ? { alignment } : {}),
    num_fmt: cleanMsoNumberFormat(declaredCssProperty(cell, 'mso-number-format')),
    fill: fillColor ? { color: fillColor } : {},
    font: engineFont(computed),
    border,
    quote_prefix: (cell.hasAttribute('x:str') || cell.hasAttribute('data-string'))
      && clipboardCellText(cell).startsWith('='),
  };
}

function richRunStyle(element) {
  const computed = element.ownerDocument.defaultView.getComputedStyle(element);
  const font = engineFont(computed);
  return {
    bold: !!font.b,
    italic: !!font.i,
    underline: !!font.u,
    strike: !!font.strike,
    size: font.sz,
    font: font.name,
    ...(font.color ? { color: font.color } : {}),
  };
}

function richRunsFromClipboardCell(cell) {
  const runs = [];
  const blockTags = new Set(['DIV', 'P', 'LI']);
  const append = (text, style) => {
    if (!text) return;
    const previous = runs[runs.length - 1];
    const key = JSON.stringify(style);
    if (previous && previous._key === key) previous.text += text;
    else runs.push({ text, ...style, _key: key });
  };
  const visit = (node, inheritedElement) => {
    if (node.nodeType === 3) {
      append(node.nodeValue || '', richRunStyle(inheritedElement || cell));
      return;
    }
    if (node.nodeType !== 1) return;
    if (node.tagName === 'BR') {
      append('\n', richRunStyle(inheritedElement || cell));
      return;
    }
    for (const child of node.childNodes) visit(child, node);
    if (blockTags.has(node.tagName)) append('\n', richRunStyle(node));
  };
  for (const child of cell.childNodes) visit(child, cell);
  const displayed = clipboardCellText(cell);
  let joined = runs.map((run) => run.text).join('');
  if (joined.endsWith('\n') && joined.slice(0, -1) === displayed) {
    const last = runs[runs.length - 1];
    last.text = last.text.slice(0, -1);
    if (!last.text) runs.pop();
    joined = displayed;
  }
  // If browser whitespace collapsing differs from the source tree, prefer the cell-level
  // value over a rich sidecar whose run offsets would no longer align with that value.
  if (joined !== displayed) return [];
  for (const run of runs) delete run._key;
  const styleKeys = new Set(runs.map((run) => JSON.stringify({ ...run, text: undefined })));
  const explicitlySegmented = !!cell.querySelector('span,font,b,strong,i,em,u,s,strike');
  return runs.length > 1 && (styleKeys.size > 1 || explicitlySegmented) ? runs : [];
}

function clipboardCellText(cell) {
  let value = '';
  const blockTags = new Set(['DIV', 'P', 'LI']);
  const visit = (node) => {
    if (node.nodeType === 3) {
      value += node.nodeValue || '';
      return;
    }
    if (node.nodeType !== 1) return;
    if (node.tagName === 'BR') {
      value += '\n';
      return;
    }
    const start = value.length;
    for (const child of node.childNodes) visit(child);
    if (blockTags.has(node.tagName) && value.length > start && !value.endsWith('\n')) value += '\n';
  };
  for (const child of cell.childNodes) visit(child);
  if (value.endsWith('\n') && !cell.lastElementChild?.matches?.('br')) value = value.slice(0, -1);
  return value.replace(/\r/g, '').replace(/\u00a0/g, ' ');
}

function clipboardCellFormula(cell) {
  return cell.getAttribute('x:fmla') || cell.getAttribute('x:formula')
    || cell.getAttribute('data-formula') || cell.getAttribute('data-fmla') || '';
}

function clipboardColumnName(column) {
  let value = '';
  for (let current = column; current > 0; current = Math.floor((current - 1) / 26)) {
    value = String.fromCharCode(65 + ((current - 1) % 26)) + value;
  }
  return value;
}

// Excel puts R1C1 expressions in x:fmla for many clipboard transfers. Convert them to an
// A1 expression anchored at the synthetic clipboard range so IronCalc can apply its normal
// relative-reference translation when the range is pasted elsewhere.
function clipboardR1C1ToA1(formula, baseRow, baseColumn) {
  const source = String(formula || '');
  let output = '';
  let index = 0;
  let inString = false;
  while (index < source.length) {
    if (source[index] === '"') {
      output += source[index++];
      if (inString && source[index] === '"') output += source[index++];
      else inString = !inString;
      continue;
    }
    if (!inString && /[Rr]/.test(source[index])) {
      const previous = source[index - 1] || '';
      const match = /^R(?:\[(-?\d+)\]|(\d+))?C(?:\[(-?\d+)\]|(\d+))?/i.exec(source.slice(index));
      const next = match ? source[index + match[0].length] || '' : '';
      if (match && !/[A-Za-z0-9_.]/.test(previous) && !/[A-Za-z0-9_]/.test(next)) {
        const rowAbsolute = match[2] != null;
        const columnAbsolute = match[4] != null;
        const row = rowAbsolute ? Number(match[2]) : baseRow + Number(match[1] || 0);
        const column = columnAbsolute ? Number(match[4]) : baseColumn + Number(match[3] || 0);
        output += row > 0 && column > 0
          ? `${columnAbsolute ? '$' : ''}${clipboardColumnName(column)}${rowAbsolute ? '$' : ''}${row}`
          : '#REF!';
        index += match[0].length;
        continue;
      }
    }
    output += source[index++];
  }
  return output;
}

function clipboardCellRawValue(cell, displayed, sourceRow = 1, sourceColumn = 1) {
  const formula = clipboardCellFormula(cell);
  if (formula) {
    const normalized = formula.startsWith('=') ? formula : `=${formula}`;
    return clipboardR1C1ToA1(normalized, sourceRow, sourceColumn);
  }
  if ((cell.hasAttribute('x:str') || cell.hasAttribute('data-string')) && displayed.startsWith('=')) {
    return `'${displayed}`;
  }
  const numeric = cell.getAttribute('x:num') || cell.getAttribute('data-value');
  return numeric == null ? displayed : numeric;
}

// Excel's HTML clipboard keeps the evaluated scalar separately from the formatted text.
// Paste Special > Values must consume that scalar (for example 1.25), not the displayed
// text (for example "1.3" after a 0.0 number format), or a copy/paste silently loses data.
function clipboardCellEvaluatedValue(cell, displayed) {
  const boolean = cell.getAttribute('x:bool') ?? cell.getAttribute('x:boolean')
    ?? cell.getAttribute('data-boolean');
  if (boolean != null) {
    const normalized = String(boolean).trim().toLowerCase();
    if (normalized === '1' || normalized === 'true') return true;
    if (normalized === '0' || normalized === 'false') return false;
  }
  const numeric = cell.getAttribute('x:num') ?? cell.getAttribute('data-value');
  if (numeric != null && String(numeric).trim() !== '') {
    const value = Number(numeric);
    if (Number.isFinite(value)) return value;
  }
  return displayed;
}

function buildHtmlTableGrid(table) {
  const anchors = [];
  const occupied = [];
  let width = 0;
  let rowIndex = 0;
  for (const tr of [...table.rows]) {
    occupied[rowIndex] ||= [];
    let column = 0;
    for (const cell of [...tr.cells]) {
      while (occupied[rowIndex][column]) column++;
      const rowspan = Math.max(1, Number(cell.rowSpan) || 1);
      const colspan = Math.max(1, Number(cell.colSpan) || 1);
      anchors.push({ row: rowIndex, col: column, rowspan, colspan, cell });
      for (let dr = 0; dr < rowspan; dr++) {
        occupied[rowIndex + dr] ||= [];
        for (let dc = 0; dc < colspan; dc++) occupied[rowIndex + dr][column + dc] = true;
      }
      column += colspan;
      width = Math.max(width, column);
    }
    rowIndex++;
  }
  return { anchors, height: Math.max(rowIndex, occupied.length), width };
}

async function parseExcelClipboardHtml(html) {
  if (!/<(?:table|td|th)\b/i.test(String(html || ''))) return null;
  const rendered = await clipboardRenderDocument(html);
  try {
    const tables = [...rendered.document.querySelectorAll('table')];
    const table = tables.sort((a, b) => b.querySelectorAll('td,th').length - a.querySelectorAll('td,th').length)[0];
    if (!table) return null;
    const grid = buildHtmlTableGrid(table);
    if (!grid.height || !grid.width) return null;
    const defaultStyle = {
      num_fmt: 'general', fill: {},
      font: { sz: 12, name: 'Calibri', family: 2, scheme: 'none' },
      border: {}, quote_prefix: false,
    };
    const data = {};
    const displayMatrix = Array.from({ length: grid.height }, () => Array(grid.width).fill(''));
    const rawMatrix = Array.from({ length: grid.height }, () => Array(grid.width).fill(''));
    const valueMatrix = Array.from({ length: grid.height }, () => Array(grid.width).fill(null));
    for (let row = 1; row <= grid.height; row++) {
      data[row] = {};
      for (let column = 1; column <= grid.width; column++) {
        data[row][column] = { text: '', is_spill: false, style: defaultStyle };
      }
    }
    const richText = [];
    const merges = [];
    for (const anchor of grid.anchors) {
      const displayed = clipboardCellText(anchor.cell);
      const raw = clipboardCellRawValue(anchor.cell, displayed, anchor.row + 1, anchor.col + 1);
      const style = engineCellStyle(anchor.cell);
      data[anchor.row + 1][anchor.col + 1] = { text: raw, is_spill: false, style };
      displayMatrix[anchor.row][anchor.col] = displayed;
      rawMatrix[anchor.row][anchor.col] = raw;
      valueMatrix[anchor.row][anchor.col] = clipboardCellEvaluatedValue(anchor.cell, displayed);
      if (!clipboardCellFormula(anchor.cell)) {
        const runs = richRunsFromClipboardCell(anchor.cell);
        if (runs.length) richText.push({ r: anchor.row, c: anchor.col, runs });
      }
      if (anchor.rowspan > 1 || anchor.colspan > 1) {
        merges.push({
          r0: anchor.row, c0: anchor.col,
          r1: anchor.row + anchor.rowspan - 1,
          c1: anchor.col + anchor.colspan - 1,
        });
      }
    }
    const display = displayMatrix.map((row) => row.join('\t')).join('\n');
    const raw = rawMatrix.map((row) => row.join('\t')).join('\n');
    return {
      version: 1,
      kind: 'unicell-range',
      originRow: 1,
      originCol: 1,
      clip: { csv: display, data, sheet: 0, range: [1, 1, grid.height, grid.width] },
      display,
      raw,
      values: valueMatrix,
      height: grid.height,
      width: grid.width,
      richText,
      merges,
      validations: [],
      objects: [],
    };
  } finally {
    rendered.iframe.remove();
  }
}

async function resolveClipboardSource(source) {
  const input = source || { text: '', html: '', unicell: null };
  let unicell = parseUniCellClipboard(input.unicell);
  if (!unicell && input.html) {
    try { unicell = await parseExcelClipboardHtml(input.html); }
    catch (error) { console.warn('Excel HTML clipboard parse failed:', error); }
  }
  return {
    text: String(input.text || unicell?.display || ''),
    html: String(input.html || ''),
    unicell,
    source: unicell ? (input.unicell ? 'unicell' : 'html') : 'text',
  };
}

function pasteRequestFromSource(source, special = 'all', state = S) {
  const request = {
    sheet: state.sheet,
    row: state.cur.r,
    col: state.cur.c,
    text: String(source?.text || source?.unicell?.display || ''),
    mode: state.cutPending && special === 'all' ? 'cut' : 'copy',
    special,
  };
  const unicell = parseUniCellClipboard(source?.unicell);
  if (unicell) request.unicell = unicell;
  return request;
}

// Pure clipboard contracts used by the browser self-test.  Keeping these hooks free of DOM
// and Clipboard API access lets the suite prove that a pending cut survives a sheet switch and
// can never be silently downgraded to a text copy when the rich payload is unavailable.
window.__unicellClipboardTest = {
  valid: validUniCellClipboard,
  parse: parseUniCellClipboard,
  request: pasteRequestFromSource,
};

async function performRichPaste(source, special = 'all', post = apiPost, state = S) {
  const resolved = await resolveClipboardSource(source);
  if (!resolved.text && !resolved.unicell) return null;
  const request = pasteRequestFromSource(resolved, special, state);
  const result = await post('/api/paste', request);
  if (state === S) {
    state.cutPending = null;
    state.sel = {
      r0: state.cur.r, c0: state.cur.c,
      r1: state.cur.r + (result.rows || 1) - 1,
      c1: state.cur.c + (result.cols || 1) - 1,
    };
    scheduleRefresh(true);
    if (special === 'all') showPasteOptionsFlyout(resolved);
  }
  return { result, request, source: resolved };
}

async function handleRichClipboardPasteEvent(event) {
  const source = clipboardDataTransferPayload(event.clipboardData);
  return performRichPaste(source, 'all');
}

async function pasteRichFromClipboard(special = 'all', suppliedSource = null) {
  try {
    let source = suppliedSource;
    if (!source) {
      try { source = await readRichClipboard(); }
      catch (error) {
        if (unicellClipboardMemory) source = { ...unicellClipboardMemory, source: 'memory' };
        else throw error;
      }
    }
    return await performRichPaste(source, special);
  } catch (error) {
    setStatus('无法读取剪贴板：' + (error?.message || '请使用 Ctrl+V'));
    return null;
  }
}

function closePasteSpecialDialog() {
  const dialog = document.getElementById('paste-special-dialog');
  if (dialog) dialog.hidden = true;
  pendingPasteSpecialSource = null;
  pendingPasteSpecialReplacesLast = false;
}

async function openPasteSpecialDialog(source = null, replacesLastPaste = false) {
  const dialog = document.getElementById('paste-special-dialog');
  if (!dialog) return;
  pendingPasteSpecialSource = source;
  pendingPasteSpecialReplacesLast = replacesLastPaste;
  dialog.hidden = false;
  dialog.querySelector('[data-paste-special]')?.focus();
  if (!pendingPasteSpecialSource) {
    try { pendingPasteSpecialSource = await readRichClipboard(); }
    catch {
      pendingPasteSpecialSource = unicellClipboardMemory
        ? { ...unicellClipboardMemory, source: 'memory' } : null;
    }
  }
}

function showPasteOptionsFlyout(source) {
  lastPasteOptionsSource = source;
  const flyout = document.getElementById('paste-options-flyout');
  if (!flyout) return;
  const viewport = gridScroll.getBoundingClientRect();
  const x = viewport.left + colX(S.sel.c1 + 1) - gridScroll.scrollLeft;
  const y = viewport.top + rowY(S.sel.r1 + 1) - gridScroll.scrollTop;
  flyout.style.left = `${Math.max(4, Math.min(innerWidth - 44, x - 34))}px`;
  flyout.style.top = `${Math.max(4, Math.min(innerHeight - 40, y - 26))}px`;
  flyout.hidden = false;
}

function hidePasteOptionsFlyout() {
  const flyout = document.getElementById('paste-options-flyout');
  if (flyout) flyout.hidden = true;
}

function initRichClipboardUi() {
  document.getElementById('btn-paste-options')?.addEventListener('click', () => openPasteSpecialDialog());
  document.getElementById('paste-special-close')?.addEventListener('click', closePasteSpecialDialog);
  document.getElementById('paste-special-cancel')?.addEventListener('click', closePasteSpecialDialog);
  document.querySelectorAll('[data-paste-special]').forEach((button) => {
    button.addEventListener('click', async () => {
      const source = pendingPasteSpecialSource;
      const replacesLastPaste = pendingPasteSpecialReplacesLast;
      const special = button.dataset.pasteSpecial;
      closePasteSpecialDialog();
      // The small post-paste options button changes the just-completed paste rather than
      // stacking another paste on top of it. The application-wide transaction history makes
      // that an exact undo + reapply operation, including merges and rich sidecars.
      if (replacesLastPaste) await api('/api/undo', { method: 'POST' });
      await pasteRichFromClipboard(special, source);
    });
  });
  document.getElementById('paste-options-flyout-open')?.addEventListener('click', () => {
    hidePasteOptionsFlyout();
    openPasteSpecialDialog(lastPasteOptionsSource, true);
  });
  document.addEventListener('mousedown', (event) => {
    const flyout = document.getElementById('paste-options-flyout');
    if (flyout && !flyout.hidden && !flyout.contains(event.target)) hidePasteOptionsFlyout();
  });
  document.getElementById('paste-special-dialog')?.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      closePasteSpecialDialog();
      gridScroll.focus();
    }
  });
}
