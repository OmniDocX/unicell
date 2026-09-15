/* Excel-native Queries & Connections inspector/editor plus a deterministic Power Query M preview.
 * The backend owns OOXML validation and byte-exact opaque-part preservation.  This module only
 * emits allow-listed metadata deltas and never exposes or submits connection credentials. */
(() => {
  'use strict';

  const byId = (id) => document.getElementById(id);
  const clone = (value) => value == null ? value : (typeof structuredClone === 'function'
    ? structuredClone(value) : JSON.parse(JSON.stringify(value)));
  const boolValue = (value, fallback = false) => {
    if (value == null || value === '') return fallback;
    if (typeof value === 'string') return !['0', 'false', 'no', 'off'].includes(value.toLowerCase());
    return !!value;
  };
  const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);
  const text = (tag, value, className) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    node.textContent = value == null ? '' : String(value);
    return node;
  };

  const SECRET_KEY = /(?:password|passwd|pwd|access[ _-]*token|token|credential|secret|api[ _-]*key)/i;
  const SECRET_PAIR = /((?:password|passwd|pwd|access\s*token|token|credential|secret|api\s*key)\s*=\s*)[^;]*/gi;
  function redactString(value) {
    return String(value ?? '').replace(SECRET_PAIR, '$1***');
  }
  function redactForDisplay(value) {
    if (Array.isArray(value)) return value.map(redactForDisplay);
    if (value && typeof value === 'object') {
      const sensitiveRecord = Object.entries(value).some(([key, item]) =>
        /^(?:name|key|parameterName)$/i.test(key) && typeof item === 'string' && SECRET_KEY.test(item));
      return Object.fromEntries(Object.entries(value).map(([key, item]) => [
        key, SECRET_KEY.test(key) || (sensitiveRecord && !/^(?:name|key|parameterName|id|type)$/i.test(key))
          ? '***' : redactForDisplay(item),
      ]));
    }
    return typeof value === 'string' ? redactString(value) : value;
  }

  const CONNECTION_BOOLS = ['refreshOnLoad', 'background', 'saveData', 'enableRefresh', 'keepAlive'];
  const QUERY_BOOLS = [
    'refreshOnLoad', 'backgroundRefresh', 'preserveFormatting', 'adjustColumnWidth',
    'fillFormulas', 'removeDataOnSave', 'disableRefresh', 'applyNumberFormats',
    'applyBorderFormats', 'applyFontFormats', 'applyPatternFormats',
    'applyAlignmentFormats', 'applyWidthHeightFormats',
  ];
  const CONNECTION_SAFE_TEXT = ['name', 'description'];
  const QUERY_SAFE_TEXT = ['name', 'connectionId', 'growShrinkType'];

  function connectionDraft(connection) {
    const attributes = connection?.attributes || {};
    const draft = {
      part: connection?.part || '', id: connection?.id ?? '',
      name: connection?.name ?? attributes.name ?? '',
      description: connection?.description ?? attributes.description ?? '',
      commandText: connection?.commandText ?? '',
      interval: attributes.interval ?? '',
    };
    for (const key of CONNECTION_BOOLS) draft[key] = boolValue(attributes[key]);
    return draft;
  }

  function queryDraft(query) {
    const attributes = query?.attributes || {};
    const draft = {
      part: query?.part || '',
      name: query?.name ?? attributes.name ?? '',
      connectionId: query?.connectionId ?? attributes.connectionId ?? '',
      growShrinkType: attributes.growShrinkType ?? '',
      loadRange: query?.linkedTables?.[0]?.loadRange ?? '',
    };
    for (const key of QUERY_BOOLS) draft[key] = boolValue(attributes[key]);
    return draft;
  }

  function buildConnectionEdit(original, draft) {
    if (!original || !draft) return null;
    const base = connectionDraft(original);
    const attributes = {};
    for (const key of CONNECTION_SAFE_TEXT) {
      if (String(draft[key] ?? '') !== String(base[key] ?? '')) attributes[key] = String(draft[key] ?? '');
    }
    for (const key of CONNECTION_BOOLS) {
      if (!!draft[key] !== !!base[key]) attributes[key] = !!draft[key];
    }
    const oldInterval = String(base.interval ?? '');
    const newInterval = String(draft.interval ?? '');
    if (newInterval !== oldInterval) attributes.interval = newInterval === '' ? null : Number(newInterval);
    const edit = { part: original.part, id: original.id };
    if (Object.keys(attributes).length) edit.attributes = attributes;
    if (String(draft.commandText ?? '') !== String(base.commandText ?? '')) {
      edit.commandText = String(draft.commandText ?? '');
    }
    return Object.keys(edit).length > 2 ? edit : null;
  }

  function buildQueryEdit(original, draft) {
    if (!original || !draft) return null;
    const base = queryDraft(original);
    const attributes = {};
    for (const key of QUERY_SAFE_TEXT) {
      if (String(draft[key] ?? '') !== String(base[key] ?? '')) attributes[key] = String(draft[key] ?? '');
    }
    for (const key of QUERY_BOOLS) {
      if (!!draft[key] !== !!base[key]) attributes[key] = !!draft[key];
    }
    const edit = { part: original.part };
    if (Object.keys(attributes).length) edit.attributes = attributes;
    if (String(draft.loadRange ?? '') !== String(base.loadRange ?? '')) {
      edit.loadRange = String(draft.loadRange ?? '').trim();
    }
    return Object.keys(edit).length > 1 ? edit : null;
  }

  function buildMRequest(query, inputName, mode, inputText) {
    const name = String(inputName || 'Input').trim() || 'Input';
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error('输入名称只能使用字母、数字和下划线，且不能以数字开头');
    const source = mode === 'csv'
      ? { csv: String(inputText ?? '') }
      : { json: JSON.parse(String(inputText || '[]')) };
    return { m: String(query ?? ''), inputs: { [name]: source } };
  }

  function requestContract(kind, payload) {
    if (kind === 'list') return { url: '/api/native-data', body: { op: 'list' } };
    if (kind === 'reset') return { url: '/api/native-data', body: { op: 'reset' } };
    if (kind === 'update') return { url: '/api/native-data', body: { op: 'update', patch: payload } };
    if (kind === 'execute') return { url: '/api/power-query/execute', body: payload };
    throw new Error(`unknown query/connection request ${kind}`);
  }
  function send(kind, payload) {
    const request = requestContract(kind, payload);
    return apiPost(request.url, request.body);
  }

  const kinds = [
    ['connections', '连接'], ['queryTables', '查询表'], ['externalLinks', '外部链接'],
    ['dataModel', '数据模型'], ['opaqueDataParts', '不透明部件'],
  ];
  const state = { model: null, kind: 'connections', selected: 0, original: null, draft: null, mode: 'workbook' };

  function setStatus(message) {
    const node = byId('qc-status');
    if (node) node.textContent = message || '';
    if (message && typeof window.setStatus === 'function') window.setStatus(message);
  }

  function ensureDialog() {
    let dialog = byId('query-connection-dialog');
    if (dialog) return dialog;
    dialog = document.createElement('div');
    dialog.id = 'query-connection-dialog'; dialog.className = 'query-connection-dialog'; dialog.hidden = true;
    dialog.innerHTML = `
      <header class="qc-title"><strong>查询与连接</strong><span>原生 OOXML 元数据 + 安全 Power Query M 子集</span><button type="button" class="qc-close" aria-label="关闭">×</button></header>
      <nav class="qc-mode-tabs"><button type="button" data-mode="workbook" class="active">工作簿数据链路</button><button type="button" data-mode="m">M 子集预览</button></nav>
      <section class="qc-workbook">
        <aside class="qc-nav"><div class="qc-kinds"></div><div class="qc-list"></div></aside>
        <main class="qc-editor"></main>
      </section>
      <section class="qc-m-runtime" hidden>
        <div class="qc-m-layout">
          <div class="qc-m-side">
            <div class="qc-card"><h3>M 查询（无网络、文件、凭据、Mashup 或 DAX 访问）</h3><textarea id="qc-m-code" class="qc-m-code" spellcheck="false">let\n    Source = Input\nin\n    Source</textarea></div>
            <div class="qc-card"><h3>内存输入</h3><div class="qc-m-toolbar"><label>名称 <input id="qc-m-name" value="Input"></label><label>格式 <select id="qc-m-mode"><option value="json">JSON</option><option value="csv">CSV</option></select></label></div><textarea id="qc-m-input" class="qc-m-input" spellcheck="false">[{"Category":"A","Amount":3},{"Category":"B","Amount":10}]</textarea><div class="qc-m-toolbar"><button type="button" id="qc-m-run" class="primary">运行预览</button><span class="qc-muted">结果只在内存中计算，不会刷新外部源。</span></div></div>
          </div>
          <div class="qc-m-side"><div class="qc-card"><h3>预览结果</h3><div id="qc-preview" class="qc-preview"><div class="qc-empty">运行后在此显示表格</div></div><ul id="qc-diagnostics" class="qc-diagnostics"></ul></div></div>
        </div>
      </section>
      <footer class="qc-footer"><span id="qc-status" class="qc-status"></span><button type="button" id="qc-reset">恢复导入状态</button><button type="button" id="qc-apply" class="primary">应用元数据更改</button></footer>`;
    document.body.appendChild(dialog);
    dialog.querySelector('.qc-close').onclick = () => { dialog.hidden = true; byId('grid-scroll')?.focus(); };
    dialog.querySelectorAll('[data-mode]').forEach((button) => {
      button.onclick = () => setMode(button.dataset.mode);
    });
    byId('qc-apply').onclick = applyDraft;
    byId('qc-reset').onclick = resetNativeData;
    byId('qc-m-run').onclick = runMPreview;
    byId('qc-m-mode').onchange = (event) => {
      const area = byId('qc-m-input');
      if (event.target.value === 'csv') area.value = 'Category,Amount\r\nA,3\r\nB,10';
      else area.value = '[{"Category":"A","Amount":3},{"Category":"B","Amount":10}]';
    };
    dialog.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') dialog.querySelector('.qc-close').click();
      if ((event.ctrlKey || event.metaKey) && event.key === 'Enter' && state.mode === 'm') runMPreview();
    });
    return dialog;
  }

  function setMode(mode) {
    state.mode = mode === 'm' ? 'm' : 'workbook';
    const dialog = ensureDialog();
    dialog.querySelectorAll('[data-mode]').forEach((button) => button.classList.toggle('active', button.dataset.mode === state.mode));
    dialog.querySelector('.qc-workbook').hidden = state.mode !== 'workbook';
    dialog.querySelector('.qc-m-runtime').hidden = state.mode !== 'm';
    byId('qc-apply').hidden = state.mode !== 'workbook';
    byId('qc-reset').hidden = state.mode !== 'workbook';
    setStatus(state.mode === 'm' ? 'M 安全子集仅接受当前面板提供的内存 JSON/CSV。' : '');
  }

  function entriesForKind(kind = state.kind) {
    if (!state.model) return [];
    if (kind === 'dataModel') return state.model.dataModel ? [state.model.dataModel] : [];
    return Array.isArray(state.model[kind]) ? state.model[kind] : [];
  }

  function entryLabel(entry, index) {
    if (state.kind === 'connections') return entry.name || `连接 ${entry.id ?? index + 1}`;
    if (state.kind === 'queryTables') return entry.name || entry.part || `查询表 ${index + 1}`;
    if (state.kind === 'externalLinks') return entry.sheetNames?.join(', ') || entry.part || `外部链接 ${index + 1}`;
    if (state.kind === 'opaqueDataParts') return entry.part || `部件 ${index + 1}`;
    return '数据模型 / Power Pivot';
  }

  function entrySub(entry) {
    if (state.kind === 'connections') return `${entry.source?.kind || 'unknown'} · ID ${entry.id ?? '?'}`;
    if (state.kind === 'queryTables') return `${entry.connectionId ? `连接 ${entry.connectionId}` : '无连接'} · ${entry.part || ''}`;
    if (state.kind === 'externalLinks') return `${entry.cachedCellCount || 0} 个缓存单元格`;
    if (state.kind === 'opaqueDataParts') return `${entry.size || 0} bytes · 只读保留`;
    return entry.executable ? '可执行' : '字节级保留（不执行 DAX）';
  }

  function selectEntry(index) {
    const entries = entriesForKind();
    state.selected = Math.max(0, Math.min(Number(index) || 0, Math.max(0, entries.length - 1)));
    state.original = entries[state.selected] || null;
    state.draft = state.kind === 'connections' ? connectionDraft(state.original)
      : state.kind === 'queryTables' ? queryDraft(state.original) : null;
    renderNavigator(); renderEditor();
  }

  function selectKind(kind) {
    state.kind = kinds.some(([key]) => key === kind) ? kind : 'connections';
    state.selected = 0; selectEntry(0);
  }

  function renderNavigator() {
    const kindBar = ensureDialog().querySelector('.qc-kinds');
    kindBar.replaceChildren();
    for (const [key, label] of kinds) {
      const count = entriesForKind(key).length;
      const button = text('button', `${label} ${count}`); button.type = 'button';
      button.classList.toggle('active', key === state.kind); button.onclick = () => selectKind(key);
      kindBar.appendChild(button);
    }
    const list = ensureDialog().querySelector('.qc-list'); list.replaceChildren();
    const entries = entriesForKind();
    entries.forEach((entry, index) => {
      const button = text('button', entryLabel(entry, index)); button.type = 'button';
      button.appendChild(text('small', entrySub(entry)));
      button.classList.toggle('active', index === state.selected); button.onclick = () => selectEntry(index);
      list.appendChild(button);
    });
    if (!entries.length) list.appendChild(text('div', '当前工作簿没有此类原生部件', 'qc-empty'));
  }

  function card(title) {
    const node = document.createElement('section'); node.className = 'qc-card';
    node.appendChild(text('h3', title)); return node;
  }

  function textField(parent, label, key, wide = false, multiline = false) {
    const row = document.createElement('label'); row.className = `qc-field${wide ? ' wide' : ''}`;
    row.appendChild(text('span', label));
    const input = document.createElement(multiline ? 'textarea' : 'input');
    if (!multiline) input.type = 'text';
    input.value = state.draft?.[key] ?? '';
    input.oninput = () => { state.draft[key] = input.value; };
    row.appendChild(input); parent.appendChild(row); return input;
  }

  function boolFields(parent, keys, labels) {
    const checks = document.createElement('div'); checks.className = 'qc-checks';
    keys.forEach((key) => {
      const label = document.createElement('label'); const input = document.createElement('input');
      input.type = 'checkbox'; input.checked = !!state.draft[key];
      input.onchange = () => { state.draft[key] = input.checked; };
      label.append(input, text('span', labels[key] || key)); checks.appendChild(label);
    });
    parent.appendChild(checks);
  }

  function renderConnection(editor, connection) {
    const source = redactForDisplay(connection.source || {});
    const identity = card('连接属性（仅提交安全字段）'); const form = document.createElement('div'); form.className = 'qc-form';
    textField(form, '名称', 'name'); textField(form, '说明', 'description');
    const interval = textField(form, '刷新间隔（分钟）', 'interval'); interval.type = 'number'; interval.min = '0';
    textField(form, '命令文本', 'commandText', true, true);
    identity.appendChild(form);
    boolFields(identity, CONNECTION_BOOLS, {
      refreshOnLoad: '打开文件时刷新', background: '允许后台刷新', saveData: '随文件保存数据',
      enableRefresh: '允许刷新', keepAlive: '保持连接',
    });
    const sourceCard = card('数据源（只读且已脱敏）');
    sourceCard.appendChild(text('p', `类型：${source.kind || 'unknown'}　部件：${connection.part || ''}`, 'qc-muted'));
    const secret = text('div', source.connection || source.url || source.sourceFile || '未公开源地址', 'qc-secret');
    sourceCard.appendChild(secret);
    if (connection.sourceRedacted) sourceCard.appendChild(text('p', '工作簿中的凭据已在后端与前端双重脱敏；保存元数据不会覆盖原凭据。', 'qc-warning'));
    const raw = text('pre', JSON.stringify(redactForDisplay({ id: connection.id, type: connection.type, source, parameters: connection.parameters, hasExtensions: connection.hasExtensions }), null, 2), 'qc-raw');
    sourceCard.appendChild(raw); editor.append(identity, sourceCard);
  }

  function renderQuery(editor, query) {
    const main = card('查询表与加载位置'); const form = document.createElement('div'); form.className = 'qc-form';
    textField(form, '查询名称', 'name'); textField(form, '连接 ID', 'connectionId');
    textField(form, '扩缩行为', 'growShrinkType'); textField(form, '加载范围', 'loadRange');
    main.appendChild(form);
    boolFields(main, QUERY_BOOLS, {
      refreshOnLoad: '打开文件时刷新', backgroundRefresh: '后台刷新', preserveFormatting: '保留格式',
      adjustColumnWidth: '调整列宽', fillFormulas: '填充公式', removeDataOnSave: '保存时移除数据',
      disableRefresh: '禁用刷新', applyNumberFormats: '应用数字格式', applyBorderFormats: '应用边框',
      applyFontFormats: '应用字体', applyPatternFormats: '应用填充', applyAlignmentFormats: '应用对齐',
      applyWidthHeightFormats: '应用行列尺寸',
    });
    const links = card('字段与原生表连接');
    for (const linked of query.linkedTables || []) links.appendChild(text('span', `${linked.name || linked.part}: ${linked.loadRange || '—'}`, 'qc-badge'));
    if (!(query.linkedTables || []).length) links.appendChild(text('p', '没有解析到关联的 Excel 表；此时不能修改加载范围。', 'qc-muted'));
    links.appendChild(text('pre', JSON.stringify(redactForDisplay({ fields: query.fields, refresh: query.refresh, hasExtensions: query.hasExtensions }), null, 2), 'qc-raw'));
    editor.append(main, links);
  }

  function renderReadOnly(editor, entry) {
    const heading = state.kind === 'externalLinks' ? '外部链接（缓存只读）'
      : state.kind === 'opaqueDataParts' ? '不透明数据部件（字节级保留）' : 'Data Model / VertiPaq';
    const main = card(heading);
    if (state.kind === 'dataModel') {
      main.appendChild(text('p', `原生部件 ${entry.parts?.length || 0} 个，关系 ${entry.relationships?.length || 0} 条。`, 'qc-muted'));
      main.appendChild(text('p', 'UniCell 会原字节保留 Mashup / VertiPaq；当前安全运行时不会执行凭据、外部刷新或 DAX。', 'qc-warning'));
    } else if (state.kind === 'externalLinks') {
      main.appendChild(text('p', `工作表：${entry.sheetNames?.join(', ') || '—'}；缓存单元格：${entry.cachedCellCount || 0}`, 'qc-muted'));
      for (const item of entry.definedNames || []) main.appendChild(text('span', `${item.name}: ${item.refersTo || '—'}`, 'qc-badge'));
    } else {
      main.appendChild(text('p', `${entry.part || ''}　${entry.size || 0} bytes　SHA-256 ${entry.sha256 || '—'}`, 'qc-muted'));
    }
    main.appendChild(text('pre', JSON.stringify(redactForDisplay(entry), null, 2), 'qc-raw'));
    editor.appendChild(main);
  }

  function renderEditor() {
    const editor = ensureDialog().querySelector('.qc-editor'); editor.replaceChildren();
    if (!state.original) { editor.appendChild(text('div', '选择左侧项目以查看详细信息', 'qc-empty')); return; }
    if (state.kind === 'connections') renderConnection(editor, state.original);
    else if (state.kind === 'queryTables') renderQuery(editor, state.original);
    else renderReadOnly(editor, state.original);
  }

  async function applyDraft() {
    try {
      let patch = null;
      if (state.kind === 'connections') {
        const edit = buildConnectionEdit(state.original, state.draft);
        if (edit) patch = { connectionEdits: [edit] };
      } else if (state.kind === 'queryTables') {
        const edit = buildQueryEdit(state.original, state.draft);
        if (edit) patch = { queryTableEdits: [edit] };
      } else {
        setStatus('外部链接、Data Model 与不透明部件仅供检查，并按原字节保留。'); return;
      }
      if (!patch) { setStatus('没有需要写入的更改。'); return; }
      setStatus('正在验证并原子写入原生 OOXML 差量…');
      state.model = await send('update', patch);
      const preferred = state.selected; selectEntry(preferred);
      setStatus('连接/查询表元数据已更新；未修改的 OOXML 子树与凭据保持不变。');
    } catch (error) { setStatus(error.message || String(error)); }
  }

  async function resetNativeData() {
    try {
      if (!confirm('恢复查询与连接的导入状态？当前会话中的原生数据链路编辑将被撤销。')) return;
      setStatus('正在恢复导入状态…'); state.model = await send('reset'); selectEntry(0); setStatus('已恢复导入时的查询与连接元数据。');
    } catch (error) { setStatus(error.message || String(error)); }
  }

  function renderPreview(result) {
    const host = byId('qc-preview'); host.replaceChildren();
    const columns = Array.isArray(result?.columns) ? result.columns : [];
    const rows = Array.isArray(result?.rows) ? result.rows : [];
    if (!result?.ok || !columns.length) {
      host.appendChild(text('div', result?.ok ? '查询返回空表' : '查询未成功，请查看诊断信息', 'qc-empty'));
    } else {
      const table = document.createElement('table'); const head = document.createElement('thead'); const tr = document.createElement('tr');
      columns.forEach((column) => tr.appendChild(text('th', column))); head.appendChild(tr); table.appendChild(head);
      const body = document.createElement('tbody');
      rows.slice(0, 500).forEach((row) => {
        const line = document.createElement('tr');
        columns.forEach((_, index) => line.appendChild(text('td', row?.[index] == null ? '' : (typeof row[index] === 'object' ? JSON.stringify(row[index]) : row[index]))));
        body.appendChild(line);
      });
      table.appendChild(body); host.appendChild(table);
      if (rows.length > 500) host.appendChild(text('div', `界面仅显示前 500 行（共 ${rows.length} 行）`, 'qc-muted'));
    }
    const diagnostics = byId('qc-diagnostics'); diagnostics.replaceChildren();
    for (const diagnostic of result?.diagnostics || []) diagnostics.appendChild(text('li', `[${diagnostic.severity || 'info'}] ${diagnostic.message || diagnostic.code || ''}`));
  }

  async function runMPreview() {
    try {
      const payload = buildMRequest(byId('qc-m-code').value, byId('qc-m-name').value, byId('qc-m-mode').value, byId('qc-m-input').value);
      setStatus('正在沙盒内执行 M 子集…');
      const result = await send('execute', payload); renderPreview(result);
      setStatus(result.ok ? `M 预览完成：${result.rows?.length || 0} 行 × ${result.columns?.length || 0} 列。` : 'M 查询未执行成功，请查看诊断信息。');
    } catch (error) { renderPreview({ ok: false, diagnostics: [{ severity: 'error', message: error.message || String(error) }] }); setStatus(error.message || String(error)); }
  }

  async function loadModel() {
    setStatus('正在解析连接、QueryTable、外部链接与数据模型关系图…');
    state.model = await send('list');
    selectKind(entriesForKind('connections').length ? 'connections' : entriesForKind('queryTables').length ? 'queryTables' : 'dataModel');
    const warnings = state.model?.warnings || [];
    setStatus(warnings.length ? `已载入；${warnings.length} 条解析警告。` : '已载入原生查询与连接模型。');
  }

  async function openDialog() {
    const dialog = ensureDialog(); dialog.hidden = false; setMode('workbook');
    try { await loadModel(); }
    catch (error) {
      state.model = { connections: [], queryTables: [], externalLinks: [], opaqueDataParts: [], dataModel: { parts: [], relationships: [], executable: false, daxRuntime: false }, warnings: [String(error)] };
      selectKind('connections'); setStatus(error.message || String(error));
    }
  }

  window.__queryConnectionsTestHooks = {
    CONNECTION_BOOLS, QUERY_BOOLS, kinds: clone(kinds), redactString, redactForDisplay,
    connectionDraft, queryDraft, buildConnectionEdit, buildQueryEdit, buildMRequest,
    request: requestContract, ensureDialog, open: openDialog,
  };
  const button = byId('btn-query-connections');
  if (button) button.onclick = openDialog;
})();
