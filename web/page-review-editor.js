/* Excel-native page layout, print, review and protection editor.
 *
 * The editor deliberately sends differential OOXML patches.  Unknown attributes, extension
 * nodes and vendor payloads never enter the form model and are therefore left untouched by the
 * Rust package editor.
 */
(() => {
  'use strict';

  const PAPER_SIZES = Object.freeze([
    ['1', 'Letter (8.5 × 11 in)'], ['5', 'Legal (8.5 × 14 in)'],
    ['3', 'Tabloid (11 × 17 in)'], ['4', 'Ledger (17 × 11 in)'],
    ['8', 'A3 (297 × 420 mm)'], ['9', 'A4 (210 × 297 mm)'],
    ['11', 'A5 (148 × 210 mm)'], ['12', 'B4 JIS (257 × 364 mm)'],
    ['13', 'B5 JIS (182 × 257 mm)'], ['14', 'Folio (8.5 × 13 in)'],
  ]);
  const MARGIN_FIELDS = Object.freeze(['left', 'right', 'top', 'bottom', 'header', 'footer']);
  const PAGE_SETUP_FIELDS = Object.freeze([
    'paperSize', 'orientation', 'scale', 'fitToWidth', 'fitToHeight', 'pageOrder',
    'firstPageNumber', 'useFirstPageNumber', 'blackAndWhite', 'draft', 'horizontalDpi', 'verticalDpi',
    'usePrinterDefaults', 'cellComments', 'errors', 'copies',
  ]);
  const PRINT_OPTION_FIELDS = Object.freeze([
    'gridLines', 'gridLinesSet', 'headings', 'horizontalCentered', 'verticalCentered',
  ]);
  const HEADER_ATTRIBUTES = Object.freeze([
    'differentOddEven', 'differentFirst', 'scaleWithDoc', 'alignWithMargins',
  ]);
  const HEADER_FIELDS = Object.freeze([
    'oddHeader', 'oddFooter', 'evenHeader', 'evenFooter', 'firstHeader', 'firstFooter',
  ]);
  const SHEET_PROTECTION_FLAG_FIELDS = Object.freeze([
    'sheet', 'objects', 'scenarios', 'formatCells', 'formatColumns', 'formatRows',
    'insertColumns', 'insertRows', 'insertHyperlinks', 'deleteColumns', 'deleteRows',
    'selectLockedCells', 'sort', 'autoFilter', 'pivotTables', 'selectUnlockedCells',
  ]);
  const SHEET_PROTECTION_FIELDS = Object.freeze([
    ...SHEET_PROTECTION_FLAG_FIELDS, 'password', 'algorithmName', 'hashValue', 'saltValue', 'spinCount',
  ]);
  const WORKBOOK_PROTECTION_FIELDS = Object.freeze([
    'lockStructure', 'lockWindows', 'lockRevision', 'workbookPassword', 'revisionsPassword',
    'workbookAlgorithmName', 'workbookHashValue', 'workbookSaltValue', 'workbookSpinCount',
    'revisionsAlgorithmName', 'revisionsHashValue', 'revisionsSaltValue', 'revisionsSpinCount',
  ]);
  const PROTECTED_RANGE_FIELDS = Object.freeze([
    'name', 'sqref', 'securityDescriptor', 'password', 'algorithmName', 'hashValue', 'saltValue', 'spinCount',
  ]);
  const BREAK_FIELDS = Object.freeze(['id', 'min', 'max', 'man', 'pt']);
  const clone = (value) => value == null ? value : (typeof structuredClone === 'function'
    ? structuredClone(value) : JSON.parse(JSON.stringify(value)));
  const equal = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  const byId = (id) => document.getElementById(id);
  const boolValue = (value, fallback = false) => value == null ? fallback
    : value === true || value === 1 || value === '1' || String(value).toLowerCase() === 'true';
  const xmlScalar = (value) => {
    if (value == null) return null;
    if (typeof value === 'boolean') return value ? '1' : '0';
    if (typeof value === 'number') return Number.isFinite(value) ? String(value) : '';
    return String(value);
  };
  const sameScalar = (left, right) => xmlScalar(left) === xmlScalar(right);
  const attr = (object, name, fallback = null) => object?.[name] ?? object?.attributes?.[name] ?? fallback;
  const setAttr = (object, name, value) => { object[name] = value; };
  const numberValue = (value, fallback = null) => value === '' || value == null ? fallback
    : Number.isFinite(Number(value)) ? Number(value) : fallback;
  const textValue = (value) => value == null ? '' : String(value);
  const nativeKey = (item, field, fallback) => textValue(item?.[`_base${field}`] ?? attr(item, field, fallback));
  const guid = () => `{${(crypto.randomUUID ? crypto.randomUUID() :
    'xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx'.replace(/[xy]/g, (token) => {
      const random = Math.random() * 16 | 0;
      return (token === 'x' ? random : (random & 3 | 8)).toString(16);
    })).toUpperCase()}}`;

  const state = {
    model: null, base: null, draft: null, sheetIndex: 0, tab: 'layout', loading: false,
    passwordChanges: { workbook: null, worksheets: {}, protectedRanges: {} },
  };
  const appState = () => typeof S === 'undefined' ? null : S;

  function node(tag, text, className) {
    const element = document.createElement(tag);
    if (className) element.className = className;
    if (text != null) element.textContent = String(text);
    return element;
  }
  function button(text, handler, className = '') {
    const element = node('button', text, className);
    element.type = 'button';
    if (handler) element.addEventListener('click', handler);
    return element;
  }
  function resetPasswordChanges() {
    state.passwordChanges = { workbook: null, worksheets: {}, protectedRanges: {} };
  }
  function passwordChangePayload() {
    const workbook = state.passwordChanges.workbook;
    const worksheets = Object.entries(state.passwordChanges.worksheets)
      .map(([sheet, change]) => ({ sheet, ...change }));
    const protectedRanges = Object.entries(state.passwordChanges.protectedRanges)
      .map(([key, change]) => {
        const [sheet, name] = key.split('\u0000'); return { sheet, name, ...change };
      });
    if (!workbook && !worksheets.length && !protectedRanges.length) return null;
    return { ...(workbook ? { workbook } : {}), ...(worksheets.length ? { worksheets } : {}),
      ...(protectedRanges.length ? { protectedRanges } : {}) };
  }
  function passwordEditor(label, currentChange, setChange) {
    const wrapper = node('div', null, 'pre-password-editor');
    const control = input(currentChange?.password || '', (value) => {
      setChange(value ? { password: value } : null);
    }, { type: 'password', placeholder: '留空表示不修改' });
    control.autocomplete = 'new-password';
    const clear = button(currentChange?.clear ? '取消清除密码' : '清除现有密码', () => {
      setChange(currentChange?.clear ? null : { clear: true }); renderBody();
    }, currentChange?.clear ? '' : 'danger');
    wrapper.append(field(label, control), clear);
    if (currentChange?.clear) wrapper.appendChild(node('span', '保存后该保护记录将不再要求密码。', 'pre-warning'));
    return wrapper;
  }
  function section(title, hint = '') {
    const element = node('section', null, 'pre-section');
    element.appendChild(node('h3', title));
    if (hint) element.appendChild(node('p', hint, 'pre-hint'));
    return element;
  }
  function field(label, control, wide = false) {
    const wrapper = node('label', null, `pre-field${wide ? ' wide' : ''}`);
    wrapper.append(node('span', label), control);
    return wrapper;
  }
  function input(value, onInput, options = {}) {
    const control = document.createElement(options.multiline ? 'textarea' : 'input');
    if (!options.multiline) control.type = options.type || 'text';
    control.value = textValue(value);
    if (options.placeholder) control.placeholder = options.placeholder;
    if (options.min != null) control.min = String(options.min);
    if (options.max != null) control.max = String(options.max);
    if (options.step != null) control.step = String(options.step);
    control.addEventListener(options.commit ? 'change' : 'input', () => onInput(control.value, control));
    return control;
  }
  function xmlText(value) {
    return textValue(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
  }
  function noteRunModel(textXml, fallbackText = '') {
    const fallback = {
      rootOpen: '<text>', rootClose: '</text>', opaqueXml: '',
      layout: [{ kind: 'run' }],
      runs: [{ text: textValue(fallbackText), rPrXml: '', style: {} }],
    };
    if (!textXml) return fallback;
    const source = textValue(textXml).trim();
    const documentXml = new DOMParser().parseFromString(source, 'application/xml');
    if (documentXml.querySelector('parsererror') || documentXml.documentElement?.localName !== 'text') return fallback;
    const root = documentXml.documentElement;
    const serializer = new XMLSerializer();
    const openingEnd = source.indexOf('>');
    const rootOpen = (openingEnd >= 0 ? source.slice(0, openingEnd + 1) : '<text>').replace(/\/>$/, '>');
    const rootClose = `</${root.nodeName}>`;
    const runs = [];
    const opaque = [];
    const layout = [];
    for (const child of root.children) {
      if (child.localName === 't') {
        runs.push({ text: child.textContent || '', rPrXml: '', style: {} });
        layout.push({ kind: 'run' });
        continue;
      }
      if (child.localName !== 'r') {
        const xml = serializer.serializeToString(child);
        opaque.push(xml); layout.push({ kind: 'opaque', xml }); continue;
      }
      const rPr = [...child.children].find((node) => node.localName === 'rPr') || null;
      const textNode = [...child.children].find((node) => node.localName === 't') || null;
      const property = (name) => rPr && [...rPr.children].find((node) => node.localName === name);
      const on = (name) => {
        const value = property(name)?.getAttribute('val');
        return value == null ? !!property(name) : !['0', 'false', 'none'].includes(value.toLowerCase());
      };
      const colorNode = property('color');
      const rgb = colorNode?.getAttribute('rgb');
      const style = {
        bold: on('b'), italic: on('i'), strike: on('strike'),
        underline: on('u'), font: property('rFont')?.getAttribute('val') || '',
        size: property('sz')?.getAttribute('val') || '',
        color: rgb && /^[0-9a-f]{6,8}$/i.test(rgb) ? `#${rgb.slice(-6)}` : '',
      };
      runs.push({ text: textNode?.textContent || '', rPrXml: rPr ? serializer.serializeToString(rPr) : '', style });
      layout.push({ kind: 'run' });
    }
    return {
      rootOpen, rootClose, opaqueXml: opaque.join(''),
      layout: runs.length ? layout : [{ kind: 'run' }, ...layout],
      runs: runs.length ? runs : fallback.runs,
    };
  }
  function noteTextXml(model, runs) {
    const renderRun = (run) => `<r>${run.rPrXml || ''}<t xml:space="preserve">${xmlText(run.text)}</t></r>`;
    const layout = Array.isArray(model.layout) && model.layout.length
      ? model.layout : [{ kind: 'run' }, ...(model.opaqueXml ? [{ kind: 'opaque', xml: model.opaqueXml }] : [])];
    const fragments = [];
    let runIndex = 0, insertAfter = -1;
    for (const item of layout) {
      if (item?.kind === 'run') {
        if (runIndex < runs.length) fragments.push(renderRun(runs[runIndex++]));
        insertAfter = fragments.length;
      } else if (item?.kind === 'opaque' && item.xml) fragments.push(item.xml);
    }
    if (runIndex < runs.length) {
      const extra = runs.slice(runIndex).map(renderRun);
      fragments.splice(insertAfter < 0 ? 0 : insertAfter, 0, ...extra);
    }
    return `${model.rootOpen || '<text>'}${fragments.join('')}${model.rootClose || '</text>'}`;
  }
  function editableNoteRuns(editor, defaultRpr = '') {
    const runs = [];
    const blockNames = new Set(['DIV', 'P', 'LI', 'UL', 'OL', 'BLOCKQUOTE', 'PRE']);
    const append = (value, requestedRpr = null) => {
      const text = textValue(value);
      if (!text) return;
      const rPrXml = requestedRpr == null
        ? (runs.at(-1)?.rPrXml ?? defaultRpr) : requestedRpr;
      if (runs.length && runs.at(-1).rPrXml === rPrXml) runs.at(-1).text += text;
      else runs.push({ text, rPrXml });
    };
    const endsWithNewline = () => !!runs.length && runs.at(-1).text.endsWith('\n');
    const walk = (current, inheritedRpr = null) => {
      if (current.nodeType === Node.TEXT_NODE) { append(current.nodeValue, inheritedRpr); return; }
      if (current.nodeType !== Node.ELEMENT_NODE) return;
      const element = current;
      const ownsRpr = Object.prototype.hasOwnProperty.call(element.dataset || {}, 'rpr');
      const rPrXml = ownsRpr ? element.dataset.rpr : inheritedRpr;
      if (element.tagName === 'BR') {
        // A new contenteditable block commonly starts with a single BR.  The block boundary
        // already contributes its newline; consume only additional consecutive BR elements.
        if (!endsWithNewline() || element.previousElementSibling?.tagName === 'BR') append('\n', rPrXml);
        return;
      }
      if (blockNames.has(element.tagName) && runs.length && !endsWithNewline()) append('\n', rPrXml);
      for (const child of element.childNodes) walk(child, rPrXml);
    };
    for (const child of editor.childNodes) walk(child, null);
    return runs.length ? runs : [{ text: '', rPrXml: defaultRpr }];
  }
  function richNoteEditor(item) {
    const model = noteRunModel(item.textXml, item.text);
    const editor = node('div', null, 'pre-rich-note-editor');
    editor.contentEditable = 'true'; editor.spellcheck = true;
    editor.setAttribute('role', 'textbox'); editor.setAttribute('aria-multiline', 'true');
    for (const run of model.runs) {
      const span = node('span', run.text, 'pre-rich-note-run'); span.dataset.rpr = run.rPrXml || '';
      if (run.style.bold) span.style.fontWeight = '700';
      if (run.style.italic) span.style.fontStyle = 'italic';
      const decorations = [run.style.underline && 'underline', run.style.strike && 'line-through'].filter(Boolean);
      if (decorations.length) span.style.textDecoration = decorations.join(' ');
      if (run.style.font) span.style.fontFamily = run.style.font;
      if (run.style.size) span.style.fontSize = `${run.style.size}pt`;
      if (run.style.color) span.style.color = run.style.color;
      editor.appendChild(span);
    }
    editor.addEventListener('input', () => {
      const runs = editableNoteRuns(editor, model.runs[0]?.rPrXml || '');
      item.text = runs.map((run) => run.text).join('');
      item.textXml = noteTextXml(model, runs);
    });
    return editor;
  }
  function select(value, choices, onChange) {
    const control = document.createElement('select');
    const normalized = choices.map((choice) => Array.isArray(choice) ? choice : [choice, choice]);
    const raw = textValue(value);
    if (raw && !normalized.some(([candidate]) => String(candidate) === raw)) {
      normalized.unshift([raw, `${raw}（保留原生值）`]);
    }
    for (const [key, label] of normalized) {
      const option = node('option', label); option.value = String(key); option.selected = String(key) === raw;
      control.appendChild(option);
    }
    control.addEventListener('change', () => onChange(control.value, control));
    return control;
  }
  function checkbox(label, checked, onChange, title = '') {
    const wrapper = node('label', null, 'pre-check');
    const control = document.createElement('input'); control.type = 'checkbox'; control.checked = !!checked;
    control.addEventListener('change', () => onChange(control.checked, control));
    wrapper.append(control, node('span', label));
    if (title) wrapper.title = title;
    return wrapper;
  }
  function columnLetters(column) {
    let value = Math.max(1, Number(column) || 1), result = '';
    while (value) { value -= 1; result = String.fromCharCode(65 + value % 26) + result; value = Math.floor(value / 26); }
    return result;
  }
  function currentCellReference() {
    const workbook = appState();
    return `${columnLetters(Number(workbook?.cur?.c || 1))}${Number(workbook?.cur?.r || 1)}`;
  }

  function normalizeBreaks(value) {
    const root = value && typeof value === 'object' ? clone(value) : { attributes: {}, items: [] };
    root.items = Array.isArray(root.items) ? root.items : [];
    root.items = root.items.map((item) => {
      const result = clone(item || {});
      result._baseid = textValue(attr(result, 'id', ''));
      return result;
    });
    return root;
  }
  function normalizeWorksheet(value, model = null) {
    const sheet = clone(value || {});
    for (const key of ['pageMargins', 'pageSetup', 'printOptions', 'headerFooter', 'sheetProtection']) {
      sheet[key] = sheet[key] == null ? null : clone(sheet[key]);
    }
    sheet.rowBreaks = normalizeBreaks(sheet.rowBreaks);
    sheet.colBreaks = normalizeBreaks(sheet.colBreaks);
    sheet.protectedRanges = (sheet.protectedRanges || []).map((item) => {
      const result = clone(item); result._basename = textValue(attr(result, 'name', '')); return result;
    });
    sheet.notes = clone(sheet.notes || { part: null, relationshipId: null, authors: [], items: [] });
    sheet.notes.items = (sheet.notes.items || []).map((item) => {
      const result = clone(item); result._baseref = textValue(result.ref); return result;
    });
    sheet.threadedComments = clone(sheet.threadedComments || { part: null, relationshipId: null, items: [] });
    sheet.threadedComments.items = (sheet.threadedComments.items || []).map((item) => {
      const result = clone(item); result._baseid = textValue(result.id); result.mentions ||= []; return result;
    });
    const names = model?.definedNames || [];
    const area = names.find((item) => item.kind === 'printArea'
      && Number(item.localSheetId) === Number(sheet.localSheetId));
    const titles = names.find((item) => item.kind === 'printTitles'
      && Number(item.localSheetId) === Number(sheet.localSheetId));
    sheet.printArea = area?.formula ?? null;
    sheet.printTitles = titles?.formula ?? null;
    return sheet;
  }
  function normalizeModel(value) {
    const model = clone(value || {});
    model.workbookProtection = model.workbookProtection == null ? null : clone(model.workbookProtection);
    model.definedNames ||= []; model.persons ||= []; model.worksheets ||= [];
    model.worksheets = model.worksheets.map((sheet) => normalizeWorksheet(sheet, model));
    return model;
  }
  function currentSheet() { return state.draft?.worksheets?.[state.sheetIndex] || null; }
  function baseSheet(draft = currentSheet()) {
    if (!draft) return null;
    return state.base?.worksheets?.find((sheet) => sheet.part === draft.part)
      || state.base?.worksheets?.find((sheet) => Number(sheet.sheetId) === Number(draft.sheetId)) || null;
  }

  function attributePatch(base, current, fields) {
    if (current == null) return base == null ? undefined : null;
    const patch = {};
    for (const name of fields) {
      const before = attr(base, name, null), after = attr(current, name, null);
      if (!sameScalar(before, after)) patch[name] = after;
    }
    return Object.keys(patch).length ? patch : undefined;
  }
  function headerFooterPatch(base, current) {
    if (current == null) return base == null ? undefined : null;
    const patch = attributePatch(base, current, HEADER_ATTRIBUTES) || {};
    for (const name of HEADER_FIELDS) {
      const before = base?.[name] ?? null, after = current?.[name] ?? null;
      if (before !== after) patch[name] = after;
    }
    return Object.keys(patch).length ? patch : undefined;
  }
  function itemDelta(baseItems, currentItems, identityField, fields, deletionField) {
    const baseByKey = new Map(baseItems.map((item) => [textValue(attr(item, identityField, '')), item]));
    const currentBaseKeys = new Set();
    const upsert = [], deleted = [];
    for (const item of currentItems) {
      const originalKey = nativeKey(item, identityField, '');
      const currentKey = textValue(attr(item, identityField, ''));
      if (originalKey) currentBaseKeys.add(originalKey);
      const before = baseByKey.get(originalKey);
      if (before && originalKey !== currentKey) deleted.push(identityField === 'id' ? Number(originalKey) : originalKey);
      const changes = {};
      for (const fieldName of fields) {
        const beforeValue = before && originalKey === currentKey ? attr(before, fieldName, null) : null;
        const afterValue = attr(item, fieldName, null);
        if (!before || originalKey !== currentKey || !sameScalar(beforeValue, afterValue)) changes[fieldName] = afterValue;
      }
      if (Object.keys(changes).length) {
        changes[identityField] = identityField === 'id' ? Number(currentKey) : currentKey;
        upsert.push(changes);
      }
    }
    for (const item of baseItems) {
      const key = textValue(attr(item, identityField, ''));
      if (!currentBaseKeys.has(key)) deleted.push(identityField === 'id' ? Number(key) : key);
    }
    const result = {};
    if (upsert.length) result.upsert = upsert;
    if (deleted.length) result[deletionField] = [...new Set(deleted)];
    return Object.keys(result).length ? result : undefined;
  }
  function notesPatch(base, current) {
    const baseItems = base?.items || [], currentItems = current?.items || [];
    const byRef = new Map(baseItems.map((item) => [textValue(item.ref), item]));
    const retained = new Set(), upsert = [], deleted = [];
    for (const item of currentItems) {
      const oldRef = textValue(item._baseref), ref = textValue(item.ref);
      if (oldRef) retained.add(oldRef);
      const before = byRef.get(oldRef);
      if (before && oldRef !== ref) deleted.push(oldRef);
      const patch = { ref };
      const replaceIdentity = !before || oldRef !== ref;
      let changed = replaceIdentity;
      if (replaceIdentity || !sameScalar(before?.author, item.author)) {
        patch.author = item.author ?? ''; changed = true;
      }
      if (replaceIdentity || !sameScalar(before?.textXml, item.textXml)) {
        if (item.textXml) patch.textXml = item.textXml;
        else patch.text = item.text ?? '';
        changed = true;
      } else if (!sameScalar(before?.text, item.text)) {
        patch.text = item.text ?? ''; changed = true;
      }
      if (changed) upsert.push(patch);
    }
    for (const item of baseItems) if (!retained.has(textValue(item.ref))) deleted.push(textValue(item.ref));
    const result = {};
    if (upsert.length) result.upsert = upsert;
    if (deleted.length) result.delete = [...new Set(deleted)];
    return Object.keys(result).length ? result : undefined;
  }
  function threadedPatch(base, current) {
    const baseItems = base?.items || [], currentItems = current?.items || [];
    const byId = new Map(baseItems.map((item) => [textValue(item.id), item]));
    const retained = new Set(), upsert = [], deleteIds = [];
    for (const item of currentItems) {
      const oldId = textValue(item._baseid), id = textValue(item.id);
      if (oldId) retained.add(oldId);
      const before = byId.get(oldId);
      const patch = { id, ref: item.ref };
      const replaceIdentity = !before || oldId !== id;
      let changed = replaceIdentity;
      for (const name of ['ref', 'personId', 'parentId', 'text']) {
        if (replaceIdentity || !sameScalar(before?.[name], item[name])) { patch[name] = item[name] ?? null; changed = true; }
      }
      const mentionsChanged = !equal(before?.mentions || [], item.mentions || []);
      if (mentionsChanged) patch.mentions = clone(item.mentions || []);
      changed ||= mentionsChanged;
      if (item.author && (!before || !item.personId)) patch.author = item.author;
      if (changed) upsert.push(patch);
    }
    for (const item of baseItems) if (!retained.has(textValue(item.id))) deleteIds.push(textValue(item.id));
    const result = {};
    if (upsert.length) result.upsert = upsert;
    if (deleteIds.length) result.deleteIds = [...new Set(deleteIds)];
    return Object.keys(result).length ? result : undefined;
  }
  function buildWorksheetPatch(base, draft) {
    const result = { sheetId: draft.sheetId, part: draft.part };
    const singletonFields = [
      ['pageMargins', MARGIN_FIELDS], ['pageSetup', PAGE_SETUP_FIELDS],
      ['printOptions', PRINT_OPTION_FIELDS], ['sheetProtection', SHEET_PROTECTION_FIELDS],
    ];
    for (const [name, fields] of singletonFields) {
      const patch = attributePatch(base?.[name], draft?.[name], fields);
      if (patch !== undefined) result[name] = patch;
    }
    const header = headerFooterPatch(base?.headerFooter, draft?.headerFooter);
    if (header !== undefined) result.headerFooter = header;
    for (const name of ['rowBreaks', 'colBreaks']) {
      const patch = itemDelta(base?.[name]?.items || [], draft?.[name]?.items || [], 'id', BREAK_FIELDS, 'deleteIds');
      if (patch) result[name] = patch;
    }
    const ranges = itemDelta(base?.protectedRanges || [], draft?.protectedRanges || [],
      'name', PROTECTED_RANGE_FIELDS, 'deleteNames');
    if (ranges) result.protectedRanges = ranges;
    const notes = notesPatch(base?.notes, draft?.notes); if (notes) result.notes = notes;
    const threaded = threadedPatch(base?.threadedComments, draft?.threadedComments);
    if (threaded) result.threadedComments = threaded;
    if ((base?.printArea ?? null) !== (draft?.printArea ?? null)) result.printArea = draft.printArea;
    if ((base?.printTitles ?? null) !== (draft?.printTitles ?? null)) result.printTitles = draft.printTitles;
    return Object.keys(result).length > 2 ? result : null;
  }
  function buildPackagePatchFor(base, draft) {
    const patch = {};
    const workbookProtection = attributePatch(base?.workbookProtection, draft?.workbookProtection,
      WORKBOOK_PROTECTION_FIELDS);
    if (workbookProtection !== undefined) patch.workbookProtection = workbookProtection;
    const worksheets = [];
    for (const sheet of draft?.worksheets || []) {
      const before = base?.worksheets?.find((item) => item.part === sheet.part)
        || base?.worksheets?.find((item) => Number(item.sheetId) === Number(sheet.sheetId));
      const difference = buildWorksheetPatch(before, sheet);
      if (difference) worksheets.push(difference);
    }
    if (worksheets.length) patch.worksheets = worksheets;
    return patch;
  }
  function buildPackagePatch() { return buildPackagePatchFor(state.base, state.draft); }

  function baseWorksheetForPatch(worksheet, baseModel = state.base) {
    return (baseModel?.worksheets || []).find((sheet) => worksheet.part && sheet.part === worksheet.part)
      || (baseModel?.worksheets || []).find((sheet) => worksheet.sheetId != null
        && sameScalar(sheet.sheetId, worksheet.sheetId))
      || (baseModel?.worksheets || []).find((sheet) => worksheet.sheet
        && (sheet.name === worksheet.sheet || Number(worksheet.sheet) === Number(sheet.localSheetId)));
  }

  function pageReviewPatchNeedsPassword(patch, passwordChanges = null, baseModel = state.base) {
    if (passwordChanges || Object.prototype.hasOwnProperty.call(patch, 'workbookProtection')) return true;
    const layoutFields = ['pageMargins', 'pageSetup', 'printOptions', 'headerFooter', 'rowBreaks',
      'colBreaks', 'printArea', 'printTitles'];
    return (patch.worksheets || []).some((worksheet) => {
      const base = baseWorksheetForPatch(worksheet, baseModel);
      const protection = base?.sheetProtection;
      if (!protection || !boolValue(attr(protection, 'sheet', false))) return false;
      if (Object.prototype.hasOwnProperty.call(worksheet, 'sheetProtection')
        || Object.prototype.hasOwnProperty.call(worksheet, 'protectedRanges')) return true;
      if (layoutFields.some((field) => Object.prototype.hasOwnProperty.call(worksheet, field))) return true;
      return (Object.prototype.hasOwnProperty.call(worksheet, 'notes')
          || Object.prototype.hasOwnProperty.call(worksheet, 'threadedComments'))
        && boolValue(attr(protection, 'objects', false));
    });
  }

  function ensureSingleton(sheet, name, defaults = {}) {
    if (sheet[name] == null) sheet[name] = clone(defaults);
    return sheet[name];
  }
  function toggleSingleton(container, sheet, name, label, defaults, rerender = true) {
    container.appendChild(checkbox(label, sheet[name] != null, (checked) => {
      sheet[name] = checked ? clone(defaults) : null;
      if (rerender) renderBody();
    }));
  }
  function attrNumberControl(object, name, fallback, options = {}) {
    return input(attr(object, name, fallback), (value, control) => {
      const number = numberValue(value, null);
      control.classList.toggle('invalid', number == null || (options.min != null && number < options.min));
      if (number != null) setAttr(object, name, number);
    }, { type: 'number', ...options });
  }
  function renderLayout(body, sheet) {
    const page = section('页面', '纸张尺寸使用 Excel/OOXML 原生 paperSize；“调整为”会让打印宽度自动适配所选纸张。');
    toggleSingleton(page, sheet, 'pageSetup', '启用页面设置', { paperSize: 9, orientation: 'portrait', fitToWidth: 1, fitToHeight: 0 });
    if (sheet.pageSetup) {
      const grid = node('div', null, 'pre-grid');
      grid.append(
        field('纸张大小', select(attr(sheet.pageSetup, 'paperSize', '9'), PAPER_SIZES,
          (value) => setAttr(sheet.pageSetup, 'paperSize', value))),
        field('方向', select(attr(sheet.pageSetup, 'orientation', 'portrait'),
          [['default', '默认'], ['portrait', '纵向'], ['landscape', '横向']],
          (value) => setAttr(sheet.pageSetup, 'orientation', value))),
        field('页面顺序', select(attr(sheet.pageSetup, 'pageOrder', 'downThenOver'),
          [['downThenOver', '先列后行'], ['overThenDown', '先行后列']],
          (value) => setAttr(sheet.pageSetup, 'pageOrder', value))),
        field('缩放比例 %', attrNumberControl(sheet.pageSetup, 'scale', 100, { min: 10, max: 400, step: 1 })),
        field('适配宽度（页）', attrNumberControl(sheet.pageSetup, 'fitToWidth', 1, { min: 0, max: 32767, step: 1 })),
        field('适配高度（页）', attrNumberControl(sheet.pageSetup, 'fitToHeight', 0, { min: 0, max: 32767, step: 1 })),
        field('首页页码', attrNumberControl(sheet.pageSetup, 'firstPageNumber', 1, { min: 1, step: 1 })),
        field('打印份数', attrNumberControl(sheet.pageSetup, 'copies', 1, { min: 1, max: 32767, step: 1 })),
        field('水平 DPI', attrNumberControl(sheet.pageSetup, 'horizontalDpi', 600, { min: 1, step: 1 })),
        field('垂直 DPI', attrNumberControl(sheet.pageSetup, 'verticalDpi', 600, { min: 1, step: 1 })),
        field('批注打印', select(attr(sheet.pageSetup, 'cellComments', 'none'),
          [['none', '无'], ['asDisplayed', '如工作表中显示'], ['atEnd', '工作表末尾']],
          (value) => setAttr(sheet.pageSetup, 'cellComments', value))),
        field('错误值打印', select(attr(sheet.pageSetup, 'errors', 'displayed'),
          [['displayed', '按显示值'], ['blank', '空白'], ['dash', '短横线'], ['NA', '#N/A']],
          (value) => setAttr(sheet.pageSetup, 'errors', value))),
      );
      const checks = node('div', null, 'pre-checks');
      for (const [name, label] of [['useFirstPageNumber', '使用首页页码'], ['blackAndWhite', '黑白打印'],
        ['draft', '草稿质量'], ['usePrinterDefaults', '使用打印机默认值']]) {
        checks.appendChild(checkbox(label, boolValue(attr(sheet.pageSetup, name)), (checked) => setAttr(sheet.pageSetup, name, checked)));
      }
      page.append(grid, checks);
    }
    body.appendChild(page);

    const margins = section('页边距（英寸）', '分别控制正文、页眉和页脚距离；只写入实际修改的属性。');
    toggleSingleton(margins, sheet, 'pageMargins', '启用自定义页边距',
      { left: .7, right: .7, top: .75, bottom: .75, header: .3, footer: .3 });
    if (sheet.pageMargins) {
      const grid = node('div', null, 'pre-grid');
      const labels = { left: '左', right: '右', top: '上', bottom: '下', header: '页眉', footer: '页脚' };
      for (const name of MARGIN_FIELDS) grid.appendChild(field(labels[name],
        attrNumberControl(sheet.pageMargins, name, name === 'left' || name === 'right' ? .7 : name === 'top' || name === 'bottom' ? .75 : .3,
          { min: 0, step: .05 })));
      margins.appendChild(grid);
    }
    body.appendChild(margins);

    const print = section('打印内容', '打印区域和标题写入工作簿原生定义名称；支持跨页重复标题。');
    const areaGrid = node('div', null, 'pre-grid two');
    areaGrid.append(
      field('打印区域', input(sheet.printArea, (value) => { sheet.printArea = value.trim() || null; },
        { placeholder: `'${sheet.name || 'Sheet1'}'!$A$1:$F$40` }), true),
      field('重复标题', input(sheet.printTitles, (value) => { sheet.printTitles = value.trim() || null; },
        { placeholder: `'${sheet.name || 'Sheet1'}'!$1:$2` }), true),
    );
    print.appendChild(areaGrid);
    toggleSingleton(print, sheet, 'printOptions', '启用打印选项', {});
    if (sheet.printOptions) {
      const checks = node('div', null, 'pre-checks');
      const labels = { gridLines: '打印网格线', gridLinesSet: '网格线已设置', headings: '打印行列标题', horizontalCentered: '水平居中', verticalCentered: '垂直居中' };
      for (const name of PRINT_OPTION_FIELDS) checks.appendChild(checkbox(labels[name], boolValue(attr(sheet.printOptions, name)),
        (checked) => setAttr(sheet.printOptions, name, checked)));
      print.appendChild(checks);
    }
    body.appendChild(print);
  }

  function renderHeaderFooter(body, sheet) {
    const header = section('页眉和页脚', '使用 Excel 代码：&P 页码、&N 总页数、&A 工作表名、&F 文件名、&D 日期、&T 时间。');
    toggleSingleton(header, sheet, 'headerFooter', '启用页眉/页脚', {});
    if (sheet.headerFooter) {
      const checks = node('div', null, 'pre-checks');
      const labels = { differentOddEven: '奇偶页不同', differentFirst: '首页不同', scaleWithDoc: '随文档缩放', alignWithMargins: '与页边距对齐' };
      for (const name of HEADER_ATTRIBUTES) checks.appendChild(checkbox(labels[name], boolValue(attr(sheet.headerFooter, name, name.startsWith('scale') || name.startsWith('align'))),
        (checked) => setAttr(sheet.headerFooter, name, checked)));
      header.appendChild(checks);
      const grid = node('div', null, 'pre-grid two');
      const labelsText = { oddHeader: '奇数页页眉', oddFooter: '奇数页页脚', evenHeader: '偶数页页眉', evenFooter: '偶数页页脚', firstHeader: '首页页眉', firstFooter: '首页页脚' };
      for (const name of HEADER_FIELDS) grid.appendChild(field(labelsText[name], input(sheet.headerFooter[name],
        (value) => { sheet.headerFooter[name] = value || null; }, { placeholder: '&L左&C居中&R右' }), true));
      header.appendChild(grid);
    }
    body.appendChild(header);
    renderBreakSection(body, sheet, 'rowBreaks', '水平分页符（行）', 1_048_575, 16_383);
    renderBreakSection(body, sheet, 'colBreaks', '垂直分页符（列）', 16_383, 1_048_575);
  }
  function renderBreakSection(body, sheet, name, idMax, coordinateMax) {
    const area = section(name === 'rowBreaks' ? '水平分页符' : '垂直分页符',
      '拖动滑块或输入编号可移动分页符；min/max 限定分页符覆盖的另一轴范围。');
    const list = node('div', null, 'pre-card-list');
    sheet[name].items.forEach((item, index) => {
      const card = node('div', null, 'pre-break-card');
      const heading = node('header'); heading.append(node('strong', `${name === 'rowBreaks' ? '行' : '列'}分页符 ${index + 1}`),
        button('删除', () => { sheet[name].items.splice(index, 1); renderBody(); }, 'danger'));
      const sliderRow = node('div', null, 'pre-slider-row');
      const number = input(attr(item, 'id', 1), (value) => {
        setAttr(item, 'id', Math.max(0, Math.min(idMax, numberValue(value, 0))));
        range.value = String(attr(item, 'id', 0));
      }, { type: 'number', min: 0, max: idMax, step: 1 });
      const range = input(attr(item, 'id', 1), (value) => {
        setAttr(item, 'id', Number(value)); number.value = value;
      }, { type: 'range', min: 0, max: idMax, step: 1 });
      sliderRow.append(field('位置', number), range);
      const grid = node('div', null, 'pre-grid');
      grid.append(field('最小范围', attrNumberControl(item, 'min', 0, { min: 0, max: coordinateMax, step: 1 })),
        field('最大范围', attrNumberControl(item, 'max', coordinateMax, { min: 0, max: coordinateMax, step: 1 })),
        checkbox('手动分页符', boolValue(attr(item, 'man', true)), (checked) => setAttr(item, 'man', checked)));
      card.append(heading, sliderRow, grid); list.appendChild(card);
    });
    area.appendChild(list);
    area.appendChild(button('添加分页符', () => {
      const last = sheet[name].items.at(-1);
      const next = Math.min(idMax, Number(attr(last, 'id', 0)) + 1 || 1);
      sheet[name].items.push({ id: next, min: 0, max: coordinateMax, man: true, _baseid: '' });
      renderBody();
    }, 'primary'));
    if (!sheet[name].items.length) area.appendChild(node('p', '当前没有手动分页符。', 'pre-empty-small'));
    body.appendChild(area);
  }

  function renderProtection(body, sheet) {
    const workbook = section('工作簿保护', '这里只编辑 OOXML 保护元数据；它不是权限服务，也不会替代文件访问控制。');
    const current = state.draft.workbookProtection;
    toggleSingleton(workbook, state.draft, 'workbookProtection', '启用工作簿保护', { lockStructure: true });
    if (state.draft.workbookProtection) {
      const checks = node('div', null, 'pre-checks');
      const labels = { lockStructure: '锁定工作簿结构', lockWindows: '锁定窗口', lockRevision: '锁定修订' };
      for (const name of WORKBOOK_PROTECTION_FIELDS) checks.appendChild(checkbox(labels[name], boolValue(attr(state.draft.workbookProtection, name)),
        (checked) => setAttr(state.draft.workbookProtection, name, checked)));
      workbook.appendChild(checks);
      workbook.appendChild(passwordEditor('设置/更换保护密码', state.passwordChanges.workbook,
        (change) => { state.passwordChanges.workbook = change; }));
      const hashes = node('details', null, 'pre-native-details');
      hashes.appendChild(node('summary', '原生密码/哈希元数据'));
      const grid = node('div', null, 'pre-grid two');
      for (const [name, label] of [
        ['workbookPassword', '传统工作簿密码哈希'], ['revisionsPassword', '传统修订密码哈希'],
        ['workbookAlgorithmName', '工作簿哈希算法'], ['workbookHashValue', '工作簿哈希值'],
        ['workbookSaltValue', '工作簿盐值'], ['workbookSpinCount', '工作簿迭代次数'],
        ['revisionsAlgorithmName', '修订哈希算法'], ['revisionsHashValue', '修订哈希值'],
        ['revisionsSaltValue', '修订盐值'], ['revisionsSpinCount', '修订迭代次数'],
      ]) grid.appendChild(field(label, input(attr(state.draft.workbookProtection, name, ''),
        (value) => setAttr(state.draft.workbookProtection, name, value || null))));
      hashes.appendChild(grid); workbook.appendChild(hashes);
    }
    if (current?.hashValue || current?.workbookPassword || current?.workbookHashValue) workbook.appendChild(node('p', '现有密码只按 Excel 校验器验证；设置新密码时写入带随机盐和 100,000 次迭代的 SHA-512，明文不会进入工作簿或撤销历史。', 'pre-warning'));
    body.appendChild(workbook);

    const protection = section('工作表保护', '勾选项对应 Excel 原生保护标志；现有 password/hashValue/saltValue/spinCount 保持不变。');
    toggleSingleton(protection, sheet, 'sheetProtection', '保护当前工作表', { sheet: true });
    if (sheet.sheetProtection) {
      const checks = node('div', null, 'pre-checks compact');
      const labels = {
        sheet: '启用保护', objects: '保护对象', scenarios: '保护方案', formatCells: '禁止设置单元格格式',
        formatColumns: '禁止设置列格式', formatRows: '禁止设置行格式', insertColumns: '禁止插入列', insertRows: '禁止插入行',
        insertHyperlinks: '禁止插入超链接', deleteColumns: '禁止删除列', deleteRows: '禁止删除行',
        selectLockedCells: '限制选择锁定单元格', sort: '禁止排序', autoFilter: '禁止筛选',
        pivotTables: '禁止使用透视表', selectUnlockedCells: '限制选择未锁定单元格',
      };
      const defaultBlocked = (name) => !['sheet', 'objects', 'scenarios', 'selectLockedCells', 'selectUnlockedCells'].includes(name);
      for (const name of SHEET_PROTECTION_FLAG_FIELDS) checks.appendChild(checkbox(labels[name], boolValue(attr(sheet.sheetProtection, name), defaultBlocked(name)),
        (checked) => setAttr(sheet.sheetProtection, name, checked)));
      protection.appendChild(checks);
      protection.appendChild(passwordEditor('设置/更换当前工作表密码', state.passwordChanges.worksheets[sheet.name],
        (change) => {
          if (change) state.passwordChanges.worksheets[sheet.name] = change;
          else delete state.passwordChanges.worksheets[sheet.name];
        }));
      const hashes = node('details', null, 'pre-native-details');
      hashes.appendChild(node('summary', '原生密码/哈希元数据'));
      const grid = node('div', null, 'pre-grid two');
      for (const [name, label] of [['password', '传统密码哈希'], ['algorithmName', '算法'], ['hashValue', '哈希值'],
        ['saltValue', '盐值'], ['spinCount', '迭代次数']]) grid.appendChild(field(label,
        input(attr(sheet.sheetProtection, name, ''), (value) => setAttr(sheet.sheetProtection, name, value || null))));
      hashes.appendChild(grid); protection.appendChild(hashes);
    }
    body.appendChild(protection);

    const ranges = section('允许编辑区域', '名称用于差量定位；修改名称会按“删除旧名称 + 新增新名称”处理。');
    const list = node('div', null, 'pre-card-list');
    sheet.protectedRanges.forEach((item, index) => {
      const card = node('div', null, 'pre-range-card');
      const heading = node('header'); heading.append(node('strong', textValue(attr(item, 'name', `区域 ${index + 1}`))),
        button('删除', () => { sheet.protectedRanges.splice(index, 1); renderBody(); }, 'danger'));
      const grid = node('div', null, 'pre-grid two');
      grid.append(field('名称', input(attr(item, 'name', ''), (value) => {
        const oldKey = `${sheet.name}\u0000${textValue(attr(item, 'name', '')).trim()}`;
        setAttr(item, 'name', value.trim());
        const nextKey = `${sheet.name}\u0000${textValue(attr(item, 'name', '')).trim()}`;
        if (oldKey !== nextKey && state.passwordChanges.protectedRanges[oldKey]) {
          state.passwordChanges.protectedRanges[nextKey] = state.passwordChanges.protectedRanges[oldKey];
          delete state.passwordChanges.protectedRanges[oldKey];
        }
      })),
        field('区域', input(attr(item, 'sqref', ''), (value) => setAttr(item, 'sqref', value.trim()), { placeholder: 'A1:B20 D1:D5' })),
        field('安全描述符', input(attr(item, 'securityDescriptor', ''), (value) => setAttr(item, 'securityDescriptor', value || null))),
        field('传统密码哈希', input(attr(item, 'password', ''), (value) => setAttr(item, 'password', value || null))));
      const baseName = nativeKey(item, 'name', attr(item, 'name', ''));
      const currentKey = () => `${sheet.name}\u0000${textValue(attr(item, 'name', '')).trim()}`;
      const baseKey = `${sheet.name}\u0000${baseName}`;
      const currentChange = state.passwordChanges.protectedRanges[currentKey()]
        || state.passwordChanges.protectedRanges[baseKey];
      const password = passwordEditor('设置/更换区域密码', currentChange, (change) => {
        delete state.passwordChanges.protectedRanges[baseKey];
        for (const key of Object.keys(state.passwordChanges.protectedRanges)) {
          if (key.startsWith(`${sheet.name}\u0000`) && key.endsWith(`\u0000${baseName}`)) delete state.passwordChanges.protectedRanges[key];
        }
        if (change) state.passwordChanges.protectedRanges[currentKey()] = change;
        else delete state.passwordChanges.protectedRanges[currentKey()];
      });
      card.append(heading, grid, password); list.appendChild(card);
    });
    ranges.appendChild(list);
    ranges.appendChild(button('添加允许编辑区域', () => {
      sheet.protectedRanges.push({ name: `Range${sheet.protectedRanges.length + 1}`, sqref: currentCellReference(), _basename: '' });
      renderBody();
    }, 'primary'));
    body.appendChild(ranges);
  }

  function renderComments(body, sheet) {
    const notes = section('旧式批注（备注）', '以原生 comments.xml + VML 往返；未编辑的富文本和 VML 形状属性保持原样。');
    const noteList = node('div', null, 'pre-card-list');
    sheet.notes.items.forEach((item, index) => {
      const card = node('article', null, 'pre-comment-card');
      const heading = node('header'); heading.append(node('strong', `备注 ${item.ref || index + 1}`),
        button('删除', () => { sheet.notes.items.splice(index, 1); renderBody(); }, 'danger'));
      const grid = node('div', null, 'pre-grid two');
      grid.append(field('单元格', input(item.ref, (value) => { item.ref = value.trim().toUpperCase(); })),
        field('作者', input(item.author, (value) => { item.author = value; })),
        field('内容（保留字符样式）', richNoteEditor(item), true));
      card.append(heading, grid); noteList.appendChild(card);
    });
    notes.appendChild(noteList);
    notes.appendChild(button('在当前单元格添加备注', () => {
      sheet.notes.items.push({ ref: currentCellReference(), author: 'UniCell', text: '', _baseref: '' }); renderBody();
    }, 'primary'));
    body.appendChild(notes);

    const threaded = section('线程评论', '支持原生回复链、人员和 @ 提及；person.xml 与 threadedComments.xml 由后端原子维护。');
    const items = sheet.threadedComments.items;
    const roots = items.filter((item) => !item.parentId);
    const list = node('div', null, 'pre-thread-list');
    const renderItem = (item, depth = 0) => {
      const index = items.indexOf(item);
      const card = node('article', null, `pre-comment-card${depth ? ' reply' : ''}`);
      const heading = node('header');
      heading.append(node('strong', `${depth ? '回复' : '评论'} · ${item.ref || ''}`),
        button('删除', () => { const id = item.id; sheet.threadedComments.items = items.filter((candidate) => candidate.id !== id && candidate.parentId !== id); renderBody(); }, 'danger'));
      const grid = node('div', null, 'pre-grid two');
      grid.append(field('单元格', input(item.ref, (value) => { item.ref = value.trim().toUpperCase(); })),
        field('人员', personSelect(item)),
        field('内容', input(item.text, (value) => { item.text = value; }, { multiline: true }), true));
      card.append(heading, grid);
      const mentions = node('div', null, 'pre-mentions');
      mentions.appendChild(node('h4', '@ 提及'));
      (item.mentions || []).forEach((mention, mentionIndex) => {
        const row = node('div', null, 'pre-mention-row');
        row.append(personSelectForMention(mention),
          input(attr(mention, 'startIndex', 0), (value) => { mention.startIndex = Math.max(0, numberValue(value, 0)); }, { type: 'number', min: 0 }),
          input(attr(mention, 'length', 1), (value) => { mention.length = Math.max(1, numberValue(value, 1)); }, { type: 'number', min: 1 }),
          button('移除', () => { item.mentions.splice(mentionIndex, 1); renderBody(); }, 'danger'));
        mentions.appendChild(row);
      });
      const mentionActions = node('div', null, 'pre-inline-actions');
      mentionActions.appendChild(button('添加 @ 提及', () => {
        const person = state.draft.persons?.[0] || item.person || null;
        if (!person?.id) { dialogStatus('请先在已有评论中选择/创建人员并保存，再添加 @ 提及。', true); return; }
        item.mentions ||= [];
        item.mentions.push({ mentionId: guid(), personId: person.id, startIndex: 0, length: Math.max(1, textValue(person.displayName).length) });
        renderBody();
      }));
      if (!depth) mentionActions.appendChild(button('回复', () => {
        items.push({ id: guid(), _baseid: '', ref: item.ref, parentId: item.id, author: 'UniCell', text: '', mentions: [] });
        renderBody();
      }, 'primary'));
      card.append(mentions, mentionActions); list.appendChild(card);
      items.filter((candidate) => candidate.parentId === item.id).forEach((reply) => renderItem(reply, depth + 1));
    };
    roots.forEach((item) => renderItem(item));
    threaded.appendChild(list);
    threaded.appendChild(button('在当前单元格新建线程', () => {
      items.push({ id: guid(), _baseid: '', ref: currentCellReference(), author: 'UniCell', text: '', mentions: [] }); renderBody();
    }, 'primary'));
    if (!roots.length) threaded.appendChild(node('p', '当前工作表没有线程评论。', 'pre-empty-small'));
    body.appendChild(threaded);
  }
  function personSelect(item) {
    const persons = state.draft.persons || [];
    const choices = [['', item.author || 'UniCell（新人员）'], ...persons.map((person) => [person.id, person.displayName || person.id])];
    return select(item.personId || item.person?.id || '', choices, (value) => {
      const person = persons.find((candidate) => candidate.id === value);
      item.personId = value || null; item.author = person?.displayName || item.author || 'UniCell'; item.person = person || null;
    });
  }
  function personSelectForMention(mention) {
    const persons = state.draft.persons || [];
    return select(attr(mention, 'personId', ''), persons.map((person) => [person.id, person.displayName || person.id]),
      (value) => { mention.personId = value; });
  }

  function renderAdvanced(body, sheet) {
    const summary = section('原生 OOXML 状态', '只读诊断视图。表单提交最小差量，未知命名空间、扩展节点和供应商属性不被覆盖。');
    const pre = node('pre', null, 'pre-json');
    pre.textContent = JSON.stringify({
      workbookPart: state.draft.workbookPart,
      personsPart: state.draft.personsPart,
      sheet: { name: sheet.name, sheetId: sheet.sheetId, part: sheet.part },
      pendingPatch: buildPackagePatch(),
    }, null, 2);
    summary.appendChild(pre); body.appendChild(summary);
  }

  function renderSidebar() {
    const host = byId('pre-sheet-list'); if (!host) return;
    host.replaceChildren();
    (state.draft?.worksheets || []).forEach((sheet, index) => {
      const control = button(sheet.name || `Sheet ${index + 1}`, () => { state.sheetIndex = index; renderDialog(); });
      control.classList.toggle('active', index === state.sheetIndex);
      control.appendChild(node('small', `sheetId ${sheet.sheetId} · ${sheet.part}`));
      host.appendChild(control);
    });
  }
  function renderBody() {
    const body = byId('pre-body'); if (!body) return;
    body.replaceChildren(); const sheet = currentSheet();
    if (!sheet) { body.appendChild(node('p', '没有可编辑工作表。', 'pre-empty')); return; }
    if (state.tab === 'layout') renderLayout(body, sheet);
    else if (state.tab === 'header') renderHeaderFooter(body, sheet);
    else if (state.tab === 'protection') renderProtection(body, sheet);
    else if (state.tab === 'comments') renderComments(body, sheet);
    else renderAdvanced(body, sheet);
  }
  function renderDialog() {
    renderSidebar();
    document.querySelectorAll('#page-review-dialog .pre-tabs button').forEach((control) => {
      control.classList.toggle('active', control.dataset.tab === state.tab);
    });
    const sheet = currentSheet();
    const subtitle = byId('pre-subtitle'); if (subtitle) subtitle.textContent = sheet ? `${sheet.name} · ${sheet.part}` : '';
    renderBody();
  }
  function dialogStatus(message, error = false) {
    const status = byId('pre-status'); if (!status) return;
    status.textContent = message; status.classList.toggle('error', error);
  }
  function setBusy(busy) {
    state.loading = busy; const dialog = byId('page-review-dialog'); if (!dialog) return;
    dialog.classList.toggle('busy', busy);
    dialog.querySelectorAll('button').forEach((control) => { if (control.id !== 'pre-close') control.disabled = busy; });
  }
  function ensureDialog() {
    let dialog = byId('page-review-dialog'); if (dialog) return dialog;
    dialog = node('div', null, 'pre-dialog'); dialog.id = 'page-review-dialog'; dialog.hidden = true;
    dialog.setAttribute('role', 'dialog'); dialog.setAttribute('aria-modal', 'true'); dialog.setAttribute('aria-label', '页面布局、审阅与保护');
    const title = node('div', null, 'pre-title');
    title.append(node('strong', '页面布局、审阅与保护'), node('span', '', 'pre-subtitle'));
    title.querySelector('.pre-subtitle').id = 'pre-subtitle';
    const close = button('×', () => { dialog.hidden = true; }); close.id = 'pre-close'; close.title = '关闭'; title.appendChild(close);
    dialog.append(title, node('div', '原生 OOXML 深层编辑；没有修改的属性和未知扩展会保持不变。', 'pre-note'));
    const main = node('div', null, 'pre-main');
    const sidebar = node('aside', null, 'pre-sidebar'); sidebar.append(node('h3', '工作表'));
    const sheetList = node('div', null, 'pre-sheet-list'); sheetList.id = 'pre-sheet-list'; sidebar.appendChild(sheetList);
    const work = node('div', null, 'pre-work');
    const tabs = node('div', null, 'pre-tabs');
    for (const [key, label] of [['layout', '页面布局'], ['header', '页眉/分页'], ['protection', '保护'], ['comments', '批注/讨论'], ['advanced', '诊断']]) {
      const control = button(label, () => { state.tab = key; renderDialog(); }); control.dataset.tab = key; tabs.appendChild(control);
    }
    const body = node('div', null, 'pre-body'); body.id = 'pre-body'; work.append(tabs, body); main.append(sidebar, work); dialog.appendChild(main);
    const footer = node('footer', null, 'pre-footer');
    const status = node('span', '', 'pre-status'); status.id = 'pre-status';
    footer.append(status,
      button('恢复导入状态', resetJournal),
      button('关闭', () => { dialog.hidden = true; }),
      button('应用到工作簿', saveDraft, 'primary'));
    dialog.appendChild(footer); document.body.appendChild(dialog); return dialog;
  }

  async function openPageReviewEditor() {
    const dialog = ensureDialog(); dialog.hidden = false; dialogStatus('正在读取页面布局、批注和保护信息…');
    try {
      setBusy(true); const model = await apiPost('/api/page-review', { op: 'list' });
      state.model = normalizeModel(model); state.base = clone(state.model); state.draft = clone(state.model);
      resetPasswordChanges();
      const workbook = appState();
      const currentName = Array.isArray(workbook?.sheets) ? workbook.sheets[workbook.sheet] : null;
      state.sheetIndex = Math.max(0, state.draft.worksheets.findIndex((sheet) => sheet.name === currentName));
      renderDialog(); dialogStatus(`${state.draft.worksheets.length} 个工作表 · ${state.draft.persons.length} 位评论人员`);
    } catch (error) { dialogStatus(error?.message || String(error), true); }
    finally { setBusy(false); }
  }
  async function saveDraft() {
    if (state.loading || !state.draft) return;
    const patch = buildPackagePatch();
    const passwordChanges = passwordChangePayload();
    if (!Object.keys(patch).length && !passwordChanges) { dialogStatus('没有需要写入的更改。'); return; }
    const touchesProtection = pageReviewPatchNeedsPassword(patch, passwordChanges);
    const hasProtection = !!state.base?.workbookProtection
      || (state.base?.worksheets || []).some((sheet) =>
        !!sheet.sheetProtection && boolValue(attr(sheet.sheetProtection, 'sheet', false)));
    let password;
    if (touchesProtection && hasProtection) {
      password = prompt('当前工作簿或工作表已受保护。请输入现有保护密码：', '');
      if (password == null) { dialogStatus('已取消：没有修改保护设置。'); return; }
    }
    try {
      setBusy(true); dialogStatus('正在校验并写入原生 OOXML 差量…');
      const model = await apiPost('/api/page-review', { op: 'update', patch,
        ...(passwordChanges ? { passwordChanges } : {}), ...(password == null ? {} : { password }) });
      state.model = normalizeModel(model); state.base = clone(state.model); state.draft = clone(state.model);
      resetPasswordChanges();
      state.sheetIndex = Math.min(state.sheetIndex, Math.max(0, state.draft.worksheets.length - 1));
      renderDialog(); dialogStatus('页面布局、批注和保护差量已写入；Ctrl+Z 可统一撤销。');
      if (typeof setStatus === 'function') setStatus('已更新 Excel 页面布局、审阅与保护');
      if (typeof scheduleRefresh === 'function') scheduleRefresh(true);
    } catch (error) { dialogStatus(error?.message || String(error), true); }
    finally { setBusy(false); }
  }
  async function resetJournal() {
    if (!confirm('恢复所有页面布局、批注和保护设置到本次导入时状态？')) return;
    const hasProtection = !!state.model?.workbookProtection
      || (state.model?.worksheets || []).some((sheet) => !!sheet.sheetProtection);
    let password;
    if (hasProtection) {
      password = prompt('恢复操作会更改当前保护状态。请输入现有保护密码：', '');
      if (password == null) return;
    }
    try {
      setBusy(true); const model = await apiPost('/api/page-review', { op: 'reset', ...(password == null ? {} : { password }) });
      state.model = normalizeModel(model); state.base = clone(state.model); state.draft = clone(state.model);
      resetPasswordChanges();
      renderDialog(); dialogStatus('已恢复导入状态。');
    } catch (error) { dialogStatus(error?.message || String(error), true); }
    finally { setBusy(false); }
  }

  window.openPageReviewEditor = openPageReviewEditor;
  window.__pageReviewEditorTest = Object.freeze({
    PAPER_SIZES, ensureDialog, normalizeModel, normalizeWorksheet,
    attributePatch, buildWorksheetPatch, buildPackagePatchFor, pageReviewPatchNeedsPassword,
    noteRunModel, noteTextXml, editableNoteRuns,
    request(op, patch) {
      return { url: '/api/page-review', body: op === 'list' ? { op: 'list' }
        : op === 'reset' ? { op: 'reset' } : { op: 'update', patch } };
    },
  });
  byId('btn-page-review')?.addEventListener('click', openPageReviewEditor);
})();
