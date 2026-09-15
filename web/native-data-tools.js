/* Native Excel PivotTable / Slicer / Timeline deep editors.  These controls edit typed OOXML
 * patches; the Rust backend owns validation, relationship cascades and byte-exact preservation. */
(() => {
  'use strict';
  const byId = (id) => document.getElementById(id);
  const clone = (value) => typeof structuredClone === 'function'
    ? structuredClone(value) : JSON.parse(JSON.stringify(value));
  const text = (tag, value, className) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    node.textContent = value == null ? '' : String(value);
    return node;
  };
  const option = (value, label, selected = false) => {
    const node = document.createElement('option');
    node.value = String(value); node.textContent = label; node.selected = selected;
    return node;
  };
  const bool = (value, fallback = false) => value == null ? fallback : !!value;
  const num = (value, fallback = 0) => Number.isFinite(Number(value)) ? Number(value) : fallback;
  const status = (message) => { if (typeof setStatus === 'function') setStatus(message); };

  function shell(id, title, note, tabs) {
    let dlg = byId(id);
    if (dlg) return dlg;
    dlg = document.createElement('div');
    dlg.id = id; dlg.className = 'native-data-dialog'; dlg.hidden = true;
    dlg.innerHTML = `
      <div class="native-data-title"><span class="native-data-title-text"></span><span class="native-data-subtitle"></span><button type="button" aria-label="关闭">×</button></div>
      <div class="native-data-note"></div>
      <div class="native-data-main">
        <aside class="native-data-sidebar"><div class="native-data-list"></div><div class="native-data-side-actions"></div></aside>
        <section class="native-data-editor"><div class="native-data-tabs"></div><div class="native-data-body"></div></section>
      </div>
      <footer class="native-data-footer"><span class="native-data-status"></span><button type="button" data-action="reset">恢复原始</button><button type="button" class="primary" data-action="save">应用到工作簿</button></footer>`;
    dlg.querySelector('.native-data-title-text').textContent = title;
    dlg.querySelector('.native-data-note').textContent = note;
    const tabBar = dlg.querySelector('.native-data-tabs');
    for (const [key, label] of tabs) {
      const button = text('button', label); button.type = 'button'; button.dataset.tab = key;
      tabBar.appendChild(button);
    }
    dlg.querySelector('.native-data-title button').onclick = () => {
      dlg.hidden = true;
      byId('grid-scroll')?.focus();
    };
    dlg.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') dlg.querySelector('.native-data-title button').click();
    });
    document.body.appendChild(dlg);
    return dlg;
  }

  const PIVOT_DISPLAY_BOOLS = [
    ['compact', '压缩布局'], ['outline', '大纲布局'], ['outlineData', '大纲显示数据'],
    ['rowGrandTotals', '行总计'], ['colGrandTotals', '列总计'], ['showHeaders', '字段标题'],
    ['showDrill', '展开/折叠按钮'], ['showEmptyRow', '显示空行项目'], ['showEmptyCol', '显示空列项目'],
    ['mergeItem', '合并并居中标签'], ['preserveFormatting', '更新时保留格式'],
    ['fieldPrintTitles', '设置打印标题'], ['itemPrintTitles', '每页打印项目标签'],
    ['showValuesRow', '显示值行'], ['enableDrill', '启用明细显示'], ['multipleFieldFilters', '每字段允许多个筛选器'],
  ];
  const PIVOT_FIELD_FLAGS = [
    ['showAll', '显示无数据项目'], ['insertBlankRow', '每个项目后插入空行'],
    ['insertPageBreak', '每个项目后分页'], ['multipleItemSelectionAllowed', '允许多项筛选'],
    ['includeNewItemsInFilter', '筛选包含新项目'], ['hideNewItems', '隐藏新项目'],
    ['dragToRow', '允许拖到行'], ['dragToCol', '允许拖到列'],
    ['dragToPage', '允许拖到筛选器'], ['dragToData', '允许拖到值'],
  ];
  const PIVOT_SUBTOTALS = [
    ['defaultSubtotal', '自动'], ['sumSubtotal', '求和'], ['countASubtotal', '计数'],
    ['avgSubtotal', '平均值'], ['maxSubtotal', '最大值'], ['minSubtotal', '最小值'],
    ['productSubtotal', '乘积'], ['countSubtotal', '数值计数'], ['stdDevSubtotal', '标准偏差'],
    ['stdDevPSubtotal', '总体标准偏差'], ['varSubtotal', '方差'], ['varPSubtotal', '总体方差'],
  ];
  const PIVOT_AGGREGATES = [
    ['sum','求和'], ['count','计数'], ['countNums','数值计数'], ['average','平均值'],
    ['max','最大值'], ['min','最小值'], ['product','乘积'], ['stdDev','标准偏差'],
    ['stdDevp','总体标准偏差'], ['var','方差'], ['varp','总体方差'],
  ];
  const PIVOT_SHOW_AS = [
    ['normal','无计算'], ['difference','差异'], ['percent','百分比'], ['percentDiff','百分比差异'],
    ['runTotal','按某字段汇总'], ['percentOfRow','行汇总的百分比'], ['percentOfCol','列汇总的百分比'],
    ['percentOfTotal','总计的百分比'], ['index','指数'],
  ];
  const PIVOT_FILTER_TYPES = [
    'captionEqual','captionNotEqual','captionBeginsWith','captionEndsWith','captionContains','captionNotContains',
    'captionGreaterThan','captionGreaterThanOrEqual','captionLessThan','captionLessThanOrEqual','captionBetween','captionNotBetween',
    'valueEqual','valueNotEqual','valueGreaterThan','valueGreaterThanOrEqual','valueLessThan','valueLessThanOrEqual','valueBetween','valueNotBetween',
    'dateEqual','dateNotEqual','dateOlderThan','dateOlderThanOrEqual','dateNewerThan','dateNewerThanOrEqual','dateBetween','dateNotBetween',
    'today','yesterday','tomorrow','thisWeek','lastWeek','nextWeek','thisMonth','lastMonth','nextMonth',
    'thisQuarter','lastQuarter','nextQuarter','thisYear','lastYear','nextYear','yearToDate',
    'Q1','Q2','Q3','Q4','M1','M2','M3','M4','M5','M6','M7','M8','M9','M10','M11','M12',
  ];
  const pivotState = {
    model: null, caches: null, info: null, part: null, draft: null, tab: 'layout', field: 0,
    refreshConfig: null, refreshResult: null,
  };

  function pivotTable() {
    return (pivotState.model?.tables || []).find((table) => table.part === pivotState.part) || null;
  }
  function pivotCache(table = pivotTable()) {
    return (pivotState.caches?.caches || []).find((cache) => cache.part === table?.cachePart
      || (table?.cacheId != null && Number(cache.cacheId) === Number(table.cacheId))) || null;
  }
  function pivotFieldName(index) {
    if (Number(index) === -2) return '∑ 值';
    const field = pivotState.draft?.fields?.[Number(index)];
    return field?.attributes?.name || field?.name || `字段 ${Number(index) + 1}`;
  }
  function normalizePivotDraft(table) {
    const draft = clone(table);
    draft.display ||= {}; draft.location ||= {}; draft.style ||= {};
    draft.axes ||= {}; draft.axes.rows ||= []; draft.axes.columns ||= [];
    draft.axes.pages ||= []; draft.axes.data ||= []; draft.filters ||= [];
    draft.fields ||= [];
    for (const field of draft.fields) {
      field.attributes ||= {}; field.subtotals ||= {}; field.sort ||= {}; field.items ||= [];
      field._hiddenItems = field.items.filter((item) => bool(item.attributes?.h)).map((item) => item.sourceIndex);
    }
    return draft;
  }
  function pivotAxisOf(index) {
    const axes = pivotState.draft.axes;
    if (axes.rows.some((value) => Number(value) === index)) return 'rows';
    if (axes.columns.some((value) => Number(value) === index)) return 'columns';
    if (axes.pages.some((value) => Number(value.fld) === index)) return 'pages';
    if (axes.data.some((value) => Number(value.fld) === index)) return 'data';
    return 'none';
  }
  function removePivotFieldFromAxes(index) {
    const axes = pivotState.draft.axes;
    axes.rows = axes.rows.filter((value) => Number(value) !== index);
    axes.columns = axes.columns.filter((value) => Number(value) !== index);
    axes.pages = axes.pages.filter((value) => Number(value.fld) !== index);
    axes.data = axes.data.filter((value) => Number(value.fld) !== index);
    if (axes.data.length <= 1) {
      axes.rows = axes.rows.filter((value) => Number(value) !== -2);
      axes.columns = axes.columns.filter((value) => Number(value) !== -2);
    }
  }
  function setPivotFieldAxis(index, axis) {
    const axes = pivotState.draft.axes;
    removePivotFieldFromAxes(index);
    if (axis === 'rows') axes.rows.push(index);
    else if (axis === 'columns') axes.columns.push(index);
    else if (axis === 'pages') axes.pages.push({ fld: index, hier: -1 });
    else if (axis === 'data') {
      axes.data.push({ fld: index, name: `求和项: ${pivotFieldName(index)}`, subtotal: 'sum', showDataAs: 'normal', baseField: 0, baseItem: 0 });
      if (axes.data.length > 1 && !axes.rows.includes(-2) && !axes.columns.includes(-2)) axes.columns.push(-2);
    }
    renderPivotTab();
  }
  function pivotPatch() {
    const draft = pivotState.draft;
    return {
      display: clone(draft.display), location: clone(draft.location), style: clone(draft.style),
      fields: draft.fields.map((field) => ({
        index: field.index,
        attributes: clone(field.attributes || {}), subtotals: clone(field.subtotals || {}), sort: clone(field.sort || {}),
        hiddenItems: clone(field._hiddenItems || []),
        items: (field.items || []).map((item) => ({ sourceIndex: item.sourceIndex, attributes: clone(item.attributes || {}) })),
      })),
      axes: clone(draft.axes), filters: clone(draft.filters),
    };
  }
  function pivotJsonDraft() {
    return pivotPatch();
  }

  function renderPivotList() {
    const dlg = byId('native-pivot-dialog');
    const list = dlg.querySelector('.native-data-list'); list.replaceChildren();
    const tables = pivotState.model?.tables || [];
    if (!tables.length) { list.appendChild(text('div', '当前工作簿没有原生数据透视表。', 'native-empty')); return; }
    for (const table of tables) {
      const button = document.createElement('button'); button.type = 'button';
      button.classList.toggle('active', table.part === pivotState.part);
      button.append(text('strong', table.name || table.part), text('span', table.sheet || '未连接工作表'),
        text('small', `${table.fields?.length || 0} 个字段 · 缓存 ${table.cacheId ?? '—'}${table.edited ? ' · 已修改' : ''}`));
      button.onclick = () => {
        pivotState.part = table.part; pivotState.draft = normalizePivotDraft(table); pivotState.field = 0;
        pivotState.refreshConfig = null; pivotState.refreshResult = null;
        renderPivotDialog();
      };
      list.appendChild(button);
    }
  }
  function renderPivotDialog() {
    const dlg = byId('native-pivot-dialog');
    renderPivotList();
    const table = pivotTable();
    dlg.querySelector('.native-data-subtitle').textContent = table ? `${table.sheet || ''} · ${table.name || table.part}` : '';
    dlg.querySelectorAll('.native-data-tabs button').forEach((button) => {
      button.classList.toggle('active', button.dataset.tab === pivotState.tab);
      button.onclick = () => { pivotState.tab = button.dataset.tab; renderPivotDialog(); };
    });
    dlg.querySelector('[data-action="save"]').disabled = !table;
    dlg.querySelector('[data-action="reset"]').disabled = !table;
    renderPivotTab();
  }
  function renderPivotTab() {
    const dlg = byId('native-pivot-dialog');
    const body = dlg.querySelector('.native-data-body'); body.replaceChildren();
    if (!pivotState.draft) { body.appendChild(text('div', '请选择一个数据透视表。', 'native-empty')); return; }
    if (pivotState.tab === 'layout') renderPivotLayout(body);
    else if (pivotState.tab === 'fields') renderPivotFields(body);
    else if (pivotState.tab === 'values') renderPivotValues(body);
    else if (pivotState.tab === 'filters') renderPivotFilters(body);
    else if (pivotState.tab === 'refresh') renderPivotRefresh(body);
    else renderPivotAdvanced(body);
  }
  function section(title) {
    const node = document.createElement('div'); node.className = 'native-section'; node.appendChild(text('h3', title)); return node;
  }
  function check(label, checked, onChange) {
    const wrapper = document.createElement('label'); wrapper.className = 'native-check';
    const input = document.createElement('input'); input.type = 'checkbox'; input.checked = checked;
    input.onchange = () => onChange(input.checked); wrapper.append(input, text('span', label)); return wrapper;
  }
  function formInput(label, value, onChange, type = 'text') {
    const wrapper = document.createElement('label'); wrapper.appendChild(text('span', label));
    const input = document.createElement('input'); input.type = type; input.value = value ?? '';
    input.oninput = () => onChange(type === 'number' ? num(input.value) : input.value); wrapper.appendChild(input); return wrapper;
  }
  function renderPivotLayout(body) {
    const draft = pivotState.draft;
    const general = section('布局与显示'); const checks = document.createElement('div'); checks.className = 'native-form-grid';
    for (const [key, label] of PIVOT_DISPLAY_BOOLS) checks.appendChild(check(label, bool(draft.display[key]), (value) => { draft.display[key] = value; }));
    general.appendChild(checks); body.appendChild(general);

    const position = section('输出区域与样式'); const form = document.createElement('div'); form.className = 'native-form-grid two';
    form.append(formInput('输出区域', draft.location.ref, (value) => { draft.location.ref = value; }),
      formInput('首标题行', draft.location.firstHeaderRow, (value) => { draft.location.firstHeaderRow = value; }, 'number'),
      formInput('首数据行', draft.location.firstDataRow, (value) => { draft.location.firstDataRow = value; }, 'number'),
      formInput('首数据列', draft.location.firstDataCol, (value) => { draft.location.firstDataCol = value; }, 'number'),
      formInput('透视表样式', draft.style.name, (value) => { draft.style.name = value; }));
    for (const [key, label] of [['showRowHeaders','行标题'],['showColHeaders','列标题'],['showRowStripes','镶边行'],['showColStripes','镶边列'],['showLastColumn','末列强调']]) {
      form.appendChild(check(label, bool(draft.style[key]), (value) => { draft.style[key] = value; }));
    }
    position.appendChild(form); body.appendChild(position);

    const axesSection = section('字段布局（可原生重排/移除）'); const grid = document.createElement('div'); grid.className = 'native-axis-grid';
    for (const [axis, title] of [['rows','行'],['columns','列'],['pages','筛选器'],['data','值']]) {
      const panel = document.createElement('div'); panel.className = 'native-axis'; panel.appendChild(text('h4', title));
      const values = draft.axes[axis] || [];
      values.forEach((entry, index) => {
        const fieldIndex = axis === 'rows' || axis === 'columns' ? Number(entry) : Number(entry.fld);
        const card = document.createElement('div'); card.className = 'native-axis-item'; card.appendChild(text('span', pivotFieldName(fieldIndex)));
        const actions = document.createElement('div');
        for (const [symbol, delta] of [['↑',-1],['↓',1]]) {
          const button = text('button', symbol); button.type = 'button'; button.disabled = index + delta < 0 || index + delta >= values.length;
          button.onclick = () => { const other = index + delta; [values[index], values[other]] = [values[other], values[index]]; renderPivotTab(); };
          actions.appendChild(button);
        }
        const remove = text('button', '×'); remove.type = 'button'; remove.onclick = () => {
          if (fieldIndex === -2) values.splice(index, 1); else removePivotFieldFromAxes(fieldIndex); renderPivotTab();
        };
        actions.appendChild(remove); card.appendChild(actions); panel.appendChild(card);
      });
      if (!values.length) panel.appendChild(text('div', '拖放区为空', 'native-muted'));
      grid.appendChild(panel);
    }
    axesSection.appendChild(grid); body.appendChild(axesSection);
  }
  function renderPivotFields(body) {
    const layout = document.createElement('div'); layout.className = 'native-field-layout';
    const list = document.createElement('div'); list.className = 'native-field-list';
    pivotState.draft.fields.forEach((field, index) => {
      const button = text('button', field.attributes?.name || field.name || `字段 ${index + 1}`);
      button.type = 'button'; button.classList.toggle('active', index === pivotState.field);
      button.onclick = () => { pivotState.field = index; renderPivotTab(); }; list.appendChild(button);
    });
    layout.appendChild(list);
    const editor = document.createElement('div'); editor.className = 'native-field-editor';
    const field = pivotState.draft.fields[pivotState.field];
    if (!field) { editor.appendChild(text('div', '没有字段。', 'native-empty')); layout.appendChild(editor); body.appendChild(layout); return; }
    editor.appendChild(text('h3', field.name || `字段 ${field.index + 1}`));
    const captionRow = document.createElement('div'); captionRow.className = 'native-field-row'; captionRow.appendChild(text('label', '自定义名称'));
    const caption = document.createElement('input'); caption.type = 'text'; caption.value = field.attributes.name || '';
    caption.oninput = () => { field.attributes.name = caption.value || null; }; captionRow.appendChild(caption); editor.appendChild(captionRow);
    const axisRow = document.createElement('div'); axisRow.className = 'native-field-row'; axisRow.appendChild(text('label', '放置区域'));
    const axis = document.createElement('select');
    for (const [value, label] of [['none','不显示'],['rows','行'],['columns','列'],['pages','筛选器'],['data','值']]) axis.appendChild(option(value, label, pivotAxisOf(field.index) === value));
    axis.onchange = () => setPivotFieldAxis(field.index, axis.value); axisRow.appendChild(axis); editor.appendChild(axisRow);
    const sortRow = document.createElement('div'); sortRow.className = 'native-field-row'; sortRow.appendChild(text('label', '排序'));
    const sort = document.createElement('select');
    for (const [value, label] of [['manual','手动'],['ascending','升序'],['descending','降序']]) sort.appendChild(option(value, label, (field.sort.sortType || 'manual') === value));
    sort.onchange = () => { field.sort.sortType = sort.value; }; sortRow.appendChild(sort); editor.appendChild(sortRow);
    const flags = section('字段选项'); const flagGrid = document.createElement('div'); flagGrid.className = 'native-form-grid';
    for (const [key, label] of PIVOT_FIELD_FLAGS) flagGrid.appendChild(check(label, bool(field.attributes[key]), (value) => { field.attributes[key] = value; }));
    flags.appendChild(flagGrid); editor.appendChild(flags);
    const subtotals = section('分类汇总'); const subtotalGrid = document.createElement('div'); subtotalGrid.className = 'native-form-grid';
    for (const [key, label] of PIVOT_SUBTOTALS) subtotalGrid.appendChild(check(label, bool(field.subtotals[key]), (value) => { field.subtotals[key] = value; }));
    subtotals.appendChild(subtotalGrid); editor.appendChild(subtotals);
    const items = section('手动项目筛选'); const itemGrid = document.createElement('div'); itemGrid.className = 'native-item-grid';
    for (const item of field.items || []) {
      const label = item.label ?? item.value ?? item.attributes?.n ?? (item.attributes?.x != null ? `项目 ${item.attributes.x}` : `项目 ${item.sourceIndex + 1}`);
      const selected = !(field._hiddenItems || []).includes(item.sourceIndex);
      itemGrid.appendChild(check(label, selected, (visible) => {
        const hidden = new Set(field._hiddenItems || []);
        if (visible) hidden.delete(item.sourceIndex); else hidden.add(item.sourceIndex);
        field._hiddenItems = [...hidden].sort((a,b) => a - b);
      }));
    }
    if (!field.items?.length) itemGrid.appendChild(text('div', '此字段没有可枚举的缓存项目。', 'native-muted'));
    items.appendChild(itemGrid); editor.appendChild(items);
    layout.appendChild(editor); body.appendChild(layout);
  }
  function renderPivotValues(body) {
    const draft = pivotState.draft; const sectionNode = section('值字段');
    draft.axes.data.forEach((value, index) => {
      const card = document.createElement('div'); card.className = 'native-value-card';
      const name = document.createElement('input'); name.value = value.name || ''; name.title = '自定义名称'; name.oninput = () => { value.name = name.value; };
      const aggregate = document.createElement('select'); aggregate.title = '汇总方式';
      for (const [key,label] of PIVOT_AGGREGATES) aggregate.appendChild(option(key,label,(value.subtotal || 'sum') === key));
      aggregate.onchange = () => { value.subtotal = aggregate.value; };
      const showAs = document.createElement('select'); showAs.title = '值显示方式';
      for (const [key,label] of PIVOT_SHOW_AS) showAs.appendChild(option(key,label,(value.showDataAs || 'normal') === key));
      showAs.onchange = () => { value.showDataAs = showAs.value; };
      const baseField = document.createElement('select'); baseField.title = '基本字段';
      draft.fields.forEach((field) => baseField.appendChild(option(field.index, field.name || `字段 ${field.index + 1}`, Number(value.baseField) === field.index)));
      baseField.onchange = () => { value.baseField = Number(baseField.value); };
      const baseItem = document.createElement('input'); baseItem.type = 'number'; baseItem.title = '基本项'; baseItem.value = value.baseItem ?? 0; baseItem.oninput = () => { value.baseItem = num(baseItem.value); };
      const numFmt = document.createElement('input'); numFmt.type = 'number'; numFmt.title = '数字格式 ID'; numFmt.value = value.numFmtId ?? ''; numFmt.oninput = () => { value.numFmtId = numFmt.value === '' ? null : num(numFmt.value); };
      const actions = document.createElement('div');
      for (const [symbol,delta] of [['↑',-1],['↓',1]]) { const button = text('button',symbol,'native-mini-btn'); button.disabled = index + delta < 0 || index + delta >= draft.axes.data.length; button.onclick = () => { const other=index+delta; [draft.axes.data[index],draft.axes.data[other]]=[draft.axes.data[other],draft.axes.data[index]]; renderPivotTab(); }; actions.appendChild(button); }
      const remove = text('button','×','native-mini-btn'); remove.onclick = () => { removePivotFieldFromAxes(Number(value.fld)); renderPivotTab(); }; actions.appendChild(remove);
      card.append(name,aggregate,showAs,baseField,baseItem,numFmt,actions); sectionNode.appendChild(card);
    });
    if (!draft.axes.data.length) sectionNode.appendChild(text('div','尚未添加值字段。','native-muted'));
    const toolbar = document.createElement('div'); toolbar.className = 'native-toolbar'; const select = document.createElement('select');
    draft.fields.filter((field) => !draft.axes.data.some((value) => Number(value.fld) === field.index)).forEach((field) => select.appendChild(option(field.index, field.name || `字段 ${field.index + 1}`)));
    const add = text('button','添加到值'); add.disabled = !select.options.length; add.onclick = () => setPivotFieldAxis(Number(select.value),'data'); toolbar.append(select,add); sectionNode.appendChild(toolbar); body.appendChild(sectionNode);
  }
  function renderPivotFilters(body) {
    const draft = pivotState.draft; const node = section('原生标签/值/日期筛选器');
    draft.filters.forEach((filter, index) => {
      filter.attributes ||= {}; const attrs = filter.attributes;
      const row = document.createElement('div'); row.className = 'native-filter-row';
      const field = document.createElement('select'); draft.fields.forEach((item) => field.appendChild(option(item.index,item.name || `字段 ${item.index + 1}`,Number(attrs.fld)===item.index))); field.onchange = () => { attrs.fld=Number(field.value); };
      const typeSelect = document.createElement('select'); PIVOT_FILTER_TYPES.forEach((key) => typeSelect.appendChild(option(key,key,(attrs.type || 'captionEqual')===key))); typeSelect.onchange = () => { attrs.type=typeSelect.value; };
      const value1 = document.createElement('input'); value1.placeholder='值 1'; value1.value=attrs.stringValue1 ?? ''; value1.oninput=()=>{ attrs.stringValue1=value1.value || null; };
      const value2 = document.createElement('input'); value2.placeholder='值 2'; value2.value=attrs.stringValue2 ?? ''; value2.oninput=()=>{ attrs.stringValue2=value2.value || null; };
      const actions=document.createElement('div');
      for (const [symbol,delta] of [['↑',-1],['↓',1]]) { const button=text('button',symbol,'native-mini-btn'); button.disabled=index+delta<0||index+delta>=draft.filters.length; button.onclick=()=>{const other=index+delta;[draft.filters[index],draft.filters[other]]=[draft.filters[other],draft.filters[index]];renderPivotTab();};actions.appendChild(button); }
      const remove=text('button','×','native-mini-btn'); remove.onclick=()=>{draft.filters.splice(index,1);renderPivotTab();};actions.appendChild(remove);
      row.append(field,typeSelect,value1,value2,actions); node.appendChild(row);
    });
    const add=text('button','添加筛选器'); add.className='native-mini-btn'; add.style.width='auto'; add.onclick=()=>{ draft.filters.push({attributes:{fld:0,type:'captionEqual'}}); renderPivotTab(); }; node.appendChild(add); body.appendChild(node);
  }
  /* ---------------- Local worksheet Pivot refresh ---------------- */
  function columnNumber(label) {
    let value = 0;
    for (const character of String(label || '').toUpperCase()) {
      if (character < 'A' || character > 'Z') return null;
      value = value * 26 + character.charCodeAt(0) - 64;
    }
    return value || null;
  }
  function columnLabel(value) {
    let number = Number(value); let label = '';
    while (number > 0) { number -= 1; label = String.fromCharCode(65 + number % 26) + label; number = Math.floor(number / 26); }
    return label;
  }
  function parseA1Cell(value) {
    const match = String(value || '').trim().replace(/\$/g, '').match(/^([A-Za-z]+)([1-9][0-9]*)$/);
    if (!match) return null;
    const col = columnNumber(match[1]); const row = Number(match[2]);
    return col && row <= 1048576 && col <= 16384 ? { row, col } : null;
  }
  function unquoteSheet(value) {
    const name = String(value || '').trim();
    return name.startsWith("'") && name.endsWith("'") ? name.slice(1, -1).replace(/''/g, "'") : name;
  }
  function parseA1Range(value) {
    let address = String(value || '').trim(); let sheet = null;
    const bang = address.lastIndexOf('!');
    if (bang >= 0) { sheet = unquoteSheet(address.slice(0, bang)); address = address.slice(bang + 1); }
    const ends = address.split(':');
    if (ends.length > 2) return null;
    const first = parseA1Cell(ends[0]); const last = parseA1Cell(ends[1] || ends[0]);
    if (!first || !last) return null;
    return { sheet, r0:Math.min(first.row,last.row), c0:Math.min(first.col,last.col), r1:Math.max(first.row,last.row), c1:Math.max(first.col,last.col) };
  }
  function sheetIndex(name, fallback = 0, info = pivotState.info) {
    const index = (info?.sheets || []).findIndex((sheet) => sheet === name);
    return index >= 0 ? index : Number.isInteger(Number(fallback)) ? Number(fallback) : 0;
  }
  function aggregateForLocal(value) {
    const key = String(value || 'sum').toLowerCase().replace(/[ _-]/g, '');
    return ({sum:'sum',count:'count',counta:'count',countnums:'count',average:'average',avg:'average',min:'min',max:'max',distinctcount:'distinctCount'})[key] || null;
  }
  function filterForLocal(filter) {
    const attrs=filter?.attributes||filter||{}; const field=Number(attrs.fld);
    if(!Number.isInteger(field)||field<0)return null;
    const op=({captionEqual:'eq',valueEqual:'eq',dateEqual:'eq',captionNotEqual:'ne',valueNotEqual:'ne',dateNotEqual:'ne',captionGreaterThan:'gt',valueGreaterThan:'gt',dateNewerThan:'gt',captionGreaterThanOrEqual:'gte',valueGreaterThanOrEqual:'gte',dateNewerThanOrEqual:'gte',captionLessThan:'lt',valueLessThan:'lt',dateOlderThan:'lt',captionLessThanOrEqual:'lte',valueLessThanOrEqual:'lte',dateOlderThanOrEqual:'lte',captionContains:'contains',captionBeginsWith:'beginsWith',captionEndsWith:'endsWith',captionBetween:'between',valueBetween:'between',dateBetween:'between'})[String(attrs.type||'captionEqual')];
    if(!op)return null;
    const result={field,op};
    if(op==='between'){result.min=attrs.stringValue1??null;result.max=attrs.stringValue2??null;}else result.value=attrs.stringValue1??null;
    return result;
  }
  function localRefreshBoundary(cache) {
    const source=cache?.source||{};const type=String(source.type||'').toLowerCase();
    const dataModel=type==='external'||type==='model'||type==='datamodel'||source.dataModel===true||source.model===true;
    const olap=dataModel||source.olap===true||source.connectionId!=null||type==='olap';
    if(dataModel)return {supported:false,kind:'dataModel',message:'该 PivotCache 来自 Data Model/VertiPaq；本地工作表聚合器不执行 DAX 或关系模型。原生缓存仍会无损保留。'};
    if(olap)return {supported:false,kind:'olap',message:'该 PivotCache 是 OLAP/外部连接；本地刷新不执行 MDX、凭据或远程查询。请使用 Excel/数据连接刷新。'};
    if(cache&&type&&type!=='worksheet')return {supported:false,kind:type,message:`该 PivotCache 源类型为 ${type}；当前本地引擎只刷新普通 worksheet cache。`};
    return {supported:true,kind:'worksheet',message:'普通工作表 PivotCache：可在 UniCell 内预览、聚合并原生回写缓存。'};
  }
  function initialRefreshConfig(table,cache,info=pivotState.info) {
    const source=cache?.source||{};const parsedSource=parseA1Range(source.ref||'A1:A2')||{r0:1,c0:1,r1:2,c1:1};
    const output=parseA1Range(table?.location?.ref||'A1')||{r0:1,c0:1,r1:1,c1:1};
    return {sourceSheet:sheetIndex(parsedSource.sheet||source.sheet,0,info),sourceRef:source.ref||'A1:A2',outputSheet:sheetIndex(output.sheet||table?.sheet,0,info),outputRef:`${columnLabel(output.c0)}${output.r0}`,writeOutput:true};
  }
  function buildLocalRefreshRequest(table,cache,config,draft=table,info=pivotState.info) {
    if(!table)throw new Error('请选择一个数据透视表。');
    const boundary=localRefreshBoundary(cache);if(!boundary.supported)throw new Error(boundary.message);
    const source=parseA1Range(config?.sourceRef);const output=parseA1Range(config?.outputRef);
    if(!source)throw new Error('源区域必须是有效的 A1 区域，例如 A1:E200。');
    if(!output)throw new Error('输出位置必须是有效的 A1 单元格，例如 H3。');
    const axes=draft?.axes||{};const fields=draft?.fields||[];
    const rows=(axes.rows||[]).map(Number).filter((field)=>field>=0);
    const columns=(axes.columns||[]).map(Number).filter((field)=>field>=0);
    const pages=(axes.pages||[]).map((entry)=>Number(entry?.fld)).filter((field)=>field>=0);
    const unsupported=[];const values=(axes.data||[]).map((entry)=>{const aggregate=aggregateForLocal(entry?.subtotal);if(!aggregate)unsupported.push(entry?.subtotal||'unknown');return {field:Number(entry?.fld),aggregate:aggregate||'sum',caption:entry?.name||undefined};}).filter((entry)=>Number.isInteger(entry.field)&&entry.field>=0);
    if(!values.length)throw new Error('本地刷新至少需要一个值字段。');
    if(unsupported.length)throw new Error(`本地刷新尚不能精确执行汇总方式：${[...new Set(unsupported)].join('、')}。请先改为求和、计数、平均值、最小值、最大值或非重复计数。`);
    const filters=(draft?.filters||[]).map(filterForLocal).filter(Boolean);
    for(const page of axes.pages||[]){const field=Number(page?.fld);const itemIndex=Number(page?.item);const item=fields[field]?.items?.find((candidate)=>Number(candidate.cacheIndex??candidate.sourceIndex)===itemIndex);if(field>=0&&itemIndex>=0&&item?.value!=null)filters.push({field,op:'eq',value:clone(item.value)});}
    const sourceSheet=source.sheet!=null?sheetIndex(source.sheet,config.sourceSheet,info):Number(config.sourceSheet||0);
    const outputSheet=output.sheet!=null?sheetIndex(output.sheet,config.outputSheet,info):Number(config.outputSheet||0);
    const request={pivotTablePart:table.part,pivotCacheDefinitionPart:table.cachePart,sourceRange:{sheet:sourceSheet,r0:source.r0,c0:source.c0,r1:source.r1,c1:source.c1,headerRow:source.r0,firstDataRow:source.r0+1},rows,columns,pages,values,filters,grandTotals:{rows:draft?.display?.rowGrandTotals!==false,columns:draft?.display?.colGrandTotals!==false}};
    return {request,output:{sheet:outputSheet,row:output.r0,col:output.c0},boundary};
  }
  function localRefreshHttpRequest(op,table,cache,config,draft=table,info=pivotState.info) {
    const built=buildLocalRefreshRequest(table,cache,config,draft,info);const body={op,request:built.request};
    if(op!=='preview'&&config?.writeOutput!==false)body.output=built.output;
    return {url:'/api/pivot-local-refresh',body,built};
  }
  function renderRefreshResult(node,result) {
    if(!result){node.appendChild(text('p','点击“预览二维结果”会使用当前 IronCalc 单元格值在内存中聚合，不修改工作簿。','native-muted'));return;}
    if(!result.ok){const error=result.error||{};node.appendChild(text('div',`${error.code||'PIVOT_REFRESH'}：${error.message||'刷新失败'}${error.path?` (${error.path})`:''}`,'native-refresh-error'));return;}
    node.appendChild(text('div',`源记录 ${result.source?.recordCount??0} · 筛选后 ${result.filteredRecordCount??0} · ${result.result?.rows?.length||0} 行结果`,'native-refresh-summary'));
    const scroller=document.createElement('div');scroller.className='native-refresh-preview';const resultTable=document.createElement('table');
    (result.result?.rows||[]).slice(0,250).forEach((row,rowIndex)=>{const tr=document.createElement('tr');(row||[]).slice(0,100).forEach((value)=>{const cell=document.createElement(rowIndex<Number(result.result?.headerRows||0)?'th':'td');cell.textContent=value==null?'':String(value);tr.appendChild(cell);});resultTable.appendChild(tr);});
    scroller.appendChild(resultTable);node.appendChild(scroller);for(const warning of result.warnings||[])node.appendChild(text('div',warning.message||warning.code,'native-refresh-warning'));
  }
  function renderPivotRefresh(body) {
    const table=pivotTable();const cache=pivotCache(table);const boundary=localRefreshBoundary(cache);pivotState.refreshConfig||=initialRefreshConfig(table,cache);
    const config=pivotState.refreshConfig;const names=pivotState.info?.sheets||[];const sourceSection=section('本地工作表刷新');
    sourceSection.appendChild(text('div',boundary.message,boundary.supported?'native-refresh-boundary ok':'native-refresh-boundary blocked'));
    const form=document.createElement('div');form.className='native-refresh-form';
    const selectField=(label,key)=>{const wrapper=document.createElement('label');wrapper.appendChild(text('span',label));const select=document.createElement('select');names.forEach((name,index)=>select.appendChild(option(index,name,Number(config[key])===index)));select.onchange=()=>{config[key]=Number(select.value);};wrapper.appendChild(select);return wrapper;};
    form.append(selectField('源工作表','sourceSheet'),formInput('源区域（含标题）',config.sourceRef,(value)=>{config.sourceRef=value;}),selectField('输出工作表','outputSheet'),formInput('输出左上角',config.outputRef,(value)=>{config.outputRef=value;}),check('应用时把二维结果写入工作表',config.writeOutput!==false,(value)=>{config.writeOutput=value;}));sourceSection.appendChild(form);
    const layout=document.createElement('div');layout.className='native-refresh-layout';const describe=(title,values)=>{const node=document.createElement('div');node.append(text('strong',title),text('span',values.length?values.join('、'):'（空）'));return node;};const draft=pivotState.draft;const axes=draft?.axes||{};
    layout.append(describe('行',(axes.rows||[]).filter((value)=>Number(value)>=0).map(pivotFieldName)),describe('列',(axes.columns||[]).filter((value)=>Number(value)>=0).map(pivotFieldName)),describe('筛选器',(axes.pages||[]).map((value)=>pivotFieldName(value.fld))),describe('值',(axes.data||[]).map((value)=>`${value.name||pivotFieldName(value.fld)} (${value.subtotal||'sum'})`)),describe('标签/值筛选',(draft?.filters||[]).map((value)=>`${pivotFieldName(value.attributes?.fld)}: ${value.attributes?.type||'eq'}`)));sourceSection.appendChild(layout);
    const toolbar=document.createElement('div');toolbar.className='native-toolbar';const preview=text('button','预览二维结果');const apply=text('button','本地刷新并应用');apply.classList.add('primary');preview.disabled=apply.disabled=!boundary.supported;
    preview.onclick=async()=>{try{dlgStatus('native-pivot-dialog','正在按当前工作表值聚合预览…');const req=localRefreshHttpRequest('preview',table,cache,config,draft);pivotState.refreshResult=await apiPost(req.url,req.body);renderPivotTab();dlgStatus('native-pivot-dialog',pivotState.refreshResult.ok?'预览完成；工作簿尚未修改。':pivotState.refreshResult.error?.message||'预览失败');}catch(error){dlgStatus('native-pivot-dialog',error.message||String(error));}};
    apply.onclick=async()=>{try{dlgStatus('native-pivot-dialog','正在重建 PivotCache、原生 PivotTable 并写入二维结果…');const req=localRefreshHttpRequest('apply',table,cache,config,draft);pivotState.refreshResult=await apiPost(req.url,req.body);const result=pivotState.refreshResult;await loadPivotTables(table.part);pivotState.refreshResult=result;pivotState.tab='refresh';renderPivotDialog();dlgStatus('native-pivot-dialog',`本地刷新完成${result.writtenRange?.r0?'，二维结果已写入工作表':''}；导出后仍是原生可编辑 PivotTable。`);status('已本地刷新原生数据透视表');}catch(error){dlgStatus('native-pivot-dialog',error.message||String(error));}};
    toolbar.append(preview,apply);sourceSection.appendChild(toolbar);body.appendChild(sourceSection);const resultSection=section('二维结果预览');renderRefreshResult(resultSection,pivotState.refreshResult);body.appendChild(resultSection);
  }
  function renderPivotAdvanced(body) {
    const node = section('完整类型化差量 JSON');
    node.appendChild(text('p','这里覆盖字段属性、分类汇总、排序、项目可见性、四个轴、值显示方式和全部原生筛选器类型；未知 OOXML 子树仍由后端逐字节保留。','native-muted'));
    const area = document.createElement('textarea'); area.className='native-json'; area.value=JSON.stringify(pivotJsonDraft(),null,2); node.appendChild(area);
    const toolbar=document.createElement('div'); toolbar.className='native-toolbar'; const apply=text('button','应用 JSON 到草稿');
    apply.onclick=()=>{ try { const parsed=JSON.parse(area.value); const draft=pivotState.draft; for(const key of ['display','location','style','axes','filters']) if(parsed[key]!=null) draft[key]=parsed[key]; if(Array.isArray(parsed.fields)){ draft.fields=parsed.fields.map((field,index)=>{ const current=pivotState.draft.fields.find((item)=>item.index===field.index)||{}; return {...current,...field,_hiddenItems:field.hiddenItems||current._hiddenItems||[]}; }); } dlgStatus('native-pivot-dialog','JSON 已载入草稿，点击“应用到工作簿”提交。'); renderPivotTab(); } catch(error){ dlgStatus('native-pivot-dialog',`JSON 错误：${error.message}`); } };
    toolbar.appendChild(apply); node.appendChild(toolbar); body.appendChild(node);
  }
  function dlgStatus(id, message) { const node=byId(id)?.querySelector('.native-data-status'); if(node) node.textContent=message; }
  async function loadPivotTables(preferred = pivotState.part) {
    const [model,caches,info] = await Promise.all([
      apiPost('/api/pivot-tables',{op:'list'}), apiPost('/api/pivot-caches',{op:'list'}), api('/api/info'),
    ]);
    pivotState.model=model; pivotState.caches=caches; pivotState.info=info;
    pivotState.part=(model.tables||[]).some((table)=>table.part===preferred)?preferred:model.tables?.[0]?.part||null;
    pivotState.draft=pivotTable()?normalizePivotDraft(pivotTable()):null; pivotState.field=0; renderPivotDialog();
  }
  function ensurePivotDialog() {
    return shell('native-pivot-dialog','原生数据透视表','直接编辑 Excel pivotTableDefinition；字段轴、汇总、筛选、样式和布局保持原生可编辑，普通工作表缓存还可在 UniCell 内本地刷新。',[
      ['layout','布局'],['fields','字段'],['values','值'],['filters','筛选'],['refresh','本地刷新'],['advanced','高级'],
    ]);
  }
  async function openPivotTables() {
    const dlg=ensurePivotDialog();
    dlg.querySelector('[data-action="save"]').onclick=async()=>{ if(!pivotState.part)return; try{ dlgStatus(dlg.id,'正在验证并写入原生 OOXML…'); const model=await apiPost('/api/pivot-tables',{op:'update',part:pivotState.part,patch:pivotPatch()}); pivotState.model=model; pivotState.draft=normalizePivotDraft(pivotTable()); renderPivotDialog(); dlgStatus(dlg.id,'已写入差量；相关 PivotCache 已设为 Excel 打开时原生刷新。'); status('已更新原生数据透视表'); }catch(error){dlgStatus(dlg.id,error.message||String(error));} };
    dlg.querySelector('[data-action="reset"]').onclick=async()=>{ if(!pivotState.part)return; const model=await apiPost('/api/pivot-tables',{op:'reset',part:pivotState.part}); pivotState.model=model; pivotState.draft=normalizePivotDraft(pivotTable()); renderPivotDialog(); dlgStatus(dlg.id,'已恢复该透视表的导入状态。'); };
    dlg.hidden=false; dlgStatus(dlg.id,'正在读取原生关系图…');
    try{await loadPivotTables();dlgStatus(dlg.id,'');}catch(error){pivotState.model={tables:[]};pivotState.draft=null;renderPivotDialog();dlgStatus(dlg.id,error.message||String(error));}
  }
  byId('btn-pivot-tables').onclick=openPivotTables;
  window.__pivotLocalRefreshTestHooks={
    parseA1Range,localRefreshBoundary,initialRefreshConfig,buildLocalRefreshRequest,
    request:localRefreshHttpRequest,ensureDialog:ensurePivotDialog,
  };

  /* ---------------- Native Slicers ---------------- */
  const slicerState = { model:null, key:null, entry:null, cache:null, view:null, originalCache:null, originalView:null, tab:'items', manualPatch:null };
  function slicerEntries(model) {
    const entries=[]; const used=new Set();
    for(const part of model?.slicerParts||[]) for(const view of part.slicers||[]) {
      const cache=(model.caches||[]).find((candidate)=>candidate.part===view.cachePart || candidate.name===view.cache) || null;
      entries.push({key:`${part.part}|${view.sourceIndex}`,part,view,cache}); if(cache)used.add(cache.part);
    }
    for(const cache of model?.caches||[]) if(!used.has(cache.part)) entries.push({key:`cache|${cache.part}`,part:null,view:null,cache});
    return entries;
  }
  function selectSlicerEntry(key) {
    const entry=slicerEntries(slicerState.model).find((item)=>item.key===key)||slicerEntries(slicerState.model)[0]||null;
    slicerState.key=entry?.key||null; slicerState.entry=entry;
    slicerState.cache=entry?.cache?clone(entry.cache):null; slicerState.view=entry?.view?clone(entry.view):null;
    slicerState.originalCache=entry?.cache?clone(entry.cache):null; slicerState.originalView=entry?.view?clone(entry.view):null;
    if(slicerState.cache){ slicerState.cache._selected=new Set((slicerState.cache.items||[]).filter((item)=>item.selected).map((item)=>item.itemIndex)); slicerState.cache._connections=new Set((slicerState.cache.connections||[]).map((item)=>`${item.tabId}|${item.name}`)); slicerState.cache._olap=clone(slicerState.cache.olapSelections||[]); }
    slicerState.manualPatch=null;
  }
  function renderSlicerList(){
    const dlg=byId('native-slicer-dialog'); const list=dlg.querySelector('.native-data-list'); list.replaceChildren(); const entries=slicerEntries(slicerState.model);
    if(!entries.length){list.appendChild(text('div','当前工作簿没有原生切片器。','native-empty'));return;}
    for(const entry of entries){const button=document.createElement('button');button.type='button';button.classList.toggle('active',entry.key===slicerState.key);button.append(text('strong',entry.view?.caption||entry.view?.name||entry.cache?.name||entry.key),text('span',entry.part?.sheet||'仅缓存'),text('small',`${entry.cache?.sourceName||'未知字段'} · ${(entry.cache?.items||[]).length} 个项目`));button.onclick=()=>{selectSlicerEntry(entry.key);renderSlicerDialog();};list.appendChild(button);}
  }
  function renderSlicerDialog(){
    const dlg=byId('native-slicer-dialog');renderSlicerList(); const subtitle=slicerState.view?.name||slicerState.cache?.name||'';dlg.querySelector('.native-data-subtitle').textContent=subtitle;
    dlg.querySelectorAll('.native-data-tabs button').forEach((button)=>{button.classList.toggle('active',button.dataset.tab===slicerState.tab);button.onclick=()=>{slicerState.tab=button.dataset.tab;renderSlicerDialog();};});
    dlg.querySelector('[data-action="save"]').disabled=!slicerState.cache;dlg.querySelector('[data-action="reset"]').disabled=!(slicerState.model?.edited);
    const body=dlg.querySelector('.native-data-body');body.replaceChildren();if(!slicerState.cache){body.appendChild(text('div','请选择一个切片器。','native-empty'));return;}
    if(slicerState.tab==='items')renderSlicerItems(body);else if(slicerState.tab==='display')renderSlicerDisplay(body);else if(slicerState.tab==='connections')renderSlicerConnections(body);else renderSlicerAdvanced(body);
  }
  function renderSlicerItems(body){
    const cache=slicerState.cache;const node=section(cache.dataKind==='olap'?'OLAP 成员选择':'切片器项目选择');
    if(cache.items?.length){const toolbar=document.createElement('div');toolbar.className='native-toolbar';const all=text('button','全选');all.onclick=()=>{cache._selected=new Set(cache.items.map((item)=>item.itemIndex));renderSlicerDialog();};const data=text('button','仅有数据');data.onclick=()=>{const values=cache.items.filter((item)=>!item.noData).map((item)=>item.itemIndex);cache._selected=new Set(values.length?values:[cache.items[0].itemIndex]);renderSlicerDialog();};toolbar.append(all,data,text('span',`已选择 ${cache._selected.size}/${cache.items.length}`,'native-muted'));node.appendChild(toolbar);const grid=document.createElement('div');grid.className='native-item-grid';for(const item of cache.items){grid.appendChild(check(`${item.label}${item.noData?'（无数据）':''}`,cache._selected.has(item.itemIndex),(selected)=>{if(selected)cache._selected.add(item.itemIndex);else if(cache._selected.size>1)cache._selected.delete(item.itemIndex);else{dlgStatus('native-slicer-dialog','Excel 原生切片器至少要保留一个选中项目。');renderSlicerDialog();}}));}node.appendChild(grid);}
    else if(cache._olap?.length){cache._olap.forEach((selection,index)=>{const row=document.createElement('div');row.className='native-filter-row';const name=document.createElement('input');name.value=selection.name;name.oninput=()=>{selection.name=name.value;};const parents=document.createElement('input');parents.value=(selection.parents||[]).join(' / ');parents.title='父成员路径（仅新成员）';parents.oninput=()=>{selection.parents=parents.value.split('/').map((value)=>value.trim()).filter(Boolean);};const remove=text('button','删除','native-mini-btn');remove.style.width='auto';remove.disabled=cache._olap.length<=1;remove.onclick=()=>{cache._olap.splice(index,1);renderSlicerDialog();};row.append(name,parents,text('span','', ''),text('span','', ''),remove);node.appendChild(row);});const add=text('button','添加 OLAP 成员');add.className='native-mini-btn';add.style.width='auto';add.onclick=()=>{cache._olap.push({_new:true,name:'[Member]',parents:[]});renderSlicerDialog();};node.appendChild(add);}
    else node.appendChild(text('div','该缓存没有可编辑的项目集合。','native-muted'));body.appendChild(node);
    const options=section('排序与无数据项目');const form=document.createElement('div');form.className='native-form-grid two';const settings=cache.tabular||cache.table;
    if(settings){const sortWrap=document.createElement('label');sortWrap.appendChild(text('span','排序'));const sort=document.createElement('select');sort.append(option('ascending','升序',settings.sortOrder==='ascending'),option('descending','降序',settings.sortOrder==='descending'));sort.onchange=()=>{settings.sortOrder=sort.value;};sortWrap.appendChild(sort);form.appendChild(sortWrap);form.appendChild(check('使用自定义列表排序',bool(settings.customListSort),(value)=>{settings.customListSort=value;}));if(cache.tabular)form.appendChild(check('显示源中已删除项目',bool(settings.showMissing,true),(value)=>{settings.showMissing=value;}));const crossWrap=document.createElement('label');crossWrap.appendChild(text('span','跨筛选显示'));const cross=document.createElement('select');for(const [value,label] of [['none','不显示无数据项目'],['showItemsWithDataAtTop','有数据项目置顶'],['showItemsWithNoData','显示无数据项目']])cross.appendChild(option(value,label,settings.crossFilter===value));cross.onchange=()=>{settings.crossFilter=cross.value;};crossWrap.appendChild(cross);form.appendChild(crossWrap);}options.appendChild(form);body.appendChild(options);
  }
  function renderSlicerDisplay(body){
    const cache=slicerState.cache;const cacheNode=section('缓存身份');const cacheForm=document.createElement('div');cacheForm.className='native-form-grid two';cacheForm.append(formInput('缓存名称',cache.name,(value)=>{cache.name=value;}),formInput('源字段',cache.sourceName,(value)=>{cache.sourceName=value;}));cacheNode.appendChild(cacheForm);body.appendChild(cacheNode);
    if(!slicerState.view){body.appendChild(text('div','此缓存没有工作表切片器视图。','native-muted'));return;}const view=slicerState.view;const node=section('切片器视图与原生样式');const form=document.createElement('div');form.className='native-form-grid two';form.append(formInput('对象名称',view.name,(value)=>{view.name=value;}),formInput('标题',view.caption??'',(value)=>{view.caption=value||null;}),formInput('样式',view.style??'',(value)=>{view.style=value||null;}),formInput('列数',view.columnCount,(value)=>{view.columnCount=Math.max(1,value);},'number'),formInput('起始项目',view.startItem,(value)=>{view.startItem=Math.max(0,value);},'number'),formInput('级别',view.level,(value)=>{view.level=Math.max(0,value);},'number'),formInput('行高（EMU）',view.rowHeight??'',(value)=>{view.rowHeight=Math.max(1,value);},'number'),check('显示标题',bool(view.showCaption,true),(value)=>{view.showCaption=value;}),check('锁定位置',bool(view.lockedPosition),(value)=>{view.lockedPosition=value;}));node.appendChild(form);body.appendChild(node);
  }
  function compatibleSlicerTargets(cache){return (slicerState.model?.pivotTables||[]).filter((target)=>cache.pivotCacheId==null||target.pivotCacheId==null||Number(target.pivotCacheId)===Number(cache.pivotCacheId));}
  function renderSlicerConnections(body){
    const cache=slicerState.cache;const node=section('数据透视表连接');for(const target of compatibleSlicerTargets(cache)){const key=`${target.tabId}|${target.name}`;const row=document.createElement('label');row.className='native-connection-row';const box=document.createElement('input');box.type='checkbox';box.checked=cache._connections.has(key);box.onchange=()=>{if(box.checked)cache._connections.add(key);else cache._connections.delete(key);};row.append(box,text('span',target.sheet),text('strong',target.name),text('span',target.valid===false?'关系无效':`缓存 ${target.cacheId??'—'}`,target.valid===false?'native-danger':'native-muted'));node.appendChild(row);}if(!compatibleSlicerTargets(cache).length)node.appendChild(text('div','没有属于同一 PivotCache 的数据透视表。','native-muted'));body.appendChild(node);
  }
  function slicerConnectionOperations(cache,original){const operations=[];const desired=cache._connections||new Set();for(const connection of original.connections||[]){const key=`${connection.tabId}|${connection.name}`;if(!desired.has(key))operations.push({op:'delete',container:connection.container,tabId:connection.tabId,name:connection.name});}for(const key of desired){if(!(original.connections||[]).some((connection)=>`${connection.tabId}|${connection.name}`===key)){const [tabId,...name]=key.split('|');operations.push({op:'add',tabId:Number(tabId),name:name.join('|')});}}return operations;}
  function slicerOlapOperations(cache,original){const operations=[];const current=cache._olap||[];for(const old of original.olapSelections||[]){const now=current.find((item)=>!item._new&&item.sourceIndex===old.sourceIndex);if(!now)operations.push({op:'delete',sourceIndex:old.sourceIndex});else if(now.name!==old.name)operations.push({op:'update',sourceIndex:old.sourceIndex,patch:{name:now.name}});}for(const item of current.filter((value)=>value._new))operations.push({op:'add',name:item.name,parents:item.parents||[]});return operations;}
  function slicerPatch(){
    const cache=slicerState.cache,original=slicerState.originalCache;const cachePatch={name:cache.name,sourceName:cache.sourceName};
    if(cache.tabular)cachePatch.tabular=clone(cache.tabular);if(cache.table)cachePatch.table=clone(cache.table);if(cache.items?.length)cachePatch.selectedItemIndexes=[...cache._selected];
    const connectionOperations=slicerConnectionOperations(cache,original);if(connectionOperations.length)cachePatch.connectionOperations=connectionOperations;const olapSelectionOperations=slicerOlapOperations(cache,original);if(olapSelectionOperations.length)cachePatch.olapSelectionOperations=olapSelectionOperations;
    const result={cacheEdits:[{part:cache.part,patch:cachePatch}],slicerPartEdits:[]};if(slicerState.view&&slicerState.entry?.part){const view=slicerState.view;const viewPatch={name:view.name,caption:view.caption,columnCount:view.columnCount,showCaption:view.showCaption,startItem:view.startItem,level:view.level,style:view.style,lockedPosition:view.lockedPosition,rowHeight:view.rowHeight};result.slicerPartEdits.push({part:slicerState.entry.part.part,patch:{operations:[{op:'update',sourceIndex:slicerState.originalView.sourceIndex,patch:viewPatch}]}});}return result;
  }
  function renderSlicerAdvanced(body){const node=section('完整原生切片器 package patch');node.appendChild(text('p','可编辑 cache、tabular/table cache、项目、OLAP 成员、连接和视图操作。改名/删除会由 Rust 原子级联 workbook 定义名称与 DrawingML 锚点。','native-muted'));const area=document.createElement('textarea');area.className='native-json';area.value=JSON.stringify(slicerState.manualPatch||slicerPatch(),null,2);const toolbar=document.createElement('div');toolbar.className='native-toolbar';const apply=text('button','使用此 JSON');apply.onclick=()=>{try{slicerState.manualPatch=JSON.parse(area.value);dlgStatus('native-slicer-dialog','高级 patch 已载入，点击“应用到工作簿”提交。');}catch(error){dlgStatus('native-slicer-dialog',`JSON 错误：${error.message}`);}};toolbar.appendChild(apply);node.append(area,toolbar);body.appendChild(node);}
  async function loadSlicers(preferred=slicerState.key){const model=await apiPost('/api/slicers',{op:'list'});slicerState.model=model;const entries=slicerEntries(model);selectSlicerEntry(entries.some((item)=>item.key===preferred)?preferred:entries[0]?.key);renderSlicerDialog();}
  async function openSlicers(){
    const dlg=shell('native-slicer-dialog','原生切片器','直接编辑 Excel SlicerCache 与工作表 slicer 视图；项目选择、OLAP 成员、样式、连接和 DrawingML 身份均保持原生。',[['items','项目'],['display','显示'],['connections','连接'],['advanced','高级']]);
    dlg.querySelector('[data-action="save"]').onclick=async()=>{try{dlgStatus(dlg.id,'正在原子验证缓存、视图、连接与绘图锚点…');const model=await apiPost('/api/slicers',{op:'update',patch:slicerState.manualPatch||slicerPatch()});slicerState.model=model;selectSlicerEntry(slicerState.key);renderSlicerDialog();dlgStatus(dlg.id,'已写入原生切片器差量。');status('已更新原生切片器');}catch(error){dlgStatus(dlg.id,error.message||String(error));}};
    dlg.querySelector('[data-action="reset"]').onclick=async()=>{const model=await apiPost('/api/slicers',{op:'reset'});slicerState.model=model;selectSlicerEntry();renderSlicerDialog();dlgStatus(dlg.id,'已恢复全部切片器的导入状态。');};
    const actions=dlg.querySelector('.native-data-side-actions');if(!actions.childElementCount){const duplicate=text('button','克隆视图');duplicate.onclick=async()=>{const entry=slicerState.entry;if(!entry?.view)return;const proposed=prompt('新切片器对象名称',`${entry.view.name}_Copy`);if(!proposed)return;const patch={slicerPartEdits:[{part:entry.part.part,patch:{operations:[{op:'add',cloneSourceIndex:entry.view.sourceIndex,name:proposed,caption:`${entry.view.caption||entry.view.name} Copy`}]}}]};try{const model=await apiPost('/api/slicers',{op:'update',patch});slicerState.model=model;selectSlicerEntry();renderSlicerDialog();}catch(error){dlgStatus(dlg.id,error.message||String(error));}};const remove=text('button','删除视图');remove.classList.add('native-danger');remove.onclick=async()=>{const entry=slicerState.entry;if(!entry?.view||!confirm(`删除切片器 ${entry.view.name}？原生 DrawingML 锚点也会同步删除。`))return;try{const patch={slicerPartEdits:[{part:entry.part.part,patch:{operations:[{op:'delete',sourceIndex:entry.view.sourceIndex}]}}]};const model=await apiPost('/api/slicers',{op:'update',patch});slicerState.model=model;selectSlicerEntry();renderSlicerDialog();}catch(error){dlgStatus(dlg.id,error.message||String(error));}};actions.append(duplicate,remove);}
    dlg.hidden=false;dlgStatus(dlg.id,'正在解析原生 SlicerCache 关系图…');try{await loadSlicers();dlgStatus(dlg.id,'');}catch(error){slicerState.model={caches:[],slicerParts:[]};selectSlicerEntry();renderSlicerDialog();dlgStatus(dlg.id,error.message||String(error));}
  }
  byId('btn-slicers').onclick=openSlicers;

  /* ---------------- Native Timelines ---------------- */
  const timelineState = {
    model:null, key:null, entry:null, cache:null, view:null,
    originalCache:null, originalView:null, tab:'range', manualPatch:null,
  };
  const timelineSame = (left, right) => JSON.stringify(left) === JSON.stringify(right);
  const timelineConnectionKey = (tabId, name) => `${tabId}\u0000${name}`;
  function decodeTimelineConnectionKey(key) {
    const split=key.indexOf('\u0000');
    return {tabId:Number(key.slice(0,split)),name:key.slice(split+1)};
  }
  function timelineDateInput(value) {
    const match=String(value||'').match(/^-?\d{4,}-\d{2}-\d{2}/);
    return match ? match[0] : '';
  }
  function timelineDateTime(date, end=false) {
    return date ? `${date}T${end?'23:59:59':'00:00:00'}` : '';
  }
  function timelineEntries(model) {
    const entries=[];const represented=new Set();
    for(const view of model?.views||[]) {
      const cache=(model.caches||[]).find((candidate)=>candidate.part===view.cachePart)
        ||(model.caches||[]).find((candidate)=>candidate.name&&candidate.name===view.cache)||null;
      if(cache)represented.add(cache.part);
      const identity=view.name||view.uid||`${view.part}|${entries.length}`;
      entries.push({key:`view|${view.part}|${identity}`,view,cache});
    }
    for(const cache of model?.caches||[]) if(!represented.has(cache.part)) {
      entries.push({key:`cache|${cache.part}`,view:null,cache});
    }
    return entries;
  }
  function selectTimelineEntry(key) {
    const entries=timelineEntries(timelineState.model);
    const entry=entries.find((item)=>item.key===key)||entries[0]||null;
    timelineState.key=entry?.key||null;timelineState.entry=entry;
    timelineState.cache=entry?.cache?clone(entry.cache):null;
    timelineState.view=entry?.view?clone(entry.view):null;
    timelineState.originalCache=entry?.cache?clone(entry.cache):null;
    timelineState.originalView=entry?.view?clone(entry.view):null;
    if(timelineState.cache) {
      timelineState.cache._connections=new Set((timelineState.cache.connections||[])
        .map((connection)=>timelineConnectionKey(connection.tabId,connection.name)));
    }
    timelineState.manualPatch=null;
  }
  function renderTimelineList() {
    const dlg=byId('native-timeline-dialog');const list=dlg.querySelector('.native-data-list');list.replaceChildren();
    const entries=timelineEntries(timelineState.model);
    if(!entries.length) {
      list.appendChild(text('div','当前工作簿没有原生时间线。','native-empty'));return;
    }
    for(const entry of entries) {
      const button=document.createElement('button');button.type='button';button.classList.toggle('active',entry.key===timelineState.key);
      const selection=entry.cache?.state?.selection;
      const range=selection?`${timelineDateInput(selection.startDate)} — ${timelineDateInput(selection.endDate)}`:'未筛选';
      button.append(
        text('strong',entry.view?.caption||entry.view?.name||entry.cache?.name||'Timeline'),
        text('span',entry.view?.sheet||'仅缓存'),
        text('small',`${entry.cache?.sourceName||'未知日期字段'} · ${range}`),
      );
      button.onclick=()=>{selectTimelineEntry(entry.key);renderTimelineDialog();};list.appendChild(button);
    }
  }
  function renderTimelineDialog() {
    const dlg=byId('native-timeline-dialog');renderTimelineList();
    dlg.querySelector('.native-data-subtitle').textContent=timelineState.view?.name||timelineState.cache?.name||'';
    dlg.querySelectorAll('.native-data-tabs button').forEach((button)=>{
      button.classList.toggle('active',button.dataset.tab===timelineState.tab);
      button.onclick=()=>{timelineState.tab=button.dataset.tab;renderTimelineDialog();};
    });
    dlg.querySelector('[data-action="save"]').disabled=!(timelineState.cache||timelineState.view?.name);
    dlg.querySelector('[data-action="reset"]').disabled=!(timelineState.model?.edited);
    const body=dlg.querySelector('.native-data-body');body.replaceChildren();
    if(!timelineState.cache&&!timelineState.view) {
      body.appendChild(text('div','请选择一个时间线。','native-empty'));return;
    }
    if(timelineState.tab==='range')renderTimelineRange(body);
    else if(timelineState.tab==='display')renderTimelineDisplay(body);
    else if(timelineState.tab==='connections')renderTimelineConnections(body);
    else renderTimelineAdvanced(body);
  }
  function timelineRangeFields(node, title, range, onChange, required=false) {
    const sectionNode=section(title);
    if(!range) {
      sectionNode.appendChild(text('div',required?'此原生缓存缺少必需的日期边界。':'当前没有日期选择。','native-muted'));
      if(required) {
        const create=text('button','创建日期边界');create.type='button';create.className='native-mini-btn';create.style.width='auto';
        create.onclick=()=>{const today=new Date().toISOString().slice(0,10);onChange({field:'startDate',value:timelineDateTime(today)});onChange({field:'endDate',value:timelineDateTime(today,true)});renderTimelineDialog();};
        sectionNode.appendChild(create);
      }
      node.appendChild(sectionNode);return;
    }
    const form=document.createElement('div');form.className='native-form-grid two';
    form.append(
      formInput('开始日期',timelineDateInput(range.startDate),(value)=>onChange({
        field:'startDate',value:timelineDateTime(value,false),
      }),'date'),
      formInput('结束日期',timelineDateInput(range.endDate),(value)=>onChange({
        field:'endDate',value:timelineDateTime(value,true),
      }),'date'),
    );
    sectionNode.appendChild(form);node.appendChild(sectionNode);
  }
  function renderTimelineRange(body) {
    const cache=timelineState.cache;
    if(!cache) {body.appendChild(text('div','该时间线视图没有可解析的 TimelineCache。','native-empty'));return;}
    const state=cache.state||=( {} );
    const filter=section('日期筛选');const filterForm=document.createElement('div');filterForm.className='native-form-grid two';
    const filterWrap=document.createElement('label');filterWrap.appendChild(text('span','筛选类型'));
    const filterSelect=document.createElement('select');const current=state.filterType||'unknown';
    if(!['dateBetween','unknown'].includes(current))filterSelect.appendChild(option(current,`保留原生相对筛选：${current}`,true));
    filterSelect.append(option('dateBetween','介于（静态日期范围）',current==='dateBetween'),option('unknown','无日期筛选',current==='unknown'));
    filterSelect.onchange=()=>{
      state.filterType=filterSelect.value;
      if(filterSelect.value==='unknown')state.selection=null;
      else if(!state.selection) {
        const today=new Date().toISOString().slice(0,10);
        state.selection=state.bounds?clone(state.bounds):{startDate:timelineDateTime(today),endDate:timelineDateTime(today,true)};
      }
      renderTimelineDialog();
    };
    filterWrap.appendChild(filterSelect);filterForm.appendChild(filterWrap);
    filterForm.appendChild(check('单一连续范围',bool(state.singleRangeFilterState,true),(value)=>{state.singleRangeFilterState=value;}));
    filter.appendChild(filterForm);
    const toolbar=document.createElement('div');toolbar.className='native-toolbar';
    const all=text('button','选择全部边界');all.type='button';all.disabled=!state.bounds;
    all.onclick=()=>{state.selection=clone(state.bounds);state.filterType='dateBetween';renderTimelineDialog();};
    const clear=text('button','清除日期筛选');clear.type='button';clear.disabled=!state.selection&&state.filterType==='unknown';
    clear.onclick=()=>{state.selection=null;state.filterType='unknown';renderTimelineDialog();};
    toolbar.append(all,clear);filter.appendChild(toolbar);
    if(state.hasMovingPeriodState||state.hasTimelinePivotFilter) {
      filter.appendChild(text('p','切换或清除静态日期范围时，会同步移除冲突的 movingPeriodState / timelinePivotFilter，并更新所有已连接数据透视表的原生日期筛选。','native-muted'));
    }
    body.appendChild(filter);
    timelineRangeFields(body,'当前选择范围',state.selection,(edit)=>{
      state.selection={...(state.selection||{}),[edit.field]:edit.value};state.filterType='dateBetween';
    },false);
    timelineRangeFields(body,'可用日期边界',state.bounds,(edit)=>{
      state.bounds={...(state.bounds||{}),[edit.field]:edit.value};
    },true);
    const audit=section('原生筛选状态（只读）');
    const auditForm=document.createElement('div');auditForm.className='native-form-grid two';
    auditForm.append(
      formInput('PivotCache',state.pivotCachePart||(state.pivotCacheId??'—'),()=>{}),
      formInput('筛选 ID',state.filterId??'—',()=>{}),
      formInput('筛选透视表',state.filterPivotName||'—',()=>{}),
      formInput('刷新版本',state.lastRefreshVersion??'—',()=>{}),
    );
    auditForm.querySelectorAll('input').forEach((input)=>{input.readOnly=true;});audit.appendChild(auditForm);body.appendChild(audit);
  }
  function timelineLevelControl(label, value, onChange) {
    const wrapper=document.createElement('label');wrapper.appendChild(text('span',label));const select=document.createElement('select');
    for(const [level,name] of [[0,'年'],[1,'季度'],[2,'月'],[3,'日']])select.appendChild(option(level,name,Number(value??0)===level));
    select.onchange=()=>onChange(Number(select.value));wrapper.appendChild(select);return wrapper;
  }
  function renderTimelineDisplay(body) {
    const view=timelineState.view;
    if(!view) {body.appendChild(text('div','该 TimelineCache 没有工作表时间线视图。','native-empty'));return;}
    const identity=section('视图身份（原生关系目标）');const identityForm=document.createElement('div');identityForm.className='native-form-grid two';
    identityForm.append(formInput('对象名称',view.name||'—',()=>{}),formInput('工作表',view.sheet||'—',()=>{}),formInput('缓存',view.cache||view.cachePart||'—',()=>{}));
    identityForm.querySelectorAll('input').forEach((input)=>{input.readOnly=true;});identity.appendChild(identityForm);body.appendChild(identity);
    const display=section('标题、级别与样式');const form=document.createElement('div');form.className='native-form-grid two';
    form.append(
      formInput('标题',view.caption??'',(value)=>{view.caption=value||null;}),
      formInput('Timeline 样式',view.style??'',(value)=>{view.style=value||null;}),
      timelineLevelControl('显示级别',view.level,(value)=>{view.level=value;}),
      timelineLevelControl('选择级别',view.selectionLevel,(value)=>{view.selectionLevel=value;}),
      formInput('滚动到日期',timelineDateInput(view.scrollPosition),(value)=>{view.scrollPosition=value?timelineDateTime(value):null;},'date'),
    );
    display.appendChild(form);
    const flags=document.createElement('div');flags.className='native-form-grid two';
    flags.append(
      check('显示标题栏',bool(view.showHeader,true),(value)=>{view.showHeader=value;}),
      check('显示所选范围标签',bool(view.showSelectionLabel,true),(value)=>{view.showSelectionLabel=value;}),
      check('显示时间级别',bool(view.showTimeLevel,true),(value)=>{view.showTimeLevel=value;}),
      check('显示水平滚动条',bool(view.showHorizontalScrollbar,true),(value)=>{view.showHorizontalScrollbar=value;}),
    );
    display.appendChild(flags);body.appendChild(display);
  }
  function compatibleTimelineTargets(cache) {
    const pivotCachePart=cache?.state?.pivotCachePart;
    return (timelineState.model?.pivotTables||[]).filter((target)=>!pivotCachePart||!target.cachePart||target.cachePart===pivotCachePart);
  }
  function renderTimelineConnections(body) {
    const cache=timelineState.cache;
    if(!cache) {body.appendChild(text('div','该时间线没有可解析的 TimelineCache。','native-empty'));return;}
    const node=section('数据透视表连接');const targets=new Map();
    for(const target of compatibleTimelineTargets(cache))targets.set(timelineConnectionKey(target.sheetId,target.name),{
      tabId:target.sheetId,name:target.name,sheet:target.sheet,cachePart:target.cachePart,exists:true,
    });
    for(const connection of cache.connections||[]) {
      const key=timelineConnectionKey(connection.tabId,connection.name);
      if(!targets.has(key))targets.set(key,{...connection,exists:false});
    }
    for(const [key,target] of targets) {
      const row=document.createElement('label');row.className='native-connection-row';const box=document.createElement('input');box.type='checkbox';box.checked=cache._connections.has(key);
      box.onchange=()=>{if(box.checked)cache._connections.add(key);else cache._connections.delete(key);};
      row.append(
        box,text('span',target.sheet||`工作表 ID ${target.tabId}`),text('strong',target.name),
        text('span',target.exists===false?'原连接目标不存在':target.cachePart||'已解析',target.exists===false?'native-danger':'native-muted'),
      );node.appendChild(row);
    }
    if(!targets.size)node.appendChild(text('div','没有属于同一 PivotCache 的数据透视表。','native-muted'));
    node.appendChild(text('p','保存时仅提交 add/remove 差量；未修改的连接及其未知 OOXML 属性保持逐字节不变。','native-muted'));body.appendChild(node);
  }
  function timelineConnectionDelta(cache, original) {
    const before=new Set((original?.connections||[]).map((connection)=>timelineConnectionKey(connection.tabId,connection.name)));
    const after=cache?._connections||new Set();const add=[];const remove=[];
    for(const key of after)if(!before.has(key))add.push(decodeTimelineConnectionKey(key));
    for(const key of before)if(!after.has(key))remove.push(decodeTimelineConnectionKey(key));
    return {add,remove};
  }
  function timelinePatch() {
    const request={};const view=timelineState.view;const originalView=timelineState.originalView;
    if(view?.name&&originalView) {
      const patch={};
      for(const key of ['caption','level','selectionLevel','scrollPosition','style','showHeader','showSelectionLabel','showTimeLevel','showHorizontalScrollbar']) {
        if(!timelineSame(view[key],originalView[key]))patch[key]=clone(view[key]);
      }
      if(Object.keys(patch).length)request.view={part:view.part,name:originalView.name,patch};
    }
    const cache=timelineState.cache;const originalCache=timelineState.originalCache;
    if(cache&&originalCache) {
      const patch={};const state=cache.state||{};const originalState=originalCache.state||{};
      for(const key of ['selection','bounds','filterType','singleRangeFilterState']) {
        if(!timelineSame(state[key],originalState[key]))patch[key]=clone(state[key]);
      }
      const delta=timelineConnectionDelta(cache,originalCache);
      if(delta.add.length||delta.remove.length)patch.connections=delta;
      const staticStateChanged=Object.prototype.hasOwnProperty.call(patch,'selection')||Object.prototype.hasOwnProperty.call(patch,'filterType');
      if(staticStateChanged&&originalState.hasMovingPeriodState)patch.clearMovingPeriodState=true;
      if(staticStateChanged&&originalState.hasTimelinePivotFilter)patch.clearTimelinePivotFilter=true;
      if(Object.keys(patch).length)request.cache={part:cache.part,patch};
    }
    return request;
  }
  function renderTimelineAdvanced(body) {
    const node=section('完整原生 Timeline 类型化差量 JSON');
    node.appendChild(text('p','可在一次原子更新中编辑 timeline 视图、TimelineCache 日期状态和连接。仅支持后端 schema 中的字段，未出现的 OOXML 属性与扩展子树保持不变。','native-muted'));
    const area=document.createElement('textarea');area.className='native-json';area.value=JSON.stringify(timelineState.manualPatch||timelinePatch(),null,2);
    const toolbar=document.createElement('div');toolbar.className='native-toolbar';const apply=text('button','使用此 JSON');apply.type='button';
    apply.onclick=()=>{try{timelineState.manualPatch=JSON.parse(area.value);dlgStatus('native-timeline-dialog','高级 Timeline patch 已载入，点击“应用到工作簿”提交。');}catch(error){dlgStatus('native-timeline-dialog',`JSON 错误：${error.message}`);}};
    const discard=text('button','回到表单差量');discard.type='button';discard.onclick=()=>{timelineState.manualPatch=null;renderTimelineDialog();};
    toolbar.append(apply,discard);node.append(area,toolbar);body.appendChild(node);
  }
  async function loadTimelines(preferred=timelineState.key) {
    const model=await apiPost('/api/timelines',{op:'list'});timelineState.model=model;const entries=timelineEntries(model);
    selectTimelineEntry(entries.some((entry)=>entry.key===preferred)?preferred:entries[0]?.key);renderTimelineDialog();
  }
  async function openTimelines() {
    const dlg=shell('native-timeline-dialog','原生时间线','直接编辑 Excel Timeline 视图和 TimelineCache；日期范围会同步到所有已连接 PivotTable 的原生筛选。',[
      ['range','日期范围'],['display','显示'],['connections','连接'],['advanced','高级'],
    ]);
    dlg.querySelector('[data-action="save"]').onclick=async()=>{
      try {
        const patch=timelineState.manualPatch||timelinePatch();
        if(!patch||!Object.keys(patch).length) {dlgStatus(dlg.id,'没有需要写入的 Timeline 更改。');return;}
        dlgStatus(dlg.id,'正在原子验证日期状态、视图、连接与 PivotTable 原生筛选…');
        const model=await apiPost('/api/timelines',{op:'update',patch});timelineState.model=model;
        selectTimelineEntry(timelineState.key);renderTimelineDialog();dlgStatus(dlg.id,'已写入原生 Timeline 差量，并同步相关数据透视表筛选。');status('已更新原生时间线');
      } catch(error) {dlgStatus(dlg.id,error.message||String(error));}
    };
    dlg.querySelector('[data-action="reset"]').onclick=async()=>{
      try {const model=await apiPost('/api/timelines',{op:'reset'});timelineState.model=model;selectTimelineEntry();renderTimelineDialog();dlgStatus(dlg.id,'已恢复全部时间线的导入状态。');}
      catch(error){dlgStatus(dlg.id,error.message||String(error));}
    };
    dlg.hidden=false;dlgStatus(dlg.id,'正在解析原生 TimelineCache、视图与 PivotTable 关系图…');
    try {await loadTimelines();dlgStatus(dlg.id,'');}
    catch(error){timelineState.model={caches:[],views:[],pivotTables:[]};selectTimelineEntry();renderTimelineDialog();dlgStatus(dlg.id,error.message||String(error));}
  }
  const timelineButton=byId('btn-timelines');if(timelineButton)timelineButton.onclick=openTimelines;
})();
