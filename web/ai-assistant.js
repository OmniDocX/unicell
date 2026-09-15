// U AI talks to a server-only OpenAI-compatible gateway, reads rich L0/L1/L2 context, and routes
// every generated cell, format, chart, and PivotTable write through one preview/confirm transaction.
'use strict';

(() => {
  const $ai = (id) => document.getElementById(id);
  const panel = $ai('ai-assistant');
  if (!panel) return;

  const thread = $ai('ai-thread');
  const contextSummary = $ai('ai-context-summary');
  const contextDetail = $ai('ai-context-detail');
  const selectionLabel = $ai('ai-selection-ref');
  const promptInput = $ai('ai-prompt');
  const requestPreview = $ai('ai-request-preview');
  const opsInput = $ai('ai-ops');
  const previewCard = $ai('ai-preview-card');
  const previewCount = $ai('ai-preview-count');
  const diffRoot = $ai('ai-diff');
  const feedbackRoot = $ai('ai-feedback');
  const acceptButton = $ai('ai-accept');
  const rejectButton = $ai('ai-reject');
  const toolList = $ai('ai-tool-list');
  const providerStatus = $ai('ai-provider-status');
  const sendButton = $ai('ai-send');
  const stopButton = $ai('ai-stop');
  const launcher = $ai('btn-ai-assistant');
  const resizer = $ai('ai-resizer');
  const fullscreenButton = $ai('ai-fullscreen');

  let digest = null;
  let aiConfig = null;
  let lastFeedback = { errors: [], validationViolations: [] };
  let pending = null;
  let returnFocus = null;
  let activeRequest = null;
  let activeStatusMessage = null;
  let busy = false;
  const chatHistory = [];
  const MAX_TOOL_ROUNDS = 6;
  const MAX_REPAIR_ROUNDS = 4;
  const MAX_TOOL_CALLS = 12;
  const MAX_AUTO_SLICE_CELLS = 50000;
  const MAX_TOOL_RESULT_CHARS = 1000000;
  const MIN_PANEL_WIDTH = 360;
  const PANEL_WIDTH_KEY = 'unicell-ai-width';
  const PANEL_FULLSCREEN_KEY = 'unicell-ai-full';

  function panelWidthBounds() {
    const min = Math.min(MIN_PANEL_WIDTH, window.innerWidth);
    return { min, max: Math.max(min, window.innerWidth - 12) };
  }

  function setPanelWidth(value, persist = true) {
    const bounds = panelWidthBounds();
    const width = Math.round(Math.max(bounds.min, Math.min(bounds.max, Number(value) || 460)));
    panel.style.width = `${width}px`;
    resizer?.setAttribute('aria-valuemin', String(bounds.min));
    resizer?.setAttribute('aria-valuemax', String(bounds.max));
    resizer?.setAttribute('aria-valuenow', String(width));
    if (persist) {
      try { localStorage.setItem(PANEL_WIDTH_KEY, String(width)); } catch (_) { /* ignore */ }
    }
    return width;
  }

  function toggleFullscreen(value, persist = true) {
    const active = value === undefined ? !panel.classList.contains('ai-full') : !!value;
    panel.classList.toggle('ai-full', active);
    panel.setAttribute('aria-modal', active ? 'true' : 'false');
    fullscreenButton?.setAttribute('aria-pressed', String(active));
    if (fullscreenButton) {
      fullscreenButton.title = active ? '退出全屏' : '全屏使用 U AI';
      fullscreenButton.setAttribute('aria-label', fullscreenButton.title);
      const label = fullscreenButton.querySelector('span');
      if (label) label.textContent = active ? '还原' : '全屏';
    }
    if (persist) {
      try { localStorage.setItem(PANEL_FULLSCREEN_KEY, active ? '1' : '0'); } catch (_) { /* ignore */ }
    }
    return active;
  }

  function restorePanelLayout() {
    let width = 460;
    let fullscreen = false;
    try {
      width = Number.parseInt(localStorage.getItem(PANEL_WIDTH_KEY) || '', 10) || width;
      fullscreen = localStorage.getItem(PANEL_FULLSCREEN_KEY) === '1';
    } catch (_) { /* ignore */ }
    setPanelWidth(width, false);
    toggleFullscreen(fullscreen, false);
  }

  function columnName(column) {
    let value = Math.max(1, Number(column) || 1);
    let out = '';
    while (value > 0) {
      value -= 1;
      out = String.fromCharCode(65 + (value % 26)) + out;
      value = Math.floor(value / 26);
    }
    return out;
  }

  function quoteSheet(name) {
    const value = String(name || 'Sheet1');
    return /^[A-Za-z_][A-Za-z0-9_.]*$/.test(value)
      ? value
      : `'${value.replaceAll("'", "''")}'`;
  }

  function currentSheetName() {
    return S.sheets[S.sheet] || `Sheet${S.sheet + 1}`;
  }

  function activeCellRef() {
    return `${quoteSheet(currentSheetName())}!${columnName(S.cur.c)}${S.cur.r}`;
  }

  function selectionRef() {
    const range = normSel();
    const first = `${columnName(range.c0)}${range.r0}`;
    const last = `${columnName(range.c1)}${range.r1}`;
    return `${quoteSheet(currentSheetName())}!${first}${first === last ? '' : `:${last}`}`;
  }

  function syncSelectionLabel() {
    selectionLabel.textContent = selectionRef();
  }

  function pretty(value, max = 16000) {
    const text = JSON.stringify(value, null, 2);
    return text.length > max ? `${text.slice(0, max)}\n…（界面显示已截断）` : text;
  }

  function addMessage(text, role = 'assistant') {
    if (!text) return;
    if (role === 'user') {
      const welcome = thread.querySelector('.ai-welcome');
      if (welcome) welcome.hidden = true;
    }
    const message = document.createElement('div');
    message.className = `ai-message ai-message-${role}`;
    message.textContent = text;
    thread.appendChild(message);
    thread.scrollTop = thread.scrollHeight;
    return message;
  }

  function setRunStatus(text) {
    const value = String(text || '').trim();
    if (!value) {
      clearRunStatus();
      return null;
    }
    if (!activeStatusMessage?.isConnected) {
      activeStatusMessage = document.createElement('div');
      activeStatusMessage.className = 'ai-message ai-message-status';
      activeStatusMessage.setAttribute('role', 'status');
      activeStatusMessage.setAttribute('aria-live', 'polite');
      activeStatusMessage.setAttribute('aria-atomic', 'true');
      const label = document.createElement('span');
      label.className = 'ai-status-text';
      activeStatusMessage.appendChild(label);
      thread.appendChild(activeStatusMessage);
    }
    const label = activeStatusMessage.querySelector('.ai-status-text');
    if (label) label.textContent = value;
    activeStatusMessage.title = value;
    activeStatusMessage.setAttribute('aria-label', value);
    thread.scrollTop = thread.scrollHeight;
    return activeStatusMessage;
  }

  function clearRunStatus() {
    activeStatusMessage?.remove();
    activeStatusMessage = null;
  }

  function setBusy(value) {
    busy = !!value;
    sendButton.disabled = busy || aiConfig?.configured === false;
    promptInput.disabled = busy;
    stopButton.hidden = !busy;
    sendButton.textContent = busy ? '思考中…' : '发送';
  }

  async function loadAiConfig() {
    providerStatus.dataset.state = '';
    providerStatus.textContent = '检查模型…';
    try {
      aiConfig = await api('/api/ai/config');
      providerStatus.textContent = aiConfig.configured ? 'AI 已连接' : 'AI 未配置';
      providerStatus.dataset.state = aiConfig.configured ? 'ready' : 'error';
      providerStatus.title = aiConfig.configured
        ? `${aiConfig.provider} · ${aiConfig.model}；服务端配置来源：${aiConfig.source}；地址和密钥不会下发到浏览器`
        : `${aiConfig.model} 尚未配置密钥；请在服务端配置 UNICELL_AI_KEY`;
      sendButton.disabled = !aiConfig.configured;
      return aiConfig;
    } catch (error) {
      aiConfig = { configured: false, model: '未知', provider: '不可用' };
      providerStatus.textContent = 'AI 暂不可用';
      providerStatus.dataset.state = 'error';
      providerStatus.title = error.message || String(error);
      sendButton.disabled = true;
      return aiConfig;
    }
  }

  function renderTools() {
    toolList.textContent = '';
    const commands = window.UniCellCommandRegistry?.list?.() || [];
    for (const command of commands) {
      const chip = document.createElement('span');
      chip.textContent = command.where ? `${command.label} · ${command.where}` : command.label;
      toolList.appendChild(chip);
    }
  }

  function renderDigest(data) {
    contextSummary.textContent = '';
    const current = data.sheets?.find((sheet) => sheet.sheet === currentSheetName()) || data;
    const lines = [
      ['工作表', data.sheetCount ?? data.sheets?.length ?? 0],
      ['当前范围', current.usedRange || '空表'],
      ['数据列', current.columns?.length ?? 0],
      ['Excel 表', data.tables?.length ?? 0],
      ['透视表', data.pivotTables?.length ?? 0],
      ['命名区域', data.definedNames?.length ?? 0],
      ['条件格式', data.conditionalFormats?.reduce?.((sum, item) => sum + (item.count || 0), 0) ?? 0],
      ['数据验证', data.dataValidations?.reduce?.((sum, item) => sum + (item.count || 0), 0) ?? 0],
    ];
    lines.forEach(([label, value], index) => {
      if (index) contextSummary.appendChild(document.createTextNode(' · '));
      const strong = document.createElement('strong');
      strong.textContent = `${label} ${value}`;
      contextSummary.appendChild(strong);
    });
  }

  async function loadDigest() {
    contextSummary.textContent = '正在读取工作簿摘要…';
    try {
      digest = await api('/api/ai/context', {
        method: 'POST',
        body: JSON.stringify({ op: 'digest' }),
      });
      renderDigest(digest);
      return digest;
    } catch (error) {
      contextSummary.textContent = `摘要读取失败：${error.message || error}`;
      throw error;
    }
  }

  async function readContext(op, reference) {
    syncSelectionLabel();
    contextDetail.hidden = false;
    contextDetail.textContent = '读取中…';
    try {
      const result = await api('/api/ai/context', {
        method: 'POST',
        body: JSON.stringify({ op, ref: reference }),
      });
      contextDetail.textContent = pretty(result);
      return result;
    } catch (error) {
      contextDetail.textContent = `读取失败：${error.message || error}`;
      throw error;
    }
  }

  function operationSchema() {
    return {
      setFormula: { op: 'setFormula', ref: 'Sheet1!C2', formula: '=SUM(A2:B2)' },
      setValue: { op: 'setValue', ref: 'Sheet1!A2', value: 'literal' },
      setRange: { op: 'setRange', ref: 'Sheet1!A2:B3', values: [[1, 2], [3, 4]] },
      clear: { op: 'clear', ref: 'Sheet1!D2:D20' },
      setFormat: {
        op: 'setFormat',
        ref: 'Sheet1!A1:F20',
        style: { 'font.bold': true, 'fill.color': '#E8F5E9', numberFormat: '#,##0.00' },
      },
      setBorder: { op: 'setBorder', ref: 'Sheet1!A1:F20', type: 'outer', style: 'thin', color: '#1F2937' },
      updateChart: {
        op: 'updateChart', sheet: 'Sheet1', chartId: 'native-chart-id',
        patch: { title: '销售趋势', legend: { show: true, position: 'bottom' } },
      },
      updatePivotTable: {
        op: 'updatePivotTable', part: 'xl/pivotTables/pivotTable1.xml',
        patch: { display: { rowGrandTotals: true, colGrandTotals: true } },
      },
    };
  }

  function readToolSchema() {
    return {
      digest: { tool: 'digest', sheet: 'optional sheet name or index' },
      slice: { tool: 'slice', ref: 'Sheet1!A1:F50' },
      detail: { tool: 'detail', ref: 'Sheet1!C12' },
      errors: { tool: 'errors', sheet: 'optional sheet name or index' },
    };
  }

  function buildModelRequest() {
    return {
      version: 2,
      assistant: 'U AI',
      task: promptInput.value.trim(),
      selection: selectionRef(),
      context: digest,
      feedbackFromPreviousAttempt: lastFeedback,
      availableCommands: window.UniCellCommandRegistry?.list?.() || [],
      readTools: readToolSchema(),
      controlledWorkbookOps: operationSchema(),
      constraints: {
        a1ReferencesOnly: true,
        neverTouchOOXML: true,
        preferFormulaOverLiteralResult: true,
        dryRunBeforeCommit: true,
        oneConfirmedBatchIsOneUndoStep: true,
        capabilityFirstNoTokenSaving: true,
        mixedTypedOpsAreAtomic: true,
      },
    };
  }

  function systemPrompt() {
    return `你是电子表格助手 U AI。你只能通过下列 JSON 协议读取和修改工作簿，不得生成、要求或修改 OOXML/XML。

每次必须只输出一个 JSON 对象，不要 Markdown、代码围栏或对象外文本：
{"message":"给用户的简洁中文说明","toolCalls":[],"ops":[],"commandSuggestions":[]}

读取工具：digest、slice、detail、errors。需要更多信息时，把 toolCalls 设为最多 12 个读取调用并让 ops 为空；系统会把结果返回给你。slice/detail 必须使用带工作表名的 A1 ref。
写入工具：setFormula、setValue、setRange、clear、setFormat、setBorder、updateChart、updatePivotTable。最终修改放进 ops；同一任务需要的不同类型操作可以混合在一个 ops 数组中，它们会原子预览、原子提交并作为一步撤销。
单元格必须用带工作表名的 A1 ref。公式必须以 = 开头，优先写公式而不是模型心算后的固定值。setValue 必须逐字保留用户指定的文本；如果用户明确说“作为文本/不要当公式”，即使文本以 = 开头也必须保留这个 =。setRange.values 必须是与 ref 区域尺寸一致的二维数组，null 表示清空。
setFormat.style 可使用 font.bold、font.italic、font.underline、font.strike、font.size、font.name、font.color、fill.color、alignment.horizontal、alignment.vertical、alignment.wrapText、numberFormat。setBorder.type 可用 all、inner、outer、top、right、bottom、left、centerh、centerv、none。
图表使用 digest.charts 中的稳定 id 和完整 model，updateChart 必须提供 sheet、chartId 和差量 patch；可编辑 title、legend、plots、axes、series 等原生模型字段。透视表使用 digest.pivotTables 中的稳定 part 和完整 model，updatePivotTable 必须提供 part 和差量 patch；可编辑 display、location、style、fields、axes、filters。需要修改数组时，先读取完整 model，再返回修改后的完整数组。不要臆造 chartId 或 part。
无需为了节省 token 省略完成任务所需的读取、推理或结构；优先获得完整上下文和正确结果。没有修改时 ops 为空。
如果用户只要求用公式计算一个纯数字算式，不要创建标题、标签、说明或重复结果，只在当前活动单元格生成一个 setFormula；公式结果显示在写入公式的同一个单元格。
message 中提到的每个工作表和单元格地址必须与 ops 完全一致；不要猜测结果位置或计算值，界面会根据 dry-run 的真实结果向用户说明。
功能区命令：仅当任务无法由上述 typed ops 完成时，才可从 availableCommands 选择最多 4 个稳定 id 放入 commandSuggestions，格式为 {"id":"命令id","reason":"为什么建议"}。格式、图表和透视表修改必须直接使用 typed ops，不得退化为命令建议。这些命令绝不能自动执行，只会显示为按钮等待用户点击。
不要声称已经写入：所有 ops 都会先 dry-run，只有用户明确接受后才会作为一步撤销提交。收到 dry-run 的公式错误或验证冲突时，先修正 ops。`;
  }

  function simpleArithmeticFormula(task) {
    const source = String(task || '').trim();
    if (!/(?:使用|用).{0,4}公式|公式.{0,4}(?:计算|算)/.test(source)) return '';
    const match = source.match(/(?:计算|算)\s*([0-9０-９.,，+\-*/×xX÷^()%（）\s]+?)(?=\s*(?:等于|是多少|的结果|$))/);
    if (!match) return '';
    const fullWidthDigits = '０１２３４５６７８９';
    const expression = match[1]
      .replace(/[０-９]/g, (digit) => String(fullWidthDigits.indexOf(digit)))
      .replace(/[，,]/g, '')
      .replace(/[xX×]/g, '*')
      .replace(/÷/g, '/')
      .replace(/（/g, '(')
      .replace(/）/g, ')')
      .replace(/\s+/g, '');
    if (!expression || expression.length > 200 || !/^[0-9.+\-*/^()%]+$/.test(expression)) return '';
    if (!/[+\-*/^%]/.test(expression.replace(/^[+\-]/, ''))) return '';
    return `=${expression}`;
  }

  function normalizeTaskOps(task, operations) {
    const formula = simpleArithmeticFormula(task);
    if (!formula) return operations;
    return [{ op: 'setFormula', ref: activeCellRef(), formula }];
  }

  function shortValue(value, limit = 72) {
    const text = String(value ?? '');
    return text.length > limit ? `${text.slice(0, limit)}…` : text;
  }

  function bareCellRef(reference) {
    const value = String(reference || '').trim();
    return (value.includes('!') ? value.slice(value.lastIndexOf('!') + 1) : value)
      .replace(/\$/g, '')
      .toUpperCase();
  }

  function matchingDiff(operation, result) {
    const wanted = bareCellRef(operation.ref);
    return (result?.diff || []).find((change) => bareCellRef(change.ref) === wanted) || null;
  }

  function operationPlanMessage(operations, result) {
    const actions = operations.slice(0, 4).map((operation) => {
      if (operation.op === 'setFormula') {
        const calculated = matchingDiff(operation, result)?.after?.formatted;
        return `将在 ${operation.ref} 写入公式 ${shortValue(operation.formula)}，计算结果显示在同一单元格${calculated !== undefined && calculated !== '' ? `；本次预览结果为 ${shortValue(calculated)}` : ''}`;
      }
      if (operation.op === 'setValue') return `将在 ${operation.ref} 写入“${shortValue(operation.value)}”`;
      if (operation.op === 'setRange') return `将批量写入 ${operation.ref}`;
      if (operation.op === 'clear') return `将清除 ${operation.ref}`;
      if (operation.op === 'setFormat') return `将修改 ${operation.ref} 的格式`;
      if (operation.op === 'setBorder') return `将修改 ${operation.ref} 的边框`;
      if (operation.op === 'updateChart') return `将修改 ${operation.sheet || '当前工作表'} 的图表 ${operation.chartId}`;
      return `将修改数据透视表 ${operation.part}`;
    });
    const remaining = operations.length - actions.length;
    return `已生成安全预览：${actions.join('；')}${remaining > 0 ? `；另有 ${remaining} 项` : ''}。确认后才会写入。`;
  }

  function appliedOperationsMessage(operations, result) {
    const actions = operations.slice(0, 4).map((operation) => {
      const change = matchingDiff(operation, result);
      const reference = change?.ref || operation.ref;
      if (operation.op === 'setFormula') {
        const calculated = change?.after?.formatted;
        return `已在 ${reference} 写入公式 ${shortValue(operation.formula)}${calculated !== undefined && calculated !== '' ? `，当前计算结果为 ${shortValue(calculated)}` : ''}`;
      }
      if (operation.op === 'setValue') return `已在 ${reference} 写入“${shortValue(change?.after?.formatted ?? operation.value)}”`;
      if (operation.op === 'setRange') return `已批量写入 ${operation.ref}`;
      if (operation.op === 'clear') return `已清除 ${reference}`;
      if (operation.op === 'setFormat') return `已修改 ${operation.ref} 的格式`;
      if (operation.op === 'setBorder') return `已修改 ${operation.ref} 的边框`;
      if (operation.op === 'updateChart') return `已修改 ${operation.sheet || '当前工作表'} 的图表 ${operation.chartId}`;
      return `已修改数据透视表 ${operation.part}`;
    });
    const remaining = operations.length - actions.length;
    return `${actions.join('；')}${remaining > 0 ? `；另完成 ${remaining} 项` : ''}。整次修改可用 Ctrl+Z 一步撤销。`;
  }

  function selectionCellCount() {
    const range = normSel();
    return (range.r1 - range.r0 + 1) * (range.c1 - range.c0 + 1);
  }

  async function gatherModelContext() {
    if (!digest) await loadDigest();
    const request = buildModelRequest();
    const selectedCells = selectionCellCount();
    if (selectedCells <= MAX_AUTO_SLICE_CELLS) {
      try {
        request.selectionSlice = await api('/api/ai/context', {
          method: 'POST',
          body: JSON.stringify({ op: 'slice', ref: selectionRef() }),
        });
      } catch (error) {
        request.selectionSlice = { unavailable: error.message || String(error) };
      }
    } else {
      request.selectionSlice = {
        deferred: true,
        ref: selectionRef(),
        cells: selectedCells,
        reason: `自动切片上限为 ${MAX_AUTO_SLICE_CELLS} 格；请用 slice 工具按需下钻`,
      };
    }
    try {
      request.activeCellDetail = await api('/api/ai/context', {
        method: 'POST',
        body: JSON.stringify({ op: 'detail', ref: activeCellRef() }),
      });
    } catch (error) {
      request.activeCellDetail = { unavailable: error.message || String(error) };
    }

    return request;
  }

  function extractJsonText(text) {
    const value = String(text || '').trim();
    if (!value) throw new Error('模型没有返回内容');
    const fenced = value.match(/^```(?:json)?\s*([\s\S]*?)\s*```$/i);
    if (fenced) return fenced[1];
    const firstObject = value.indexOf('{');
    const lastObject = value.lastIndexOf('}');
    if (firstObject >= 0 && lastObject > firstObject) return value.slice(firstObject, lastObject + 1);
    const firstArray = value.indexOf('[');
    const lastArray = value.lastIndexOf(']');
    if (firstArray >= 0 && lastArray > firstArray) return value.slice(firstArray, lastArray + 1);
    return value;
  }

  function parseAssistantEnvelope(text) {
    let parsed;
    try {
      parsed = JSON.parse(extractJsonText(text));
    } catch (error) {
      throw new Error(`模型返回不是有效的受控 JSON：${error.message}`);
    }
    if (Array.isArray(parsed)) parsed = { message: '', toolCalls: [], ops: parsed };
    if (!parsed || typeof parsed !== 'object') throw new Error('模型返回必须是 JSON 对象');
    const toolCalls = Array.isArray(parsed.toolCalls) ? parsed.toolCalls : [];
    if (toolCalls.length > MAX_TOOL_CALLS) throw new Error(`模型单轮读取工具不能超过 ${MAX_TOOL_CALLS} 个`);
    toolCalls.forEach((call, index) => {
      if (!call || typeof call !== 'object' || !['digest', 'slice', 'detail', 'errors'].includes(call.tool)) {
        throw new Error(`模型第 ${index + 1} 个读取工具不受支持`);
      }
      if (['slice', 'detail'].includes(call.tool) && (typeof call.ref !== 'string' || !call.ref.trim())) {
        throw new Error(`模型第 ${index + 1} 个 ${call.tool} 缺少 A1 ref`);
      }
    });
    const ops = Array.isArray(parsed.ops)
      ? parsed.ops
      : Array.isArray(parsed.operations) ? parsed.operations : [];
    if (ops.length) parseOps(JSON.stringify(ops));
    if (toolCalls.length && ops.length) throw new Error('模型必须先完成读取，不能在同一轮同时读取和写入');
    const commandSuggestions = Array.isArray(parsed.commandSuggestions) ? parsed.commandSuggestions : [];
    if (commandSuggestions.length > 4) throw new Error('模型单轮命令建议不能超过 4 个');
    const commandIds = new Set((window.UniCellCommandRegistry?.list?.() || []).map((command) => command.id));
    commandSuggestions.forEach((suggestion, index) => {
      if (!suggestion || typeof suggestion.id !== 'string' || !commandIds.has(suggestion.id)) {
        throw new Error(`模型第 ${index + 1} 个功能区命令 id 无效`);
      }
    });
    return {
      message: typeof parsed.message === 'string' ? parsed.message.trim() : '',
      toolCalls,
      ops,
      commandSuggestions,
    };
  }

  function renderCommandSuggestions(suggestions) {
    if (!suggestions.length) return;
    const catalog = new Map((window.UniCellCommandRegistry?.list?.() || []).map((command) => [command.id, command]));
    const root = document.createElement('div');
    root.className = 'ai-command-suggestions';
    for (const suggestion of suggestions) {
      const command = catalog.get(suggestion.id);
      if (!command) continue;
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = `执行：${command.label}`;
      button.title = suggestion.reason || `${command.where}；仅在点击后执行`;
      button.addEventListener('click', () => {
        const executed = window.UniCellCommandRegistry?.run?.(suggestion.id) === true;
        if (executed) {
          button.disabled = true;
          button.textContent = `已执行：${command.label}`;
          window.UniCellUI?.toast?.(`U AI 命令：${command.label}`);
        } else {
          window.UniCellUI?.toast?.('命令已失效，请刷新后重试', { tone: 'error' });
        }
      });
      root.appendChild(button);
    }
    if (root.childElementCount) {
      thread.appendChild(root);
      thread.scrollTop = thread.scrollHeight;
    }
  }

  async function executeReadTool(call) {
    const payload = { op: call.tool };
    if (call.ref) payload.ref = call.ref;
    if (call.sheet !== undefined) payload.sheet = call.sheet;
    const result = await api('/api/ai/context', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
    const encoded = JSON.stringify(result);
    if (encoded.length > MAX_TOOL_RESULT_CHARS) {
      return {
        ok: false,
        tool: call.tool,
        ref: call.ref || null,
        error: `结果有 ${encoded.length} 字符，超过单个工具上下文预算；请请求更窄的区域`,
      };
    }
    return { ok: true, call, result };
  }

  async function callModel(messages) {
    activeRequest = new AbortController();
    return api('/api/ai/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ messages }),
      signal: activeRequest.signal,
    });
  }

  function rememberConversation(task, response) {
    chatHistory.push({ role: 'user', content: task });
    chatHistory.push({ role: 'assistant', content: response || '已生成受控工作簿操作。' });
    while (chatHistory.length > 12) chatHistory.shift();
  }

  async function runAssistant(overrideTask) {
    const task = String(overrideTask ?? promptInput.value).trim();
    if (!task) {
      promptInput.focus();
      throw new Error('请先输入任务');
    }
    if (!aiConfig?.configured) await loadAiConfig();
    if (!aiConfig?.configured) throw new Error('AI 服务尚未配置');

    addMessage(task, 'user');
    setBusy(true);
    resetPending();
    setRunStatus('正在理解你的任务…');
    try {
      // Pure-number arithmetic is deterministic and does not need a language model to invent
      // an operation envelope. Route it straight to the sandboxed workbook engine so a valid
      // formula, the active-cell address, and the calculated result can never drift apart.
      const deterministicOps = normalizeTaskOps(task, []);
      if (deterministicOps.length) {
        setRunStatus('正在用工作簿公式引擎计算并生成安全预览…');
        const preview = await previewOps(deterministicOps, { announce: false });
        const safeMessage = operationPlanMessage(deterministicOps, preview);
        clearRunStatus();
        addMessage(safeMessage);
        rememberConversation(task, safeMessage);
        return {
          message: safeMessage,
          toolCalls: [],
          ops: deterministicOps,
          commandSuggestions: [],
          preview,
          deterministic: true,
        };
      }

      setRunStatus('正在组织工作簿摘要、选区和活动单元格上下文…');
      const context = await gatherModelContext();
      requestPreview.hidden = false;
      requestPreview.textContent = pretty(context, 30000);
      const messages = [
        { role: 'system', content: systemPrompt() },
        ...chatHistory,
        { role: 'user', content: `用户任务：\n${task}\n\n当前工作簿上下文：\n${JSON.stringify(context)}` },
      ];
      let toolRounds = 0;
      let repairRounds = 0;
      while (toolRounds <= MAX_TOOL_ROUNDS) {
        setRunStatus(toolRounds
          ? '正在根据新增的工作簿信息调整处理方案…'
          : '正在分析数据并生成可预览的处理方案…');
        const response = await callModel(messages);
        const envelope = parseAssistantEnvelope(response.content);
        if (envelope.toolCalls.length) {
          if (toolRounds >= MAX_TOOL_ROUNDS) throw new Error('模型读取轮次超过安全上限');
          toolRounds += 1;
          const results = [];
          for (const [index, call] of envelope.toolCalls.entries()) {
            const target = call.ref || call.sheet || '工作簿';
            setRunStatus(`正在读取 ${index + 1}/${envelope.toolCalls.length}：${call.tool} · ${target}…`);
            results.push(await executeReadTool(call));
          }
          messages.push({ role: 'assistant', content: response.content });
          messages.push({
            role: 'user',
            content: `读取工具结果（只据此继续，仍按规定 JSON 输出）：\n${JSON.stringify(results)}`,
          });
          continue;
        }

        renderCommandSuggestions(envelope.commandSuggestions);
        const controlledOps = normalizeTaskOps(task, envelope.ops);
        if (!controlledOps.length) {
          clearRunStatus();
          if (envelope.message) addMessage(envelope.message);
          rememberConversation(task, envelope.message);
          return envelope;
        }

        setRunStatus('正在校验公式、数据验证和修改范围…');
        const preview = await previewOps(controlledOps, { announce: false });
        const feedback = feedbackText(preview);
        if (feedback && repairRounds < MAX_REPAIR_ROUNDS) {
          repairRounds += 1;
          setRunStatus(`检测到问题，正在修正方案 ${repairRounds}/${MAX_REPAIR_ROUNDS}…`);
          messages.push({ role: 'assistant', content: response.content });
          messages.push({
            role: 'user',
            content: `这些操作尚未写入。dry-run 返回以下结构化问题，请修正并重新输出完整 JSON：\n${JSON.stringify(lastFeedback)}`,
          });
          continue;
        }
        const safeMessage = operationPlanMessage(controlledOps, preview);
        clearRunStatus();
        addMessage(safeMessage);
        rememberConversation(task, safeMessage);
        return { ...envelope, message: safeMessage, ops: controlledOps, preview };
      }
      throw new Error('模型工具循环没有在限制内完成');
    } catch (error) {
      clearRunStatus();
      if (error.name === 'AbortError') addMessage('已停止本次 U AI 请求。');
      else addMessage(error.message || String(error), 'error');
      throw error;
    } finally {
      clearRunStatus();
      activeRequest = null;
      setBusy(false);
    }
  }

  async function copyModelRequest() {
    const text = pretty(buildModelRequest(), Number.MAX_SAFE_INTEGER);
    try {
      await navigator.clipboard.writeText(text);
    } catch (_) {
      requestPreview.hidden = false;
      requestPreview.textContent = text;
      const range = document.createRange();
      range.selectNodeContents(requestPreview);
      const selection = getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      document.execCommand('copy');
      selection.removeAllRanges();
    }
    window.UniCellUI?.toast?.('模型请求包已复制');
  }

  function parseOps(text = opsInput.value) {
    let value;
    try {
      value = JSON.parse(text);
    } catch (error) {
      throw new Error(`操作 JSON 无效：${error.message}`);
    }
    if (!Array.isArray(value) || !value.length) throw new Error('操作必须是非空 JSON 数组');
    value.forEach((operation, index) => {
      if (!operation || typeof operation !== 'object' || Array.isArray(operation)) {
        throw new Error(`第 ${index + 1} 个操作必须是对象`);
      }
      const supported = ['setFormula', 'setValue', 'setRange', 'clear', 'setFormat', 'setBorder', 'updateChart', 'updatePivotTable'];
      if (!supported.includes(operation.op)) {
        throw new Error(`第 ${index + 1} 个操作不受支持：${operation.op || '缺少 op'}`);
      }
      const usesRef = ['setFormula', 'setValue', 'setRange', 'clear', 'setFormat', 'setBorder'].includes(operation.op);
      if (usesRef && (typeof operation.ref !== 'string' || !operation.ref.trim())) {
        throw new Error(`第 ${index + 1} 个操作缺少 A1 ref`);
      }
      if (operation.op === 'setFormula' && (typeof operation.formula !== 'string' || !operation.formula.startsWith('='))) {
        throw new Error(`第 ${index + 1} 个 setFormula 必须提供以 = 开头的 formula`);
      }
      if (operation.op === 'setValue' && !Object.hasOwn(operation, 'value')) {
        throw new Error(`第 ${index + 1} 个 setValue 缺少 value`);
      }
      if (operation.op === 'setRange' && (!Array.isArray(operation.values) || !operation.values.length
        || operation.values.some((row) => !Array.isArray(row)))) {
        throw new Error(`第 ${index + 1} 个 setRange 必须提供非空二维 values 数组`);
      }
      if (operation.op === 'setFormat' && (!operation.style || typeof operation.style !== 'object'
        || Array.isArray(operation.style) || !Object.keys(operation.style).length)) {
        throw new Error(`第 ${index + 1} 个 setFormat 必须提供非空 style 对象`);
      }
      if (operation.op === 'updateChart') {
        if (typeof operation.chartId !== 'string' || !operation.chartId.trim()) {
          throw new Error(`第 ${index + 1} 个 updateChart 缺少 chartId`);
        }
        if (!operation.patch || typeof operation.patch !== 'object' || Array.isArray(operation.patch)
          || !Object.keys(operation.patch).length) {
          throw new Error(`第 ${index + 1} 个 updateChart 必须提供非空 patch 对象`);
        }
      }
      if (operation.op === 'updatePivotTable') {
        if (typeof operation.part !== 'string' || !operation.part.trim()) {
          throw new Error(`第 ${index + 1} 个 updatePivotTable 缺少稳定 part`);
        }
        if (!operation.patch || typeof operation.patch !== 'object' || Array.isArray(operation.patch)
          || !Object.keys(operation.patch).length) {
          throw new Error(`第 ${index + 1} 个 updatePivotTable 必须提供非空 patch 对象`);
        }
      }
    });
    return value;
  }

  function resetPending() {
    pending = null;
    acceptButton.disabled = true;
    rejectButton.disabled = true;
  }

  function feedbackText(result) {
    const parts = [];
    if (result.errors?.length) {
      parts.push(`公式错误 ${result.errors.length} 个：\n${result.errors.map((item) => `${item.ref}: ${item.value || item.error || item.reason || 'error'}`).join('\n')}`);
    }
    if (result.validationViolations?.length) {
      parts.push(`数据验证违反 ${result.validationViolations.length} 个：\n${result.validationViolations.map((item) => `${item.ref}: ${item.reason || 'validation failed'}`).join('\n')}`);
    }
    return parts.join('\n\n');
  }

  function renderDiff(result) {
    previewCard.hidden = false;
    const changedObjects = result.changedObjects || 0;
    previewCount.textContent = `${result.changedCells || 0} 个单元格 + ${changedObjects} 个对象${result.diffTruncated ? '（已截断）' : ''}`;
    diffRoot.textContent = '';
    const changes = result.diff || [];
    const objectChanges = result.objectDiff || [];
    if (!changes.length && !objectChanges.length) {
      const empty = document.createElement('div');
      empty.className = 'ai-diff-empty';
      empty.textContent = '没有可见变化。';
      diffRoot.appendChild(empty);
    }
    if (changes.length) {
      const table = document.createElement('table');
      table.className = 'ai-diff-table';
      const head = document.createElement('thead');
      const headRow = document.createElement('tr');
      ['地址', '修改前', '修改后'].forEach((label) => {
        const cell = document.createElement('th');
        cell.textContent = label;
        headRow.appendChild(cell);
      });
      head.appendChild(headRow);
      const body = document.createElement('tbody');
      const snapshotText = (snapshot, other) => {
        const content = String(snapshot?.content ?? '');
        const formatted = String(snapshot?.formatted ?? '');
        const value = content.startsWith('=') && formatted && formatted !== content
          ? `${content}\n→ ${formatted}`
          : content || formatted;
        const style = JSON.stringify(snapshot?.style || {});
        const otherStyle = JSON.stringify(other?.style || {});
        return style !== otherStyle ? `${value ? `${value}\n` : ''}格式：${style}` : value;
      };
      changes.forEach((change) => {
        const row = document.createElement('tr');
        [change.ref, snapshotText(change.before, change.after), snapshotText(change.after, change.before)].forEach((value) => {
          const cell = document.createElement('td');
          cell.textContent = String(value);
          row.appendChild(cell);
        });
        body.appendChild(row);
      });
      table.append(head, body);
      diffRoot.appendChild(table);
    }
    if (objectChanges.length) {
      const title = document.createElement('h4');
      title.className = 'ai-diff-section-title';
      title.textContent = '图表 / 数据透视表对象差异';
      diffRoot.appendChild(title);
      const table = document.createElement('table');
      table.className = 'ai-diff-table ai-object-diff-table';
      const head = document.createElement('thead');
      const headRow = document.createElement('tr');
      ['对象', '修改前', '修改后'].forEach((label) => {
        const cell = document.createElement('th');
        cell.textContent = label;
        headRow.appendChild(cell);
      });
      head.appendChild(headRow);
      const body = document.createElement('tbody');
      objectChanges.forEach((change) => {
        const row = document.createElement('tr');
        [change.target, pretty(change.before?.model ?? change.before, Number.MAX_SAFE_INTEGER),
          pretty(change.after?.model ?? change.after, Number.MAX_SAFE_INTEGER)].forEach((value) => {
          const cell = document.createElement('td');
          cell.textContent = String(value);
          row.appendChild(cell);
        });
        body.appendChild(row);
      });
      table.append(head, body);
      diffRoot.appendChild(table);
    }
    lastFeedback = {
      errors: result.errors || [],
      validationViolations: result.validationViolations || [],
    };
    const feedback = feedbackText(result);
    feedbackRoot.hidden = !feedback;
    feedbackRoot.textContent = feedback;
    if (feedback) acceptButton.textContent = '仍要写入';
    else acceptButton.textContent = '确认写入';
  }

  async function previewOps(override, options = {}) {
    if (override) opsInput.value = JSON.stringify(override, null, 2);
    let operations;
    try {
      operations = parseOps();
    } catch (error) {
      addMessage(error.message || String(error), 'error');
      throw error;
    }
    resetPending();
    const result = await api('/api/ai/apply', {
      method: 'POST',
      body: JSON.stringify({ dryRun: true, sheet: S.sheet, ops: operations }),
    });
    pending = { ops: operations, preview: result };
    renderDiff(result);
    acceptButton.disabled = false;
    rejectButton.disabled = false;
    if (options.announce !== false) {
      addMessage(`预览完成：${result.changedCells || 0} 个单元格、${result.changedObjects || 0} 个对象会变化。${feedbackText(result) ? '检测到结构化错误反馈，请检查后再确认。' : '未检测到公式错误或验证冲突。'}`);
    }
    return result;
  }

  async function applyPending() {
    if (!pending) throw new Error('没有待确认的预览');
    const operations = pending.ops;
    acceptButton.disabled = true;
    rejectButton.disabled = true;
    try {
      const result = await apiPost('/api/ai/apply', {
        dryRun: false,
        sheet: S.sheet,
        ops: operations,
      });
      renderDiff(result);
      pending = null;
      scheduleRefresh(true);
      updateDimension();
      window.updateHistoryState?.();
      setStatus(`U AI 已应用 ${result.changedCells || 0} 个单元格、${result.changedObjects || 0} 个对象（一步撤销）`);
      window.UniCellUI?.toast?.('U AI 改动已写入，可用 Ctrl+Z 一步撤销');
      addMessage(appliedOperationsMessage(operations, result));
      return result;
    } catch (error) {
      acceptButton.disabled = false;
      rejectButton.disabled = false;
      addMessage(`写入失败：${error.message || error}`, 'error');
      throw error;
    }
  }

  function rejectPending() {
    resetPending();
    previewCard.hidden = true;
    addMessage('已拒绝预览，没有写入工作簿。');
  }

  function insertTemplate(kind) {
    const reference = activeCellRef();
    const template = kind === 'setFormula'
      ? [{ op: 'setFormula', ref: reference, formula: '=SUM(A1:A10)' }]
      : kind === 'setValue'
        ? [{ op: 'setValue', ref: reference, value: '文本' }]
        : [{ op: 'clear', ref: reference }];
    opsInput.value = JSON.stringify(template, null, 2);
    resetPending();
    previewCard.hidden = true;
    opsInput.focus();
  }

  function resizePrompt() {
    promptInput.style.height = 'auto';
    promptInput.style.height = `${Math.min(160, Math.max(28, promptInput.scrollHeight))}px`;
  }

  async function open(initialPrompt = '') {
    returnFocus = document.activeElement;
    panel.hidden = false;
    launcher?.setAttribute('aria-expanded', 'true');
    syncSelectionLabel();
    renderTools();
    if (initialPrompt) {
      promptInput.value = initialPrompt;
    }
    resizePrompt();
    promptInput.focus();
    const [context] = await Promise.all([
      loadDigest().catch(() => null),
      loadAiConfig(),
    ]);
    return context;
  }

  function close() {
    panel.hidden = true;
    launcher?.setAttribute('aria-expanded', 'false');
    if (returnFocus instanceof HTMLElement && returnFocus.isConnected) returnFocus.focus();
    returnFocus = null;
  }

  $ai('ai-close').addEventListener('click', close);
  fullscreenButton?.addEventListener('click', () => toggleFullscreen());
  panel.querySelector('.ai-head')?.addEventListener('dblclick', (event) => {
    if (!event.target.closest('button')) toggleFullscreen();
  });
  if (resizer) {
    resizer.addEventListener('pointerdown', (event) => {
      if (event.button !== 0 || panel.classList.contains('ai-full')) return;
      event.preventDefault();
      document.body.classList.add('ai-resizing');
      const move = (nextEvent) => setPanelWidth(window.innerWidth - nextEvent.clientX, false);
      const end = () => {
        document.body.classList.remove('ai-resizing');
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', end);
        window.removeEventListener('pointercancel', end);
        setPanelWidth(panel.getBoundingClientRect().width, true);
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', end);
      window.addEventListener('pointercancel', end);
    });
    resizer.addEventListener('keydown', (event) => {
      if (!['ArrowLeft', 'ArrowRight'].includes(event.key) || panel.classList.contains('ai-full')) return;
      event.preventDefault();
      const step = event.shiftKey ? 96 : 32;
      const direction = event.key === 'ArrowLeft' ? 1 : -1;
      setPanelWidth(panel.getBoundingClientRect().width + direction * step);
    });
  }
  window.addEventListener('resize', () => {
    if (!panel.classList.contains('ai-full')) setPanelWidth(panel.getBoundingClientRect().width, false);
  });
  panel.querySelector('.ai-suggestions')?.addEventListener('click', (event) => {
    const suggestion = event.target.closest('[data-ai-prompt]');
    if (!suggestion) return;
    promptInput.value = suggestion.dataset.aiPrompt || '';
    resizePrompt();
    promptInput.focus();
    promptInput.setSelectionRange(promptInput.value.length, promptInput.value.length);
  });
  launcher?.addEventListener('click', () => open().catch((error) => {
    window.UniCellUI?.toast?.(`U AI 打开失败：${error.message || error}`, { tone: 'error' });
  }));
  $ai('ai-refresh-context').addEventListener('click', () => loadDigest());
  $ai('ai-read-selection').addEventListener('click', () => readContext('slice', selectionRef()));
  $ai('ai-read-detail').addEventListener('click', () => readContext('detail', activeCellRef()));
  sendButton.addEventListener('click', () => runAssistant().catch(() => undefined));
  stopButton.addEventListener('click', () => activeRequest?.abort());
  $ai('ai-copy-request').addEventListener('click', copyModelRequest);
  $ai('ai-template-formula').addEventListener('click', () => insertTemplate('setFormula'));
  $ai('ai-template-value').addEventListener('click', () => insertTemplate('setValue'));
  $ai('ai-template-clear').addEventListener('click', () => insertTemplate('clear'));
  $ai('ai-preview').addEventListener('click', () => previewOps().catch(() => undefined));
  acceptButton.addEventListener('click', () => applyPending().catch(() => undefined));
  rejectButton.addEventListener('click', rejectPending);
  opsInput.addEventListener('input', () => {
    resetPending();
    previewCard.hidden = true;
  });
  promptInput.addEventListener('input', resizePrompt);
  promptInput.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && !event.shiftKey && !busy) {
      event.preventDefault();
      runAssistant().catch(() => undefined);
    }
  });
  document.addEventListener('pointerup', () => { if (!panel.hidden) setTimeout(syncSelectionLabel, 0); });
  document.addEventListener('keyup', () => { if (!panel.hidden) syncSelectionLabel(); });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && !panel.hidden && $ai('cmdk')?.hidden) {
      if (panel.classList.contains('ai-full')) toggleFullscreen(false);
      else close();
    }
  });

  restorePanelLayout();

  const contract = {
    open,
    close,
    loadDigest,
    loadAiConfig,
    readContext,
    buildModelRequest,
    parseAssistantEnvelope,
    simpleArithmeticFormula,
    normalizeTaskOps,
    operationPlanMessage,
    appliedOperationsMessage,
    setRunStatus,
    clearRunStatus,
    executeReadTool,
    runAssistant,
    parseOps,
    renderDiff,
    previewOps,
    applyPending,
    rejectPending,
    setWidth: setPanelWidth,
    toggleFullscreen,
    selectionRef,
    activeCellRef,
    getState: () => ({
      digest,
      aiConfig,
      lastFeedback,
      pending,
      busy,
      chatHistory: [...chatHistory],
      open: !panel.hidden,
      fullscreen: panel.classList.contains('ai-full'),
      width: panel.getBoundingClientRect().width,
      advancedOpen: false,
    }),
  };
  window.UniCellAI = contract;
  window.__unicellAiTest = contract;
})();
