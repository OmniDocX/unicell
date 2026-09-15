/* Excel-native table / AutoFilter / multi-sort editor.
 *
 * The server applies these requests as validated OOXML byte-range edits.  This UI deliberately
 * emits differential operations for existing columns, filter columns and sort conditions so an
 * imported workbook's unknown attributes, extLst payloads and relationships remain untouched.
 */
(() => {
  'use strict';

  const byId = (id) => document.getElementById(id);
  const clone = (value) => value == null ? value : (typeof structuredClone === 'function'
    ? structuredClone(value) : JSON.parse(JSON.stringify(value)));
  const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
  const boolValue = (value, fallback = false) => value == null ? fallback
    : value === true || value === 1 || value === '1' || value === 'true';
  const numberValue = (value, fallback = null) => value == null || value === '' ? fallback
    : Number.isFinite(Number(value)) ? Number(value) : fallback;
  const setStatusMessage = (message) => {
    if (typeof setStatus === 'function') setStatus(message);
  };
  const FILTER_KINDS = Object.freeze([
    ['filters', '值 / 日期组'],
    ['customFilters', '自定义条件'],
    ['dynamicFilter', '动态日期 / 平均值'],
    ['top10', '前十项 / 后十项'],
    ['colorFilter', '单元格 / 字体颜色'],
    ['iconFilter', '图标集'],
  ]);
  const SORT_BY = Object.freeze([
    ['', '单元格值'], ['cellColor', '单元格颜色'], ['fontColor', '字体颜色'], ['icon', '图标'],
  ]);
  const TOTAL_FUNCTIONS = Object.freeze([
    ['', '无'], ['sum', '求和'], ['average', '平均值'], ['count', '计数'], ['countNums', '数值计数'],
    ['max', '最大值'], ['min', '最小值'], ['stdDev', '标准偏差'], ['var', '方差'], ['custom', '自定义公式'],
  ]);
  const CUSTOM_OPERATORS = Object.freeze([
    ['equal', '等于'], ['notEqual', '不等于'], ['greaterThan', '大于'],
    ['greaterThanOrEqual', '大于等于'], ['lessThan', '小于'], ['lessThanOrEqual', '小于等于'],
  ]);
  const DYNAMIC_TYPES = Object.freeze([
    'aboveAverage', 'belowAverage', 'today', 'yesterday', 'tomorrow', 'thisWeek', 'lastWeek',
    'nextWeek', 'thisMonth', 'lastMonth', 'nextMonth', 'thisQuarter', 'lastQuarter', 'nextQuarter',
    'thisYear', 'lastYear', 'nextYear', 'yearToDate', 'Q1', 'Q2', 'Q3', 'Q4',
    'M1', 'M2', 'M3', 'M4', 'M5', 'M6', 'M7', 'M8', 'M9', 'M10', 'M11', 'M12',
  ]);
  const STYLE_NAMES = Object.freeze([
    'TableStyleMedium2', 'TableStyleMedium4', 'TableStyleMedium9', 'TableStyleMedium15',
    'TableStyleLight1', 'TableStyleLight9', 'TableStyleLight16',
    'TableStyleDark1', 'TableStyleDark4', 'TableStyleDark11',
  ]);

  const state = {
    model: null, kind: 'table', key: null, base: null, draft: null, tab: 'table',
    rewriteStructuredReferences: true, allowDeleteReferencedColumns: false,
    allowDeleteReferencedTable: false, manualPatch: null, loading: false,
  };

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
  function option(value, label, selected = false) {
    const element = node('option', label);
    element.value = String(value); element.selected = selected;
    return element;
  }
  function section(title, hint = '') {
    const element = node('section', null, 'nte-section');
    element.appendChild(node('h3', title));
    if (hint) element.appendChild(node('p', hint, 'nte-hint'));
    return element;
  }
  function labeled(label, control, className = '') {
    const row = node('label', null, `nte-field ${className}`.trim());
    row.append(node('span', label), control);
    return row;
  }
  function textInput(value, onInput, options = {}) {
    const input = document.createElement(options.multiline ? 'textarea' : 'input');
    if (!options.multiline) input.type = options.type || 'text';
    input.value = value == null ? '' : String(value);
    if (options.placeholder) input.placeholder = options.placeholder;
    if (options.readOnly) input.readOnly = true;
    if (options.min != null) input.min = String(options.min);
    if (options.max != null) input.max = String(options.max);
    if (options.step != null) input.step = String(options.step);
    input.addEventListener(options.commit ? 'change' : 'input', () => onInput(input.value, input));
    return input;
  }
  function selectInput(value, choices, onChange) {
    const select = document.createElement('select');
    const normalized = choices.map((choice) => Array.isArray(choice) ? choice : [choice, choice]);
    if (value != null && value !== '' && !normalized.some(([key]) => String(key) === String(value))) {
      normalized.unshift([value, `${value}（原生值）`]);
    }
    for (const choice of normalized) {
      const [key, label] = Array.isArray(choice) ? choice : [choice, choice];
      select.appendChild(option(key, label, String(key) === String(value ?? '')));
    }
    select.addEventListener('change', () => onChange(select.value, select));
    return select;
  }
  function checkbox(label, checked, onChange, title = '') {
    const wrapper = node('label', null, 'nte-check');
    const input = document.createElement('input'); input.type = 'checkbox'; input.checked = !!checked;
    input.addEventListener('change', () => onChange(input.checked, input));
    wrapper.append(input, node('span', label));
    if (title) wrapper.title = title;
    return wrapper;
  }

  function columnLetters(column) {
    let value = Math.max(1, Number(column) || 1), result = '';
    while (value) { value -= 1; result = String.fromCharCode(65 + value % 26) + result; value = Math.floor(value / 26); }
    return result;
  }
  function columnNumber(letters) {
    let result = 0;
    for (const char of String(letters).toUpperCase()) result = result * 26 + char.charCodeAt(0) - 64;
    return result;
  }
  function parseRange(reference) {
    const raw = String(reference || '').split('!').pop().replace(/\$/g, '').trim();
    const match = /^([A-Z]+)(\d+)(?::([A-Z]+)(\d+))?$/i.exec(raw);
    if (!match) return null;
    const range = {
      c0: columnNumber(match[1]), r0: Number(match[2]),
      c1: columnNumber(match[3] || match[1]), r1: Number(match[4] || match[2]),
    };
    return range.c0 > 0 && range.r0 > 0 && range.c1 >= range.c0 && range.r1 >= range.r0 ? range : null;
  }
  function formatRange(range) {
    return `${columnLetters(range.c0)}${range.r0}:${columnLetters(range.c1)}${range.r1}`;
  }
  function selectionRange() {
    const selected = typeof normSel === 'function' ? normSel() : (S?.sel || { r0: 1, c0: 1, r1: 1, c1: 1 });
    return formatRange({
      r0: Math.min(selected.r0, selected.r1), c0: Math.min(selected.c0, selected.c1),
      r1: Math.max(selected.r0, selected.r1), c1: Math.max(selected.c0, selected.c1),
    });
  }
  function dataRange(reference, headerRows = 1, totalRows = 0, columnOffset = null) {
    const range = parseRange(reference);
    if (!range) return reference || '';
    range.r0 = Math.min(range.r1, range.r0 + Number(headerRows || 0));
    range.r1 = Math.max(range.r0, range.r1 - Number(totalRows || 0));
    if (columnOffset != null) range.c0 = range.c1 = Math.min(range.c1, range.c0 + Number(columnOffset));
    return formatRange(range);
  }
  function excelSerial(date) {
    return (Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) - Date.UTC(1899, 11, 30)) / 86400000;
  }
  function dynamicDateInterval(type) {
    const now = new Date(), day = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const addDays = (date, count) => new Date(date.getFullYear(), date.getMonth(), date.getDate() + count);
    const month = (offset = 0) => new Date(day.getFullYear(), day.getMonth() + offset, 1);
    const year = (offset = 0) => new Date(day.getFullYear() + offset, 0, 1);
    const quarter = (offset = 0) => new Date(day.getFullYear(), Math.floor(day.getMonth() / 3) * 3 + offset * 3, 1);
    const week = (offset = 0) => addDays(day, -((day.getDay() + 6) % 7) + offset * 7);
    let start, end;
    if (type === 'today') [start, end] = [day, addDays(day, 1)];
    else if (type === 'yesterday') [start, end] = [addDays(day, -1), day];
    else if (type === 'tomorrow') [start, end] = [addDays(day, 1), addDays(day, 2)];
    else if (type === 'thisWeek') [start, end] = [week(), week(1)];
    else if (type === 'lastWeek') [start, end] = [week(-1), week()];
    else if (type === 'nextWeek') [start, end] = [week(1), week(2)];
    else if (type === 'thisMonth') [start, end] = [month(), month(1)];
    else if (type === 'lastMonth') [start, end] = [month(-1), month()];
    else if (type === 'nextMonth') [start, end] = [month(1), month(2)];
    else if (type === 'thisQuarter') [start, end] = [quarter(), quarter(1)];
    else if (type === 'lastQuarter') [start, end] = [quarter(-1), quarter()];
    else if (type === 'nextQuarter') [start, end] = [quarter(1), quarter(2)];
    else if (type === 'thisYear') [start, end] = [year(), year(1)];
    else if (type === 'lastYear') [start, end] = [year(-1), year()];
    else if (type === 'nextYear') [start, end] = [year(1), year(2)];
    else if (type === 'yearToDate') [start, end] = [year(), addDays(day, 1)];
    else if (/^Q[1-4]$/.test(type)) {
      start = new Date(day.getFullYear(), (Number(type.slice(1)) - 1) * 3, 1);
      end = new Date(day.getFullYear(), Number(type.slice(1)) * 3, 1);
    } else if (/^M(?:[1-9]|1[0-2])$/.test(type)) {
      start = new Date(day.getFullYear(), Number(type.slice(1)) - 1, 1);
      end = new Date(day.getFullYear(), Number(type.slice(1)), 1);
    }
    return start && end ? [excelSerial(start), excelSerial(end)] : null;
  }
  function runtimeFilter(filterColumn, firstColumn) {
    const definition = filterColumn.definition || {}, config = definition.config || {};
    const col = firstColumn + Number(filterColumn.columnId || 0);
    if (definition.kind === 'filters') {
      const result = { col, kind: 'values', values: config.values || [], includeBlank: !!config.blank };
      if ((config.dateGroups || []).length && !(config.values || []).length) {
        return { col, kind: 'dateGroups', groups: config.dateGroups };
      }
      return result;
    }
    if (definition.kind === 'customFilters') {
      const conditions = (config.conditions || []).slice(0, 2);
      if (!conditions.length) return null;
      return {
        col, kind: 'custom', operator: conditions[0].operator || 'equal', value: String(conditions[0].val ?? ''),
        ...(conditions[1] ? { second: { operator: conditions[1].operator || 'equal', value: String(conditions[1].val ?? '') }, join: config.and ? 'and' : 'or' } : {}),
      };
    }
    if (definition.kind === 'dynamicFilter') {
      if (config.type === 'aboveAverage' || config.type === 'belowAverage') {
        return { col, kind: 'dynamic', direction: config.type };
      }
      const interval = dynamicDateInterval(config.type);
      return interval ? { col, kind: 'custom', operator: 'greaterThanOrEqual', value: String(interval[0]),
        second: { operator: 'lessThan', value: String(interval[1]) }, join: 'and' } : null;
    }
    if (definition.kind === 'top10') return { col, kind: 'top10', direction: config.top === false ? 'bottom' : 'top', percent: !!config.percent, count: Number(config.val || 10) };
    if (definition.kind === 'colorFilter') return { col, kind: 'color', source: config.cellColor === false ? 'font' : 'fill', dxfId: Number(config.dxfId || 0) };
    return null; // icon filters remain native and are evaluated by Excel's conditional-format engine.
  }
  function buildRuntimeOperations(base, draft) {
    const operations = [], tableRange = parseRange(draft.reference || draft.autoFilter?.reference);
    if (!tableRange) return operations;
    const headerRows = state.kind === 'sheet' ? 1 : Number(draft.headerRowCount || 0);
    const totalRows = state.kind === 'sheet' ? 0 : Number(draft.totalsRowCount || 0);
    const data = parseRange(dataRange(draft.reference || draft.autoFilter?.reference, headerRows, totalRows));
    const sort = draft.autoFilter?.sortState || draft.sortState;
    if (sort && data && (sort.conditions || []).length) {
      const conditions = sort.conditions.map((condition) => {
        const reference = parseRange(condition.reference);
        return reference ? { col: reference.c0, order: condition.descending ? 'descending' : 'ascending',
          sortOn: condition.sortBy || 'values', ...(condition.dxfId != null ? { dxfId: Number(condition.dxfId) } : {}) } : null;
      }).filter(Boolean);
      if (conditions.length) operations.push({ type: 'sort', request: { sheet: Number(S?.sheet || 0), ...data, headerRows: 0,
        r0: data.r0, c0: data.c0, r1: data.r1, c1: data.c1, caseSensitive: !!sort.caseSensitive, conditions } });
    }
    const filterRange = parseRange(draft.autoFilter?.reference || draft.reference);
    if (draft.autoFilter && filterRange) {
      const filters = (draft.autoFilter.filterColumns || []).map((column) => runtimeFilter(column, filterRange.c0)).filter(Boolean);
      operations.push({ type: 'filter', request: { sheet: Number(S?.sheet || 0), r0: filterRange.r0, c0: filterRange.c0,
        r1: Math.max(filterRange.r0, filterRange.r1 - totalRows), c1: filterRange.c1, headerRows, ...(filters.length ? { filters } : { clear: true }) } });
    } else {
      const oldRange = parseRange(base?.autoFilter?.reference || base?.reference);
      if (oldRange) operations.push({ type: 'filter', request: { sheet: Number(S?.sheet || 0), r0: oldRange.r0, c0: oldRange.c0,
        r1: oldRange.r1, c1: oldRange.c1, headerRows: state.kind === 'sheet' ? 1 : Number(base?.headerRowCount || 0), clear: true } });
    }
    return operations;
  }
  function currentSheetName() { return Array.isArray(S?.sheets) ? (S.sheets[S.sheet] || '') : ''; }
  function currentWorksheet() {
    const worksheets = state.model?.worksheets || [];
    return worksheets.find((sheet) => sheet.sheet === currentSheetName())
      || worksheets.find((sheet) => Number(sheet.sheetId) === Number(S?.sheet) + 1)
      || worksheets[Number(S?.sheet) || 0] || worksheets[0] || null;
  }
  function selectedHeaderNames(width, row, firstColumn) {
    const used = new Set();
    return Array.from({ length: width }, (_, offset) => {
      const cached = S?.cellsCache?.get?.(`${row},${firstColumn + offset}`);
      let name = String(cached?.v ?? cached?.content ?? '').trim() || `Column${offset + 1}`;
      const root = name; let suffix = 2;
      while (used.has(name.toLocaleLowerCase())) name = `${root}_${suffix++}`;
      used.add(name.toLocaleLowerCase());
      return name;
    });
  }

  function defaultFilterDefinition(kind) {
    if (kind === 'filters') return { blank: false, calendarType: null, values: [], dateGroups: [] };
    if (kind === 'customFilters') return { and: false, conditions: [{ operator: 'equal', val: '' }] };
    if (kind === 'dynamicFilter') return { type: 'thisMonth', val: null, maxVal: null };
    if (kind === 'top10') return { top: true, percent: false, val: 10, filterVal: null };
    if (kind === 'colorFilter') return { dxfId: 0, cellColor: true };
    if (kind === 'iconFilter') return { iconSet: '3Arrows', iconId: 0 };
    return {};
  }
  function attr(object, key, fallback = null) {
    return object?.[key] ?? object?.attributes?.[key] ?? fallback;
  }
  function normalizeDefinition(definition) {
    if (!definition?.kind) return { kind: 'filters', config: defaultFilterDefinition('filters'), hasExtensions: false };
    const kind = definition.kind;
    const config = defaultFilterDefinition(kind);
    for (const key of Object.keys(config)) {
      if (key === 'values') {
        config.values = (definition.criteria || []).filter((item) => item.kind === 'filter')
          .map((item) => String(attr(item, 'val', '')));
      } else if (key === 'dateGroups') {
        config.dateGroups = (definition.criteria || []).filter((item) => item.kind === 'dateGroupItem')
          .map((item) => clone(item.attributes || {}));
      } else if (key === 'conditions') {
        config.conditions = (definition.criteria || []).filter((item) => item.kind === 'customFilter')
          .map((item) => ({ operator: attr(item, 'operator', 'equal'), val: String(attr(item, 'val', '')) }));
        if (!config.conditions.length) config.conditions.push({ operator: 'equal', val: '' });
      } else {
        const value = attr(definition, key, config[key]);
        config[key] = typeof config[key] === 'boolean' ? boolValue(value, config[key]) : value;
      }
    }
    return { kind, config, hasExtensions: !!definition.hasExtensions };
  }
  function normalizeFilterColumn(column, index) {
    return {
      sourceIndex: Number(column?.sourceIndex ?? index), columnId: Number(column?.columnId ?? 0),
      hiddenButton: !!column?.hiddenButton, showButton: column?.showButton !== false,
      definition: normalizeDefinition(column?.definition), hasExtensions: !!column?.hasExtensions,
      _new: !!column?._new,
    };
  }
  function normalizeSort(sort) {
    if (!sort) return null;
    return {
      reference: sort.reference || '', caseSensitive: !!sort.caseSensitive,
      columnSort: !!sort.columnSort, sortMethod: sort.sortMethod || '',
      hasExtensions: !!sort.hasExtensions,
      conditions: (sort.conditions || []).map((condition, index) => ({
        sourceIndex: Number(condition.sourceIndex ?? index), reference: condition.reference || '',
        descending: !!condition.descending, sortBy: condition.sortBy || '',
        dxfId: condition.dxfId ?? null, iconSet: condition.iconSet ?? null,
        iconId: condition.iconId ?? null, customList: condition.customList ?? null,
        _new: !!condition._new,
      })),
    };
  }
  function normalizeAutoFilter(filter) {
    if (!filter) return null;
    return {
      reference: filter.reference || '', hasExtensions: !!filter.hasExtensions,
      filterColumns: (filter.filterColumns || []).map(normalizeFilterColumn),
      sortState: normalizeSort(filter.sortState),
    };
  }
  function normalizeStyle(style) {
    if (!style) return null;
    return {
      name: style.name || '', showFirstColumn: !!style.showFirstColumn,
      showLastColumn: !!style.showLastColumn, showRowStripes: !!style.showRowStripes,
      showColumnStripes: !!style.showColumnStripes,
    };
  }
  function normalizeTable(table) {
    return {
      part: table.part, sheet: table.sheet, sheetId: table.sheetId, sheetPart: table.sheetPart,
      id: Number(table.id), name: table.name || table.displayName || '',
      displayName: table.displayName || table.name || '', reference: table.reference || 'A1:A1',
      headerRowCount: Number(table.headerRowCount ?? 1), totalsRowCount: Number(table.totalsRowCount ?? 0),
      totalsRowShown: !!table.totalsRowShown, insertRow: !!table.insertRow,
      insertRowShift: !!table.insertRowShift, published: !!table.published,
      columns: (table.columns || []).map((column, index) => ({
        sourceIndex: Number(column.sourceIndex ?? index), id: Number(column.id), name: column.name || `Column${index + 1}`,
        totalsRowLabel: column.totalsRowLabel ?? null, totalsRowFunction: column.totalsRowFunction ?? null,
        calculatedColumnFormula: column.calculatedColumnFormula ?? null,
        totalsRowFormula: column.totalsRowFormula ?? null,
        hasXmlColumnProperties: !!column.hasXmlColumnProperties, hasExtensions: !!column.hasExtensions,
        _new: !!column._new,
      })),
      autoFilter: normalizeAutoFilter(table.autoFilter), sortState: normalizeSort(table.sortState),
      styleInfo: normalizeStyle(table.styleInfo), hasExtensions: !!table.hasExtensions,
    };
  }
  function normalizeWorksheet(sheet) {
    return {
      sheet: sheet.sheet, sheetId: sheet.sheetId, sheetPart: sheet.sheetPart,
      autoFilter: normalizeAutoFilter(sheet.autoFilter), sortState: normalizeSort(sheet.sortState),
    };
  }

  function serializeDefinition(definition) {
    const config = definition?.config || {};
    const payload = {};
    if (definition.kind === 'filters') {
      payload.blank = !!config.blank;
      if (config.calendarType) payload.calendarType = config.calendarType;
      payload.values = (config.values || []).map(String);
      payload.dateGroups = clone(config.dateGroups || []);
    } else if (definition.kind === 'customFilters') {
      payload.and = !!config.and;
      payload.conditions = (config.conditions || []).slice(0, 2).map((item) => ({
        operator: item.operator || 'equal', val: String(item.val ?? ''),
      }));
    } else if (definition.kind === 'dynamicFilter') {
      payload.type = config.type || 'thisMonth';
      if (config.val != null && config.val !== '') payload.val = config.val;
      if (config.maxVal != null && config.maxVal !== '') payload.maxVal = config.maxVal;
    } else if (definition.kind === 'top10') {
      payload.top = config.top !== false; payload.percent = !!config.percent;
      payload.val = numberValue(config.val, 10);
      if (config.filterVal != null && config.filterVal !== '') payload.filterVal = numberValue(config.filterVal, config.filterVal);
    } else if (definition.kind === 'colorFilter') {
      payload.dxfId = Math.max(0, numberValue(config.dxfId, 0)); payload.cellColor = config.cellColor !== false;
    } else if (definition.kind === 'iconFilter') {
      payload.iconSet = config.iconSet || '3Arrows'; payload.iconId = Math.max(0, numberValue(config.iconId, 0));
    }
    return payload;
  }
  function serializeFilterColumn(column) {
    const payload = {
      colId: Math.max(0, Number(column.columnId) || 0), hiddenButton: !!column.hiddenButton,
      showButton: column.showButton !== false,
    };
    payload[column.definition.kind] = serializeDefinition(column.definition);
    return payload;
  }
  function serializeSortCondition(condition) {
    const payload = { ref: condition.reference || '', descending: !!condition.descending };
    if (condition.sortBy) payload.sortBy = condition.sortBy;
    if (condition.dxfId != null && condition.dxfId !== '') payload.dxfId = Math.max(0, Number(condition.dxfId) || 0);
    if (condition.iconSet) payload.iconSet = condition.iconSet;
    if (condition.iconId != null && condition.iconId !== '') payload.iconId = Math.max(0, Number(condition.iconId) || 0);
    if (condition.customList) payload.customList = condition.customList;
    return payload;
  }
  function serializeSort(sort) {
    return {
      ref: sort.reference || '', caseSensitive: !!sort.caseSensitive, columnSort: !!sort.columnSort,
      sortMethod: sort.sortMethod || null,
      conditions: (sort.conditions || []).map(serializeSortCondition),
    };
  }
  function serializeAutoFilter(filter) {
    return {
      ref: filter.reference || '', filterColumns: (filter.filterColumns || []).map(serializeFilterColumn),
      sortState: filter.sortState ? serializeSort(filter.sortState) : null,
    };
  }

  function sortMetadata(sort) {
    return sort ? {
      reference: sort.reference || '', caseSensitive: !!sort.caseSensitive,
      columnSort: !!sort.columnSort, sortMethod: sort.sortMethod || '',
    } : null;
  }
  function sortConditionComparable(condition) {
    return serializeSortCondition(condition);
  }
  function buildSortPatch(base, draft) {
    if (!base && !draft) return undefined;
    if (base && !draft) return null;
    if (!base && draft) return serializeSort(draft);
    const patch = {};
    const oldMeta = sortMetadata(base), newMeta = sortMetadata(draft);
    const mapping = [['reference', 'ref'], ['caseSensitive', 'caseSensitive'], ['columnSort', 'columnSort'], ['sortMethod', 'sortMethod']];
    for (const [modelKey, patchKey] of mapping) if (!equal(oldMeta[modelKey], newMeta[modelKey])) {
      patch[patchKey] = modelKey === 'sortMethod' && !newMeta[modelKey] ? null : newMeta[modelKey];
    }
    const baseExisting = (base.conditions || []).map((item, index) => ({ ...item, sourceIndex: Number(item.sourceIndex ?? index) }));
    const draftExisting = (draft.conditions || []).filter((item) => !item._new);
    const operations = [];
    for (const current of draftExisting) {
      const original = baseExisting.find((item) => item.sourceIndex === current.sourceIndex);
      if (original && !equal(sortConditionComparable(original), sortConditionComparable(current))) {
        operations.push({ op: 'update', sourceIndex: original.sourceIndex, patch: serializeSortCondition(current) });
      }
    }
    const keptIndexes = new Set(draftExisting.map((item) => item.sourceIndex));
    for (const original of [...baseExisting].sort((a, b) => b.sourceIndex - a.sourceIndex)) {
      if (!keptIndexes.has(original.sourceIndex)) operations.push({ op: 'delete', sourceIndex: original.sourceIndex });
    }
    const additions = (draft.conditions || []).filter((item) => item._new);
    for (const item of additions) operations.push({ op: 'add', patch: serializeSortCondition(item) });
    const currentTokens = baseExisting.filter((item) => keptIndexes.has(item.sourceIndex))
      .map((item) => `b:${item.sourceIndex}`).concat(additions.map((_, index) => `n:${index}`));
    let nextNew = 0;
    const desiredTokens = (draft.conditions || []).map((item) => item._new ? `n:${nextNew++}` : `b:${item.sourceIndex}`);
    const order = desiredTokens.map((token) => currentTokens.indexOf(token));
    if (order.length > 1 && order.every((index) => index >= 0) && order.some((index, position) => index !== position)) {
      operations.push({ op: 'reorder', order });
    }
    if (operations.length) patch.conditionOperations = operations;
    return Object.keys(patch).length ? patch : undefined;
  }
  function buildFilterColumnOperations(base, draft) {
    const baseColumns = (base?.filterColumns || []).map((item, index) => ({ ...item, sourceIndex: Number(item.sourceIndex ?? index) }));
    const current = draft?.filterColumns || [];
    const operations = [];
    for (const item of current.filter((entry) => !entry._new)) {
      const original = baseColumns.find((entry) => entry.sourceIndex === item.sourceIndex);
      if (original && !equal(serializeFilterColumn(original), serializeFilterColumn(item))) {
        operations.push({ op: 'update', sourceIndex: original.sourceIndex, patch: serializeFilterColumn(item) });
      }
    }
    const kept = new Set(current.filter((entry) => !entry._new).map((entry) => entry.sourceIndex));
    for (const original of [...baseColumns].sort((a, b) => b.sourceIndex - a.sourceIndex)) {
      if (!kept.has(original.sourceIndex)) operations.push({ op: 'delete', sourceIndex: original.sourceIndex });
    }
    for (const item of current.filter((entry) => entry._new)) operations.push({ op: 'add', patch: serializeFilterColumn(item) });
    return operations;
  }
  function buildAutoFilterPatch(base, draft) {
    if (!base && !draft) return undefined;
    if (base && !draft) return null;
    if (!base && draft) return serializeAutoFilter(draft);
    const patch = {};
    if (String(base.reference || '') !== String(draft.reference || '')) patch.ref = draft.reference || null;
    const operations = buildFilterColumnOperations(base, draft);
    if (operations.length) patch.filterColumnOperations = operations;
    const sortPatch = buildSortPatch(base.sortState, draft.sortState);
    if (sortPatch !== undefined) patch.sortState = sortPatch;
    return Object.keys(patch).length ? patch : undefined;
  }
  function styleComparable(style) {
    return style ? {
      name: style.name || '', showFirstColumn: !!style.showFirstColumn,
      showLastColumn: !!style.showLastColumn, showRowStripes: !!style.showRowStripes,
      showColumnStripes: !!style.showColumnStripes,
    } : null;
  }
  function buildColumnOperations(baseColumns, draftColumns) {
    const operations = [];
    const byId = new Map((baseColumns || []).map((column) => [Number(column.id), column]));
    for (const column of draftColumns || []) {
      const original = !column._new ? byId.get(Number(column.id)) : null;
      const payload = {
        id: Number(column.id), name: String(column.name || ''),
        totalsRowLabel: column.totalsRowLabel || null,
        totalsRowFunction: column.totalsRowFunction || null,
        calculatedColumnFormula: column.calculatedColumnFormula || null,
        totalsRowFormula: column.totalsRowFormula || null,
      };
      if (!original) operations.push({ op: 'add', patch: payload });
      else {
        const originalPayload = {
          id: Number(original.id), name: String(original.name || ''),
          totalsRowLabel: original.totalsRowLabel || null,
          totalsRowFunction: original.totalsRowFunction || null,
          calculatedColumnFormula: original.calculatedColumnFormula || null,
          totalsRowFormula: original.totalsRowFormula || null,
        };
        if (!equal(originalPayload, payload)) operations.push({ op: 'update', id: Number(original.id), patch: payload });
      }
    }
    const currentIds = new Set((draftColumns || []).filter((column) => !column._new).map((column) => Number(column.id)));
    for (const original of baseColumns || []) if (!currentIds.has(Number(original.id))) {
      operations.push({ op: 'delete', id: Number(original.id) });
    }
    return operations;
  }
  function buildTablePartPatch(base, draft) {
    const patch = {};
    for (const [key, patchKey = key] of [
      ['name'], ['displayName'], ['reference', 'ref'], ['headerRowCount'], ['totalsRowCount'],
      ['totalsRowShown'], ['insertRow'], ['insertRowShift'], ['published'],
    ]) if (!equal(base[key], draft[key])) patch[patchKey] = draft[key];
    const columns = buildColumnOperations(base.columns, draft.columns);
    if (columns.length) patch.columnOperations = columns;
    const autoFilter = buildAutoFilterPatch(base.autoFilter, draft.autoFilter);
    if (autoFilter !== undefined) patch.autoFilter = autoFilter;
    const sortState = buildSortPatch(base.sortState, draft.sortState);
    if (sortState !== undefined) patch.sortState = sortState;
    if (!equal(styleComparable(base.styleInfo), styleComparable(draft.styleInfo))) {
      patch.styleInfo = draft.styleInfo ? styleComparable(draft.styleInfo) : null;
    }
    return patch;
  }
  function buildCreatePayload(draft) {
    return {
      sheetPart: draft.sheetPart, sheet: draft.sheet, sheetId: Number(draft.sheetId),
      name: draft.name, displayName: draft.displayName, ref: draft.reference,
      headerRowCount: Number(draft.headerRowCount), totalsRowCount: Number(draft.totalsRowCount),
      totalsRowShown: !!draft.totalsRowShown,
      columns: draft.columns.map((column) => ({
        id: Number(column.id), name: column.name,
        totalsRowLabel: column.totalsRowLabel || null,
        totalsRowFunction: column.totalsRowFunction || null,
        calculatedColumnFormula: column.calculatedColumnFormula || null,
        totalsRowFormula: column.totalsRowFormula || null,
      })),
      autoFilter: draft.autoFilter ? serializeAutoFilter(draft.autoFilter) : false,
      sortState: draft.sortState ? serializeSort(draft.sortState) : null,
      styleInfo: draft.styleInfo ? styleComparable(draft.styleInfo) : false,
    };
  }
  function buildPackagePatch() {
    if (state.manualPatch) return clone(state.manualPatch);
    if (state.kind === 'create') return { createTables: [buildCreatePayload(state.draft)] };
    if (state.kind === 'sheet') {
      const patch = {};
      const filter = buildAutoFilterPatch(state.base.autoFilter, state.draft.autoFilter);
      const sort = buildSortPatch(state.base.sortState, state.draft.sortState);
      if (filter !== undefined) patch.autoFilter = filter;
      if (sort !== undefined) patch.sortState = sort;
      return Object.keys(patch).length ? { worksheetEdits: [{ part: state.draft.sheetPart, patch }] } : {};
    }
    const patch = buildTablePartPatch(state.base, state.draft);
    const packagePatch = Object.keys(patch).length ? { tableEdits: [{ part: state.draft.part, patch }] } : {};
    if (state.rewriteStructuredReferences) packagePatch.rewriteStructuredReferences = true;
    if (state.allowDeleteReferencedColumns) packagePatch.allowDeleteReferencedColumns = true;
    return packagePatch;
  }

  function ensureDialog() {
    let dialog = byId('native-table-dialog');
    if (dialog) return dialog;
    dialog = node('div', null, 'nte-dialog'); dialog.id = 'native-table-dialog'; dialog.hidden = true;
    dialog.innerHTML = `
      <header class="nte-title"><strong>Excel 表、排序与自动筛选</strong><span class="nte-subtitle"></span><button type="button" data-action="close" aria-label="关闭">×</button></header>
      <div class="nte-note">直接编辑 Excel 原生 table / autoFilter / sortState。仅提交差量，未触碰的 OOXML 扩展和样式保持原样。</div>
      <main class="nte-main">
        <aside class="nte-sidebar"><div class="nte-list"></div><div class="nte-side-actions"></div></aside>
        <section class="nte-work"><nav class="nte-tabs"></nav><div class="nte-body"></div></section>
      </main>
      <footer class="nte-footer"><span class="nte-status"></span><button type="button" data-action="reset">恢复导入状态</button><button type="button" class="primary" data-action="save">应用到工作簿</button></footer>`;
    document.body.appendChild(dialog);
    dialog.querySelector('[data-action="close"]').addEventListener('click', () => {
      dialog.hidden = true; byId('grid-scroll')?.focus();
    });
    dialog.querySelector('[data-action="save"]').addEventListener('click', saveDraft);
    dialog.querySelector('[data-action="reset"]').addEventListener('click', resetJournal);
    dialog.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') dialog.querySelector('[data-action="close"]').click();
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') { event.preventDefault(); saveDraft(); }
    });
    return dialog;
  }
  function dialogStatus(message, isError = false) {
    const status = ensureDialog().querySelector('.nte-status');
    status.textContent = message || ''; status.classList.toggle('error', !!isError);
  }
  function setBusy(busy) {
    state.loading = busy;
    const dialog = ensureDialog();
    dialog.classList.toggle('busy', busy);
    dialog.querySelectorAll('button,input,select,textarea').forEach((control) => {
      if (!control.matches('[data-action="close"]')) control.disabled = busy;
    });
  }

  function tableKey(table) { return `table:${table.part}`; }
  function sheetKey(sheet) { return `sheet:${sheet.sheetPart}`; }
  function selectKey(key) {
    const table = (state.model?.tables || []).find((entry) => tableKey(entry) === key);
    if (table) {
      state.kind = 'table'; state.key = key; state.base = normalizeTable(table); state.draft = clone(state.base); state.tab = 'table';
    } else {
      const sheet = (state.model?.worksheets || []).find((entry) => sheetKey(entry) === key) || currentWorksheet();
      if (!sheet) { state.kind = 'sheet'; state.key = null; state.base = state.draft = null; return; }
      state.kind = 'sheet'; state.key = sheetKey(sheet); state.base = normalizeWorksheet(sheet); state.draft = clone(state.base); state.tab = 'filter';
    }
    state.manualPatch = null;
  }
  function nextTableName() {
    const used = new Set((state.model?.tables || []).flatMap((table) => [table.name, table.displayName]).map((name) => String(name).toLowerCase()));
    let suffix = Math.max(1, ...(state.model?.tables || []).map((table) => Number(table.id) || 0)) + 1;
    while (used.has(`table${suffix}`.toLowerCase())) suffix += 1;
    return `Table${suffix}`;
  }
  function createFromSelection() {
    const sheet = currentWorksheet();
    if (!sheet) { dialogStatus('当前工作簿没有可用工作表。', true); return; }
    const reference = selectionRange(), parsed = parseRange(reference);
    if (!parsed) { dialogStatus('当前选区不是有效 A1 区域。', true); return; }
    const width = parsed.c1 - parsed.c0 + 1, names = selectedHeaderNames(width, parsed.r0, parsed.c0);
    const name = nextTableName();
    state.kind = 'create'; state.key = 'create'; state.base = null; state.tab = 'table'; state.manualPatch = null;
    state.draft = {
      sheet: sheet.sheet, sheetId: sheet.sheetId, sheetPart: sheet.sheetPart,
      name, displayName: name, reference, headerRowCount: 1, totalsRowCount: 0,
      totalsRowShown: false, insertRow: false, insertRowShift: false, published: false,
      columns: names.map((columnName, index) => ({
        id: index + 1, name: columnName, totalsRowLabel: null, totalsRowFunction: null,
        calculatedColumnFormula: null, totalsRowFormula: null, _new: true,
      })),
      autoFilter: normalizeAutoFilter({ reference, filterColumns: [], sortState: null }),
      sortState: null,
      styleInfo: normalizeStyle({ name: 'TableStyleMedium2', showRowStripes: true }),
    };
    renderDialog(); dialogStatus('已从当前选区生成表草稿；应用前不会修改工作簿。');
  }
  function useSelectionForRangeFilter() {
    const sheet = currentWorksheet();
    if (!sheet) { dialogStatus('当前工作簿没有可用工作表。', true); return; }
    selectKey(sheetKey(sheet));
    state.draft.autoFilter = normalizeAutoFilter({ reference: selectionRange(), filterColumns: [], sortState: null });
    state.tab = 'filter'; renderDialog(); dialogStatus('已用当前选区创建普通自动筛选草稿。');
  }

  function renderList() {
    const dialog = ensureDialog(), list = dialog.querySelector('.nte-list'); list.replaceChildren();
    list.appendChild(node('h4', 'Excel 表'));
    const tables = state.model?.tables || [];
    if (!tables.length) list.appendChild(node('div', '没有原生表', 'nte-empty-small'));
    for (const table of tables) {
      const item = button('', () => { selectKey(tableKey(table)); renderDialog(); });
      item.dataset.kind = 'table'; item.dataset.part = table.part;
      item.classList.toggle('active', state.kind === 'table' && state.key === tableKey(table));
      item.append(node('strong', table.displayName || table.name), node('span', `${table.sheet} · ${table.reference}`), node('small', `${table.columns?.length || 0} 列`));
      list.appendChild(item);
    }
    list.appendChild(node('h4', '普通区域筛选 / 排序'));
    for (const sheet of state.model?.worksheets || []) {
      const item = button('', () => { selectKey(sheetKey(sheet)); renderDialog(); });
      item.dataset.kind = 'worksheet'; item.dataset.part = sheet.sheetPart;
      item.classList.toggle('active', state.kind === 'sheet' && state.key === sheetKey(sheet));
      const detail = sheet.autoFilter?.reference || sheet.sortState?.reference || '尚未设置';
      item.append(node('strong', sheet.sheet), node('span', detail), node('small', sheet.autoFilter ? '自动筛选' : '普通区域'));
      list.appendChild(item);
    }
    const actions = dialog.querySelector('.nte-side-actions'); actions.replaceChildren();
    actions.append(
      button('从选区建表', createFromSelection, 'primary'),
      button('选区筛选', useSelectionForRangeFilter),
    );
    if (state.kind === 'table') actions.appendChild(button('删除表', deleteSelectedTable, 'danger'));
  }
  function renderTabs() {
    const tabs = ensureDialog().querySelector('.nte-tabs'); tabs.replaceChildren();
    const choices = state.kind === 'sheet'
      ? [['filter', '自动筛选'], ['sort', '多条件排序'], ['advanced', '高级差量']]
      : [['table', state.kind === 'create' ? '新建表' : '表'], ['columns', '列与公式'], ['filter', '自动筛选'], ['sort', '多条件排序'], ['advanced', '高级差量']];
    if (!choices.some(([key]) => key === state.tab)) state.tab = choices[0][0];
    for (const [key, label] of choices) {
      const item = button(label, () => { state.tab = key; state.manualPatch = null; renderDialog(); });
      item.dataset.tab = key; item.classList.toggle('active', key === state.tab); tabs.appendChild(item);
    }
  }
  function renderDialog() {
    const dialog = ensureDialog(); renderList(); renderTabs();
    const subtitle = dialog.querySelector('.nte-subtitle');
    subtitle.textContent = state.kind === 'create' ? `${state.draft?.sheet || ''} · ${state.draft?.reference || ''}`
      : state.kind === 'table' ? `${state.draft?.displayName || ''} · ${state.draft?.sheet || ''}`
        : `${state.draft?.sheet || ''} · 普通区域`;
    const body = dialog.querySelector('.nte-body'); body.replaceChildren();
    if (!state.draft) { body.appendChild(node('div', '没有可编辑的工作表。', 'nte-empty')); return; }
    if (state.tab === 'table') renderTableTab(body);
    else if (state.tab === 'columns') renderColumnsTab(body);
    else if (state.tab === 'filter') renderFilterTab(body);
    else if (state.tab === 'sort') renderSortTab(body);
    else renderAdvancedTab(body);
  }

  function setTableRange(reference) {
    const parsed = parseRange(reference);
    if (!parsed) { dialogStatus('表范围必须是有效的 A1 区域，例如 A1:D20。', true); return false; }
    const width = parsed.c1 - parsed.c0 + 1;
    const columns = state.draft.columns;
    let nextId = Math.max(0, ...columns.map((column) => Number(column.id) || 0)) + 1;
    while (columns.length < width) columns.push({
      id: nextId++, name: `Column${columns.length + 1}`, totalsRowLabel: null,
      totalsRowFunction: null, calculatedColumnFormula: null, totalsRowFormula: null, _new: true,
    });
    if (columns.length > width) columns.splice(width);
    state.draft.reference = formatRange(parsed);
    if (state.draft.autoFilter) {
      const filterRange = { ...parsed, r1: Math.max(parsed.r0, parsed.r1 - Number(state.draft.totalsRowCount || 0)) };
      state.draft.autoFilter.reference = formatRange(filterRange);
      state.draft.autoFilter.filterColumns = state.draft.autoFilter.filterColumns.filter((item) => item.columnId < width);
    }
    dialogStatus(`表范围已同步为 ${state.draft.reference}，列数 ${width}。`);
    return true;
  }
  function resizeRangeForColumnCount() {
    const parsed = parseRange(state.draft.reference);
    if (!parsed) return;
    parsed.c1 = parsed.c0 + Math.max(1, state.draft.columns.length) - 1;
    state.draft.reference = formatRange(parsed);
    if (state.draft.autoFilter) {
      parsed.r1 = Math.max(parsed.r0, parsed.r1 - Number(state.draft.totalsRowCount || 0));
      state.draft.autoFilter.reference = formatRange(parsed);
    }
  }
  function renderTableTab(body) {
    const draft = state.draft, metadata = section('表范围与标识', '改名会同步重写已解析到的结构化引用；缩放范围会自动补齐或裁剪右侧表列。');
    const grid = node('div', null, 'nte-grid two');
    const name = textInput(draft.displayName, (value) => { draft.name = draft.displayName = value.trim(); });
    name.dataset.field = 'table-name';
    const range = textInput(draft.reference, (value) => { if (setTableRange(value)) renderDialog(); }, { commit: true });
    range.dataset.field = 'table-range';
    grid.append(
      labeled('表名称', name),
      labeled('表范围', range),
      labeled('标题行', selectInput(draft.headerRowCount, [['1', '有标题行'], ['0', '无标题行']], (value) => { draft.headerRowCount = Number(value); })),
      labeled('汇总行', selectInput(draft.totalsRowCount, [['0', '关闭'], ['1', '开启']], (value) => {
        draft.totalsRowCount = Number(value); draft.totalsRowShown = value === '1'; setTableRange(draft.reference); renderDialog();
      })),
    );
    const tools = node('div', null, 'nte-toolbar');
    tools.append(
      button('范围取当前选区', () => { setTableRange(selectionRange()); renderDialog(); }),
      button('按当前列数调整右边界', () => { resizeRangeForColumnCount(); renderDialog(); }),
    );
    metadata.append(grid, tools); body.appendChild(metadata);

    const style = section('表样式选项', '保留 Excel 原生 TableStyle 名称；可直接输入自定义样式名。');
    const styleInfo = draft.styleInfo || (draft.styleInfo = normalizeStyle({ name: 'TableStyleMedium2', showRowStripes: true }));
    const styleName = textInput(styleInfo.name, (value) => { styleInfo.name = value.trim(); });
    styleName.setAttribute('list', 'nte-table-style-list');
    let datalist = byId('nte-table-style-list');
    if (!datalist) { datalist = node('datalist'); datalist.id = 'nte-table-style-list'; STYLE_NAMES.forEach((value) => datalist.appendChild(option(value, value))); document.body.appendChild(datalist); }
    const styleGrid = node('div', null, 'nte-grid two');
    styleGrid.append(labeled('样式名称', styleName), checkbox('第一列强调', styleInfo.showFirstColumn, (value) => { styleInfo.showFirstColumn = value; }),
      checkbox('最后一列强调', styleInfo.showLastColumn, (value) => { styleInfo.showLastColumn = value; }),
      checkbox('镶边行', styleInfo.showRowStripes, (value) => { styleInfo.showRowStripes = value; }),
      checkbox('镶边列', styleInfo.showColumnStripes, (value) => { styleInfo.showColumnStripes = value; }));
    style.appendChild(styleGrid); body.appendChild(style);

    const behavior = section('Excel 表行为');
    const behaviorGrid = node('div', null, 'nte-grid two');
    behaviorGrid.append(
      checkbox('显示筛选按钮', !!draft.autoFilter, (value) => {
        draft.autoFilter = value ? normalizeAutoFilter({ reference: draft.reference, filterColumns: [], sortState: null }) : null; renderDialog();
      }),
      checkbox('插入行', draft.insertRow, (value) => { draft.insertRow = value; }),
      checkbox('插入时下移单元格', draft.insertRowShift, (value) => { draft.insertRowShift = value; }),
      checkbox('发布表', draft.published, (value) => { draft.published = value; }),
      checkbox('重写结构化引用', state.rewriteStructuredReferences, (value) => { state.rewriteStructuredReferences = value; }, '表/列改名时安全更新公式文本'),
      checkbox('允许删除被公式引用的列', state.allowDeleteReferencedColumns, (value) => { state.allowDeleteReferencedColumns = value; }, '默认拒绝危险删除'),
      checkbox('允许删除被公式引用的表', state.allowDeleteReferencedTable, (value) => { state.allowDeleteReferencedTable = value; }, '切片器依赖仍会被服务端拒绝'),
    );
    behavior.appendChild(behaviorGrid);
    if (draft.hasExtensions) behavior.appendChild(node('p', '此表包含 extLst；不修改的扩展节点会逐字节保留。', 'nte-warning'));
    body.appendChild(behavior);
  }

  function uniqueColumnName(candidate, ignoreIndex) {
    const value = String(candidate || '').trim();
    if (!value) return false;
    return !state.draft.columns.some((column, index) => index !== ignoreIndex && column.name.toLocaleLowerCase() === value.toLocaleLowerCase());
  }
  function renderColumnsTab(body) {
    const draft = state.draft, area = section('表列、计算列与汇总行', '计算列和汇总公式以 Excel 原生 tableColumn 子节点保存。删除/新增列会同步表右边界。');
    const list = node('div', null, 'nte-column-list');
    draft.columns.forEach((column, index) => {
      const card = node('article', null, 'nte-column-card'); card.dataset.columnId = String(column.id);
      const header = node('header'); header.append(node('strong', `第 ${index + 1} 列 · ID ${column.id}`));
      const remove = button('删除', () => {
        if ((column.hasExtensions || column.hasXmlColumnProperties) && !confirm('此列含有原生扩展或 XML 映射。仍要删除该列吗？')) return;
        draft.columns.splice(index, 1); if (!draft.columns.length) { draft.columns.push(column); dialogStatus('Excel 表至少保留一列。', true); return; }
        resizeRangeForColumnCount(); renderDialog();
      }, 'danger');
      header.appendChild(remove);
      const grid = node('div', null, 'nte-grid two');
      const name = textInput(column.name, (value, input) => {
        const valid = uniqueColumnName(value, index); input.classList.toggle('invalid', !valid);
        if (valid) column.name = value.trim();
      });
      grid.append(
        labeled('列名', name),
        labeled('汇总函数', selectInput(column.totalsRowFunction || '', TOTAL_FUNCTIONS, (value) => { column.totalsRowFunction = value || null; })),
        labeled('汇总标签', textInput(column.totalsRowLabel || '', (value) => { column.totalsRowLabel = value || null; })),
        labeled('计算列公式', textInput(column.calculatedColumnFormula || '', (value) => { column.calculatedColumnFormula = value || null; }, { placeholder: '=[@数量]*[@单价]' })),
        labeled('自定义汇总公式', textInput(column.totalsRowFormula || '', (value) => { column.totalsRowFormula = value || null; }, { placeholder: '=SUBTOTAL(109,[金额])' })),
      );
      card.append(header, grid);
      if (column.hasExtensions || column.hasXmlColumnProperties) card.appendChild(node('p', '含未知扩展 / XML 列映射；未编辑的子节点保持原样。', 'nte-warning'));
      list.appendChild(card);
    });
    area.appendChild(list);
    area.appendChild(button('添加表列', () => {
      const nextId = Math.max(0, ...draft.columns.map((column) => Number(column.id) || 0)) + 1;
      let suffix = draft.columns.length + 1, name = `Column${suffix}`;
      while (!uniqueColumnName(name, -1)) name = `Column${++suffix}`;
      draft.columns.push({ id: nextId, name, totalsRowLabel: null, totalsRowFunction: null, calculatedColumnFormula: null, totalsRowFormula: null, _new: true });
      resizeRangeForColumnCount(); renderDialog();
    }, 'primary'));
    body.appendChild(area);
  }

  function availableFilterWidth() {
    const parsed = parseRange(state.draft.autoFilter?.reference || state.draft.reference || selectionRange());
    return parsed ? parsed.c1 - parsed.c0 + 1 : (state.draft.columns?.length || 1);
  }
  function filterColumnLabel(columnId) {
    if (state.kind !== 'sheet' && state.draft.columns?.[columnId]) return state.draft.columns[columnId].name;
    const range = parseRange(state.draft.autoFilter?.reference || selectionRange());
    return range ? columnLetters(range.c0 + Number(columnId)) : `列 ${Number(columnId) + 1}`;
  }
  function parseJsonArray(value, fallback, context) {
    try {
      const parsed = JSON.parse(value || '[]');
      if (!Array.isArray(parsed)) throw new Error('必须是数组');
      dialogStatus(''); return parsed;
    } catch (error) { dialogStatus(`${context} JSON：${error.message}`, true); return fallback; }
  }
  function renderDefinitionEditor(host, filterColumn) {
    const definition = filterColumn.definition, config = definition.config;
    if (definition.kind === 'filters') {
      host.append(
        labeled('值（每行一个）', textInput((config.values || []).join('\n'), (value) => { config.values = value === '' ? [] : value.split(/\r?\n/); }, { multiline: true })),
        checkbox('包含空白', config.blank, (value) => { config.blank = value; }),
        labeled('日历类型', textInput(config.calendarType || '', (value) => { config.calendarType = value || null; }, { placeholder: 'gregorian' })),
        labeled('日期组 JSON', textInput(JSON.stringify(config.dateGroups || []), (value) => { config.dateGroups = parseJsonArray(value, config.dateGroups || [], '日期组'); }, { multiline: true, commit: true })),
      );
    } else if (definition.kind === 'customFilters') {
      const conditions = config.conditions || (config.conditions = []);
      host.appendChild(checkbox('两个条件使用 AND（否则 OR）', config.and, (value) => { config.and = value; }));
      conditions.slice(0, 2).forEach((condition, index) => {
        const row = node('div', null, 'nte-condition-row');
        row.append(
          selectInput(condition.operator || 'equal', CUSTOM_OPERATORS, (value) => { condition.operator = value; }),
          textInput(condition.val ?? '', (value) => { condition.val = value; }),
          button('移除', () => { conditions.splice(index, 1); renderDialog(); }, 'danger'),
        ); host.appendChild(row);
      });
      if (conditions.length < 2) host.appendChild(button('添加第二条件', () => { conditions.push({ operator: 'equal', val: '' }); renderDialog(); }));
    } else if (definition.kind === 'dynamicFilter') {
      host.append(
        labeled('动态类型', selectInput(config.type || 'thisMonth', DYNAMIC_TYPES, (value) => { config.type = value; })),
        labeled('阈值 val', textInput(config.val ?? '', (value) => { config.val = value || null; })),
        labeled('最大值 maxVal', textInput(config.maxVal ?? '', (value) => { config.maxVal = value || null; })),
      );
    } else if (definition.kind === 'top10') {
      host.append(
        checkbox('前几项（取消为后几项）', config.top !== false, (value) => { config.top = value; }),
        checkbox('按百分比', !!config.percent, (value) => { config.percent = value; }),
        labeled('数量 / 百分比', textInput(config.val ?? 10, (value) => { config.val = Math.max(0, numberValue(value, 10)); }, { type: 'number', min: 0 })),
        labeled('缓存阈值', textInput(config.filterVal ?? '', (value) => { config.filterVal = value || null; })),
      );
    } else if (definition.kind === 'colorFilter') {
      host.append(
        labeled('DXF 样式 ID', textInput(config.dxfId ?? 0, (value) => { config.dxfId = Math.max(0, numberValue(value, 0)); }, { type: 'number', min: 0 })),
        checkbox('单元格填充色（取消为字体色）', config.cellColor !== false, (value) => { config.cellColor = value; }),
      );
    } else if (definition.kind === 'iconFilter') {
      host.append(
        labeled('图标集', textInput(config.iconSet || '3Arrows', (value) => { config.iconSet = value; })),
        labeled('图标 ID', textInput(config.iconId ?? 0, (value) => { config.iconId = Math.max(0, numberValue(value, 0)); }, { type: 'number', min: 0 })),
      );
    }
    if (definition.hasExtensions) host.appendChild(node('p', '该筛选定义含 extLst；保持同一类型编辑时扩展仍会保留。切换类型会替换该定义。', 'nte-warning'));
  }
  function renderFilterTab(body) {
    const draft = state.draft;
    const settings = section('自动筛选范围', state.kind === 'sheet' ? '普通区域筛选直接写入 worksheet/autoFilter。' : '表筛选范围随表范围和汇总行同步。');
    const enabled = !!draft.autoFilter;
    settings.appendChild(checkbox('启用自动筛选', enabled, (value) => {
      draft.autoFilter = value ? normalizeAutoFilter({ reference: draft.reference || selectionRange(), filterColumns: [], sortState: null }) : null;
      renderDialog();
    }));
    if (enabled) {
      const filterRange = textInput(draft.autoFilter.reference, (value) => { draft.autoFilter.reference = value.trim(); }, { commit: true });
      filterRange.dataset.field = 'filter-range';
      const tools = node('div', null, 'nte-toolbar');
      tools.append(labeled('筛选范围', filterRange), button('使用当前选区', () => { draft.autoFilter.reference = selectionRange(); renderDialog(); }));
      settings.appendChild(tools);
      if (draft.autoFilter.hasExtensions) settings.appendChild(node('p', '此 autoFilter 含扩展；未修改的筛选列和扩展节点保持原样。', 'nte-warning'));
    }
    body.appendChild(settings);
    if (!enabled) return;

    const rules = section('筛选列', '支持 Excel 六类原生筛选：值/日期组、自定义、动态、前十、颜色和图标。');
    const width = availableFilterWidth();
    const list = node('div', null, 'nte-filter-list');
    draft.autoFilter.filterColumns.forEach((filterColumn, index) => {
      const card = node('article', null, 'nte-filter-card'); card.dataset.filterColumn = String(filterColumn.columnId);
      const heading = node('header');
      heading.append(node('strong', filterColumnLabel(filterColumn.columnId)), button('删除筛选', () => {
        draft.autoFilter.filterColumns.splice(index, 1); renderDialog();
      }, 'danger'));
      const row = node('div', null, 'nte-grid three');
      const columns = Array.from({ length: width }, (_, columnId) => [String(columnId), `${filterColumnLabel(columnId)} (${columnId})`]);
      row.append(
        labeled('列', selectInput(filterColumn.columnId, columns, (value) => { filterColumn.columnId = Number(value); })),
        labeled('筛选类型', selectInput(filterColumn.definition.kind, FILTER_KINDS, (value) => {
          if (filterColumn.definition.hasExtensions && !confirm('切换筛选类型会替换当前定义（filterColumn 自身扩展仍保留）。继续吗？')) return renderDialog();
          filterColumn.definition = { kind: value, config: defaultFilterDefinition(value), hasExtensions: false }; renderDialog();
        })),
        checkbox('显示下拉按钮', filterColumn.showButton !== false, (value) => { filterColumn.showButton = value; }),
        checkbox('隐藏按钮', !!filterColumn.hiddenButton, (value) => { filterColumn.hiddenButton = value; }),
      );
      const editor = node('div', null, 'nte-definition'); renderDefinitionEditor(editor, filterColumn);
      card.append(heading, row, editor);
      if (filterColumn.hasExtensions) card.appendChild(node('p', 'filterColumn 含 extLst；差量编辑不会移除它。', 'nte-warning'));
      list.appendChild(card);
    });
    rules.appendChild(list);
    rules.appendChild(button('添加筛选列', () => {
      const used = new Set(draft.autoFilter.filterColumns.map((entry) => Number(entry.columnId)));
      const columnId = Array.from({ length: width }, (_, index) => index).find((index) => !used.has(index));
      if (columnId == null) { dialogStatus('筛选范围内每一列都已有筛选定义。', true); return; }
      draft.autoFilter.filterColumns.push({
        sourceIndex: -1, columnId, hiddenButton: false, showButton: true,
        definition: { kind: 'filters', config: defaultFilterDefinition('filters'), hasExtensions: false },
        hasExtensions: false, _new: true,
      }); renderDialog();
    }, 'primary'));
    body.appendChild(rules);
  }

  function defaultSort(reference, nested = false) {
    const table = state.draft;
    const source = reference || table.autoFilter?.reference || table.reference || selectionRange();
    return {
      reference: dataRange(source, state.kind === 'sheet' ? 1 : table.headerRowCount, state.kind === 'sheet' ? 0 : table.totalsRowCount),
      caseSensitive: false, columnSort: false, sortMethod: '', hasExtensions: false,
      conditions: [], _nested: nested,
    };
  }
  function renderSortEditor(host, title, owner, key, reference, nested = false) {
    const area = section(title, nested ? '该 sortState 位于 autoFilter 内。' : '独立 worksheet/table sortState；可保留 Excel 的额外排序定义。');
    const current = owner[key];
    area.appendChild(checkbox('启用此排序定义', !!current, (value) => { owner[key] = value ? defaultSort(reference, nested) : null; renderDialog(); }));
    if (!current) { host.appendChild(area); return; }
    const grid = node('div', null, 'nte-grid three');
    grid.append(
      labeled('排序范围', textInput(current.reference || '', (value) => { current.reference = value.trim(); })),
      labeled('中文排序方式', selectInput(current.sortMethod || '', [['', '默认'], ['stroke', '笔画'], ['pinYin', '拼音']], (value) => { current.sortMethod = value; })),
      checkbox('区分大小写', current.caseSensitive, (value) => { current.caseSensitive = value; }),
      checkbox('按列方向排序', current.columnSort, (value) => { current.columnSort = value; }),
    );
    area.appendChild(grid);
    const list = node('div', null, 'nte-sort-list');
    current.conditions.forEach((condition, index) => {
      const row = node('div', null, 'nte-sort-row'); row.dataset.sortIndex = String(index);
      row.append(
        node('span', `级别 ${index + 1}`, 'nte-sort-level'),
        labeled('区域', textInput(condition.reference, (value) => { condition.reference = value.trim(); })),
        labeled('依据', selectInput(condition.sortBy || '', SORT_BY, (value) => { condition.sortBy = value; renderDialog(); })),
        labeled('次序', selectInput(condition.descending ? 'desc' : 'asc', [['asc', '升序'], ['desc', '降序']], (value) => { condition.descending = value === 'desc'; })),
      );
      if (condition.sortBy === 'cellColor' || condition.sortBy === 'fontColor') row.appendChild(labeled('DXF ID', textInput(condition.dxfId ?? 0, (value) => { condition.dxfId = Math.max(0, numberValue(value, 0)); }, { type: 'number', min: 0 })));
      if (condition.sortBy === 'icon') {
        row.append(labeled('图标集', textInput(condition.iconSet || '3Arrows', (value) => { condition.iconSet = value; })),
          labeled('图标 ID', textInput(condition.iconId ?? 0, (value) => { condition.iconId = Math.max(0, numberValue(value, 0)); }, { type: 'number', min: 0 })));
      }
      row.append(labeled('自定义序列', textInput(condition.customList || '', (value) => { condition.customList = value || null; }, { placeholder: '高,中,低' })));
      const actions = node('div', null, 'nte-row-actions');
      actions.append(
        button('↑', () => { if (index > 0) { [current.conditions[index - 1], current.conditions[index]] = [current.conditions[index], current.conditions[index - 1]]; renderDialog(); } }),
        button('↓', () => { if (index + 1 < current.conditions.length) { [current.conditions[index + 1], current.conditions[index]] = [current.conditions[index], current.conditions[index + 1]]; renderDialog(); } }),
        button('删除', () => { current.conditions.splice(index, 1); renderDialog(); }, 'danger'),
      );
      row.appendChild(actions); list.appendChild(row);
    });
    area.appendChild(list);
    area.appendChild(button('添加排序级别', () => {
      const parsed = parseRange(reference || current.reference || selectionRange());
      const offset = parsed ? Math.min(current.conditions.length, parsed.c1 - parsed.c0) : 0;
      const conditionReference = dataRange(reference || current.reference || selectionRange(),
        state.kind === 'sheet' ? 1 : state.draft.headerRowCount,
        state.kind === 'sheet' ? 0 : state.draft.totalsRowCount, offset);
      current.conditions.push({ sourceIndex: -1, reference: conditionReference, descending: false, sortBy: '', dxfId: null, iconSet: null, iconId: null, customList: null, _new: true });
      renderDialog();
    }, 'primary'));
    if (current.hasExtensions) area.appendChild(node('p', '此 sortState 含 extLst；修改排序级别时顶层扩展仍保留。', 'nte-warning'));
    host.appendChild(area);
  }
  function renderSortTab(body) {
    const draft = state.draft;
    if (draft.autoFilter) renderSortEditor(body, '自动筛选内的多条件排序', draft.autoFilter, 'sortState', draft.autoFilter.reference, true);
    else body.appendChild(node('p', '未启用自动筛选；可在“自动筛选”页启用后添加筛选内排序。', 'nte-hint'));
    renderSortEditor(body, '独立原生排序状态', draft, 'sortState', draft.reference || selectionRange(), false);
  }
  function renderAdvancedTab(body) {
    const area = section('完整 package 差量', '这是即将发送到 /api/tables 的请求 patch。可用于精确补充后端支持的未来 OOXML 字段。');
    const textarea = node('textarea', null, 'nte-json'); textarea.dataset.field = 'package-patch';
    textarea.value = JSON.stringify(state.manualPatch || buildPackagePatch(), null, 2);
    const toolbar = node('div', null, 'nte-toolbar');
    toolbar.append(
      button('使用此 JSON', () => {
        try { state.manualPatch = JSON.parse(textarea.value); dialogStatus('高级差量已载入；点击“应用到工作簿”提交。'); }
        catch (error) { dialogStatus(`JSON 错误：${error.message}`, true); }
      }),
      button('回到表单差量', () => { state.manualPatch = null; renderDialog(); dialogStatus('已回到表单生成的最小差量。'); }),
    );
    area.append(textarea, toolbar); body.appendChild(area);
  }

  function validateDraft() {
    if (state.kind === 'create' || state.kind === 'table') {
      const draft = state.draft, parsed = parseRange(draft.reference);
      if (!/^(?:[_\\]|\p{L})(?:[_\.]|\p{L}|\p{N})*$/u.test(draft.displayName || '')) throw new Error('表名称必须是有效的 Excel 名称，且不能含空格');
      if (!parsed) throw new Error('表范围不是有效 A1 区域');
      const width = parsed.c1 - parsed.c0 + 1;
      if (width !== draft.columns.length) throw new Error(`表范围宽度 ${width} 与列数 ${draft.columns.length} 不一致`);
      const names = new Set();
      for (const column of draft.columns) {
        const name = String(column.name || '').trim().toLocaleLowerCase();
        if (!name) throw new Error('表列名不能为空');
        if (names.has(name)) throw new Error(`表列名重复：${column.name}`);
        names.add(name);
      }
    }
    for (const filter of [state.draft.autoFilter]) if (filter) {
      if (!parseRange(filter.reference)) throw new Error('自动筛选范围不是有效 A1 区域');
      const ids = new Set();
      for (const column of filter.filterColumns) {
        if (ids.has(Number(column.columnId))) throw new Error(`筛选列 colId ${column.columnId} 重复`);
        ids.add(Number(column.columnId));
      }
    }
  }
  async function saveDraft() {
    if (!state.draft || state.loading) return;
    try {
      validateDraft();
      const patch = buildPackagePatch();
      if (!Object.keys(patch).some((key) => ['tableEdits', 'worksheetEdits', 'createTables', 'deleteTables'].includes(key))) {
        dialogStatus('没有需要写入的更改。'); return;
      }
      setBusy(true); dialogStatus('正在原子校验并写入原生 OOXML…');
      const previousPart = state.draft.part, previousName = state.draft.displayName;
      const runtime = buildRuntimeOperations(state.base, state.draft);
      state.model = await apiPost('/api/tables', { op: 'update', patch, runtime });
      let key = previousPart ? `table:${previousPart}` : null;
      if (state.kind === 'create') {
        const created = (state.model.tables || []).find((table) => table.displayName === previousName);
        key = created ? tableKey(created) : null;
      }
      selectKey(key || state.key); renderDialog();
      dialogStatus('已写入 Excel 原生表 / 自动筛选 / 排序差量。');
      setStatusMessage('已更新 Excel 表、筛选和排序');
      if (typeof scheduleRefresh === 'function') scheduleRefresh(true);
    } catch (error) { dialogStatus(error.message || String(error), true); }
    finally { setBusy(false); }
  }
  async function deleteSelectedTable() {
    if (state.kind !== 'table' || !state.draft?.part) return;
    if (!confirm(`删除 Excel 表“${state.draft.displayName}”？工作表单元格数据会保留。`)) return;
    try {
      setBusy(true); dialogStatus('正在检查结构化引用、切片器和关系…');
      const deletion = { part: state.draft.part };
      if (state.allowDeleteReferencedTable) deletion.allowDeleteReferencedTable = true;
      const range = parseRange(state.draft.autoFilter?.reference || state.draft.reference);
      const runtime = range ? [{ type: 'filter', request: {
        sheet: Number(S?.sheet || 0), r0: range.r0, c0: range.c0, r1: range.r1, c1: range.c1,
        headerRows: Number(state.draft.headerRowCount || 0), clear: true,
      } }] : [];
      state.model = await apiPost('/api/tables', { op: 'update', patch: { deleteTables: [deletion] }, runtime });
      const sheet = (state.model.worksheets || []).find((item) => item.sheet === state.draft.sheet) || state.model.worksheets?.[0];
      selectKey(sheet ? sheetKey(sheet) : null); renderDialog(); dialogStatus('表已删除；单元格数据仍在工作表中。');
    } catch (error) { dialogStatus(error.message || String(error), true); }
    finally { setBusy(false); }
  }
  async function resetJournal() {
    if (!confirm('恢复所有表、普通筛选和排序到本次导入时状态？')) return;
    try {
      setBusy(true); state.model = await apiPost('/api/tables', { op: 'reset' });
      const preferred = state.key; selectKey(preferred); renderDialog(); dialogStatus('已恢复导入状态。');
    } catch (error) { dialogStatus(error.message || String(error), true); }
    finally { setBusy(false); }
  }
  async function openNativeTableEditor() {
    const dialog = ensureDialog(); dialog.hidden = false; dialogStatus('正在读取原生表、筛选和排序…');
    try {
      setBusy(true); state.model = await apiPost('/api/tables', { op: 'list' });
      const sheet = currentWorksheet();
      const current = { row: Number(S?.cur?.r || 1), col: Number(S?.cur?.c || 1) };
      const hit = (state.model.tables || []).find((table) => {
        if (table.sheet !== currentSheetName()) return false;
        const range = parseRange(table.reference);
        return range && current.row >= range.r0 && current.row <= range.r1 && current.col >= range.c0 && current.col <= range.c1;
      });
      selectKey(hit ? tableKey(hit) : sheet ? sheetKey(sheet) : tableKey(state.model.tables?.[0] || {}));
      renderDialog();
      dialogStatus(`${state.model.tables?.length || 0} 个 Excel 表，${state.model.worksheets?.length || 0} 个工作表。`);
    } catch (error) { dialogStatus(error.message || String(error), true); }
    finally { setBusy(false); }
  }

  window.openNativeTableEditor = openNativeTableEditor;
  window.__nativeTableEditorTest = Object.freeze({
    FILTER_KINDS, ensureDialog, parseRange, formatRange, normalizeTable, normalizeWorksheet,
    normalizeAutoFilter, normalizeSort, buildSortPatch, buildAutoFilterPatch, buildRuntimeOperations,
    buildTablePartPatch, buildCreatePayload,
    buildPackagePatchFor(kind, base, draft) {
      const before = { kind: state.kind, base: state.base, draft: state.draft, manualPatch: state.manualPatch };
      state.kind = kind; state.base = clone(base); state.draft = clone(draft); state.manualPatch = null;
      try { return buildPackagePatch(); }
      finally { Object.assign(state, before); }
    },
    buildRuntimeOperationsFor(kind, base, draft) {
      const before = { kind: state.kind, base: state.base, draft: state.draft };
      state.kind = kind; state.base = clone(base); state.draft = clone(draft);
      try { return buildRuntimeOperations(state.base, state.draft); }
      finally { Object.assign(state, before); }
    },
    request(op, patch, runtime = []) { return { url: '/api/tables', body: op === 'list' ? { op: 'list' } : { op: 'update', patch, runtime } }; },
  });
  const ribbonButton = byId('btn-tables');
  if (ribbonButton) ribbonButton.addEventListener('click', openNativeTableEditor);
})();
