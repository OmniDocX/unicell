/* UniCell UI Kit —— 主题切换 / Toast / Tooltip / 命令面板 / 快捷键总览 / 无障碍
 *
 * 这一层完全建立在既有 DOM 之上：命令面板直接复用功能区按钮本身（点击它们），
 * 因此新增功能区命令后无需在这里登记，面板会自动收录。
 *
 * 必须最后载入：它会包装 app.js 的 setStatus，并读取 functions.js 的 FN_LIST。
 */
(function () {
  'use strict';

  const $ = (id) => document.getElementById(id);
  const html = document.documentElement;

  /* app.js / functions.js 里的 `const S` 和 `const FN_LIST` 是脚本级词法绑定，
     不会挂到 window 上，只能用裸标识符读取。 */
  const appState = () => (typeof S !== 'undefined' ? S : null);
  const functionList = () => (typeof FN_LIST !== 'undefined' && Array.isArray(FN_LIST) ? FN_LIST : []);

  /* ================= 主题 ================= */
  const THEME_KEY = 'unicell-theme';
  function currentTheme() {
    return html.dataset.theme === 'dark' ? 'dark' : 'light';
  }
  function applyTheme(theme, persist = true) {
    const next = theme === 'dark' ? 'dark' : 'light';
    html.dataset.theme = next;
    if (persist) { try { localStorage.setItem(THEME_KEY, next); } catch (e) { /* 隐私模式忽略 */ } }
    const btn = $('btn-theme');
    if (btn) {
      // 图标显示"点击后会切到的主题"，与 Google 的行为一致
      btn.dataset.ic = next === 'dark' ? 'light' : 'dark';
      btn.setAttribute('aria-pressed', String(next === 'dark'));
      btn.title = next === 'dark' ? '切换为浅色主题' : '切换为深色主题';
    }
  }
  function toggleTheme() {
    const next = currentTheme() === 'dark' ? 'light' : 'dark';
    applyTheme(next);
    toast(next === 'dark' ? '已切换到深色主题' : '已切换到浅色主题');
  }

  /* ================= Toast ================= */
  const toastHost = $('uk-toasts');
  function toast(message, options = {}) {
    if (!toastHost || !message) return null;
    const { tone = 'default', duration = 4000, actionLabel, onAction } = options;
    const el = document.createElement('div');
    el.className = 'uk-toast';
    el.dataset.tone = tone;
    const text = document.createElement('span');
    text.textContent = message;
    el.appendChild(text);
    if (actionLabel) {
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.textContent = actionLabel;
      btn.onclick = () => { dismiss(); onAction?.(); };
      el.appendChild(btn);
    }
    toastHost.appendChild(el);
    // 同时最多留 3 条，避免批量操作把屏幕刷满
    while (toastHost.children.length > 3) toastHost.firstElementChild.remove();
    let timer = duration > 0 ? setTimeout(dismiss, duration) : 0;
    function dismiss() {
      clearTimeout(timer);
      if (!el.isConnected || el.classList.contains('leaving')) return;
      el.classList.add('leaving');
      setTimeout(() => el.remove(), 160);
    }
    el.addEventListener('mouseenter', () => clearTimeout(timer));
    el.addEventListener('mouseleave', () => { if (duration > 0) timer = setTimeout(dismiss, 1200); });
    return dismiss;
  }

  /* 把状态栏里"值得知道"的消息升级成 Toast，其余仍然只写状态栏。 */
  const TOAST_ERROR = /^(错误|警告|.*失败)/;
  const TOAST_NOTICE = /^(已保存|已另存为|已下载|已打开|已导出|已新建|已恢复|已导入|发现)/;
  function hookStatusBar() {
    const original = window.setStatus;
    if (typeof original !== 'function' || original.__ukWrapped) return;
    const wrapped = function (message) {
      original(message);
      const text = String(message ?? '');
      if (TOAST_ERROR.test(text)) toast(text, { tone: 'error', duration: 7000 });
      else if (TOAST_NOTICE.test(text)) toast(text, { tone: 'success', duration: 3200 });
    };
    wrapped.__ukWrapped = true;
    window.setStatus = wrapped;
  }

  /* ================= Tooltip ================= */
  /* 悬停时临时摘掉 title 以屏蔽系统气泡，移开再放回去，
     这样动态生成的对话框也能自动享受深色气泡，无需初始化扫描。 */
  let tipEl = null;
  let tipTimer = 0;
  let tipOwner = null;

  function tipTarget(node) {
    if (!(node instanceof Element)) return null;
    const el = node.closest('[title], [data-uk-title]');
    if (!el) return null;
    const text = el.getAttribute('title') || el.getAttribute('data-uk-title');
    return text && text.trim() ? el : null;
  }
  function showTip(el) {
    const text = (el.getAttribute('title') || el.getAttribute('data-uk-title') || '').trim();
    if (!text) return;
    if (el.hasAttribute('title')) {
      el.setAttribute('data-uk-title', text);
      el.removeAttribute('title');
    }
    if (!tipEl) {
      tipEl = document.createElement('div');
      tipEl.className = 'uk-tip';
      tipEl.setAttribute('role', 'tooltip');
      document.body.appendChild(tipEl);
    }
    tipEl.textContent = text;
    tipEl.style.left = '0px';
    tipEl.style.top = '0px';
    const anchor = el.getBoundingClientRect();
    const box = tipEl.getBoundingClientRect();
    let left = anchor.left + anchor.width / 2 - box.width / 2;
    let top = anchor.bottom + 8;
    if (top + box.height > innerHeight - 8) top = anchor.top - box.height - 8;
    left = Math.max(8, Math.min(left, innerWidth - box.width - 8));
    tipEl.style.left = `${Math.round(left)}px`;
    tipEl.style.top = `${Math.round(Math.max(8, top))}px`;
    tipEl.classList.add('show');
    tipOwner = el;
  }
  function hideTip() {
    clearTimeout(tipTimer);
    if (tipOwner && tipOwner.hasAttribute('data-uk-title')) {
      tipOwner.setAttribute('title', tipOwner.getAttribute('data-uk-title'));
      tipOwner.removeAttribute('data-uk-title');
    }
    tipOwner = null;
    tipEl?.classList.remove('show');
  }
  document.addEventListener('pointerover', (e) => {
    const el = tipTarget(e.target);
    if (!el || el === tipOwner) return;
    hideTip();
    clearTimeout(tipTimer);
    tipTimer = setTimeout(() => showTip(el), 450);
  });
  document.addEventListener('pointerout', (e) => {
    if (tipOwner && e.relatedTarget instanceof Node && tipOwner.contains(e.relatedTarget)) return;
    hideTip();
  });
  document.addEventListener('pointerdown', hideTip, true);
  addEventListener('blur', hideTip);
  addEventListener('scroll', hideTip, true);

  /* ================= 命令索引 ================= */
  const TAB_NAMES = { file: '文件', home: '开始', insert: '插入', formula: '公式', data: '数据', view: '视图' };

  function labelOf(el) {
    const tx = el.querySelector?.('.rb-tx');
    if (tx?.textContent.trim()) return tx.textContent.trim();
    const aria = el.getAttribute('aria-label');
    if (aria?.trim()) return aria.trim();
    const text = (el.textContent || '').trim();
    if (text) return text;
    return (el.getAttribute('title') || el.getAttribute('data-uk-title') || '').split(/[（(]/)[0].trim();
  }
  function hintOf(el) {
    const title = el.getAttribute('title') || el.getAttribute('data-uk-title') || '';
    const shortcut = title.match(/\(([^)]*(?:Ctrl|Alt|Shift|F\d)[^)]*)\)/i);
    return shortcut ? shortcut[1] : '';
  }

  function collectCommands() {
    const items = [];
    for (const panel of document.querySelectorAll('#ribbon .rb-panel')) {
      const tab = TAB_NAMES[panel.dataset.tab] || panel.dataset.tab || '';
      for (const group of panel.querySelectorAll('.rb-group')) {
        const where = group.querySelector('.rb-glabel')?.textContent.trim() || '';
        for (const btn of group.querySelectorAll('button')) {
          if (btn.hidden || btn.closest('[hidden]')) continue;
          const label = labelOf(btn);
          if (!label) continue;
          items.push({
            id: btn.id || `ribbon:${panel.dataset.tab || 'unknown'}:${label}`,
            kind: 'command',
            label,
            where: where ? `${tab} · ${where}` : tab,
            keys: hintOf(btn),
            icon: btn.dataset.ic || btn.querySelector('.rb-ic')?.dataset.ic || '',
            run: () => btn.click(),
          });
        }
      }
    }
    for (const [id, label, icon] of [
      ['btn-ai-assistant', '打开 U AI 工作簿助手', 'sparkle'],
      ['btn-account', 'OmniDoc 账号登录与退出', ''],
      ['btn-theme', '切换深色 / 浅色主题', 'dark'],
      ['btn-shortcuts', '键盘快捷键', 'keyboard'],
    ]) {
      if ($(id)) items.push({ id, kind: 'command', label, where: '外观', keys: '', icon, run: () => $(id).click() });
    }
    return items;
  }

  function collectSheets() {
    return [...document.querySelectorAll('#sheet-tabs .sheet-tab')].map((tab, index) => ({
      kind: 'sheet',
      label: tab.textContent.trim() || `工作表${index + 1}`,
      where: '切换到工作表',
      keys: '',
      icon: 'sheet',
      run: () => tab.click(),
    }));
  }

  function collectFunctions() {
    return functionList().map((name) => ({
      kind: 'function',
      label: name,
      where: '插入函数',
      keys: '',
      icon: 'fx',
      run: () => insertFunction(name),
    }));
  }

  // The palette and U AI intentionally share one catalog. U AI receives stable ids and a
  // no-argument schema, never DOM callbacks; execution happens only after a user clicks the
  // corresponding suggestion button in the assistant panel.
  function publicCommandCatalog() {
    return collectCommands().map(({ run, ...item }) => ({
      ...item,
      parameters: { type: 'object', additionalProperties: false },
      requiresUserGesture: true,
    }));
  }

  function runRegisteredCommand(id) {
    const command = collectCommands().find((item) => item.id === id);
    if (!command) return false;
    command.run();
    return true;
  }

  /* 把 =FUNC( 写进当前单元格并保持在编辑态，让用户接着填参数。 */
  function insertFunction(name) {
    const editor = $('cell-editor');
    const state = appState();
    if (typeof window.startEdit !== 'function' || !editor || !state) return;
    if (state.editing) editor.value += `${name}(`;
    else window.startEdit(`=${name}(`, false);
    if (!state.editing) return;
    editor.focus();
    const end = editor.value.length;
    editor.setSelectionRange(end, end);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
  }

  /* ================= 模糊匹配 ================= */
  /* 连续子串优先，其次按顺序的子序列匹配（打 "tjgs" 也能找到"条件格式"的拼音场景之外，
     至少保证 "格式" 能命中"条件格式"、"清除格式"）。 */
  function match(text, query) {
    if (!query) return { score: 0, ranges: [] };
    const haystack = text.toLowerCase();
    const needle = query.toLowerCase();
    const direct = haystack.indexOf(needle);
    if (direct >= 0) {
      return { score: 1000 - direct * 4 - (text.length - needle.length), ranges: [[direct, direct + needle.length]] };
    }
    const ranges = [];
    let cursor = 0;
    let gaps = 0;
    for (const ch of needle) {
      const found = haystack.indexOf(ch, cursor);
      if (found < 0) return null;
      gaps += found - cursor;
      const last = ranges[ranges.length - 1];
      if (last && last[1] === found) last[1] = found + 1;
      else ranges.push([found, found + 1]);
      cursor = found + 1;
    }
    return { score: 400 - gaps * 3 - text.length, ranges };
  }
  function highlight(text, ranges) {
    if (!ranges.length) return document.createTextNode(text);
    const frag = document.createDocumentFragment();
    let at = 0;
    for (const [start, end] of ranges) {
      if (start > at) frag.appendChild(document.createTextNode(text.slice(at, start)));
      const mark = document.createElement('mark');
      mark.textContent = text.slice(start, end);
      frag.appendChild(mark);
      at = end;
    }
    if (at < text.length) frag.appendChild(document.createTextNode(text.slice(at)));
    return frag;
  }

  /* ================= 命令面板 ================= */
  const cmdk = $('cmdk');
  const cmdkInput = $('cmdk-input');
  const cmdkList = $('cmdk-list');
  let pool = [];
  let results = [];
  let active = 0;
  let restoreFocus = null;

  function openPalette() {
    if (!cmdk) return;
    restoreFocus = document.activeElement;
    pool = [...collectCommands(), ...collectSheets(), ...collectFunctions()];
    cmdk.hidden = false;
    cmdkInput.value = '';
    render('');
    cmdkInput.focus();
  }
  function closePalette() {
    if (!cmdk || cmdk.hidden) return;
    cmdk.hidden = true;
    if (restoreFocus instanceof HTMLElement && restoreFocus.isConnected) restoreFocus.focus();
    restoreFocus = null;
  }

  function render(query) {
    const trimmed = query.trim();
    if (!trimmed) {
      // 空查询显示最常用的一组，而不是几百条函数
      results = pool.filter((item) => item.kind !== 'function').slice(0, 40).map((item) => ({ item, ranges: [] }));
    } else {
      const scored = [];
      for (const item of pool) {
        const hit = match(item.label, trimmed);
        if (!hit) continue;
        // 命令排在函数前面：同名时用户多半想执行命令
        const bonus = item.kind === 'command' ? 120 : item.kind === 'sheet' ? 60 : 0;
        scored.push({ item, ranges: hit.ranges, score: hit.score + bonus });
      }
      scored.sort((a, b) => b.score - a.score);
      results = scored.slice(0, 60);
      if (!results.length) {
        results = [{
          item: {
            kind: 'ai',
            label: `问 U AI：${trimmed}`,
            where: 'AI 助手',
            keys: '',
            icon: 'sparkle',
            run: () => {
              if (window.UniCellAI?.open) window.UniCellAI.open(trimmed);
              else toast('U AI 尚未完成加载，请稍后再试', { tone: 'error' });
            },
          },
          ranges: [],
        }];
      }
    }
    active = 0;
    paint();
  }

  function paint() {
    cmdkList.textContent = '';
    if (!results.length) {
      const empty = document.createElement('div');
      empty.className = 'cmdk-empty';
      empty.textContent = '没有匹配的命令或函数';
      cmdkList.appendChild(empty);
      return;
    }
    let lastKind = null;
    results.forEach((entry, index) => {
      const { item, ranges } = entry;
      if (item.kind !== lastKind) {
        lastKind = item.kind;
        const head = document.createElement('div');
        head.className = 'cmdk-section';
        head.textContent = { command: '命令', sheet: '工作表', function: '工作表函数' }[item.kind] || '';
        if (item.kind === 'ai') head.textContent = 'U AI';
        cmdkList.appendChild(head);
      }
      const row = document.createElement('button');
      row.type = 'button';
      row.className = 'cmdk-item';
      row.setAttribute('role', 'option');
      row.setAttribute('aria-selected', String(index === active));
      if (item.icon) row.dataset.ic = item.icon;
      const label = document.createElement('span');
      label.className = 'cmdk-label';
      label.appendChild(highlight(item.label, ranges));
      row.appendChild(label);
      if (item.where) {
        const where = document.createElement('span');
        where.className = 'cmdk-where';
        where.textContent = item.where;
        row.appendChild(where);
      }
      if (item.keys) {
        const keys = document.createElement('span');
        keys.className = 'cmdk-keys';
        for (const part of item.keys.split('+')) {
          const kbd = document.createElement('kbd');
          kbd.textContent = part.trim();
          keys.appendChild(kbd);
        }
        row.appendChild(keys);
      }
      row.onclick = () => execute(index);
      cmdkList.appendChild(row);
    });
    scrollActiveIntoView();
  }

  function rowAt(index) {
    return cmdkList.querySelectorAll('.cmdk-item')[index] || null;
  }
  function setActive(index) {
    if (!results.length) return;
    rowAt(active)?.setAttribute('aria-selected', 'false');
    active = (index + results.length) % results.length;
    rowAt(active)?.setAttribute('aria-selected', 'true');
    scrollActiveIntoView();
  }
  function scrollActiveIntoView() {
    rowAt(active)?.scrollIntoView({ block: 'nearest' });
  }
  function execute(index) {
    const entry = results[index];
    if (!entry) return;
    closePalette();
    // 等面板真正关闭再执行，命令自己弹出的对话框才能正常拿到焦点
    setTimeout(() => { try { entry.item.run(); } catch (error) { toast(`执行失败：${error.message || error}`, { tone: 'error' }); } }, 0);
  }

  cmdkInput?.addEventListener('input', () => render(cmdkInput.value));
  cmdkInput?.addEventListener('keydown', (e) => {
    if (e.key === 'ArrowDown') { e.preventDefault(); setActive(active + 1); }
    else if (e.key === 'ArrowUp') { e.preventDefault(); setActive(active - 1); }
    else if (e.key === 'Home') { e.preventDefault(); setActive(0); }
    else if (e.key === 'End') { e.preventDefault(); setActive(results.length - 1); }
    else if (e.key === 'Enter') { e.preventDefault(); execute(active); }
  });
  cmdk?.addEventListener('pointerdown', (e) => { if (e.target === cmdk) closePalette(); });

  /* ================= 快捷键总览 ================= */
  const SHORTCUTS = [
    ['编辑', [
      ['撤销', ['Ctrl', 'Z']], ['重做', ['Ctrl', 'Y']],
      ['复制', ['Ctrl', 'C']], ['剪切', ['Ctrl', 'X']], ['粘贴', ['Ctrl', 'V']],
      ['选择性粘贴', ['Ctrl', 'Alt', 'V']],
      ['编辑当前单元格', ['F2']], ['把内容填入整个选区', ['Ctrl', 'Enter']],
      ['取消编辑 / 取消剪切 / 退出格式刷', ['Esc']],
    ]],
    ['格式', [
      ['加粗', ['Ctrl', 'B']], ['倾斜', ['Ctrl', 'I']], ['下划线', ['Ctrl', 'U']],
    ]],
    ['公式与计算', [
      ['自动求和', ['Alt', '=']], ['重新计算', ['F9']],
    ]],
    ['文件', [
      ['保存', ['Ctrl', 'S']], ['另存为', ['Ctrl', 'Shift', 'S']], ['打印 / 导出 PDF', ['Ctrl', 'P']],
    ]],
    ['导航与查找', [
      ['查找 / 替换', ['Ctrl', 'F']], ['跳到区域边缘', ['Ctrl', '方向键']],
      ['选择到区域边缘', ['Ctrl', 'Shift', '方向键']],
    ]],
    ['视图', [
      ['放大 / 缩小', ['Ctrl', '滚轮']], ['恢复 100%', ['Ctrl', '0']],
      ['搜索命令与函数', ['Ctrl', '/']], ['本对话框', ['Ctrl', 'Shift', '/']],
    ]],
  ];

  function buildShortcuts() {
    const body = $('shortcuts-body');
    if (!body || body.childElementCount) return;
    for (const [title, rows] of SHORTCUTS) {
      const group = document.createElement('section');
      group.className = 'sc-group';
      const heading = document.createElement('h3');
      heading.textContent = title;
      group.appendChild(heading);
      for (const [label, keys] of rows) {
        const row = document.createElement('div');
        row.className = 'sc-row';
        const name = document.createElement('span');
        name.textContent = label;
        const combo = document.createElement('span');
        combo.className = 'sc-keys';
        keys.forEach((key, index) => {
          if (index) combo.appendChild(document.createTextNode('+'));
          const kbd = document.createElement('kbd');
          kbd.textContent = key;
          combo.appendChild(kbd);
        });
        row.append(name, combo);
        group.appendChild(row);
      }
      body.appendChild(group);
    }
  }

  /* ================= 通用对话框行为 ================= */
  function openDialog(el) {
    if (!el) return;
    el.__ukReturn = document.activeElement;
    el.hidden = false;
    // 焦点落在卡片本身而不是第一个按钮：既让 Tab 从头开始、屏幕阅读器读到标题，
    // 又不会在关闭按钮上留下一圈焦点环。
    const card = el.querySelector('.uk-dialog-card') || el;
    card.tabIndex = -1;
    card.focus();
  }
  function closeDialog(el) {
    if (!el || el.hidden) return;
    el.hidden = true;
    const back = el.__ukReturn;
    if (back instanceof HTMLElement && back.isConnected) back.focus();
    el.__ukReturn = null;
  }
  document.addEventListener('click', (e) => {
    const close = e.target.closest?.('[data-uk-close]');
    if (close) closeDialog(close.closest('.uk-dialog'));
  });
  document.addEventListener('pointerdown', (e) => {
    if (e.target instanceof Element && e.target.classList.contains('uk-dialog')) closeDialog(e.target);
  });

  /* 焦点圈在打开的浮层内，Tab 不会跑到底下的表格里。 */
  document.addEventListener('keydown', (e) => {
    if (e.key !== 'Tab') return;
    const layer = [...document.querySelectorAll('.uk-dialog:not([hidden]), .cmdk:not([hidden])')].pop();
    if (!layer) return;
    const focusable = [...layer.querySelectorAll('button, [href], input:not([type="hidden"]), select, textarea, [tabindex]:not([tabindex="-1"])')]
      .filter((el) => !el.disabled && el.offsetParent !== null);
    if (!focusable.length) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (e.shiftKey && document.activeElement === first) { e.preventDefault(); last.focus(); }
    else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
  });

  /* ================= 全局键位 ================= */
  document.addEventListener('keydown', (e) => {
    const ctrl = e.ctrlKey || e.metaKey;
    if (ctrl && e.key === '/' && !e.shiftKey) {
      e.preventDefault();
      if (cmdk && !cmdk.hidden) closePalette(); else openPalette();
      return;
    }
    if (ctrl && e.shiftKey && (e.key === '/' || e.key === '?')) {
      e.preventDefault();
      $('btn-shortcuts')?.click();
      return;
    }
    if (e.key === 'Escape') {
      if (cmdk && !cmdk.hidden) { e.preventDefault(); closePalette(); return; }
      const layer = [...document.querySelectorAll('.uk-dialog:not([hidden])')].pop();
      if (layer) { e.preventDefault(); closeDialog(layer); }
    }
  }, true);

  /* ================= 无障碍：状态镜像 ================= */
  /* app.js 用 .on / .active 表达开关态，这里把它同步到 aria-pressed，
     屏幕阅读器与自动化测试才能读到真实状态。 */
  function syncPressed(el) {
    if (!el.hasAttribute('aria-pressed')) return;
    el.setAttribute('aria-pressed', String(el.classList.contains('on') || el.classList.contains('active')));
  }
  function syncTabs() {
    for (const tab of document.querySelectorAll('#ribbon .rb-tab')) {
      tab.setAttribute('aria-selected', String(tab.classList.contains('active')));
      tab.tabIndex = tab.classList.contains('active') ? 0 : -1;
    }
  }
  const classObserver = new MutationObserver((records) => {
    for (const record of records) {
      const el = record.target;
      if (!(el instanceof HTMLElement)) continue;
      if (el.classList.contains('rb-tab')) syncTabs();
      else syncPressed(el);
    }
  });

  /* 工作表标签由 app.js 用 div 生成，这里补上 tab 语义与左右键导航，
     让它和功能区选项卡一样可以纯键盘操作。 */
  function decorateSheetTabs() {
    const tabs = [...document.querySelectorAll('#sheet-tabs .sheet-tab')];
    for (const tab of tabs) {
      const active = tab.classList.contains('active');
      tab.setAttribute('role', 'tab');
      tab.setAttribute('aria-selected', String(active));
      tab.tabIndex = active ? 0 : -1;
      if (tab.dataset.ukWired) continue;
      tab.dataset.ukWired = '1';
      tab.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); tab.click(); return; }
        const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
        if (!step) return;
        e.preventDefault();
        const all = [...document.querySelectorAll('#sheet-tabs .sheet-tab')];
        const next = all[(all.indexOf(tab) + step + all.length) % all.length];
        next.click();
        next.focus();
      });
    }
  }

  /* 窗口太窄（或浏览器放大）时功能区放不下，改为横向滚动。
     这里给两端加渐隐提示，并让滚轮在功能区上直接横向滚，省得去找滚动条。 */
  function wireRibbonOverflow() {
    const panels = document.querySelector('.rb-panels');
    if (!panels) return;
    const sync = () => {
      const panel = panels.querySelector('.rb-panel.active');
      if (!panel) return;
      const max = panel.scrollWidth - panel.clientWidth;
      panels.classList.toggle('overflow-start', panel.scrollLeft > 1);
      panels.classList.toggle('overflow-end', max > 1 && panel.scrollLeft < max - 1);
    };
    panels.addEventListener('scroll', sync, true);
    panels.addEventListener('wheel', (e) => {
      const panel = panels.querySelector('.rb-panel.active');
      if (!panel || panel.scrollWidth <= panel.clientWidth || e.ctrlKey) return;
      const delta = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      if (!delta) return;
      e.preventDefault();
      panel.scrollLeft += delta;
    }, { passive: false });
    addEventListener('resize', sync);
    new MutationObserver(sync).observe(panels, {
      subtree: true, attributes: true, attributeFilter: ['class'],
    });
    sync();
  }

  /* 功能区选项卡：左右方向键切换，符合 WAI-ARIA tablist 惯例 */
  function wireTabKeys() {
    const tabs = [...document.querySelectorAll('#ribbon .rb-tab')];
    for (const tab of tabs) {
      tab.addEventListener('keydown', (e) => {
        const step = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
        if (!step) return;
        e.preventDefault();
        const next = tabs[(tabs.indexOf(tab) + step + tabs.length) % tabs.length];
        next.click();
        next.focus();
      });
    }
  }

  /* ================= 工作簿标题 ================= */
  /* document.title 由 app.js 维护；把它同步到顶栏，省得再改 app.js 的多处调用点。 */
  function syncWorkbookTitle() {
    const el = $('workbook-title');
    if (!el) return;
    const name = (document.title || '').split(' — ')[0].trim();
    if (name && !name.startsWith('TESTS ')) el.textContent = name;
  }

  /* ================= 启动 ================= */
  function init() {
    applyTheme(currentTheme(), false);
    hookStatusBar();
    buildShortcuts();
    wireTabKeys();
    wireRibbonOverflow();
    syncTabs();

    $('btn-theme')?.addEventListener('click', toggleTheme);
    $('btn-command-palette')?.addEventListener('click', openPalette);
    $('btn-shortcuts')?.addEventListener('click', () => {
      buildShortcuts();
      openDialog($('shortcuts-dialog'));
    });

    for (const el of document.querySelectorAll('[aria-pressed]')) syncPressed(el);
    classObserver.observe(document.getElementById('ribbon') || document.body, {
      subtree: true, attributes: true, attributeFilter: ['class'],
    });

    decorateSheetTabs();
    const sheetTabs = $('sheet-tabs');
    if (sheetTabs) {
      new MutationObserver(decorateSheetTabs).observe(sheetTabs, {
        childList: true, subtree: true, attributes: true, attributeFilter: ['class'],
      });
    }

    syncWorkbookTitle();
    new MutationObserver(syncWorkbookTitle).observe(
      document.querySelector('title') || document.head, { childList: true },
    );
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', init);
  else init();

  window.UniCellUI = {
    toast,
    setTheme: applyTheme,
    toggleTheme,
    openPalette,
    closePalette,
    openDialog,
    closeDialog,
  };
  window.UniCellCommandRegistry = {
    list: publicCommandCatalog,
    run: runRegisteredCommand,
  };
})();
