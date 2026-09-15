// OpenCell 回归自测（仅 ?test=auto 时由 index.html 加载）
// 结果写入 document.title（TESTS n/m ALLPASS|HASFAIL）与 #test-results 面板
'use strict';

// Wait for the app's asynchronous first workbook/object/lifecycle load to settle before replacing
// it with the test workbook. On a cold Rust build or busy browser, 800 ms allowed the stale first
// load to race the /api/new reset and made otherwise unrelated grid/AI tests nondeterministic.
const bootSelfTest = () => setTimeout(runSelfTest, 1800);
if (document.readyState === 'complete') bootSelfTest();
else window.addEventListener('load', bootSelfTest, { once: true });

async function runSelfTest() {
  const out = [];
  const ok = (name, cond, detail) => out.push(`${cond ? 'PASS' : 'FAIL'} ${name}${cond ? '' : ' :: ' + detail}`);
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const waitUntil = async (predicate, timeout = 2000) => {
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      if (await predicate()) return true;
      await wait(50);
    }
    return !!(await predicate());
  };
  // Browser capture can fail transiently while an iframe/canvas is settling or
  // while Chromium is busy decoding the previous frame. Keep retries bounded:
  // callers still get a real exception after the last attempt, but one dropped
  // frame cannot abort every unrelated regression that follows it.
  const retryTransient = async (label, task, attempts = 3, delay = 120) => {
    let lastError = null;
    for (let attempt = 1; attempt <= attempts; attempt += 1) {
      try {
        return await task(attempt);
      } catch (error) {
        lastError = error;
        if (attempt < attempts) await wait(delay * attempt);
      }
    }
    const detail = lastError?.message || String(lastError || 'unknown failure');
    const failure = new Error(`${label} failed after ${attempts} attempts: ${detail}`);
    failure.cause = lastError;
    throw failure;
  };
  try {
    await api('/api/new', { method: 'POST' });
    await loadWorkbook();
    await wait(300);

    // T1 打字→Enter 提交，光标下移一格（防回归：Enter 冒泡双跳�?
    gridScroll.focus();
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '1', bubbles: true }));
    await wait(80);
    editor.value = '100';
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    // commitEdit 需要等待 Rust /api/input 返回后才移动光标；按状态等待，避免
    // release 首次热身或慢机器上固定 300ms 造成假失败。
    await waitUntil(() => S.cur.r === 2 && S.cur.c === 1);
    ok('T1 Enter光标下移一格', S.cur.r === 2 && S.cur.c === 1, `cur=${S.cur.r},${S.cur.c}`);

    // T2 公式计算
    await apiPost('/api/input', { sheet: S.sheet, row: 2, col: 1, value: '200' });
    await apiPost('/api/input', { sheet: S.sheet, row: 3, col: 1, value: '=SUM(A1:A2)' });
    const c3 = await api(`/api/cell?sheet=${S.sheet}&row=3&col=1`);
    ok('T2 SUM公式=300', c3.formatted === '300', c3.formatted);

    // T3 鼠标点击定位（坐标由真实几何计算，每次取最�?rect，不写死行高�?
    const cellXY = (r, c) => {
      const rect = gridScroll.getBoundingClientRect();
      return [rect.left + colX(c) + colWidth(c) / 2 - gridScroll.scrollLeft,
              rect.top + rowY(r) + rowHeight(r) / 2 - gridScroll.scrollTop];
    };
    const down = (x, y) => gridScroll.dispatchEvent(new MouseEvent('mousedown', { clientX: x, clientY: y, bubbles: true, cancelable: true }));
    const move = (x, y) => document.dispatchEvent(new MouseEvent('mousemove', { clientX: x, clientY: y, bubbles: true }));
    const up = () => document.dispatchEvent(new MouseEvent('mouseup', { bubbles: true }));
    const clickCell = (r, c) => { const [x, y] = cellXY(r, c); down(x, y); up(); };
    clickCell(1, 1);
    ok('T3 点击A1定位', S.cur.r === 1 && S.cur.c === 1, `cur=${S.cur.r},${S.cur.c} scrollTop=${gridScroll.scrollTop} defH=${S.defH}`);

    // T4 拖拽选区 + 状态栏统计（down/move/up 同步完成，避免真实鼠标事件插入）
    { const [x1, y1] = cellXY(1, 1); down(x1, y1); const [x3, y3] = cellXY(3, 1); move(x3, y3); up(); }
    const n4 = normSel();
    ok('T4a 拖拽选区A1:A3', n4.r0 === 1 && n4.r1 === 3 && n4.c0 === 1 && n4.c1 === 1, JSON.stringify(n4));
    await wait(400);
    const statsJ = await api(`/api/stats?sheet=${S.sheet}&r0=1&c0=1&r1=3&c1=1`);
    ok('T4b 统计求和600', statsJ.sum === 600 && statsJ.count === 3 && statsJ.numbers === 3, JSON.stringify(statsJ));

    // T5 公式栏回�?
    clickCell(3, 1);
    await wait(350);
    ok('T5 公式栏显示公式', formulaInput.value === '=SUM(A1:A2)', `"${formulaInput.value}" cur=${S.cur.r},${S.cur.c}`);

    // T6 加粗样式
    await styleSel('font.b', 'true');
    await wait(250);
    const c6 = await api(`/api/cell?sheet=${S.sheet}&row=3&col=1`);
    ok('T6 加粗样式', c6.style.b === true, JSON.stringify(c6.style));

    // T7 插行后公式引用追�?
    await apiPost('/api/rows', { sheet: S.sheet, op: 'insert', row: 2, count: 1 });
    const c7 = await api(`/api/cell?sheet=${S.sheet}&row=4&col=1`);
    ok('T7 插行后公式追随', c7.content === '=SUM(A1:A3)' && c7.formatted === '300', `${c7.content} = ${c7.formatted}`);

    // T8 撤销插行
    await api('/api/undo', { method: 'POST' });
    const c8 = await api(`/api/cell?sheet=${S.sheet}&row=3&col=1`);
    ok('T8 撤销插行', c8.formatted === '300', c8.formatted);

    // T9 Sheet 新建/重命�?删除
    await apiPost('/api/sheet', { op: 'new' });
    await apiPost('/api/sheet', { op: 'rename', sheet: 1, name: '数据表' });
    const j9 = await api('/api/info');
    ok('T9a Sheet新建+重命名', j9.sheets.length === 2 && j9.sheets[1] === '数据表', JSON.stringify(j9.sheets));
    await apiPost('/api/sheet', { op: 'delete', sheet: 1 });
    const j9b = await api('/api/info');
    ok('T9b Sheet删除', j9b.sheets.length === 1, JSON.stringify(j9b.sheets));

    // T10 内部复制→粘贴公式相对平�?
    const cp = await apiPost('/api/copy', { sheet: S.sheet, r0: 3, c0: 1, r1: 3, c1: 1 });
    await apiPost('/api/paste', { sheet: S.sheet, row: 3, col: 2, text: cp.tsv });
    const c10 = await api(`/api/cell?sheet=${S.sheet}&row=3&col=2`);
    ok('T10 粘贴公式平移', c10.content === '=SUM(B1:B2)', c10.content);

    // T11 数字格式
    await apiPost('/api/style', { sheet: S.sheet, r0: 1, c0: 1, r1: 1, c1: 1, path: 'num_fmt', value: '$#,##0.00' });
    const c11 = await api(`/api/cell?sheet=${S.sheet}&row=1&col=1`);
    ok('T11 货币格式', c11.formatted === '$100.00', c11.formatted);

    // T12 列宽持久�?
    await apiPost('/api/colwidth', { sheet: S.sheet, c0: 1, c1: 1, width: 160 });
    const v12 = await api(`/api/view?sheet=${S.sheet}&r0=1&c0=1&r1=1&c1=1`);
    ok('T12 列宽设置', Math.abs(v12.colWidths[0] - 160) < 1, v12.colWidths[0]);

    // T13 公式函数自动补全：输�?=SU 弹出候�?
    clickCell(6, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    editor.value = '=SU';
    editor.setSelectionRange(3, 3);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(120);
    const hintShown = !document.getElementById('fn-hint').hidden;
    const hintFirst = (document.querySelector('#fn-hint .fi') || {}).textContent || '';
    ok('T13a 补全弹出', hintShown && hintFirst.startsWith('SU'), `shown=${hintShown} first=${hintFirst}`);
    // 继续输入�?=SUM，首候选应�?SUM，Tab 接受 �?插入函数名和左括�?
    editor.value = '=SUM';
    editor.setSelectionRange(4, 4);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(120);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true }));
    await wait(80);
    ok('T13b Tab接受补全', editor.value === '=SUM(', editor.value);
    // 补上参数并提�?
    editor.value = editor.value + 'A1:A2)';
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(80);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await wait(300);
    const c13 = await api(`/api/cell?sheet=${S.sheet}&row=6&col=1`);
    // 注：引擎会像 Excel 一样从源单元格继承货币格式，故可能显示 $300.00
    ok('T13c 补全后公式可算', c13.content === '=SUM(A1:A2)' && /300/.test(c13.formatted), `${c13.content} = ${c13.formatted}`);

    // T14 引用前缀不误弹（=A1 不应出补全）
    clickCell(7, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    editor.value = '=A1';
    editor.setSelectionRange(3, 3);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(120);
    ok('T14 单元格引用不误弹补全', document.getElementById('fn-hint').hidden, 'hint shown for A1');
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(150);

    // T15 Backspace = 清空并进入编辑态（Excel 语义�?
    clickCell(1, 2);
    await apiPost('/api/input', { sheet: S.sheet, row: 1, col: 2, value: 'temp' });
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: 'Backspace', bubbles: true, cancelable: true }));
    await wait(150);
    ok('T15 Backspace进入编辑态', S.editing === true && editor.value === '', `editing=${S.editing} val="${editor.value}"`);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(100);

    // T16 Ctrl+A 两段式：先数据区域再全表
    clickCell(2, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', ctrlKey: true, bubbles: true, cancelable: true }));
    await wait(250);
    const dim = await api(`/api/dimension?sheet=${S.sheet}`);
    const n16 = normSel();
    ok('T16a Ctrl+A选数据区域', n16.r0 === dim.minRow && n16.r1 === dim.maxRow && n16.c0 === dim.minCol && n16.c1 === dim.maxCol, `${JSON.stringify(n16)} vs ${JSON.stringify(dim)}`);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: 'a', ctrlKey: true, bubbles: true, cancelable: true }));
    await wait(250);
    const n16b = normSel();
    ok('T16b 再Ctrl+A选整表', n16b.r0 === 1 && n16b.c0 === 1 && n16b.r1 >= dim.maxRow && n16b.c1 >= dim.maxCol, JSON.stringify(n16b));

    // T17 查找 API：匹配显示�?
    const f17 = await apiPost('/api/find', { sheet: S.sheet, text: '300' });
    ok('T17 查找匹配', f17.matches.length >= 2, `matches=${f17.matches.length}`);

    // T18 替换 API：替换公式文本并重算
    await apiPost('/api/input', { sheet: S.sheet, row: 8, col: 1, value: 'hello world' });
    const r18 = await apiPost('/api/replace', { sheet: S.sheet, find: 'world', replace: 'excel', all: true });
    const c18 = await api(`/api/cell?sheet=${S.sheet}&row=8&col=1`);
    ok('T18 全部替换', r18.replaced >= 1 && c18.formatted === 'hello excel', `replaced=${r18.replaced} v=${c18.formatted}`);

    // T19 剪切粘贴 = Excel 移动语义：外部公式引用追随、源区清�?
    await apiPost('/api/input', { sheet: S.sheet, row: 20, col: 1, value: '10' });
    await apiPost('/api/input', { sheet: S.sheet, row: 20, col: 2, value: '=A20*2' });
    const cp19 = await apiPost('/api/copy', { sheet: S.sheet, r0: 20, c0: 1, r1: 20, c1: 1 });
    await apiPost('/api/paste', { sheet: S.sheet, row: 22, col: 1, text: cp19.tsv, mode: 'cut' });
    const b20 = await api(`/api/cell?sheet=${S.sheet}&row=20&col=2`);
    const a20 = await api(`/api/cell?sheet=${S.sheet}&row=20&col=1`);
    const a22 = await api(`/api/cell?sheet=${S.sheet}&row=22&col=1`);
    ok('T19 剪切移动语义：引用追随+清源',
      b20.content === '=A22*2' && b20.formatted === '20' && a20.formatted === '' && a22.formatted === '10',
      `B20=${b20.content}(${b20.formatted}) A20="${a20.formatted}" A22=${a22.formatted}`);

    // T20 Ctrl+Enter 区域填充：公式相对引用随锚点平移
    await apiPost('/api/batch', { sheet: S.sheet, cells: [
      { r: 24, c: 1, v: '1' }, { r: 25, c: 1, v: '2' }, { r: 26, c: 1, v: '3' }] });
    await apiPost('/api/inputrange', { sheet: S.sheet, r0: 24, c0: 2, r1: 26, c1: 2, row: 24, col: 2, value: '=A24+1' });
    const b25 = await api(`/api/cell?sheet=${S.sheet}&row=25&col=2`);
    const b26 = await api(`/api/cell?sheet=${S.sheet}&row=26&col=2`);
    ok('T20 Ctrl+Enter区域公式平移', b25.content === '=A25+1' && b25.formatted === '3' && b26.formatted === '4',
      `B25=${b25.content}(${b25.formatted}) B26=${b26.formatted}`);

    // T21 F4 循环绝对/相对引用
    clickCell(28, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    editor.value = '=A1';
    editor.setSelectionRange(3, 3);
    const f4 = () => editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'F4', bubbles: true, cancelable: true }));
    f4();
    const f4a = editor.value;
    f4();
    const f4b = editor.value;
    f4();
    const f4c = editor.value;
    f4();
    const f4d = editor.value;
    ok('T21 F4引用循环', f4a === '=$A$1' && f4b === '=A$1' && f4c === '=$A1' && f4d === '=A1',
      `${f4a} ${f4b} ${f4c} ${f4d}`);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(100);

    // T22 点选插入引用（point mode）：点击/拖拽生成引用与区�?
    clickCell(28, 2);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    { const [x, y] = cellXY(20, 1); down(x, y); }
    const t22a = editor.value;
    { const [x, y] = cellXY(21, 1); move(x, y); }
    const t22b = editor.value;
    up();
    ok('T22 点选/拖拽插入引用', t22a === '=A20' && t22b === '=A20:A21', `click=${t22a} drag=${t22b}`);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(100);

    // T23 方向键点选引用（公式态下 Arrow 插入引用而非移动光标�?
    clickCell(28, 3);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true }));
    const t23a = editor.value;
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowUp', bubbles: true, cancelable: true }));
    const t23b = editor.value;
    ok('T23 方向键点选引用', t23a === '=C27' && t23b === '=C26', `${t23a} ${t23b}`);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(100);

    // T24 Alt+= 自动求和：推断上方连续数字块
    await apiPost('/api/batch', { sheet: S.sheet, cells: [
      { r: 30, c: 1, v: '1' }, { r: 31, c: 1, v: '2' }, { r: 32, c: 1, v: '3' }] });
    clickCell(33, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', altKey: true, bubbles: true, cancelable: true }));
    await wait(500);
    const t24formula = editor.value;
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await wait(300);
    const a33 = await api(`/api/cell?sheet=${S.sheet}&row=33&col=1`);
    ok('T24 Alt+=自动求和', t24formula === '=SUM(A30:A32)' && a33.formatted === '6', `${t24formula} = ${a33.formatted}`);

    // T25 循环引用 �?#CIRC! 错误而非卡死
    await apiPost('/api/input', { sheet: S.sheet, row: 35, col: 1, value: '=A35+1' });
    const a35 = await api(`/api/cell?sheet=${S.sheet}&row=35&col=1`);
    ok('T25 循环引用报#CIRC!', a35.formatted.includes('#CIRC'), a35.formatted);

    // T26 编辑公式时引用彩色高亮框
    clickCell(36, 1);
    gridScroll.dispatchEvent(new KeyboardEvent('keydown', { key: '=', bubbles: true }));
    await wait(80);
    editor.value = '=A1+B2';
    editor.setSelectionRange(6, 6);
    editor.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(120);
    const boxes = document.querySelectorAll('#ref-layer .ref-box').length;
    ok('T26 引用高亮框', boxes === 2, `boxes=${boxes}`);
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(100);

    // T27 边框：所有框线写�?+ 样式回读
    await apiPost('/api/border', { sheet: S.sheet, r0: 40, c0: 1, r1: 41, c1: 2, type: 'all', style: 'thin', color: '#000000' });
    const c27 = await api(`/api/cell?sheet=${S.sheet}&row=40&col=1`);
    const br27 = c27.style.br || {};
    ok('T27 边框全框线', br27.t && br27.b && br27.l && br27.r && br27.t.s === 'thin', JSON.stringify(br27));

    // T28 字体家族（母项目 UniDoc 字体清单�?
    await apiPost('/api/fontname', { sheet: S.sheet, r0: 40, c0: 1, r1: 40, c1: 1, name: '微软雅黑' });
    const c28 = await api(`/api/cell?sheet=${S.sheet}&row=40&col=1`);
    ok('T28 字体设置', c28.style.fn === '微软雅黑', c28.style.fn);

    // T29 格式刷：源格样式（含边框/字体）刷到目标区�?
    await apiPost('/api/copystyle', { sheet: S.sheet, sr0: 40, sc0: 1, sr1: 40, sc1: 1, dr0: 43, dc0: 3, dr1: 44, dc1: 4 });
    const c29 = await api(`/api/cell?sheet=${S.sheet}&row=44&col=4`);
    ok('T29 格式刷', c29.style.fn === '微软雅黑' && c29.style.br && !!c29.style.br.t, `fn=${c29.style.fn} br=${JSON.stringify(c29.style.br)}`);

    // T30 冻结窗格视觉�?
    await apiPost('/api/freeze', { sheet: S.sheet, rows: 2, cols: 1 });
    scheduleRefresh(true);
    await wait(600);
    const fzRows = document.getElementById('freeze-rows');
    const fzCells = fzRows ? fzRows.querySelectorAll('.cell').length : 0;
    ok('T30 冻结窗格渲染', fzRows && fzRows.style.display !== 'none' && fzCells > 0, `cells=${fzCells}`);
    const fzRowHeaders = [...document.querySelectorAll('#freeze-row-headers .row-hdr')];
    const fzColHeaders = [...document.querySelectorAll('#freeze-col-headers .col-hdr')];
    const frozenHeadersSized = [...fzRowHeaders, ...fzColHeaders].every((el) => {
      const rect = el.getBoundingClientRect();
      return rect.width > 0 && rect.height > 0;
    });
    ok('T30 冻结表头标签', fzRowHeaders.length === 2 && fzColHeaders.length === 1 && frozenHeadersSized,
      `rows=${fzRowHeaders.length} cols=${fzColHeaders.length}`);
    await apiPost('/api/freeze', { sheet: S.sheet, rows: 0, cols: 0 });
    scheduleRefresh(true);
    await wait(300);

    // T31 复杂函数电池：数�?统计/查找/文本/逻辑/日期/财务/工程/动态数�?
    await apiPost('/api/batch', { sheet: S.sheet, cells: [
      { r: 1, c: 6, v: '10' }, { r: 2, c: 6, v: '20' }, { r: 3, c: 6, v: '30' }, { r: 4, c: 6, v: '40' }, { r: 5, c: 6, v: '50' },
      { r: 1, c: 7, v: 'a' }, { r: 2, c: 7, v: 'b' }, { r: 3, c: 7, v: 'c' }, { r: 4, c: 7, v: 'a' }, { r: 5, c: 7, v: 'b' },
    ] });
    const fnCases = [
      ['=ROUND(3.14159,2)', '3.14'], ['=ROUNDUP(3.141,1)', '3.2'], ['=ROUNDDOWN(3.149,1)', '3.1'],
      ['=MROUND(7,3)', '6'], ['=CEILING(4.3,0.5)', '4.5'], ['=FLOOR(4.3,0.5)', '4'],
      ['=INT(-4.3)', '-5'], ['=MOD(10,3)', '1'], ['=POWER(2,10)', '1024'], ['=SQRT(144)', '12'],
      ['=ABS(-7)', '7'], ['=GCD(12,18)', '6'], ['=LCM(4,6)', '12'], ['=FACT(5)', '120'],
      ['=SUMPRODUCT(F1:F2,F3:F4)', '1100'], ['=SUMIF(F1:F5,">25")', '120'],
      ['=SUMIFS(F1:F5,G1:G5,"a")', '50'], ['=QUOTIENT(17,4)', '4'], ['=SIGN(-3)', '-1'], ['=TRUNC(8.97)', '8'],
      ['=AVERAGE(F1:F5)', '30'], ['=AVERAGEIF(F1:F5,">20")', '40'], ['=AVERAGEIFS(F1:F5,G1:G5,"b")', '35'],
      ['=COUNT(F1:F5)', '5'], ['=COUNTA(G1:G5)', '5'], ['=COUNTIF(G1:G5,"a")', '2'],
      ['=COUNTIFS(F1:F5,">15",G1:G5,"b")', '2'], ['=MAX(F1:F5)', '50'], ['=MIN(F1:F5)', '10'],
      ['=MAXIFS(F1:F5,G1:G5,"a")', '40'], ['=MINIFS(F1:F5,G1:G5,"b")', '20'], ['=MEDIAN(F1:F5)', '30'],
      ['=LARGE(F1:F5,2)', '40'], ['=SMALL(F1:F5,2)', '20'],
      ['=VLOOKUP(30,F1:G5,2,FALSE)', 'c'], ['=INDEX(F1:F5,3)', '30'], ['=MATCH(40,F1:F5,0)', '4'],
      ['=INDEX(G1:G5,MATCH(50,F1:F5,0))', 'b'], ['=XLOOKUP(20,F1:F5,G1:G5)', 'b'], ['=XMATCH(30,F1:F5)', '3'],
      ['=CHOOSE(2,"x","y","z")', 'y'], ['=OFFSET(F1,2,0)', '30'], ['=INDIRECT("F4")', '40'],
      ['=ROW(F7)', '7'], ['=COLUMN(F1)', '6'], ['=ROWS(F1:F5)', '5'],
      ['=CONCAT("Ex","cel")', 'Excel'], ['=TEXTJOIN("-",TRUE,G1:G3)', 'a-b-c'],
      ['=LEFT("Spreadsheet",6)', 'Spread'], ['=RIGHT("Spreadsheet",5)', 'sheet'], ['=MID("Spreadsheet",7,5)', 'sheet'],
      ['=LEN("Excel")', '5'], ['=UPPER("abc")', 'ABC'], ['=LOWER("ABC")', 'abc'], ['=PROPER("hello world")', 'Hello World'],
      ['=TRIM("  a  b  ")', 'a b'], ['=SUBSTITUTE("banana","a","o")', 'bonono'], ['=REPLACE("abcdef",2,3,"XY")', 'aXYef'],
      ['=REPT("ab",3)', 'ababab'], ['=FIND("c","abc")', '3'], ['=SEARCH("C","abc")', '3'],
      ['=EXACT("a","a")', 'TRUE'], ['=VALUE("42")', '42'], ['=TEXT(1234.5,"#,##0.00")', '1,234.50'],
      ['=CHAR(65)', 'A'], ['=CODE("A")', '65'], ['=UNICHAR(20013)', '中'],
      ['=IF(1>2,"y","n")', 'n'], ['=IFS(1>2,"a",2>1,"b")', 'b'], ['=SWITCH(2,1,"one",2,"two","other")', 'two'],
      ['=IFERROR(1/0,"err")', 'err'], ['=IFNA(NA(),"na")', 'na'],
      ['=AND(TRUE,1>0)', 'TRUE'], ['=OR(FALSE,1>2)', 'FALSE'], ['=XOR(TRUE,FALSE)', 'TRUE'], ['=NOT(TRUE)', 'FALSE'],
      ['=ISNUMBER(1)', 'TRUE'], ['=ISTEXT("a")', 'TRUE'], ['=ISBLANK(Z99)', 'TRUE'], ['=ISEVEN(4)', 'TRUE'], ['=ISODD(3)', 'TRUE'],
      ['=YEAR(DATE(2026,7,31))', '2026'], ['=MONTH(DATE(2026,7,31))', '7'], ['=DAY(DATE(2026,7,31))', '31'],
      ['=WEEKDAY(DATE(2026,7,31))', '6'], ['=DAY(EOMONTH(DATE(2026,2,1),0))', '28'],
      ['=DAYS(DATE(2026,1,10),DATE(2026,1,1))', '9'], ['=NETWORKDAYS(DATE(2026,7,27),DATE(2026,7,31))', '5'],
      ['=ROUND(PMT(0.05/12,60,-10000),2)', '188.71'], ['=ROUND(FV(0.05,10,-100),2)', '1257.79'],
      ['=ROUND(NPV(0.1,100,100,100),2)', '248.69'], ['=SLN(10000,1000,5)', '$1,800.00'], // SLN：引擎自动附加货币格式，数�?1800
      ['=BASE(255,16)', 'FF'], ['=DECIMAL("FF",16)', '255'], ['=BITAND(6,3)', '2'], ['=BITOR(4,1)', '5'],
      ['=ROMAN(2026)', 'MMXXVI'], ['=ARABIC("MMXXVI")', '2026'], ['=DELTA(5,5)', '1'],
      ['=SUM(SEQUENCE(5))', '15'], ['=SUMPRODUCT(SEQUENCE(3),SEQUENCE(3))', '14'],
      ['=SUM(FILTER(F1:F5,F1:F5>25))', '120'],
      ['=SUM(TAKE(F1:F5,2))', '30'], ['=SUM(DROP(F1:F5,3))', '90'],
      ['=LET(x,10,y,20,x*y)', '200'], ['=REDUCE(0,F1:F5,LAMBDA(a,b,a+b))', '150'],
    ];
    const fnFails = [];
    for (const [formula, expect] of fnCases) {
      // 清格式防止上一条用例（如财务函数自动带货币格式）污染断言
      await apiPost('/api/clear', { sheet: S.sheet, r0: 50, c0: 10, r1: 50, c1: 10, what: 'all' });
      await apiPost('/api/input', { sheet: S.sheet, row: 50, col: 10, value: formula });
      const r = await api(`/api/cell?sheet=${S.sheet}&row=50&col=10`);
      if (r.formatted !== expect) fnFails.push(`${formula} �?"${r.formatted}" (期望 "${expect}")`);
    }
    ok(`T31 复杂函数电池 ${fnCases.length - fnFails.length}/${fnCases.length}`, fnFails.length === 0, fnFails.join(' | '));

    // T32 单元格数字格式电�?
    const fmtCases = [
      ['1234.567', '#,##0.00', '1,234.57'],
      ['1234.567', '#,##0', '1,235'],
      ['1234.567', '$#,##0.00', '$1,234.57'],
      ['0.4567', '0.00%', '45.67%'],
      ['1234.567', '0.00E+00', '1.23E+03'],
      ['45000', 'yyyy-mm-dd', '2023-03-15'],
      ['0.5', 'hh:mm:ss', '12:00:00'],
    ];
    const fmtFails = [];
    for (const [val, fmt, expect] of fmtCases) {
      await apiPost('/api/input', { sheet: S.sheet, row: 52, col: 10, value: val });
      await apiPost('/api/style', { sheet: S.sheet, r0: 52, c0: 10, r1: 52, c1: 10, path: 'num_fmt', value: fmt });
      const r = await api(`/api/cell?sheet=${S.sheet}&row=52&col=10`);
      if (r.formatted !== expect) fmtFails.push(`${val}[${fmt}] �?"${r.formatted}" (期望 "${expect}")`);
      await apiPost('/api/style', { sheet: S.sheet, r0: 52, c0: 10, r1: 52, c1: 10, path: 'num_fmt', value: 'general' });
    }
    ok(`T32 数字格式电池 ${fmtCases.length - fmtFails.length}/${fmtCases.length}`, fmtFails.length === 0, fmtFails.join(' | '));

    // T33 引擎 Rust 重构：数组参数不�?NIMPL（vendor 分支 cast/TEXTJOIN/CONCAT 补丁�?
    const arrCases = [
      ['=TEXTJOIN(",",TRUE,UNIQUE(G1:G5))', 'a,b,c'],
      ['=CONCAT(SEQUENCE(3))', '123'],
      ['=LEN(UNIQUE(G1:G5))', '1'],
      ['=TEXTJOIN("-",TRUE,SORT(G1:G3))', 'a-b-c'],
    ];
    const arrFails = [];
    for (const [formula, expect] of arrCases) {
      await apiPost('/api/clear', { sheet: S.sheet, r0: 54, c0: 10, r1: 54, c1: 10, what: 'all' });
      await apiPost('/api/input', { sheet: S.sheet, row: 54, col: 10, value: formula });
      const r = await api(`/api/cell?sheet=${S.sheet}&row=54&col=10`);
      if (r.formatted !== expect) arrFails.push(`${formula} �?"${r.formatted}" (期望 "${expect}")`);
    }
    ok(`T33 数组参数引擎补丁 ${arrCases.length - arrFails.length}/${arrCases.length}`, arrFails.length === 0, arrFails.join(' | '));

    // T34 合并单元格：合并→清单回读→取消
    await apiPost('/api/input', { sheet: S.sheet, row: 56, col: 2, value: '合并标题' });
    const m34 = await apiPost('/api/merge', { sheet: S.sheet, r0: 56, c0: 2, r1: 57, c1: 4, op: 'merge' });
    const hit34 = (m34.merges || []).some((m) => m.r0 === 56 && m.c0 === 2 && m.r1 === 57 && m.c1 === 4);
    const tl34 = await api(`/api/cell?sheet=${S.sheet}&row=56&col=2`);
    const m34b = await apiPost('/api/merge', { sheet: S.sheet, r0: 56, c0: 2, r1: 57, c1: 4, op: 'unmerge' });
    const gone34 = !(m34b.merges || []).some((m) => m.r0 === 56);
    ok('T34 合并/取消合并', hit34 && tl34.formatted === '合并标题' && gone34, `merge=${hit34} tl=${tl34.formatted} unmerge=${gone34}`);

    // T35 条件格式�?25 浅红填充，命�?未命中各自正�?
    await apiPost('/api/cf', { sheet: S.sheet, r0: 1, c0: 6, r1: 5, c1: 6, op: 'add', rule: {
      type: 'CellIs', operator: 'GreaterThan', formula: '25', formula2: null, stop_if_true: false,
      format: { font: { color: '#9C0006' }, fill: { color: '#FFC7CE' }, border: null, num_fmt: null, alignment: null },
    } });
    const v35 = await api(`/api/view?sheet=${S.sheet}&r0=1&c0=6&r1=5&c1=6`);
    const f4cell = v35.cells.find((c) => c.r === 4 && c.c === 6); // 40 > 25 �?命中
    const f1cell = v35.cells.find((c) => c.r === 1 && c.c === 6); // 10 �?不命�?
    const hitBg = f4cell && f4cell.s.bg && f4cell.s.bg.toUpperCase() === '#FFC7CE';
    const missBg = !f1cell || !f1cell.s.bg;
    // 清理规则避免影响其它用例
    const l35 = await apiPost('/api/cf', { sheet: S.sheet, op: 'list' });
    for (const r of (l35.rules || []).map((x) => x.index).sort((a, b) => b - a)) {
      await apiPost('/api/cf', { sheet: S.sheet, op: 'delete', index: r });
    }
    ok('T35 条件格式着色', hitBg && missBg, `hit=${JSON.stringify(f4cell && f4cell.s.bg)} miss=${JSON.stringify(f1cell && f1cell.s.bg)} rules=${(l35.rules || []).length}`);

    // T36 xlsx 样式保真回环：新建→上样式→导出→导入→样式一�?
    await api('/api/new', { method: 'POST' });
    await apiPost('/api/input', { sheet: 0, row: 1, col: 1, value: '保真' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'font.b', value: 'true' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'font.color', value: '#C00000' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'fill.color', value: '#FFEB9C' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'num_fmt', value: '$#,##0.00' });
    await apiPost('/api/border', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, type: 'all', style: 'thin', color: '#000000' });
    await apiPost('/api/input', { sheet: 0, row: 2, col: 1, value: '100' });
    await apiPost('/api/style', { sheet: 0, r0: 2, c0: 1, r1: 2, c1: 1, path: 'num_fmt', value: '$#,##0.00' });
    const before = await api(`/api/cell?sheet=0&row=1&col=1`);
    // 导出 xlsx 字节
    const xbuf = await (await fetch('/api/export')).arrayBuffer();
    // 导回（整个工作簿重建�?
    await fetch('/api/import', { method: 'POST', body: xbuf });
    await loadWorkbook();
    await wait(200);
    const after = await api(`/api/cell?sheet=0&row=1&col=1`);
    const c2 = await api(`/api/cell?sheet=0&row=2&col=1`);
    const sb = before.style, sa = after.style;
    const same = sa.b === sb.b
      && (sa.fc || '').toUpperCase() === (sb.fc || '').toUpperCase()
      && (sa.bg || '').toUpperCase() === (sb.bg || '').toUpperCase()
      && sa.nf === sb.nf
      && !!(sa.br && sa.br.t) === !!(sb.br && sb.br.t);
    ok('T36 xlsx 样式保真回环',
      same && c2.formatted === '$100.00',
      `before=${JSON.stringify(sb)} after=${JSON.stringify(sa)} c2=${c2.formatted}`);

    // T37 格线只画一遍（Excel 清脆 1px）：单元格无边框 + spacer 背景格线
    await api('/api/new', { method: 'POST' });
    await loadWorkbook();
    await apiPost('/api/input', { sheet: 0, row: 1, col: 1, value: 'x' });
    scheduleRefresh(true);
    await wait(500);
    const cellEl = document.querySelector('#cells-layer .cell');
    const cs = getComputedStyle(cellEl || document.createElement('div'));
    const glines = document.querySelectorAll('#grid-lines .gline-v, #grid-lines .gline-h').length;
    ok('T37 格线 DOM 单层清晰', !!cellEl && cs.borderRightWidth === '0px' && glines > 0,
      `cellBorder=${cs.borderRightWidth} gridLines=${glines}`);

    // T38 单元格格式对话框内调色板色块
    setCursor(1, 1);
    await openCellFormatDialog();
    await wait(150);
    const sw38 = document.querySelector('#cfmt-fc-wrap .color-swatch');
    sw38.click();
    await wait(150);
    const pop38 = document.querySelector('.color-pop');
    const swCount = pop38 ? pop38.querySelectorAll('.cp-sw').length : 0;
    pop38.querySelectorAll('.cp-std .cp-sw')[1].click(); // 标准色第 2 �?#FF0000
    await wait(100);
    ok('T38 对话框调色板', !!sw38 && swCount === 70 && sw38.dataset.hex.toUpperCase() === '#FF0000',
      `sw=${!!sw38} count=${swCount} hex=${sw38.dataset.hex}`);
    document.getElementById('cfmt-cancel').click();
    await wait(100);

    // T39 缩放档位生效 + 浏览器原生缩放屏蔽（不做锡点调整滚动）
    setZoom(1); gridScroll.scrollLeft = 0; gridScroll.scrollTop = 0;
    await wait(200);
    const rect0 = gridScroll.getBoundingClientRect();
    const wEv = new WheelEvent('wheel', { clientX: rect0.left + 150, clientY: rect0.top + 100, deltaY: -100, ctrlKey: true, bubbles: true, cancelable: true });
    gridScroll.dispatchEvent(wEv);
    await wait(300);
    ok('T39 缩放+屏蔽原生', S.zoom > 1 && wEv.defaultPrevented === true && gridScroll.scrollTop === 0,
      `zoom=${S.zoom} prevented=${wEv.defaultPrevented} scrollTop=${gridScroll.scrollTop}`);
    setZoom(1); gridScroll.scrollLeft = 0; gridScroll.scrollTop = 0; await wait(200);

    // T40 显示公式模式
    await apiPost('/api/input', { sheet: S.sheet, row: 3, col: 1, value: '=1+2' });
    S.showFormulas = true;
    scheduleRefresh(true);
    await wait(400);
    const fv = [...document.querySelectorAll('#cells-layer .cell')].find((c) => c.textContent === '=1+2');
    S.showFormulas = false;
    scheduleRefresh(true);
    await wait(200);
    ok('T40 显示公式', !!fv && fv.classList.contains('formula-view'), `found=${!!fv}`);

    // T41 LaTeX 公式渲染（无损：存源码、显渲染�?
    await apiPost('/api/input', { sheet: S.sheet, row: 5, col: 1, value: '$E=mc^2$' });
    setCursor(5, 1); // 触发滚动让 A5 进入可视区（小视口环境也能渲染）
    scheduleRefresh(true);
    await wait(500);
    const ltx = document.querySelector('#cells-layer .cell .latex-src');
    const ltxContent = await api(`/api/cell?sheet=${S.sheet}&row=5&col=1`);
    ok('T41 LaTeX渲染无损', !!ltx && ltx.dataset.src === '$E=mc^2$' && ltxContent.content === '$E=mc^2$',
      `rendered=${!!ltx} content=${ltxContent.content}`);

    // T42 公式板块：函数库插入 + 追踪箭头
    setCursor(6, 1);
    insertFunction('SUM');
    const insOk = S.editing && editor.value === '=SUM(';
    editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    await wait(150);
    await apiPost('/api/input', { sheet: S.sheet, row: 7, col: 1, value: '=A1+A3' });
    setCursor(7, 1);
    await wait(200);
    $('fr-trace-prec').click();
    await wait(300);
    const traceLines = document.querySelectorAll('#trace-layer line').length;
    clearTrace();
    ok('T42 函数插入+追踪箭头', insOk && traceLines >= 2, `insert=${insOk} lines=${traceLines}`);

    // T43 合并单元格撤销/重做（单 undo 单元恢复内容+合并列表，unidoc 级别）
    await api('/api/new', { method: 'POST' });
    await apiPost('/api/input', { sheet: 0, row: 1, col: 1, value: 'T' });
    await apiPost('/api/input', { sheet: 0, row: 1, col: 2, value: 'X' });
    await apiPost('/api/merge', { sheet: 0, r0: 1, c0: 1, r1: 2, c1: 2, op: 'merge' });
    let v43 = await api(`/api/view?sheet=0&r0=1&c0=1&r1=2&c1=2`);
    const merged43 = (v43.merges || []).length === 1;
    await api('/api/undo', { method: 'POST' });
    v43 = await api(`/api/view?sheet=0&r0=1&c0=1&r1=2&c1=2`);
    const b143 = await api(`/api/cell?sheet=0&row=1&col=2`);
    const undone43 = (v43.merges || []).length === 0 && b143.formatted === 'X';
    await api('/api/redo', { method: 'POST' });
    v43 = await api(`/api/view?sheet=0&r0=1&c0=1&r1=2&c1=2`);
    const redone43 = (v43.merges || []).length === 1;
    ok('T43 合并撤销/重做', merged43 && undone43 && redone43, `merge=${merged43} undo=${undone43} redo=${redone43}`);

    // T44 多操作连续撤销（输入/样式/插行均可逆）
    await api('/api/new', { method: 'POST' });
    await apiPost('/api/input', { sheet: 0, row: 1, col: 1, value: '10' });
    await apiPost('/api/input', { sheet: 0, row: 2, col: 1, value: '20' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'font.b', value: 'true' });
    await apiPost('/api/rows', { sheet: 0, op: 'insert', row: 1, count: 1 });
    // 撤销插行 → 恢复 A1=10
    await api('/api/undo', { method: 'POST' });
    let a1 = await api(`/api/cell?sheet=0&row=1&col=1`);
    const u1 = a1.formatted === '10';
    // 撤销加粗 → A1 不再加粗
    await api('/api/undo', { method: 'POST' });
    a1 = await api(`/api/cell?sheet=0&row=1&col=1`);
    const u2 = a1.style.b === false;
    // 撤销输入 20 → A2 空
    await api('/api/undo', { method: 'POST' });
    let a2 = await api(`/api/cell?sheet=0&row=2&col=1`);
    const u3 = a2.formatted === '';
    // 重做回来
    await api('/api/redo', { method: 'POST' });
    await api('/api/redo', { method: 'POST' });
    a2 = await api(`/api/cell?sheet=0&row=2&col=1`);
    const r3 = a2.formatted === '20';
    ok('T44 连续撤销覆盖', u1 && u2 && u3 && r3, `u1=${u1} u2=${u2} u3=${u3} redo=${r3}`);

    // T45 状态栏：有效数据行列数 + 缩放滑块 + 视图切换
    await api('/api/new', { method: 'POST' });
    await loadWorkbook();
    await apiPost('/api/batch', { sheet: 0, cells: [{ r: 1, c: 1, v: 'a' }, { r: 50, c: 8, v: 'z' }] });
    scheduleRefresh(true);
    await wait(1300); // 等节流刷新
    const dimText = $('status-dim').textContent;
    const dimOk = dimText.includes('50') && dimText.includes('8');
    const slider = $('st-zoom-slider');
    slider.value = 200;
    slider.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(200);
    const zoomOk = Math.abs(S.zoom - 2) < 0.01 && $('st-zoom-level').textContent === '200%';
    setZoom(1); await wait(150);
    $('st-view-formulas').click();
    await wait(200);
    const sfOk = S.showFormulas === true && $('st-view-formulas').classList.contains('on');
    setShowFormulas(false);
    ok('T45 状态栏行列数+缩放滑块+视图切换', dimOk && zoomOk && sfOk, `dim="${dimText}" zoom=${S.zoom} sf=${sfOk}`);

    // T46 插入对象：文本框/SVG/沙盒HTML 渲染 + 锚定切换 + 删除
    await api('/api/new', { method: 'POST' });
    await loadWorkbook();
    await wait(300);
    await insertObject('text', { html: '测试文本框' });
    await insertObject('svg', { svg: DEFAULT_SVG });
    await insertObject('html', { code: DEFAULT_HTML });
    await wait(300);
    const objs = document.querySelectorAll('#objects-layer .cell-obj, #frozen-objects-layer .cell-obj');
    const olist = await api(`/api/objects?sheet=0`);
    const t46a = objs.length === 3 && (olist.objects || []).length === 3;
    // 锚定切换：第一个对象改为绝对位置
    const o0 = S.objects[0];
    selectObject(o0.id);
    $('btn-ins-abs').click();
    await wait(200);
    const t46b = S.objects[0].mode === 'abs';
    // 删除
    selectObject(o0.id);
    $('btn-ins-del').click();
    await wait(200);
    const t46c = document.querySelectorAll('#objects-layer .cell-obj, #frozen-objects-layer .cell-obj').length === 2;
    ok('T46 插入对象系统', t46a && t46b && t46c, `render=${objs.length} api=${(olist.objects||[]).length} abs=${t46b} afterDel=${t46c}`);

    // T47 导出 udoc（UDOC3：尾目录 + 部件级 Brotli/ZIP 混合压缩）+ xlsx 导出不崩
    await insertObject('text', { html: '导出测试' });
    const udocBuf = await (await fetch('/api/export-udoc')).arrayBuffer();
    const u8 = new Uint8Array(udocBuf);
    const head = String.fromCharCode(...u8.slice(0, 8));
    const tail = String.fromCharCode(...u8.slice(u8.length - 64, u8.length - 56));
    const t47a = head === 'UDOC3PKG' && tail === 'UD3DIR01' && u8.length > 100;
    const xbuf47 = await (await fetch('/api/export')).arrayBuffer();
    const t47b = xbuf47.byteLength > 500; // xlsx 导出不崩且有内容
    ok('T47 导出 udoc(UDOC3)+xlsx 兼容', t47a && t47b, `head=${head} tail=${tail} udoc=${u8.length}B xlsx=${xbuf47.byteLength}B`);

    // T48 xlsx 嵌图：SVG 对象转 EMF 注入 drawing 图层（zip 文件名明文可检）
    const objX = await (await fetch('/api/export', { method: 'POST', body: JSON.stringify({ objects: [{ id: 'o1', type: 'svg', sheet: 0, mode: 'cell', r: 2, c: 2, x: 8, y: 8, w: 200, h: 150, svg: '<svg xmlns="http://www.w3.org/2000/svg"><circle cx="50" cy="50" r="40"/></svg>' }] }) })).arrayBuffer();
    const objTxt = new TextDecoder('latin1').decode(objX);
    const t48 = objTxt.includes('unicellDrawing1.xml') && objTxt.includes('unicellImage1.emf') && objTxt.includes('unicellDrawing1.xml.rels');
    ok('T48 xlsx 嵌图(SVG→EMF)', t48, `drawing=${objTxt.includes('unicellDrawing1.xml')} emf=${objTxt.includes('unicellImage1.emf')} rels=${objTxt.includes('unicellDrawing1.xml.rels')} bytes=${objX.byteLength}`);

    // T54 对象交互：8方向缩放、文本单击拖动/双击编辑、点击外部取消绿色边框
    const resizeObj = S.objects.find((o) => o.type === 'svg') || S.objects[0];
    selectObject(resizeObj.id);
    let resizeEl = document.querySelector(`.cell-obj[data-id="${resizeObj.id}"]`);
    const handlesOk = resizeEl.querySelectorAll('.obj-handle').length === 8
      && !!resizeEl.querySelector('.obj-viewport');
    const wBefore = resizeObj.w, hBefore = resizeObj.h;
    const se = resizeEl.querySelector('.obj-handle[data-h="se"]');
    se.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true, pointerId: 71, clientX: 100, clientY: 100 }));
    document.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, pointerId: 71, clientX: 140, clientY: 130 }));
    document.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 71, clientX: 140, clientY: 130 }));
    const resizeOk = Math.abs(resizeObj.w - (wBefore + 40)) < 0.1
      && Math.abs(resizeObj.h - (hBefore + 30)) < 0.1;

    const textObj = S.objects.find((o) => o.type === 'text');
    const textEl = document.querySelector(`.cell-obj[data-id="${textObj.id}"] .obj-text`);
    const tx0 = textObj.x, ty0 = textObj.y;
    textEl.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true, pointerId: 73, clientX: 120, clientY: 120 }));
    document.dispatchEvent(new PointerEvent('pointermove', { bubbles: true, pointerId: 73, clientX: 155, clientY: 145 }));
    document.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerId: 73, clientX: 155, clientY: 145 }));
    const textDragOk = Math.abs(textObj.x - (tx0 + 35)) < 0.1
      && Math.abs(textObj.y - (ty0 + 25)) < 0.1;
    textEl.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    const textKey = new KeyboardEvent('keydown', { key: 'a', ctrlKey: true, bubbles: true, cancelable: true });
    textEl.dispatchEvent(textKey);
    const textOk = textEl.isContentEditable
      && getComputedStyle(textEl).userSelect === 'text'
      && !textKey.defaultPrevented;

    selectObject(resizeObj.id);
    document.body.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerId: 72 }));
    const blurOk = S.selObj === null && !resizeEl.classList.contains('sel');
    ok('T54 对象缩放+文本拖动/双击编辑+外部失焦', handlesOk && resizeOk && textDragOk && textOk && blurOk,
      `handles=${handlesOk} resize=${resizeObj.w}x${resizeObj.h} textDrag=${textDragOk} edit=${textOk} blur=${blurOk}`);

    // T55 冻结窗格对象：锚点落在冻结行时必须进入独立层，纵向滚动不再依靠逐帧补偿。
    const frozenObj = {
      id: 't55-frozen', sheet: S.sheet, type: 'svg', mode: 'abs',
      r: 1, c: 2, x: 100, y: 100, w: 80, h: 40, config: { svg: DEFAULT_SVG },
    };
    S.objects.push(frozenObj);
    S.frozen = { rows: 1, cols: 0 };
    gridScroll.scrollTop = 0;
    renderObjects();
    const frozenEl = document.querySelector('.cell-obj[data-id="t55-frozen"]');
    const top0 = parseFloat(frozenEl.style.top);
    gridScroll.scrollTop = 120;
    updateObjPositions();
    const top1 = parseFloat(frozenEl.style.top);
    const frozenOk = frozenEl.parentElement === frozenObjectsLayer
      && Math.abs(top1 - top0) < 0.1;
    gridScroll.scrollTop = 0;
    S.objects = S.objects.filter((o) => o.id !== frozenObj.id);
    renderObjects();
    ok('T55 冻结区图片独立图层滚动不晃动', frozenOk,
      `layer=${frozenEl.parentElement && frozenEl.parentElement.id} top=${top0}->${top1}`);

    // T56 合并单元格在主网格/冻结层共用同一渲染器：覆盖区内不能残留左上角普通副本。
    const mergeFrag = document.createDocumentFragment();
    appendCellsWithMerges(mergeFrag, {
      r0: 1, r1: 1, c0: 1, c1: 3,
      cells: [{ r: 1, c: 1, v: '合并标题', t: 'Text', s: { wr: true } }],
      merges: [{ r0: 1, r1: 1, c0: 1, c1: 3 }],
    });
    const mergeHost = document.createElement('div');
    mergeHost.appendChild(mergeFrag);
    const mergeCopies = mergeHost.querySelectorAll('.cell');
    const mergeOk = mergeCopies.length === 1
      && mergeCopies[0].classList.contains('merged')
      && mergeCopies[0].textContent === '合并标题';
    ok('T56 冻结层合并单元格无重复文字叠影', mergeOk,
      `copies=${mergeCopies.length} merged=${mergeCopies[0] && mergeCopies[0].classList.contains('merged')}`);

    // T57 Excel sharedStrings 富文本 run：颜色、粗体、字号和普通正文必须共存。
    const richHost = document.createElement('div');
    const richRendered = renderRichTextInto(richHost, [
      { text: '必填', bold: true, size: 12, color: '#FF0000', font: 'Microsoft YaHei' },
      { text: ' 正文', bold: false, size: 9, color: '#000000', font: 'Microsoft YaHei' },
    ]);
    const richSpans = richHost.querySelectorAll('.rich-run');
    const richOk = richRendered && richSpans.length === 2
      && richSpans[0].style.fontWeight === 'bold'
      && richSpans[0].style.color === 'rgb(255, 0, 0)'
      && richSpans[0].style.fontSize === `${12 * S.zoom}px`
      && richSpans[1].style.fontSize === `${9 * S.zoom}px`;
    ok('T57 sharedStrings 富文本颜色/粗体/字号', richOk,
      `runs=${richSpans.length} red=${richSpans[0] && richSpans[0].style.color}`);

    // T58 SVG→EMF→SVG：模板关键色、红色和透明度均须保持；EMF alpha 按 8-bit 量化。
    const vectorSvg = '<svg xmlns="http://www.w3.org/2000/svg" width="50" height="10">'
      + '<rect width="10" height="10" fill="#00BAF5"/><rect x="10" width="10" height="10" fill="#2972F4"/>'
      + '<rect x="20" width="10" height="10" fill="#FEF9F8"/><rect x="30" width="10" height="10" fill="#FF0000" fill-opacity="0.4"/>'
      + '<path d="M40 1L49 9" fill="none" stroke="#267EF0" stroke-opacity="0.65"/></svg>';
    const emf58 = await (await fetch('/api/svg2emf', {
      method: 'POST', body: JSON.stringify({ svg: vectorSvg }),
    })).arrayBuffer();
    const vectorBack58 = await (await fetch('/api/emf2svg', { method: 'POST', body: emf58 })).text();
    const lower58 = vectorBack58.toLowerCase();
    const vectorOk58 = ['#00baf5','#2972f4','#fef9f8','#ff0000','#267ef0'].every((c) => lower58.includes(c))
      && lower58.includes('fill-opacity="0.4"')
      && /stroke-opacity="0\.65(?:0|098)/.test(lower58);
    ok('T58 矢量关键颜色与透明度往返保真', vectorOk58,
      `bytes=${emf58.byteLength} colors=${['#00baf5','#2972f4','#fef9f8','#ff0000','#267ef0'].filter((c) => lower58.includes(c)).length}/5`);

    // T59 Excel 公式栏克隆：名称框/控制区/展开按钮 + 取消编辑；udoc 类型只能位于顶层。
    setCursor(1, 1);
    await wait(180);
    const before59 = await api(`/api/cell?sheet=${S.sheet}&row=1&col=1`);
    formulaInput.focus();
    formulaInput.value = '此内容必须被取消';
    formulaInput.dispatchEvent(new Event('input', { bubbles: true }));
    const edit59 = S.editing && formulaRow.classList.contains('editing');
    $('formula-cancel').click();
    await wait(180);
    const after59 = await api(`/api/cell?sheet=${S.sheet}&row=1&col=1`);
    const collapsedHeight59 = formulaRow.getBoundingClientRect().height;
    $('formula-expand').click();
    await wait(160);
    const expandedHeight59 = formulaRow.getBoundingClientRect().height;
    const expanded59 = formulaRow.classList.contains('expanded')
      && $('formula-expand').getAttribute('aria-expanded') === 'true';
    $('formula-expand').click();
    const nameW59 = document.querySelector('.formula-name-wrap').getBoundingClientRect().width;
    const controlsW59 = $('formula-edit-controls').getBoundingClientRect().width;
    const udoc59 = await api('/api/udoc-json');
    const schema59 = udoc59.format === 'udoc' && udoc59.unidoc_type === 'cell'
      && udoc59.manifest && !Object.prototype.hasOwnProperty.call(udoc59.manifest, 'unidoc_type')
      && udoc59.manifest.compression?.strategy === 'hybrid-br-zip'
      && udoc59.manifest.compression?.text === 'brotli-q9'
      && udoc59.manifest.compression?.wholeFile === false;
    const compact59 = matchMedia('(max-width: 720px)').matches;
    // 浏览器页面缩放会同比缩小 getBoundingClientRect()，但不会改变 CSS 媒体查询；
    // 这里验证紧凑布局允许的实际最小可用宽度，桌面布局自然也满足该下限。
    const formulaWidths59 = nameW59 >= 100 && controlsW59 >= 110;
    const formula59 = edit59 && !S.editing && before59.content === after59.content
      && formulaWidths59 && expanded59;
    ok('T59 Excel公式栏交互 + udoc顶层类型', formula59 && schema59,
      `edit=${edit59} cancel=${before59.content === after59.content} compact=${compact59} name=${nameW59} controls=${controlsW59} expanded=${collapsedHeight59}/${expandedHeight59} schema=${schema59}`);

    // T60 Excel 图表替代层：HTML 图表在沙箱 iframe 中呈现且仍由对象护盾接管拖拽。
    const chart60 = {
      id: 't60-chart', sheet: S.sheet, type: 'html', mode: 'abs', r: 2, c: 2,
      x: 120, y: 80, w: 320, h: 180,
      config: { source: 'xlsx-chart', chartType: 'bar', excelChartXml: '<c:barChart/>', code: '<!doctype html><html><body><canvas id="chart"></canvas></body></html>' },
    };
    S.objects.push(chart60);
    renderObjects();
    const chartEl60 = document.querySelector('.cell-obj[data-id="t60-chart"]');
    const iframe60 = chartEl60 && chartEl60.querySelector('iframe');
    const chartOk60 = !!iframe60 && iframe60.getAttribute('sandbox') === 'allow-scripts'
      && iframe60.srcdoc.includes('<canvas id="chart">') && !!chartEl60.querySelector('.obj-shield');
    S.objects = S.objects.filter((o) => o.id !== chart60.id);
    renderObjects();
    ok('T60 Excel图表→HTML iframe替代层', chartOk60,
      `iframe=${!!iframe60} sandbox=${iframe60 && iframe60.getAttribute('sandbox')} shield=${!!(chartEl60 && chartEl60.querySelector('.obj-shield'))}`);

    // T61 当前工作表真实行列区域 → 独立打印 DOM → 浏览器原生 print/PDF。
    // autoprint=0 只用于自动化预览检查；正式按钮使用 autoprint=1。
    await api('/api/new', { method: 'POST' });
    await apiPost('/api/batch', { sheet: 0, cells: [{ r: 1, c: 1, v: '打印标题' }, { r: 75, c: 9, v: '实际末格' }] });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'font.b', value: 'true' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'font.color', value: '#C00000' });
    await apiPost('/api/style', { sheet: 0, r0: 1, c0: 1, r1: 1, c1: 1, path: 'fill.color', value: '#FFEB9C' });
    await apiPost('/api/colwidth', { sheet: 0, c0: 9, c1: 9, width: 143 });
    await apiPost('/api/rowheight', { sheet: 0, r0: 75, r1: 75, height: 33 });
    await apiPost('/api/merge', { sheet: 0, r0: 1, c0: 1, r1: 2, c1: 2, op: 'merge' });
    await apiPost('/api/objects', { op: 'add', sheet: 0, object: {
      id: 't61-print-object', sheet: 0, type: 'svg', mode: 'abs', r: 1, c: 1,
      x: 950, y: 1700, w: 300, h: 100, config: { svg: DEFAULT_SVG },
    } });
    const printPreview61 = await (await fetch('/api/print-html?sheet=0&autoprint=0')).text();
    const printDoc61 = new DOMParser().parseFromString(printPreview61, 'text/html');
    const printBody61 = printDoc61.body;
    const merged61 = printDoc61.querySelector('td[rowspan="2"][colspan="2"]');
    const styled61 = merged61 && merged61.getAttribute('style') || '';
    const geometry61 = printBody61.dataset.printRows === '75'
      && printBody61.dataset.printCols === '9'
      && parseFloat(printBody61.dataset.printWidth) >= 1250
      && parseFloat(printBody61.dataset.printHeight) >= 1800;
    const structure61 = !!merged61 && !!printDoc61.querySelector('.print-objects svg')
      && styled61.includes('font-weight:bold') && styled61.includes('#C00000')
      && printPreview61.includes('@page unicell-paper{size:') && !printPreview61.includes('window.print()')
      && !printDoc61.querySelector('#ribbon, #formula-row, #statusbar, .sheet-tabs');
    const nativePrint61 = (await (await fetch('/api/print-html?sheet=0&autoprint=1')).text()).includes('window.print()');
    const actualFrame61 = await printCurrentSheetPdf({ suppressPrint: true });
    const actualPrintDoc61 = actualFrame61.contentDocument;
    const printHandler61 = actualFrame61.dataset.printReady === 'true'
      && !!actualPrintDoc61.querySelector('main.print-document > .print-page')
      && actualPrintDoc61.body.dataset.unicellPrintOnly === 'true'
      && !actualPrintDoc61.querySelector('#ribbon, #formula-row, #statusbar, .sheet-tabs')
      && String(printCurrentSheetPdf).includes('window.location.assign')
      && !String(printCurrentSheetPdf).includes('printWindow.print()')
      && !String(printCurrentSheetPdf).includes("window.open(");
    actualFrame61.remove();
    const button61 = !!$('btn-file-print-pdf') && $('btn-file-print-pdf').onclick === printCurrentSheetPdf;
    ok('T61 真实行列打印区域+专用文档原生打印/PDF', geometry61 && structure61 && nativePrint61 && printHandler61 && button61,
      `rows=${printBody61.dataset.printRows} cols=${printBody61.dataset.printCols} size=${printBody61.dataset.printWidth}x${printBody61.dataset.printHeight} merge=${!!merged61} appChrome=${!!printDoc61.querySelector('#ribbon, #formula-row, #statusbar, .sheet-tabs')} framePrint=${printHandler61} native=${nativePrint61} button=${button61}`);

    // T62 Ctrl+P 必须进入同一纯打印链路；A4/A5 等纸张设置必须成为真实 @page 规则。
    const a4Print62 = await (await fetch('/api/print-html?sheet=0&paper=a4&orientation=landscape&autoprint=0')).text();
    const a5Print62 = await (await fetch('/api/print-html?sheet=0&paper=a5&orientation=portrait&autoprint=0')).text();
    const a4Doc62 = new DOMParser().parseFromString(a4Print62, 'text/html');
    const a5Doc62 = new DOMParser().parseFromString(a5Print62, 'text/html');
    const papers62 = a4Doc62.body.dataset.printPaper === 'A4'
      && a4Doc62.body.dataset.printOrientation === 'landscape'
      && a4Print62.includes('@page unicell-paper{size:297.000mm 210.000mm;margin:0}')
      && parseFloat(a4Doc62.body.dataset.printScale) === 1
      && a5Doc62.body.dataset.printPaper === 'A5'
      && a5Doc62.body.dataset.printOrientation === 'portrait'
      && a5Print62.includes('@page unicell-paper{size:148.000mm 210.000mm;margin:0}');
    const paperOptions62 = ['actual', 'sheet', 'a3', 'a4', 'a5', 'letter', 'legal'].every((v) =>
      Array.from($('print-paper-size').options).some((o) => o.value === v));
    const scalingOptions62 = ['sheet', 'none', 'fit-width', 'fit-height', 'fit-sheet'].every((v) =>
      Array.from($('print-scaling').options).some((o) => o.value === v));
    const scopeOptions62 = ['sheet', 'workbook'].every((v) =>
      Array.from($('print-scope').options).some((o) => o.value === v));
    $('print-paper-size').value = 'a4';
    $('print-orientation').disabled = false;
    $('print-orientation').value = 'landscape';
    $('print-scaling').disabled = false;
    $('print-scaling').value = 'none';
    const ctrlP62 = new KeyboardEvent('keydown', { key: 'p', code: 'KeyP', ctrlKey: true, bubbles: true, cancelable: true });
    Object.defineProperty(ctrlP62, '__unicellSuppressPrint', { value: true });
    window.dispatchEvent(ctrlP62);
    const shortcutReady62 = await waitUntil(() => document.getElementById('unicell-native-print-frame')?.dataset.printReady === 'true', 5000);
    const shortcutFrame62 = document.getElementById('unicell-native-print-frame');
    const shortcutBody62 = shortcutFrame62 && shortcutFrame62.contentDocument.body;
    const shortcut62 = ctrlP62.defaultPrevented && shortcutReady62
      && shortcutBody62.dataset.unicellPrintOnly === 'true'
      && shortcutBody62.dataset.printPaper === 'A4'
      && shortcutBody62.dataset.printOrientation === 'landscape'
      && !shortcutFrame62.contentDocument.querySelector('#ribbon, #formula-row, #statusbar, .sheet-tabs');
    if (shortcutFrame62) shortcutFrame62.remove();
    $('print-paper-size').value = 'actual';
    syncPrintPageControls();
    ok('T62 Ctrl+P纯打印+A3/A4/A5/Letter/Legal页面设置', papers62 && paperOptions62 && scalingOptions62 && scopeOptions62 && shortcut62,
      `papers=${papers62} options=${paperOptions62}/${scalingOptions62}/${scopeOptions62} ctrlP=${ctrlP62.defaultPrevented} ready=${shortcutReady62} shortcutPaper=${shortcutBody62 && shortcutBody62.dataset.printPaper}`);

    // T63: same explicit physical-page model as generate_bg.py; split at real row/column edges.
    const pages63 = Array.from(a5Doc62.querySelectorAll('main.print-document > section.print-page'));
    const pageCount63 = Number(a5Doc62.body.dataset.printPages);
    const numbered63 = pages63.every((page, index) => Number(page.dataset.pageNumber) === index + 1);
    const sliced63 = pages63.some((page) => Number(page.dataset.pageRow) > 1)
      && pages63.some((page) => Number(page.dataset.pageCol) > 1);
    const fixedGeometry63 = pages63.length > 1
      && pages63.every((page) => page.querySelector(':scope > .page-content'))
      && parseFloat(a5Doc62.body.dataset.pageWidth) > 0
      && parseFloat(a5Doc62.body.dataset.pageHeight) > 0;
    const css63 = a5Print62.includes('.print-page:last-child{break-after:auto;page-break-after:auto}')
      && a5Print62.includes('break-after:page;page-break-after:always')
      && !a5Print62.includes('.print-sheet')
      && !a5Print62.includes('zoom:');
    ok('T63 explicit physical pages + real row/column pagination + no print scaling',
      pageCount63 === pages63.length && numbered63 && sliced63 && fixedGeometry63 && css63,
      `pages=${pages63.length}/${pageCount63} numbered=${numbered63} sliced=${sliced63} geometry=${fixedGeometry63} css=${css63}`);

    // T64 Excel scaling semantics: selecting paper alone does not scale; Fit modes shrink only.
    const fitWidth64 = await (await fetch('/api/print-html?sheet=0&paper=a5&orientation=portrait&scaling=fit-width&autoprint=0')).text();
    const fitHeight64 = await (await fetch('/api/print-html?sheet=0&paper=a5&orientation=portrait&scaling=fit-height&autoprint=0')).text();
    const fitSheet64 = await (await fetch('/api/print-html?sheet=0&paper=a5&orientation=portrait&scaling=fit-sheet&autoprint=0')).text();
    const fitWidthDoc64 = new DOMParser().parseFromString(fitWidth64, 'text/html');
    const fitHeightDoc64 = new DOMParser().parseFromString(fitHeight64, 'text/html');
    const fitSheetDoc64 = new DOMParser().parseFromString(fitSheet64, 'text/html');
    const widthPages64 = Array.from(fitWidthDoc64.querySelectorAll('.print-page'));
    const heightPages64 = Array.from(fitHeightDoc64.querySelectorAll('.print-page'));
    const fitModes64 = a5Doc62.body.dataset.printScaling === 'none'
      && parseFloat(a5Doc62.body.dataset.printScale) === 1
      && fitWidthDoc64.body.dataset.printScaling === 'fit-width'
      && parseFloat(fitWidthDoc64.body.dataset.printScale) > 0
      && parseFloat(fitWidthDoc64.body.dataset.printScale) < 1
      && widthPages64.every((page) => Number(page.dataset.pageCol) === 1)
      && fitHeightDoc64.body.dataset.printScaling === 'fit-height'
      && heightPages64.every((page) => Number(page.dataset.pageRow) === 1)
      && fitSheetDoc64.body.dataset.printScaling === 'fit-sheet'
      && Number(fitSheetDoc64.body.dataset.printPages) === 1
      && fitWidth64.includes('.page-surface{') && fitWidth64.includes('transform:scale(')
      && !fitWidth64.includes('zoom:');
    ok('T64 Excel无缩放/所有列一页/所有行一页/整表一页', fitModes64,
      `none=${a5Doc62.body.dataset.printScale} width=${fitWidthDoc64.body.dataset.printScale}/${widthPages64.length} height=${fitHeightDoc64.body.dataset.printScale}/${heightPages64.length} sheet=${fitSheetDoc64.body.dataset.printScale}/${fitSheetDoc64.body.dataset.printPages}`);

    // T65 动态沙盒 HTML 必须导出“当前 DOM/Canvas 状态”的真实 3× Chromium PNG，
    // 不能再出现“交互内容需在 UniCell 中查看”的占位图；重复导出命中有界 LRU 缓存。
    const testName65 = 'T65 当前沙盒DOM/Canvas+跨Sheet对象→Chromium 3×PNG→XLSX Drawing';
    let readyListener65 = null;
    try {
      await api('/api/new', { method: 'POST' });
      await loadWorkbook();
      await apiPost('/api/sheet', { op: 'new' });
      await updateSheets();
      const offsheet65 = {
        id: 't65-offsheet-html', sheet: 1, type: 'html', mode: 'cell', r: 2, c: 2,
        x: 8, y: 8, w: 180, h: 80,
        config: { code: '<!doctype html><html><body style="margin:0;background:#217346;color:white">第二工作表图表</body></html>' },
      };
      await apiPost('/api/objects', { op: 'add', sheet: 1, object: offsheet65 });
      const live65 = {
        id: 't65-live-html', sheet: 0, type: 'html', mode: 'abs', r: 1, c: 1,
        x: 48, y: 36, w: 300, h: 120,
        config: { code: `<!doctype html><html><body style="margin:0;background:#17365d;color:white;font:700 18px sans-serif">
          <input id="state" value="初始值"><canvas id="plot" width="180" height="48"></canvas>
          <script>setTimeout(()=>{state.value='动态当前值';const c=plot.getContext('2d');c.fillStyle='#ff9900';c.fillRect(0,0,180,48);c.fillStyle='#fff';c.fillText('LIVE',12,28);},30);<\/script></body></html>` },
      };
      let ready65 = false;
      readyListener65 = (event) => {
        const data = event.data || {};
        if (data.type === 'unicell-html-snapshot-ready' && data.objectId === live65.id) ready65 = true;
      };
      window.addEventListener('message', readyListener65);
      S.objects.push(live65);
      await apiPost('/api/objects', { op: 'add', sheet: 0, object: live65 });
      renderObjects();
      await wait(500);
      const frame65 = htmlObjectIframe(live65);
      await waitUntil(() => ready65, 2500);

      let snapshotAttempts65 = 0;
      const snapshot65 = await retryTransient('T65 DOM snapshot', async () => {
        snapshotAttempts65 += 1;
        const snapshot = await requestHtmlObjectSnapshot(live65, 3500);
        if (!snapshot) throw new Error('empty iframe snapshot');
        return snapshot;
      }, 2, 150);
      const state65 = snapshot65.includes('value="动态当前值"')
        && snapshot65.includes('data:image/png') && !snapshot65.includes("setTimeout(()=>");
      const decodePng65 = (png) => new Promise((resolve, reject) => {
        const image = new Image();
        image.onload = () => resolve(image);
        image.onerror = () => reject(new Error('Chromium returned an undecodable PNG data URL'));
        image.src = png;
      });
      const resetPngCache65 = () => {
        htmlPngExportCache.clear();
        if (typeof htmlPngExportCacheBytes === 'number') htmlPngExportCacheBytes = 0;
      };

      let captureAttempts65 = 0;
      const captureStart65 = performance.now();
      const captured65 = await retryTransient('T65 Chromium PNG capture', async () => {
        captureAttempts65 += 1;
        const png = await htmlObjectToPng(live65, 3);
        if (!png?.startsWith('data:image/png;base64,')) {
          resetPngCache65();
          throw new Error('capture did not return a PNG data URL');
        }
        let image;
        try {
          image = await decodePng65(png);
        } catch (error) {
          resetPngCache65();
          throw error;
        }
        if (image.naturalWidth !== live65.w * 3 || image.naturalHeight !== live65.h * 3) {
          resetPngCache65();
          throw new Error(`capture geometry ${image.naturalWidth}x${image.naturalHeight}`);
        }
        return { png, image };
      }, 3, 180);
      const firstMs65 = performance.now() - captureStart65;
      const png65 = captured65.png;
      const pngImage65 = captured65.image;
      const firstCacheSize65 = htmlPngExportCache.size;

      let cacheAttempts65 = 0;
      const cacheStart65 = performance.now();
      const pngAgain65 = await retryTransient('T65 cached PNG capture', async () => {
        cacheAttempts65 += 1;
        const png = await htmlObjectToPng(live65, 3);
        if (!png?.startsWith('data:image/png;base64,')) throw new Error('cached capture is not PNG');
        return png;
      }, 2, 100);
      const cacheMs65 = performance.now() - cacheStart65;

      let exportCaptureAttempts65 = 0;
      const objectPayload65 = await retryTransient('T65 export object capture', async () => {
        exportCaptureAttempts65 += 1;
        const objects = await collectObjectsForExport();
        if (!Array.isArray(objects)) throw new Error('export capture did not return an object list');
        return objects;
      }, 3, 180);
      const exportedObject65 = objectPayload65.find((o) => o.id === live65.id);
      const exportedOffsheet65 = objectPayload65.find((o) => o.id === offsheet65.id);
      const exportResp65 = await fetch('/api/export?name=html-capture-test', {
        method: 'POST', body: JSON.stringify({ objects: objectPayload65 }),
      });
      const xlsx65 = new Uint8Array(await exportResp65.arrayBuffer());
      const zipText65 = new TextDecoder('latin1').decode(xlsx65);
      const capture65 = png65.startsWith('data:image/png;base64,') && !!pngImage65
        && pngImage65.naturalWidth === live65.w * 3 && pngImage65.naturalHeight === live65.h * 3;
      const cache65 = firstCacheSize65 > 0 && pngAgain65 === png65 && cacheMs65 < Math.max(500, firstMs65 * 0.5);
      const drawing65 = exportResp65.ok && xlsx65[0] === 0x50 && xlsx65[1] === 0x4b
        && zipText65.includes('xl/media/unicellImage1.png') && zipText65.includes('xl/drawings/unicellDrawing1.xml')
        && zipText65.includes('xl/media/unicellImage2.png') && zipText65.includes('xl/drawings/unicellDrawing2.xml')
        && exportedObject65 && exportedObject65.mode === 'abs'
        && exportedOffsheet65 && exportedOffsheet65.sheet === 1;
      ok(testName65, state65 && capture65 && cache65 && drawing65,
        `state=${state65} ready=${ready65} frame=${!!frame65}/${frame65 && frame65.srcdoc.includes('unicell-html-snapshot-response')}`
          + ` png=${pngImage65 && pngImage65.naturalWidth}x${pngImage65 && pngImage65.naturalHeight}`
          + ` attempts=${snapshotAttempts65}/${captureAttempts65}/${cacheAttempts65}/${exportCaptureAttempts65}`
          + ` first=${firstMs65.toFixed(0)}ms cache=${cacheMs65.toFixed(0)}ms/${firstCacheSize65} drawing=${drawing65}`);
    } catch (error) {
      ok(testName65, false, `isolated capture failure: ${error?.message || String(error)}`);
    } finally {
      if (readyListener65) window.removeEventListener('message', readyListener65);
    }

    // T66 SmartArt 富文本详情首次打开：removing/detailsRow 都为 null 时不能误判为关闭。
    const smartart66 = {
      id: 't66-smartart', sheet: S.sheet, type: 'svg', mode: 'abs', r: 2, c: 2,
      x: 20, y: 20, w: 240, h: 120,
      config: { nativeDrawing: { kind: 'smartart', name: 'RegressionSmartArt', model: {
        layout: 'urn:unicell:selftest',
        nodes: [{
          id: 't66-root', parentId: null, kind: 'node', order: 0, text: 'Rich root',
          paragraphs: [{ text: 'Rich root', runs: [{
            kind: 'r', text: 'Rich root', font: 'Arial', size: 12, bold: true,
            italic: false, underline: 'sng', color: '#0070C0', alpha: 0.8,
          }] }],
        }],
      } } },
    };
    openNativeDrawingEditor(smartart66);
    const dialog66 = document.getElementById('native-drawing-dialog');
    const format66 = dialog66 && dialog66.querySelector('[data-nodes] [data-a="format"]');
    if (format66) format66.click();
    const details66 = dialog66 && dialog66.querySelector('[data-smart-text-details]');
    const smartRich66 = !!details66 && !details66.hidden
      && details66.querySelectorAll('.native-text-paragraph').length === 1
      && details66.querySelector('[data-k="runText"]')?.value === 'Rich root'
      && details66.querySelector('[data-k="font"]')?.value === 'Arial'
      && details66.querySelector('[data-k="underline"]')?.value === 'sng';
    dialog66?.querySelector('[data-close]')?.click();
    ok('T66 SmartArt节点富文本详情首次点击即可打开', smartRich66,
      `dialog=${!!dialog66} format=${!!format66} visible=${!!details66 && !details66.hidden}`);

    // T66C 原生图表深编 UI：轴/主次绘图区/标签/趋势线/误差线均生成差量模型，未知字段不丢失。
    const chart66c = {
      id: 't66c-chart', sheet: S.sheet, type: 'html', mode: 'abs', r: 2, c: 2,
      x: 20, y: 20, w: 480, h: 260,
      config: { nativeDrawing: { kind: 'chart', name: 'RegressionChart', model: {
        chartType: 'bar', title: 'Sales', legend: { show: true, position: 'right', vendorLegend: 'keep' },
        plots: [{ index: 0, chartType: 'bar', axisIds: [10, 20], axisGroup: 'primary', grouping: 'clustered', barDirection: 'col', gapWidth: 150, overlap: 0, smooth: null, varyColors: false, dataLabels: null, vendorPlot: 'keep' }],
        axes: [
          { id: 10, axisType: 'category', position: 'bottom', delete: false, title: '', scaling: { orientation: 'minMax', min: null, max: null, logBase: null }, numberFormat: null, majorGridlines: false, minorGridlines: false, majorTickMark: 'out', minorTickMark: 'none', tickLabelPosition: 'nextTo', crossAxisId: 20, crosses: 'autoZero', crossesAt: null, crossBetween: '', auto: true, labelAlignment: 'ctr', labelOffset: 100, tickLabelSkip: null, tickMarkSkip: null, noMultiLevelLabels: false, majorUnit: null, minorUnit: null, baseTimeUnit: '', majorTimeUnit: '', minorTimeUnit: '', displayUnits: null, vendorAxis: 'category-keep' },
          { id: 20, axisType: 'value', position: 'left', delete: false, title: 'Amount', scaling: { orientation: 'minMax', min: 0, max: 100, logBase: null, vendorScale: 'keep' }, numberFormat: { code: '0.00', sourceLinked: false, vendorFormat: 'keep' }, majorGridlines: true, minorGridlines: false, majorTickMark: 'out', minorTickMark: 'none', tickLabelPosition: 'nextTo', crossAxisId: 10, crosses: 'autoZero', crossesAt: null, crossBetween: 'between', auto: null, labelAlignment: '', labelOffset: null, tickLabelSkip: null, tickMarkSkip: null, noMultiLevelLabels: null, majorUnit: 10, minorUnit: 2, baseTimeUnit: '', majorTimeUnit: '', minorTimeUnit: '', displayUnits: { builtIn: 'thousands', custom: null, showLabel: true, label: 'K', vendorUnits: 'keep' }, vendorAxis: 'value-keep' },
        ],
        series: [{ name: 'North', bindingMode: 'embedded', categories: ['A', 'B'], values: [10, 20], categoryFormula: '', valueFormula: '', color: '#4472C4', pointOverrides: [], plotIndex: 0, plotType: 'bar', axisIds: [10, 20], axisGroup: 'primary',
          dataLabels: { delete: null, position: 'center', numberFormat: null, separator: ', ', showLegendKey: null, showValue: false, showCategoryName: true, showSeriesName: null, showPercent: null, showBubbleSize: null, showLeaderLines: null, showDataLabelsRange: null, labels: [], vendorLabels: 'keep' },
          trendlines: [{ index: 0, name: 'Forecast', type: 'poly', order: 2, period: null, forward: 1, backward: null, intercept: null, displayRSquared: false, displayEquation: true, label: 'Fit', vendorTrend: 'keep' }],
          errorBars: [{ index: 0, direction: 'y', barType: 'both', valueType: 'fixedVal', noEndCap: false, value: 2, plus: null, minus: null, vendorError: 'keep' }],
          vendorSeries: 'keep',
        }],
        vendorChart: 'keep',
      } } },
    };
    openNativeDrawingEditor(chart66c);
    const dialog66c = document.getElementById('native-drawing-dialog');
    const change66c = (control, value, eventName = control?.tagName === 'SELECT' ? 'change' : 'input') => {
      if (!control) return;
      if (control.type === 'checkbox') control.checked = !!value;
      else control.value = value;
      control.dispatchEvent(new Event(eventName, { bubbles: true }));
    };
    const valueAxis66c = dialog66c?.querySelector('[data-axes] .native-axis-card:nth-child(2)');
    change66c(valueAxis66c?.querySelector('[data-k=title]'), 'Revenue');
    change66c(valueAxis66c?.querySelector('[data-k=min]'), '-5');
    change66c(valueAxis66c?.querySelector('[data-k=numFmtCode]'), '$#,##0');
    change66c(dialog66c?.querySelector('[data-plots] [data-k=gapWidth]'), '120');
    dialog66c?.querySelector('[data-series-row="1"] [data-a=advanced]')?.click();
    change66c(dialog66c?.querySelector('[data-series-labels] [data-dl=showValue]'), 'true');
    change66c(dialog66c?.querySelector('[data-series-trendlines] [data-k=forward]'), '3');
    change66c(dialog66c?.querySelector('[data-series-errorbars] [data-k=noEndCap]'), 'true');
    dialog66c?.querySelector('[data-add-secondary]')?.click();
    change66c(dialog66c?.querySelector('[data-series-row="1"] [data-k=axisGroup]'), 'secondary');
    let collected66c = null;
    let differential66c = null;
    try { collected66c = dialog66c?._nativeCollect?.(); } catch (error) { console.error('T66C collect', error); }
    try { differential66c = dialog66c?._nativeDiff?.(); } catch (error) { console.error('T66C diff', error); }
    const valueAxisPatch66c = differential66c?.axes?.find((axis) => axis.id === 20);
    const primaryPlotPatch66c = differential66c?.plots?.find((plot) => plot.index === 0);
    const seriesPatch66c = differential66c?.series?.[0];
    const differentialOk66c = !!differential66c
      && valueAxisPatch66c?.title === 'Revenue' && valueAxisPatch66c?.scaling?.min === -5
      && valueAxisPatch66c?.numberFormat?.code === '$#,##0'
      && !Object.hasOwn(valueAxisPatch66c, 'vendorAxis') && !Object.hasOwn(valueAxisPatch66c, 'displayUnits')
      && primaryPlotPatch66c?.gapWidth === 120 && !Object.hasOwn(primaryPlotPatch66c, 'vendorPlot')
      && seriesPatch66c?.dataLabels?.showValue === true && !Object.hasOwn(seriesPatch66c.dataLabels, 'vendorLabels')
      && seriesPatch66c?.trendlines?.[0]?.forward === 3 && !Object.hasOwn(seriesPatch66c.trendlines[0], 'vendorTrend')
      && seriesPatch66c?.errorBars?.[0]?.noEndCap === true && !Object.hasOwn(seriesPatch66c.errorBars[0], 'vendorError');
    const deepChart66c = !!collected66c
      && collected66c.chartType === 'combo' && collected66c.plots.length === 2 && collected66c.axes.length === 4
      && collected66c.axes[1].title === 'Revenue' && collected66c.axes[1].scaling.min === -5
      && collected66c.axes[1].numberFormat.code === '$#,##0' && collected66c.axes[1].vendorAxis === 'value-keep'
      && collected66c.axes[1].scaling.vendorScale === 'keep' && collected66c.axes[1].displayUnits.vendorUnits === 'keep'
      && collected66c.plots[0].gapWidth === 120 && collected66c.plots[0].vendorPlot === 'keep'
      && collected66c.plots[1].axisGroup === 'secondary'
      && collected66c.series[0].plotIndex === 1 && collected66c.series[0].axisGroup === 'secondary'
      && collected66c.series[0].dataLabels.showValue === true && collected66c.series[0].dataLabels.vendorLabels === 'keep'
      && collected66c.series[0].trendlines[0].forward === 3 && collected66c.series[0].trendlines[0].vendorTrend === 'keep'
      && collected66c.series[0].errorBars[0].noEndCap === true && collected66c.series[0].errorBars[0].vendorError === 'keep'
      && collected66c.series[0].vendorSeries === 'keep' && collected66c.vendorChart === 'keep'
      && differentialOk66c;
    dialog66c?.querySelector('[data-close]')?.click();
    ok('T66C 原生图表坐标轴/标签/趋势线/误差线深编', deepChart66c,
      `dialog=${!!dialog66c} plots=${collected66c?.plots?.length || 0} axes=${collected66c?.axes?.length || 0} seriesPlot=${collected66c?.series?.[0]?.plotIndex} diff=${differentialOk66c}`);

    // T67 Excel 数据验证：完整规则字段、稳定 ID、结构编辑跟随、UI 回读及 XLSX 重导入。
    await apiPost('/api/dv', { sheet: S.sheet, op: 'clear' });
    const add67 = await apiPost('/api/dv', { sheet: S.sheet, op: 'add', rule: {
      id: '', sqref: 'A70:A75', type: 'whole', operator: 'between',
      allowBlank: true, inCellDropdown: true, showInputMessage: true, showErrorMessage: true,
      errorStyle: 'stop', promptTitle: '请输入整数', prompt: '允许 1 到 10',
      errorTitle: '无效值', error: '只能输入 1 到 10 的整数', formula1: '1', formula2: '10',
    } });
    await apiPost('/api/dv', { sheet: S.sheet, op: 'add', rule: {
      id: '', sqref: 'B70:B72', type: 'list', operator: null,
      allowBlank: false, inCellDropdown: true, showInputMessage: false, showErrorMessage: true,
      errorStyle: 'stop', formula1: '"是,否"', formula2: null,
    } });
    await apiPost('/api/rows', { sheet: S.sheet, op: 'insert', row: 72, count: 2 });
    const list67 = await apiPost('/api/dv', { sheet: S.sheet, op: 'list' });
    openDvDialog();
    await waitUntil(() => document.querySelectorAll('#dv-rule-list .dv-rule-item').length === 2, 3000);
    document.querySelector('#dv-rule-list .dv-rule-item')?.click();
    const ui67 = $('dv-sqref').value === 'A70:A77' && $('dv-type').value === 'whole'
      && $('dv-operator').value === 'between' && $('dv-formula1').value === '1'
      && $('dv-formula2').value === '10' && $('dv-show-error').checked;
    $('dv-close').click();
    setCursor(70, 2);
    const arrowReady67 = await waitUntil(() => !$('dv-cell-dropdown')?.hidden, 3000);
    $('dv-cell-dropdown')?.click();
    const listReady67 = await waitUntil(() => document.querySelectorAll('#dv-cell-list button').length === 2, 3000);
    document.querySelectorAll('#dv-cell-list button')[1]?.click();
    let dropdownCell67 = null;
    const dropdown67 = await waitUntil(async () => {
      dropdownCell67 = await api(`/api/cell?sheet=${S.sheet}&row=70&col=2`);
      return dropdownCell67.content === '否';
    }, 3000);
    const export67 = await fetch('/api/export?name=dv-selftest', { method: 'POST', body: '{}' });
    const xlsx67 = await export67.arrayBuffer();
    await api('/api/new', { method: 'POST' });
    await api('/api/import', { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: xlsx67 });
    await loadWorkbook();
    const roundtrip67 = await apiPost('/api/dv', { sheet: 0, op: 'list' });
    const rule67 = roundtrip67.rules?.find((rule) => rule.type === 'whole');
    const dvOk67 = !!add67.rule?.id && list67.rules?.[0]?.sqref === 'A70:A77' && ui67
      && list67.rules?.find((rule) => rule.type === 'list')?.sqref === 'B70:B74' && dropdown67
      && rule67?.sqref === 'A70:A77' && rule67?.type === 'whole'
      && rule67?.formula1 === '1' && rule67?.formula2 === '10'
      && rule67?.promptTitle === '请输入整数' && rule67?.errorStyle === 'stop';
    ok('T67 Excel数据验证编辑+结构跟随+UI+XLSX原生往返', dvOk67,
      `id=${add67.rule?.id} moved=${list67.rules?.[0]?.sqref} ui=${ui67} arrow=${arrowReady67} list=${listReady67} dropdown=${dropdown67} cell=${JSON.stringify(dropdownCell67)} roundtrip=${JSON.stringify(rule67)}`);

    // T68 PivotCache API must remain safe for ordinary workbooks without native pivot caches.
    // Native-cache mutation and OOXML byte preservation are covered by the Rust fixture tests.
    const pivotList68 = await apiPost('/api/pivot-caches', { op: 'list' });
    const pivotReset68 = await apiPost('/api/pivot-caches', {
      op: 'reset', part: 'xl/pivotCache/nonexistent-selftest.xml',
    });
    const pivotOk68 = Array.isArray(pivotList68.caches) && pivotList68.caches.length === 0
      && Array.isArray(pivotReset68.caches) && pivotReset68.caches.length === 0;
    ok('T68 PivotCache API 空工作簿 list/reset 契约', pivotOk68,
      `list=${JSON.stringify(pivotList68)} reset=${JSON.stringify(pivotReset68)}`);

    // T69 合并单元格必须只有一个逻辑编辑目标。点击/双击任意从属格都应解析到
    // 左上角；选框和编辑框覆盖完整合并区；提交及 Enter 导航不得写入隐藏从属格。
    await apiPost('/api/input', { sheet: S.sheet, row: 80, col: 2, value: 'merge-anchor' });
    await apiPost('/api/style', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 80, c1: 2, path: 'font.b', value: 'true',
    });
    const merged69 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'merge',
    });
    S.merges = merged69.merges || [];
    setCursor(80, 2);
    const [subX69, subY69] = cellXY(81, 4);
    down(subX69, subY69); up();
    const selected69 = normSel();
    const hit69 = S.cur.r === 80 && S.cur.c === 2
      && selected69.r0 === 80 && selected69.c0 === 2
      && selected69.r1 === 81 && selected69.c1 === 4;

    gridScroll.dispatchEvent(new MouseEvent('dblclick', {
      clientX: subX69, clientY: subY69, bubbles: true, cancelable: true,
    }));
    const editReady69 = await waitUntil(() => S.editing && S.editCell?.r === 80 && S.editCell?.c === 2, 2000);
    const expectedWidth69 = colX(4) + colWidth(4) - colX(2) + 1;
    const expectedHeight69 = rowY(81) + rowHeight(81) - rowY(80) + 1;
    const geometry69 = editReady69
      && Math.abs(parseFloat(editor.style.left) - (colX(2) - 1)) < 0.6
      && Math.abs(parseFloat(editor.style.top) - (rowY(80) - 1)) < 0.6
      && Math.abs(parseFloat(editor.style.width) - expectedWidth69) < 0.6
      && Math.abs(parseFloat(editor.style.height) - expectedHeight69) < 0.6;
    if (editReady69) {
      editor.value = 'merged-editor-value';
      editor.dispatchEvent(new Event('input', { bubbles: true }));
      editor.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    }
    const moved69 = await waitUntil(() => !S.editing && S.cur.r === 82 && S.cur.c === 2, 3000);
    const anchor69 = await api(`/api/cell?sheet=${S.sheet}&row=80&col=2`);
    await apiPost('/api/merge', { sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'unmerge' });
    const hidden69 = await api(`/api/cell?sheet=${S.sheet}&row=81&col=4`);
    const edit69 = anchor69.content === 'merged-editor-value' && anchor69.style?.b === true
      && hidden69.content === '';

    // 公式栏走独立的 focus/input/Enter 路径，也必须仍提交到合并锚点。
    const remerged69 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'merge',
    });
    S.merges = remerged69.merges || [];
    setCursor(80, 2);
    const [formulaX69, formulaY69] = cellXY(81, 4);
    down(formulaX69, formulaY69); up();
    formulaInput.focus();
    await waitUntil(() => S.editing && S.editCell?.r === 80 && S.editCell?.c === 2, 2000);
    formulaInput.value = 'merged-formula-bar-value';
    formulaInput.dispatchEvent(new Event('input', { bubbles: true }));
    formulaInput.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true }));
    await waitUntil(() => !S.editing, 3000);
    const formulaAnchor69 = await api(`/api/cell?sheet=${S.sheet}&row=80&col=2`);
    await apiPost('/api/merge', { sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'unmerge' });
    const formulaHidden69 = await api(`/api/cell?sheet=${S.sheet}&row=81&col=4`);
    const formula69 = formulaAnchor69.content === 'merged-formula-bar-value'
      && formulaHidden69.content === '';

    // 服务端也必须做最后一道防线：即使旧前端/外部调用者直接把坐标写到合并区
    // 的右下角，仍只能落到锚点；取消合并后从属格必须保持为空。
    const guardMerge69 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'merge',
    });
    S.merges = guardMerge69.merges || [];
    await apiPost('/api/input', { sheet: S.sheet, row: 81, col: 4, value: 'server-guard-value' });
    const guardedRead69 = await api(`/api/cell?sheet=${S.sheet}&row=81&col=4`);
    await apiPost('/api/merge', { sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'unmerge' });
    const guardedAnchor69 = await api(`/api/cell?sheet=${S.sheet}&row=80&col=2`);
    const guardedHidden69 = await api(`/api/cell?sheet=${S.sheet}&row=81&col=4`);
    const guard69 = guardedRead69.content === 'server-guard-value'
      && guardedRead69.row === 80 && guardedRead69.col === 2 && guardedRead69.mergedAnchor === true
      && guardedAnchor69.content === 'server-guard-value' && guardedHidden69.content === '';

    const rangeMerge69 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'merge',
    });
    S.merges = rangeMerge69.merges || [];
    await apiPost('/api/inputrange', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4,
      row: 81, col: 4, value: 'range-guard-value',
    });
    await apiPost('/api/merge', { sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'unmerge' });
    const rangeCells69 = await Promise.all([
      [80, 2], [80, 3], [80, 4], [81, 2], [81, 3], [81, 4],
    ].map(([row, col]) => api(`/api/cell?sheet=${S.sheet}&row=${row}&col=${col}`)));
    const rangeGuard69 = rangeCells69[0].content === 'range-guard-value'
      && rangeCells69.slice(1).every((cell) => cell.content === '');

    const batchMerge69 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'merge',
    });
    S.merges = batchMerge69.merges || [];
    let batchRejected69 = false;
    try {
      await apiPost('/api/batch', { sheet: S.sheet, cells: [
        { r: 80, c: 2, v: 'batch-anchor' }, { r: 81, c: 4, v: 'batch-subordinate' },
      ] });
    } catch (error) {
      batchRejected69 = /merged cell|合并/i.test(String(error?.message || error));
    }
    await apiPost('/api/merge', { sheet: S.sheet, r0: 80, c0: 2, r1: 81, c1: 4, op: 'unmerge' });
    const batchAnchor69 = await api(`/api/cell?sheet=${S.sheet}&row=80&col=2`);
    const batchHidden69 = await api(`/api/cell?sheet=${S.sheet}&row=81&col=4`);
    const batchGuard69 = batchRejected69 && batchAnchor69.content === 'range-guard-value'
      && batchHidden69.content === '';
    ok('T69 合并区命中锚点+整区选框/编辑框+编辑器/公式栏不写从属格',
      hit69 && geometry69 && moved69 && edit69 && formula69 && guard69 && rangeGuard69 && batchGuard69,
      `hit=${hit69} sel=${JSON.stringify(selected69)} editReady=${editReady69} geometry=${geometry69}`
        + ` moved=${moved69} anchor=${JSON.stringify(anchor69)}`
        + ` hidden=${JSON.stringify(hidden69)} formula=${formula69}/${JSON.stringify(formulaHidden69)}`
        + ` guard=${guard69}/${JSON.stringify(guardedRead69)}/${JSON.stringify(guardedHidden69)}`
        + ` range=${rangeGuard69}/${JSON.stringify(rangeCells69)} batch=${batchGuard69}/${batchRejected69}`);

    // T70 普通 shared string 在公式栏选中局部字符后才提升为富文本；公式不能提升。
    const plain70 = '14212323243966god';
    await apiPost('/api/input', { sheet: S.sheet, row: 85, col: 2, value: plain70 });
    setCursor(85, 2);
    await waitUntil(() => formulaInput.value === plain70, 2000);
    formulaInput.focus();
    await waitUntil(() => S.editing && S.editCell?.r === 85 && S.editCell?.c === 2, 2000);
    formulaInput.setSelectionRange(3, 9);
    formulaInput.dispatchEvent(new Event('select', { bubbles: true }));
    const promoted70 = applyRichFormat('color', '#FF0000', false);
    const draftRuns70 = richFormulaRunsFromDom();
    const draft70 = promoted70 && S.editHasRichText && richRunsText(draftRuns70) === plain70
      && draftRuns70.some((run) => run.text === plain70.slice(3, 9) && run.color === '#FF0000');
    await commitEdit();
    await waitUntil(() => !S.editing, 3000);
    const saved70 = await api(`/api/cell?sheet=${S.sheet}&row=85&col=2`);
    const savedRuns70 = saved70.richText || [];
    const roundtrip70 = saved70.content === plain70 && saved70.hasRichText
      && richRunsText(savedRuns70) === plain70
      && savedRuns70.some((run) => run.text === plain70.slice(3, 9) && run.color === '#FF0000');

    // 主题色 token 必须经过 DOM 编辑层原样保留，视觉颜色与存储 token 分离。
    const theme70 = normalizeRichRuns([{ text: 'theme', color: [4, 0.25], resolvedColor: '#7795CB' }]);
    renderRichFormulaRuns(theme70);
    const themeDom70 = richFormulaRunsFromDom();
    const themeApi70 = richRunsForApi(themeDom70);
    const themeToken70 = Array.isArray(themeDom70[0]?.color) && themeDom70[0].color[0] === 4
      && themeDom70[0].resolvedColor === '#7795CB'
      && Array.isArray(themeApi70[0]?.color) && themeApi70[0].color[1] === 0.25;

    await apiPost('/api/input', { sheet: S.sheet, row: 86, col: 2, value: '=1+1' });
    setCursor(86, 2);
    await waitUntil(() => formulaInput.value === '=1+1', 2000);
    formulaInput.focus();
    await waitUntil(() => S.editing && S.editCell?.r === 86 && S.editCell?.c === 2, 2000);
    formulaInput.setSelectionRange(1, 3);
    formulaInput.dispatchEvent(new Event('select', { bubbles: true }));
    const formulaStayedPlain70 = !promotePlainConstantToRichEdit() && !S.editHasRichText;
    cancelEditUI();
    await updateFormulaBar();
    ok('T70 普通文本局部字符→富文本runs；主题色token保留；公式不提升',
      draft70 && roundtrip70 && themeToken70 && formulaStayedPlain70,
      `draft=${draft70}/${JSON.stringify(draftRuns70)} saved=${roundtrip70}/${JSON.stringify(savedRuns70)}`
        + ` theme=${themeToken70}/${JSON.stringify(themeApi70)} formulaPlain=${formulaStayedPlain70}`);

    // T71 双击富文本单元格必须在原单元格上编辑；保存后再次双击仍能进入，runs 不拍平。
    const merged71 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 85, c0: 2, r1: 86, c1: 3, op: 'merge',
    });
    S.merges = merged71.merges || [];
    S.cellsCache.delete('85,2');
    setCursor(85, 2);
    scheduleRefresh(true);
    await waitUntil(() => formulaInput.value === plain70, 3000);
    const dblclick71 = () => {
      const target = mergedCellRect(85, 2);
      const viewport = gridScroll.getBoundingClientRect();
      gridScroll.dispatchEvent(new MouseEvent('dblclick', {
        bubbles: true,
        clientX: viewport.left + target.left - gridScroll.scrollLeft + Math.min(8, target.width / 2),
        clientY: viewport.top + target.top - gridScroll.scrollTop + Math.min(8, target.height / 2),
      }));
    };
    dblclick71();
    const opened71 = await waitUntil(() => S.editing && S.editHasRichText
      && S.richEditSurface === richCellEditor && richCellEditor.style.display === 'block'
      && richCellEditor.textContent === plain70, 3000);
    await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    const editRect71 = mergedCellRect(85, 2);
    const geometry71 = Math.abs(parseFloat(richCellEditor.style.left) - (editRect71.left - 1)) < 0.5
      && Math.abs(parseFloat(richCellEditor.style.top) - (editRect71.top - 1)) < 0.5
      && richCellEditor.offsetWidth >= editRect71.width
      && richCellEditor.offsetHeight >= editRect71.height
      && richCellEditor.scrollWidth <= richCellEditor.clientWidth + 1
      && richCellEditor.scrollHeight <= richCellEditor.clientHeight + 1;
    const geometryProbe71 = {
      editRect: editRect71,
      style: { left: richCellEditor.style.left, top: richCellEditor.style.top,
        width: richCellEditor.style.width, height: richCellEditor.style.height },
      offset: [richCellEditor.offsetWidth, richCellEditor.offsetHeight],
      client: [richCellEditor.clientWidth, richCellEditor.clientHeight],
      scroll: [richCellEditor.scrollWidth, richCellEditor.scrollHeight],
    };
    const beforeRuns71 = richRunsFromDom(richCellEditor);
    const redSpan71 = [...richCellEditor.querySelectorAll('.rich-run')]
      .find((span) => JSON.parse(span.dataset.colorToken || 'null') === '#FF0000');
    if (redSpan71) redSpan71.textContent += 'X';
    richCellEditor.dispatchEvent(new Event('input', { bubbles: true }));
    const changedText71 = plain70.slice(0, 9) + 'X' + plain70.slice(9);
    await commitEdit();
    await waitUntil(() => !S.editing, 3000);
    const saved71 = await api(`/api/cell?sheet=${S.sheet}&row=85&col=2`);
    const savedStyle71 = saved71.content === changedText71 && saved71.hasRichText
      && (saved71.richText || []).some((run) => run.text.includes('X') && run.color === '#FF0000')
      && beforeRuns71.length >= 3 && (saved71.richText || []).length >= 3;

    // 走同一个真实 dblclick handler 再开一次，防止上次 contenteditable/selection 状态把入口卡死。
    dblclick71();
    const reopened71 = await waitUntil(() => S.editing && S.editHasRichText
      && S.richEditSurface === richCellEditor && richCellEditor.style.display === 'block'
      && richCellEditor.textContent === changedText71, 3000);
    const reopenedRuns71 = reopened71 ? richRunsFromDom(richCellEditor) : [];
    const reopenStyle71 = reopenedRuns71.some((run) => run.text.includes('X') && run.color === '#FF0000');
    cancelEditUI();
    const unmerged71 = await apiPost('/api/merge', { sheet: S.sheet, r0: 85, c0: 2, r1: 86, c1: 3, op: 'unmerge' });
    S.merges = unmerged71.merges || [];
    ok('T71 双击原单元格富文本编辑+完整合并区几何+提交后可再次双击',
      opened71 && geometry71 && savedStyle71 && reopened71 && reopenStyle71,
      `open=${opened71} geometry=${geometry71}/${JSON.stringify(geometryProbe71)}`
        + ` saved=${savedStyle71}/${JSON.stringify(saved71.richText)}`
        + ` reopen=${reopened71}/${JSON.stringify(reopenedRuns71)}`);

    // T72 Excel 式本地生命周期：dirty/beforeunload、IndexedDB 自动恢复、handle 原位写入、
    // Excel/CSV/udoc/无损 HTML 另存为映射与最近文件 Blob 回退。测试不弹系统文件选择器。
    const lifecycleBefore72 = {
      extension: S.excelExtension, name: S.localFileName, handle: S.fileHandle,
      dirty: S.dirty, revision: S.dirtyRevision,
    };
    S.excelExtension = 'xlsm';
    const picker72 = buildSavePickerOptions('宏测试.xlsx');
    const macroMime72 = excelMime('xlsm');
    const pickerExtensions72 = picker72.types.flatMap((type) => Object.values(type.accept).flat());
    const pickerOk72 = picker72.suggestedName.endsWith('.xlsm')
      && picker72.excludeAcceptAllOption === true
      && picker72.types[0].accept[macroMime72]?.[0] === '.xlsm'
      && ['.xlsm', '.udoc', '.html', '.csv'].every((ext) => pickerExtensions72.includes(ext))
      && workbookFormatFromName('报告.udoc') === 'udoc'
      && workbookFormatFromName('报告.html') === 'html'
      && workbookFormatFromName('报告.csv') === 'csv';
    S.excelExtension = 'xlsx';
    S.localFileName = 'lifecycle.udoc';
    markWorkbookDirty('T72');
    const dirtyEvent72 = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(dirtyEvent72);
    const guardDirty72 = dirtyEvent72.defaultPrevented;
    const autosave72 = await writeAutosaveSnapshot();
    const scopeA72 = currentStorageScope();
    const autosaveKey72 = currentAutosaveKey();
    const recentKey72 = scopedLocalRecordId('file', 'lifecycle.xlsx');
    const storedAutosave72 = await localDbGet('autosaves', autosaveKey72);
    const autosaveOk72 = !!autosave72?.blob && autosave72.blob.size > 200
      && storedAutosave72?.revision === autosave72.revision
      && storedAutosave72.blob.size === autosave72.blob.size
      && autosave72.name === 'lifecycle.xlsx' && S.dirty;
    const udocSaveBlob72 = await createPersistenceBlob('udoc');
    const htmlSaveBlob72 = await createPersistenceBlob('html');
    const csvSaveBlob72 = await createPersistenceBlob('csv');
    const udocMagic72 = new TextDecoder().decode(await udocSaveBlob72.slice(0, 5).arrayBuffer());
    const htmlHead72 = await htmlSaveBlob72.slice(0, 256).text();
    const csvHead72 = new Uint8Array(await csvSaveBlob72.slice(0, 3).arrayBuffer());
    const alternateSaveOk72 = udocMagic72 === 'UDOC3'
      && /<!doctype html/i.test(htmlHead72)
      && csvHead72[0] === 0xEF && csvHead72[1] === 0xBB && csvHead72[2] === 0xBF
      && udocSaveBlob72.type === UDOC_MIME && htmlSaveBlob72.type === LOSSLESS_HTML_MIME
      && csvSaveBlob72.type === CSV_MIME;

    let written72 = null;
    let closed72 = false;
    const fakeHandle72 = {
      name: 'lifecycle.xlsx',
      queryPermission: async () => 'granted',
      createWritable: async () => ({
        write: async (blob) => { written72 = blob; },
        close: async () => { closed72 = true; },
      }),
    };
    await writeWorkbookToHandle(fakeHandle72, autosave72.blob);
    await addRecentFile(fakeHandle72.name, autosave72.blob, fakeHandle72);
    const recent72 = await localDbGet('recent', recentKey72);
    const handleOk72 = written72 === autosave72.blob && closed72;
    const recentOk72 = recent72?.name === 'lifecycle.xlsx'
      && recent72.blob?.size === autosave72.blob.size;

    // A second server storage namespace must not discover A's autosave/recent
    // entries, nor a legacy unscoped `autosaves/current` record. Clearing B's
    // recent list must leave A's private record intact.
    const legacyAutosave72 = await localDbGet('autosaves', LOCAL_AUTOSAVE_KEY);
    await localDbPut('autosaves', {
      id: LOCAL_AUTOSAVE_KEY, name: 'LEGACY_SECRET.xlsx', updatedAt: Date.now(), blob: autosave72.blob,
    });
    S.storageScope = `selftest-${crypto.randomUUID?.() || Date.now()}`;
    S.recoveryRecord = null;
    const isolatedRecovery72 = !await refreshRecoveryState();
    const isolatedRecent72 = !(await localDbGetAll('recent')).filter(isCurrentStorageRecord).length;
    await clearCurrentRecentFiles();
    S.storageScope = scopeA72;
    S.recoveryRecord = autosave72;
    const aRecentSurvived72 = !!await localDbGet('recent', recentKey72);
    if (legacyAutosave72) await localDbPut('autosaves', legacyAutosave72);
    else await localDbDelete('autosaves', LOCAL_AUTOSAVE_KEY);
    const storageIsolation72 = isolatedRecovery72 && isolatedRecent72 && aRecentSurvived72;

    await markWorkbookClean({ name: 'lifecycle.xlsx', handle: fakeHandle72, clearRecovery: true });
    const cleanEvent72 = new Event('beforeunload', { cancelable: true });
    window.dispatchEvent(cleanEvent72);
    const cleanOk72 = !S.dirty && !cleanEvent72.defaultPrevented
      && !await localDbGet('autosaves', autosaveKey72);
    await localDbDelete('recent', recentKey72);
    S.excelExtension = lifecycleBefore72.extension;
    S.localFileName = lifecycleBefore72.name;
    S.fileHandle = lifecycleBefore72.handle;
    S.dirty = lifecycleBefore72.dirty;
    S.dirtyRevision = lifecycleBefore72.revision;
    updateFileLifecycleUI();
    ok('T72 本地保存生命周期：Excel/CSV/udoc/HTML 另存为+xlsm映射+dirty保护+会话隔离恢复+handle写入+最近文件',
      pickerOk72 && guardDirty72 && autosaveOk72 && handleOk72 && recentOk72
        && alternateSaveOk72 && storageIsolation72 && cleanOk72,
      `picker=${pickerOk72}/${JSON.stringify(picker72)} guard=${guardDirty72}`
        + ` autosave=${autosaveOk72}/${autosave72?.blob?.size}/${storedAutosave72?.blob?.size}`
        + ` alternate=${alternateSaveOk72}/${udocSaveBlob72.size}/${htmlSaveBlob72.size}/${csvSaveBlob72.size}`
        + ` handle=${handleOk72} recent=${recentOk72} isolated=${storageIsolation72}`
        + ` clean=${cleanOk72}`);

    // T73 跨应用高保真剪贴板：Chrome web custom MIME + HTML + 文本三级写入；
    // 自定义载荷读取、选择性粘贴请求，以及 Excel HTML/CSS/合并/富文本解析。
    await apiPost('/api/input', { sheet: S.sheet, row: 90, col: 1, value: 'clipboard' });
    const serverClipboard73 = await apiPost('/api/copy', {
      sheet: S.sheet, r0: 90, c0: 1, r1: 90, c1: 1,
    });
    let writtenItems73 = null;
    class FakeClipboardItem73 {
      constructor(parts) { this.parts = parts; this.types = Object.keys(parts); }
      async getType(type) { return this.parts[type]; }
    }
    const fakeWriter73 = { write: async (items) => { writtenItems73 = items; } };
    const tier73 = await writeRichClipboard(serverClipboard73, fakeWriter73, FakeClipboardItem73);
    const types73 = writtenItems73?.[0]?.types || [];
    const mimeOk73 = tier73 === 'custom+html+text'
      && types73.includes('text/plain') && types73.includes('text/html')
      && types73.includes(UNICELL_CLIPBOARD_MIME);

    const customSource73 = await readRichClipboard({ read: async () => writtenItems73 });
    const customOk73 = customSource73.source === 'unicell'
      && customSource73.unicell?.kind === 'unicell-range'
      && customSource73.text === serverClipboard73.tsv;
    let pasteCall73 = null;
    const fakeState73 = { sheet: 0, cur: { r: 91, c: 3 }, cutPending: null };
    await performRichPaste(customSource73, 'transpose-formats', async (path, body) => {
      pasteCall73 = { path, body };
      return { rows: 1, cols: 1 };
    }, fakeState73);
    const pasteSpecialOk73 = pasteCall73?.path === '/api/paste'
      && pasteCall73.body.special === 'transpose-formats'
      && pasteCall73.body.row === 91 && pasteCall73.body.col === 3
      && pasteCall73.body.unicell?.version === 1;

    const excelHtml73 = `<html><head><style>
      td.xl73 { font: bold 12pt Calibri; color:#123456; background:#ffeeaa;
        text-align:center; vertical-align:top; mso-number-format:"0.00";
        border:2px solid #008000; white-space:pre-wrap; }
      .red73 { color:#ff0000; font-weight:bold; }
    </style></head><body><table>
      <tr><td class="xl73" rowspan="2" x:num="1.25"><span class="red73">红</span><span>蓝</span></td>
          <td x:fmla="=RC[-1]*2">2.50</td></tr>
      <tr><td>尾</td></tr>
    </table><script>parent.__clipboardScriptRan=true</script></body></html>`;
    const parsedHtml73 = await parseExcelClipboardHtml(excelHtml73);
    const firstStyle73 = parsedHtml73?.clip?.data?.[1]?.[1]?.style;
    const htmlOk73 = parsedHtml73?.height === 2 && parsedHtml73?.width === 2
      && parsedHtml73.merges?.some((merge) => merge.r0 === 0 && merge.c0 === 0 && merge.r1 === 1 && merge.c1 === 0)
      && parsedHtml73.clip.data[1][1].text === '1.25'
      && parsedHtml73.clip.data[1][2].text === '=A1*2'
      && parsedHtml73.values?.[0]?.[0] === 1.25
      && firstStyle73?.font?.b === true && firstStyle73.font.color === '#123456'
      && firstStyle73.fill?.color === '#FFEEAA' && firstStyle73.num_fmt === '0.00'
      && firstStyle73.alignment?.horizontal === 'center' && firstStyle73.alignment?.wrap_text === true
      && firstStyle73.border?.top?.style === 'medium'
      && parsedHtml73.richText?.[0]?.runs?.some((run) => run.text === '红' && run.color === '#FF0000')
      && !window.__clipboardScriptRan;
    ok('T73 高保真剪贴板：多MIME+custom读取+选择性粘贴+Excel HTML安全解析',
      mimeOk73 && customOk73 && pasteSpecialOk73 && htmlOk73,
      `mime=${mimeOk73}/${types73.join(',')} custom=${customOk73}`
        + ` special=${pasteSpecialOk73}/${JSON.stringify(pasteCall73?.body)}`
        + ` html=${htmlOk73}/${JSON.stringify(parsedHtml73)}`);

    // T74 Excel 表 / 普通 AutoFilter / 多条件排序：入口、六类筛选和最小差量请求契约。
    const tableHooks74 = window.__nativeTableEditorTest;
    const tableDialog74 = tableHooks74?.ensureDialog();
    const tableBase74 = tableHooks74?.normalizeTable({
      part: 'xl/tables/table74.xml', sheet: 'Sheet1', sheetId: 1, sheetPart: 'xl/worksheets/sheet1.xml',
      id: 74, name: 'Table74', displayName: 'Table74', reference: 'A1:B4',
      headerRowCount: 1, totalsRowCount: 0, totalsRowShown: false,
      columns: [
        { sourceIndex: 0, id: 1, name: 'Name' },
        { sourceIndex: 1, id: 2, name: 'Amount', calculatedColumnFormula: '=[@Amount]*2' },
      ],
      autoFilter: {
        reference: 'A1:B4', filterColumns: [{
          sourceIndex: 0, columnId: 0, showButton: true,
          definition: { kind: 'filters', attributes: { blank: 0 }, criteria: [{ kind: 'filter', attributes: { val: 'A' } }] },
        }],
        sortState: { reference: 'A2:B4', conditions: [
          { sourceIndex: 0, reference: 'A2:A4', descending: false },
          { sourceIndex: 1, reference: 'B2:B4', descending: true },
        ] },
      },
      styleInfo: { name: 'TableStyleMedium2', showRowStripes: true },
    });
    const tableDraft74 = JSON.parse(JSON.stringify(tableBase74));
    tableDraft74.name = tableDraft74.displayName = 'Sales74';
    tableDraft74.reference = 'A1:C4';
    tableDraft74.columns[1].name = 'NetAmount';
    tableDraft74.columns.push({ id: 3, name: 'Region', totalsRowFunction: null, calculatedColumnFormula: null, _new: true });
    tableDraft74.autoFilter.reference = 'A1:C4';
    tableDraft74.autoFilter.filterColumns[0].definition.config.values.push('B');
    tableDraft74.autoFilter.sortState.conditions.reverse();
    const tablePatch74 = tableHooks74?.buildPackagePatchFor('table', tableBase74, tableDraft74);
    const partPatch74 = tablePatch74?.tableEdits?.[0]?.patch;
    const columnOps74 = partPatch74?.columnOperations || [];
    const filterOps74 = partPatch74?.autoFilter?.filterColumnOperations || [];
    const sortOps74 = partPatch74?.autoFilter?.sortState?.conditionOperations || [];
    const runtime74 = tableHooks74?.buildRuntimeOperationsFor('table', tableBase74, tableDraft74) || [];
    const request74 = tableHooks74?.request('update', tablePatch74, runtime74);
    const tableContract74 = !!tableHooks74 && !!tableDialog74 && !!document.getElementById('btn-tables')
      && tableDialog74.querySelectorAll('.nte-tabs').length === 1
      && tableHooks74.FILTER_KINDS.length === 6
      && tableHooks74.FILTER_KINDS.map(([kind]) => kind).join(',')
        === 'filters,customFilters,dynamicFilter,top10,colorFilter,iconFilter'
      && tablePatch74.rewriteStructuredReferences === true
      && partPatch74.ref === 'A1:C4' && partPatch74.displayName === 'Sales74'
      && columnOps74.some((operation) => operation.op === 'update' && operation.id === 2)
      && columnOps74.some((operation) => operation.op === 'add' && operation.patch?.name === 'Region')
      && filterOps74.some((operation) => operation.op === 'update' && operation.sourceIndex === 0)
      && sortOps74.some((operation) => operation.op === 'reorder')
      && runtime74.some((operation) => operation.type === 'sort')
      && runtime74.some((operation) => operation.type === 'filter')
      && request74?.url === '/api/tables' && request74?.body?.op === 'update'
      && request74?.body?.runtime?.length === runtime74.length;
    ok('T74 Excel表/普通筛选/多条件排序：入口+六类筛选+差量请求契约', tableContract74,
      `kinds=${tableHooks74?.FILTER_KINDS?.length} patch=${JSON.stringify(tablePatch74)} request=${JSON.stringify(request74)}`);

    // T75 页面布局/审阅/保护：A3/A4/A5/Letter、打印名称、分页符、保护、备注、
    // 线程回复/@mentions 均使用 /api/page-review 的子节点级差量，未知 OOXML 不进入 patch。
    const pageHooks75 = window.__pageReviewEditorTest;
    const pageDialog75 = pageHooks75?.ensureDialog();
    const pageBase75 = pageHooks75?.normalizeModel({
      workbookPart: 'xl/workbook.xml', personsPart: 'xl/persons/person.xml',
      workbookProtection: { lockStructure: '0', vendorLock: 'keep' },
      definedNames: [
        { kind: 'printArea', localSheetId: 0, formula: "'Sheet1'!$A$1:$C$20" },
        { kind: 'other', localSheetId: 0, name: 'VendorName', formula: '42', attributes: { vendor: 'keep' } },
      ],
      persons: [{ id: '{PERSON-75}', displayName: 'Chen', userId: 'chen@example.test', providerId: 'None' }],
      worksheets: [{
        name: 'Sheet1', sheetId: 1, localSheetId: 0, part: 'xl/worksheets/sheet1.xml',
        pageMargins: { left: '0.7', right: '0.7', top: '0.75', bottom: '0.75', header: '0.3', footer: '0.3', vendor: 'keep' },
        pageSetup: { paperSize: '9', orientation: 'portrait', fitToWidth: '1', fitToHeight: '0', vendor: 'keep' },
        printOptions: { gridLines: '0' },
        headerFooter: { attributes: { differentFirst: '0', vendor: 'keep' }, oddHeader: '&CSheet1', oddFooter: null },
        rowBreaks: { attributes: { count: '1', vendor: 'keep' }, items: [{ id: '20', min: '0', max: '16383', man: '1', vendor: 'keep' }] },
        colBreaks: null,
        sheetProtection: { sheet: '0', vendorHash: 'keep' },
        protectedRanges: [{ name: 'Input', sqref: 'A1:B5', securityDescriptor: 'opaque', vendor: 'keep' }],
        notes: { items: [{ ref: 'B3', author: 'Alice', text: 'old', attributes: { vendor: 'keep' } }] },
        threadedComments: { items: [{
          id: '{COMMENT-75}', ref: 'D5', personId: '{PERSON-75}', text: 'old thread',
          person: { id: '{PERSON-75}', displayName: 'Chen' }, mentions: [], attributes: { vendor: 'keep' },
        }] },
      }],
    });
    const pageDraft75 = JSON.parse(JSON.stringify(pageBase75));
    pageDraft75.workbookProtection.lockStructure = true;
    const sheet75 = pageDraft75.worksheets[0];
    sheet75.pageSetup.paperSize = '11';
    sheet75.pageSetup.orientation = 'landscape';
    sheet75.pageSetup.fitToWidth = 1;
    sheet75.pageSetup.fitToHeight = 0;
    sheet75.pageMargins.left = 0.35;
    sheet75.printArea = "'Sheet1'!$A$1:$F$40";
    sheet75.printTitles = "'Sheet1'!$1:$2";
    sheet75.headerFooter.oddHeader = '&L&F&C&A&R&P/&N';
    sheet75.rowBreaks.items[0].id = 25;
    sheet75.protectedRanges[0].sqref = 'A1:C9';
    sheet75.sheetProtection.sheet = true;
    sheet75.notes.items[0].text = 'new note';
    sheet75.threadedComments.items[0].text = 'new thread';
    sheet75.threadedComments.items[0].mentions = [{
      mentionId: '{MENTION-75}', personId: '{PERSON-75}', startIndex: 0, length: 4,
    }];
    sheet75.threadedComments.items.push({
      id: '{REPLY-75}', _baseid: '', ref: 'D5', parentId: '{COMMENT-75}',
      personId: '{PERSON-75}', text: 'reply', mentions: [],
    });
    const pagePatch75 = pageHooks75?.buildPackagePatchFor(pageBase75, pageDraft75);
    const sheetPatch75 = pagePatch75?.worksheets?.[0];
    const request75 = pageHooks75?.request('update', pagePatch75);
    const serialized75 = JSON.stringify(pagePatch75 || {});
    const protectedBase75 = JSON.parse(JSON.stringify(pageBase75));
    protectedBase75.worksheets[0].sheetProtection = { sheet: '1', objects: '1' };
    const protectedLayout75 = { worksheets: [{ sheetId: 1, part: 'xl/worksheets/sheet1.xml',
      pageSetup: { paperSize: '9' } }] };
    const protectedComment75 = { worksheets: [{ sheetId: 1, part: 'xl/worksheets/sheet1.xml',
      notes: { upsert: [{ ref: 'A1', text: 'review' }] } }] };
    const commentsAllowedBase75 = JSON.parse(JSON.stringify(protectedBase75));
    commentsAllowedBase75.worksheets[0].sheetProtection.objects = '0';
    const richNoteModel75 = pageHooks75?.noteRunModel(
      '<text vendor="keep"><r><rPr><b/><color rgb="FFFF0000"/></rPr><t>红色</t></r><v:marker xmlns:v="urn:vendor" order="middle"/><r><rPr><i/><v:opaque xmlns:v="urn:vendor"/></rPr><t>斜体</t></r></text>',
      '',
    );
    const richNoteXml75 = pageHooks75?.noteTextXml(richNoteModel75, richNoteModel75?.runs || []);
    const richNoteFirstEnd75 = richNoteXml75.indexOf('</r>');
    const richNoteMarker75 = richNoteXml75.indexOf('v:marker');
    const richNoteSecondStart75 = richNoteXml75.indexOf('<r>', richNoteFirstEnd75 + 1);
    const nestedNoteEditor75 = document.createElement('div');
    const nestedBold75 = document.createElement('span');
    nestedBold75.dataset.rpr = '<rPr><b/></rPr>'; nestedBold75.textContent = 'A';
    const nestedBlock75 = document.createElement('div');
    const nestedItalic75 = document.createElement('span');
    nestedItalic75.dataset.rpr = '<rPr><i/></rPr>'; nestedItalic75.textContent = 'B';
    nestedBlock75.append(nestedItalic75, document.createElement('br'), document.createTextNode('C'));
    nestedNoteEditor75.append(nestedBold75, nestedBlock75);
    const nestedNoteRuns75 = pageHooks75?.editableNoteRuns(nestedNoteEditor75, '');
    const opaqueOnlyModel75 = pageHooks75?.noteRunModel(
      '<text><v:only xmlns:v="urn:vendor"/></text>', 'fallback',
    );
    const opaqueOnlyXml75 = pageHooks75?.noteTextXml(opaqueOnlyModel75, opaqueOnlyModel75?.runs || []);
    const pageContract75 = !!pageHooks75 && !!pageDialog75 && !!document.getElementById('btn-page-review')
      && pageDialog75.querySelectorAll('.pre-tabs button').length === 5
      && ['1', '8', '9', '11'].every((code) => pageHooks75.PAPER_SIZES.some(([value]) => value === code))
      && pagePatch75.workbookProtection.lockStructure === true
      && sheetPatch75.pageSetup.paperSize === '11' && sheetPatch75.pageSetup.orientation === 'landscape'
      && sheetPatch75.pageMargins.left === 0.35
      && sheetPatch75.printArea === "'Sheet1'!$A$1:$F$40"
      && sheetPatch75.printTitles === "'Sheet1'!$1:$2"
      && sheetPatch75.headerFooter.oddHeader.includes('&P')
      && sheetPatch75.rowBreaks.deleteIds.includes(20)
      && sheetPatch75.rowBreaks.upsert.some((item) => item.id === 25 && item.man === '1')
      && sheetPatch75.protectedRanges.upsert.some((item) => item.name === 'Input' && item.sqref === 'A1:C9')
      && sheetPatch75.notes.upsert.some((item) => item.ref === 'B3' && item.text === 'new note')
      && sheetPatch75.threadedComments.upsert.some((item) => item.id === '{COMMENT-75}' && item.mentions?.length === 1)
      && sheetPatch75.threadedComments.upsert.some((item) => item.parentId === '{COMMENT-75}' && item.text === 'reply')
      && !serialized75.includes('vendorLock') && !serialized75.includes('vendorHash')
      && !serialized75.includes('VendorName') && !serialized75.includes('"vendor"')
      && pageHooks75.pageReviewPatchNeedsPassword(protectedLayout75, null, protectedBase75) === true
      && pageHooks75.pageReviewPatchNeedsPassword(protectedComment75, null, protectedBase75) === true
      && pageHooks75.pageReviewPatchNeedsPassword(protectedComment75, null, commentsAllowedBase75) === false
      && richNoteModel75?.runs?.length === 2 && richNoteModel75.runs[0].style.bold === true
      && richNoteModel75.runs[0].style.color === '#FF0000' && richNoteModel75.runs[1].style.italic === true
      && richNoteXml75.includes('vendor="keep"') && richNoteXml75.includes('v:opaque')
      && richNoteFirstEnd75 >= 0 && richNoteMarker75 > richNoteFirstEnd75
      && richNoteSecondStart75 > richNoteMarker75
      && richNoteXml75.includes('<t xml:space="preserve">红色</t>')
      && richNoteXml75.includes('<t xml:space="preserve">斜体</t>')
      && nestedNoteRuns75?.map((run) => run.text).join('') === 'A\nB\nC'
      && nestedNoteRuns75?.[0]?.rPrXml.includes('<b')
      && nestedNoteRuns75?.[1]?.rPrXml.includes('<i')
      && opaqueOnlyXml75?.includes('v:only') && opaqueOnlyXml75.includes('fallback')
      && request75?.url === '/api/page-review' && request75?.body?.op === 'update'
      && request75.body.patch === pagePatch75;
    ok('T75 页面布局/审阅/保护：纸型+打印+分页符+保护+批注线程的最小差量契约', pageContract75,
      `papers=${JSON.stringify(pageHooks75?.PAPER_SIZES)} patch=${serialized75} request=${JSON.stringify(request75)}`);
    // T76 Rich text must remain a character-run editor in the grid.  Enter through a subordinate
    // point of a merged range, leave without changes, reopen, edit one run, and reopen again.
    const richSeed76 = [
      { text: 'Bold', bold: true, size: 15, font: 'Calibri', color: '#C00000' },
      { text: 'Under', italic: true, underline: true, size: 11, font: 'Arial', color: '#0070C0' },
      { text: 'Strike', strike: true, size: 9, font: 'Microsoft YaHei', color: [4, 0.25] },
    ];
    await apiPost('/api/rich-text', { sheet: S.sheet, row: 90, col: 6, runs: richSeed76 });
    const merge76 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 90, c0: 6, r1: 91, c1: 7, op: 'merge',
    });
    S.merges = merge76.merges || [];
    S.cellsCache.delete('90,6');
    setCursor(89, 5);
    const dblclickSubordinate76 = () => {
      const [x, y] = cellXY(91, 7);
      gridScroll.dispatchEvent(new MouseEvent('dblclick', {
        clientX: x, clientY: y, bubbles: true, cancelable: true,
      }));
    };
    dblclickSubordinate76();
    const opened76 = await waitUntil(() => S.editing && S.editHasRichText
      && S.editCell?.r === 90 && S.editCell?.c === 6
      && richCellEditor.style.display === 'block' && richCellEditor.textContent === 'BoldUnderStrike', 3000);
    const spans76 = [...richCellEditor.querySelectorAll('.rich-run')];
    const visualRuns76 = opened76 && spans76.length === 3
      && spans76[0].style.fontWeight === 'bold' && spans76[0].style.fontSize === '15pt'
      && spans76[0].style.fontFamily.includes('Calibri')
      && spans76[0].dataset.resolvedColor === '#C00000'
      && spans76[1].style.fontStyle === 'italic'
      && spans76[1].style.textDecoration.includes('underline')
      && spans76[1].style.fontFamily.includes('Arial')
      && spans76[2].style.textDecoration.includes('line-through')
      && spans76[2].dataset.colorToken === '[4,0.25]';

    // Commit an untouched DOM.  This must be a no-op, preserving all original run boundaries.
    await commitEdit();
    await waitUntil(() => !S.editing, 3000);
    const untouched76 = await api(`/api/cell?sheet=${S.sheet}&row=90&col=6`);
    const noChange76 = richRunsEqual(untouched76.richText || [], richSeed76);

    // Reopen the same merged cell through its bottom-right subordinate point.
    dblclickSubordinate76();
    const reopened76 = await waitUntil(() => S.editing && S.editHasRichText
      && S.editCell?.r === 90 && S.editCell?.c === 6
      && richCellEditor.textContent === 'BoldUnderStrike', 3000);
    const reopenRuns76 = reopened76 ? richRunsFromDom(richCellEditor) : [];
    const reopenStyles76 = richRunsEqual(reopenRuns76, richSeed76);
    const under76 = [...richCellEditor.querySelectorAll('.rich-run')][1];
    if (under76) under76.textContent += 'X';
    richCellEditor.dispatchEvent(new Event('input', { bubbles: true }));
    await commitEdit();
    await waitUntil(() => !S.editing, 3000);
    const saved76 = await api(`/api/cell?sheet=${S.sheet}&row=90&col=6`);
    const changed76 = saved76.content === 'BoldUnderXStrike'
      && (saved76.richText || []).some((run) => run.text === 'UnderX' && run.italic && run.underline
        && run.size === 11 && run.font === 'Arial' && run.color === '#0070C0');

    dblclickSubordinate76();
    const reopenedAgain76 = await waitUntil(() => S.editing && S.editHasRichText
      && S.editCell?.r === 90 && S.editCell?.c === 6
      && richCellEditor.textContent === 'BoldUnderXStrike', 3000);
    const reopenedAgainRuns76 = reopenedAgain76 ? richRunsFromDom(richCellEditor) : [];
    const reopenedAgainStyle76 = reopenedAgainRuns76.some((run) => run.text === 'UnderX'
      && run.italic && run.underline && run.size === 11 && run.font === 'Arial' && run.color === '#0070C0');
    cancelEditUI();
    const unmerge76 = await apiPost('/api/merge', {
      sheet: S.sheet, r0: 90, c0: 6, r1: 91, c1: 7, op: 'unmerge',
    });
    S.merges = unmerge76.merges || [];
    ok('T76 富文本双击：原样 runs / 合并锚点 / 无修改保真 / 提交后重复打开',
      opened76 && visualRuns76 && noChange76 && reopened76 && reopenStyles76
        && changed76 && reopenedAgain76 && reopenedAgainStyle76,
      `open=${opened76} visual=${visualRuns76} noChange=${noChange76}`
        + ` reopen=${reopened76}/${reopenStyles76} changed=${changed76}`
        + ` reopenAgain=${reopenedAgain76}/${reopenedAgainStyle76}`);

    // T77 Queries & Connections UI contract: only allow-listed OOXML deltas leave the panel,
    // secrets remain redacted, and M preview requests contain caller-supplied in-memory data only.
    const qcHooks77 = window.__queryConnectionsTestHooks;
    const qcDialog77 = qcHooks77?.ensureDialog();
    const connection77 = {
      part: 'xl/connections.xml', id: '7', name: 'Orders', description: 'original',
      commandText: 'select old', sourceRedacted: true,
      attributes: { name: 'Orders', description: 'original', refreshOnLoad: '0', background: '1', saveData: '1', enableRefresh: '1', keepAlive: '0', interval: '15' },
      source: { kind: 'database', connection: 'Server=local;User=alice;Password=secret;Access Token=hidden' },
      parameters: [{ name: 'Password', string: 'parameter-secret' }],
    };
    const connectionDraft77 = qcHooks77?.connectionDraft(connection77);
    if (connectionDraft77) {
      connectionDraft77.name = 'Orders 2026'; connectionDraft77.refreshOnLoad = true;
      connectionDraft77.background = false; connectionDraft77.interval = '30';
      connectionDraft77.commandText = 'select new';
    }
    const connectionEdit77 = qcHooks77?.buildConnectionEdit(connection77, connectionDraft77);
    const connectionSerialized77 = JSON.stringify(connectionEdit77 || {});
    const redacted77 = qcHooks77?.redactForDisplay(connection77);
    const query77 = {
      part: 'xl/queryTables/queryTable1.xml', name: 'OrdersQuery', connectionId: '7',
      attributes: { name: 'OrdersQuery', connectionId: '7', refreshOnLoad: '0', preserveFormatting: '1', backgroundRefresh: '1' },
      linkedTables: [{ part: 'xl/tables/table1.xml', name: 'Orders', loadRange: 'A1:B8' }],
    };
    const queryDraft77 = qcHooks77?.queryDraft(query77);
    if (queryDraft77) { queryDraft77.loadRange = 'C2:D20'; queryDraft77.refreshOnLoad = true; queryDraft77.preserveFormatting = false; }
    const queryEdit77 = qcHooks77?.buildQueryEdit(query77, queryDraft77);
    const mJson77 = qcHooks77?.buildMRequest('Table.SelectRows(Input, each [Amount] > 3)', 'Input', 'json', '[{"Amount":3},{"Amount":10}]');
    const mCsv77 = qcHooks77?.buildMRequest('Input', 'Input', 'csv', 'Name,Value\r\nA,1');
    const updateRequest77 = qcHooks77?.request('update', { connectionEdits: [connectionEdit77] });
    const executeRequest77 = qcHooks77?.request('execute', mJson77);
    const qcContract77 = !!qcHooks77 && !!qcDialog77 && !!document.getElementById('btn-query-connections')
      && qcHooks77.kinds?.map(([key]) => key).join(',') === 'connections,queryTables,externalLinks,dataModel,opaqueDataParts'
      && qcDialog77.querySelectorAll('[data-mode]').length === 2
      && redacted77?.source?.connection === 'Server=local;User=alice;Password=***;Access Token=***'
      && redacted77?.parameters?.[0]?.string === '***'
      && connectionEdit77?.part === connection77.part && connectionEdit77?.id === '7'
      && connectionEdit77?.attributes?.name === 'Orders 2026'
      && connectionEdit77?.attributes?.refreshOnLoad === true
      && connectionEdit77?.attributes?.background === false
      && connectionEdit77?.attributes?.interval === 30
      && connectionEdit77?.commandText === 'select new'
      && !connectionSerialized77.includes('secret') && !connectionSerialized77.includes('hidden')
      && !connectionSerialized77.includes('connectionString') && !connectionSerialized77.includes('source')
      && queryEdit77?.part === query77.part && queryEdit77?.loadRange === 'C2:D20'
      && queryEdit77?.attributes?.refreshOnLoad === true && queryEdit77?.attributes?.preserveFormatting === false
      && mJson77?.inputs?.Input?.json?.[1]?.Amount === 10
      && mCsv77?.inputs?.Input?.csv === 'Name,Value\r\nA,1'
      && updateRequest77?.url === '/api/native-data' && updateRequest77?.body?.op === 'update'
      && executeRequest77?.url === '/api/power-query/execute'
      && executeRequest77?.body === mJson77;
    ok('T77 查询与连接：脱敏展示+安全元数据差量+loadRange+内存 M 预览契约', qcContract77,
      `redacted=${JSON.stringify(redacted77)} connection=${connectionSerialized77}`
        + ` query=${JSON.stringify(queryEdit77)} m=${JSON.stringify(mJson77)}`);

    // T78 Native Pivot local refresh: derive a typed worksheet request from the current native
    // PivotTable/PivotCache model, preview without output writes, then apply with package targets.
    const pivotHooks78 = window.__pivotLocalRefreshTestHooks;
    const pivotDialog78 = pivotHooks78?.ensureDialog();
    const pivotTable78 = {
      part:'xl/pivotTables/pivotTable1.xml', cachePart:'xl/pivotCache/pivotCacheDefinition1.xml',
      cacheId:7, sheet:'Pivot', location:{ref:'H3:N30'}, display:{rowGrandTotals:true,colGrandTotals:false},
      fields:[
        {index:0,name:'Region',items:[{sourceIndex:0,cacheIndex:0,value:'West'},{sourceIndex:1,cacheIndex:1,value:'East'}]},
        {index:1,name:'Product',items:[]},{index:2,name:'Amount',items:[]},
        {index:3,name:'Channel',items:[
          {sourceIndex:0,cacheIndex:0,value:'West'},{sourceIndex:1,cacheIndex:1,value:'East'},
        ]},
      ],
      axes:{rows:[0],columns:[1,-2],pages:[{fld:3,item:1}],data:[{fld:2,subtotal:'sum',name:'Revenue'}]},
      filters:[{attributes:{fld:0,type:'captionContains',stringValue1:'e'}}],
    };
    const pivotCache78 = {part:pivotTable78.cachePart,cacheId:7,source:{type:'worksheet',sheet:'Data',ref:'A1:D40'}};
    const pivotInfo78 = {sheets:['Pivot','Data']};
    const pivotConfig78 = pivotHooks78?.initialRefreshConfig(pivotTable78,pivotCache78,pivotInfo78);
    if (pivotConfig78) { pivotConfig78.sourceRef="'Data'!A1:D40"; pivotConfig78.outputRef="'Pivot'!H3"; }
    const pivotPreview78 = pivotHooks78?.request('preview',pivotTable78,pivotCache78,pivotConfig78,pivotTable78,pivotInfo78);
    const pivotApply78 = pivotHooks78?.request('apply',pivotTable78,pivotCache78,pivotConfig78,pivotTable78,pivotInfo78);
    const olapBoundary78 = pivotHooks78?.localRefreshBoundary({source:{type:'external',connectionId:9}});
    const pivotRequest78 = pivotPreview78?.body?.request;
    const pivotContract78 = !!pivotHooks78 && !!pivotDialog78
      && pivotDialog78.querySelector('[data-tab="refresh"]') != null
      && pivotHooks78.parseA1Range("'Raw Data'!$B$2:$E$99")?.sheet === 'Raw Data'
      && pivotHooks78.parseA1Range("'Raw Data'!$B$2:$E$99")?.c1 === 5
      && pivotConfig78?.sourceSheet === 1 && pivotConfig78?.outputSheet === 0
      && pivotPreview78?.url === '/api/pivot-local-refresh' && pivotPreview78?.body?.op === 'preview'
      && !Object.prototype.hasOwnProperty.call(pivotPreview78.body,'output')
      && pivotRequest78?.pivotTablePart === pivotTable78.part
      && pivotRequest78?.pivotCacheDefinitionPart === pivotTable78.cachePart
      && pivotRequest78?.sourceRange?.sheet === 1 && pivotRequest78?.sourceRange?.r1 === 40
      && pivotRequest78?.rows?.join(',') === '0' && pivotRequest78?.columns?.join(',') === '1'
      && pivotRequest78?.pages?.join(',') === '3'
      && pivotRequest78?.values?.[0]?.field === 2 && pivotRequest78?.values?.[0]?.aggregate === 'sum'
      && pivotRequest78?.filters?.some((filter)=>filter.field===0&&filter.op==='contains'&&filter.value==='e')
      && pivotRequest78?.filters?.some((filter)=>filter.field===3&&filter.op==='eq'&&filter.value==='East')
      && pivotApply78?.body?.op === 'apply' && pivotApply78?.body?.output?.sheet === 0
      && pivotApply78?.body?.output?.row === 3 && pivotApply78?.body?.output?.col === 8
      && olapBoundary78?.supported === false && olapBoundary78?.kind === 'dataModel';
    ok('T78 原生透视表：worksheet cache 本地预览/二维结果/原生应用 + OLAP/Data Model 边界',pivotContract78,
      `preview=${JSON.stringify(pivotPreview78)} olap=${JSON.stringify(olapBoundary78)}`);

    // T79 Every non-blank validation kind, including custom formulas, must be decided by the
    // Rust hypothetical-cell evaluator.  The request carries the exact rule id and candidate.
    const dvHooks79 = window.__dvRuntimeTestHooks;
    const customRule79 = {
      id: 'dv-custom-79', type: 'custom', sqref: 'C3:C9', formula1: 'LEN(C3)<=5',
      allowBlank: false, showErrorMessage: true, errorStyle: 'stop',
    };
    const request79 = dvHooks79?.request(2, 5, 3, customRule79, 'abcdef');
    let captured79 = null;
    const outcome79 = await dvHooks79?.evaluate(2, 5, 3, customRule79, 'abcdef', async (url, body) => {
      captured79 = { url, body };
      return { valid: false, reason: 'customFormulaFalse' };
    });
    const dvSource79 = String(validateDvCellInput);
    const dvContract79 = !!dvHooks79
      && request79?.url === '/api/dv' && request79?.body?.op === 'validate'
      && request79?.body?.sheet === 2 && request79?.body?.row === 5 && request79?.body?.col === 3
      && request79?.body?.id === 'dv-custom-79' && request79?.body?.value === 'abcdef'
      && captured79?.url === '/api/dv' && captured79?.body?.id === 'dv-custom-79'
      && outcome79?.valid === false && outcome79?.reason === 'customFormulaFalse'
      && dvSource79.includes('requestDvRuntimeValidation')
      && !dvSource79.includes("rule.type === 'custom'");
    ok('T79 数据验证运行时：所有类型走 Rust 候选值求值，自定义公式不再无条件放行', dvContract79,
      `request=${JSON.stringify(request79)} captured=${JSON.stringify(captured79)} outcome=${JSON.stringify(outcome79)}`);

    // T81 What-If Analysis exposes all three Excel workflows. Preview requests remain read-only;
    // apply requests use the generic workbook-scoped local transaction.
    const whatIfHooks81 = window.__unicellWhatIfTest;
    document.getElementById('btn-what-if')?.click();
    const whatIfDialog81 = document.getElementById('what-if-dialog');
    if (whatIfDialog81) {
      document.getElementById('wi-goal-target').value = 'C9';
      document.getElementById('wi-goal-changing').value = 'A2';
      document.getElementById('wi-goal-value').value = '42';
      document.getElementById('wi-goal-lower').value = '0';
      document.getElementById('wi-goal-upper').value = '100';
    }
    const goal81 = whatIfHooks81?.goalRequest();
    const values81 = whatIfHooks81?.values('1,2\nTRUE;word');
    const cells81 = whatIfHooks81?.scenarioCells('A1:B2 C3');
    const whatIfContract81 = !!whatIfHooks81 && !!whatIfDialog81
      && whatIfDialog81.querySelectorAll('[data-wi-tab]').length === 3
      && whatIfDialog81.querySelector('[data-wi-panel="goal"]')
      && whatIfDialog81.querySelector('[data-wi-panel="table"]')
      && whatIfDialog81.querySelector('[data-wi-panel="scenario"]')
      && goal81?.target?.sheet === S.sheet && goal81?.target?.row === 9 && goal81?.target?.column === 3
      && goal81?.changing?.row === 2 && goal81?.changing?.column === 1
      && goal81?.targetValue === 42 && goal81?.lowerBound === 0 && goal81?.upperBound === 100
      && values81?.[0] === 1 && values81?.[2] === true && values81?.[3] === 'word'
      && cells81?.length === 5 && cells81?.[4]?.row === 3 && cells81?.[4]?.column === 3
      && apiPostMutates('/api/what-if',{op:'preview',kind:'goalSeek'}) === false
      && apiPostMutates('/api/what-if',{op:'list',kind:'scenarios'}) === false
      && apiPostMutates('/api/what-if',{op:'apply',kind:'goalSeek'}) === true;
    ok('T81 假设分析：目标求解隔离预览/原子写回 + 一/二变量数据表 + 原生方案管理器 + 统一本机事务',
      whatIfContract81,
      `goal=${JSON.stringify(goal81)} values=${JSON.stringify(values81)}`);
    whatIfDialog81?.remove();

    // T82 Calculation settings are read back from the workbook, validated in the ribbon panel,
    // and routed as one workbook-scoped local mutation. `op:get` must remain read-only.
    const calcHooks82 = window.__unicellCalculationTest;
    const calcState82 = await fetch('/api/calcmode', {
      method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ op: 'get' }),
    }).then((response) => response.json());
    const previousMode82 = document.getElementById('fr-calcmode').value;
    const calcPanel82 = await calcHooks82?.open?.();
    if (calcPanel82) {
      document.getElementById('fr-calcmode').value = 'manual';
      calcPanel82.querySelector('[data-calc="enabled"]').checked = true;
      calcPanel82.querySelector('[data-calc="maxIterations"]').value = '321';
      calcPanel82.querySelector('[data-calc="maxChange"]').value = '0.0001';
    }
    const calcPayload82 = calcPanel82 && calcHooks82?.payload(calcPanel82);
    const calcContract82 = !!calcHooks82 && !!document.getElementById('fr-iteration') && !!calcPanel82
      && calcState82?.ok === true && ['auto', 'manual'].includes(calcState82.mode)
      && typeof calcState82.enabled === 'boolean' && Number.isInteger(calcState82.maxIterations)
      && Number.isFinite(calcState82.maxChange)
      && calcPayload82?.op === 'set' && calcPayload82?.mode === 'manual'
      && calcPayload82?.enabled === true && calcPayload82?.maxIterations === 321
      && calcPayload82?.maxChange === 0.0001
      && apiPostMutates('/api/calcmode', { op: 'get' }) === false
      && apiPostMutates('/api/calcmode', calcPayload82) === true;
    ok('T82 计算设置：calcPr 回读 + 迭代参数校验 + 只读查询 + 工作簿级本机事务', calcContract82,
      `state=${JSON.stringify(calcState82)} payload=${JSON.stringify(calcPayload82)}`);
    document.getElementById('fr-calcmode').value = previousMode82;
    calcPanel82?.remove();

    // T83 The CF manager owns only standard executable semantics. Metadata edits must use the
    // narrow update contract, visual rules must not acquire a fake stopIfTrue field, and x14 /
    // unknown extensions are explicitly advertised as preserve-only rather than locally executed.
    const cfHooks83 = window.__unicellCfManagerTest;
    const cfState83 = await fetch('/api/cf', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ sheet: S.sheet, op: 'list' }),
    }).then((response) => response.json());
    const cfDialog83 = await cfHooks83?.open?.();
    const standard83 = cfState83?.rules?.find((rule) => rule.canStopIfTrue)
      || { index: 17, canStopIfTrue: true };
    const visual83 = cfState83?.rules?.find((rule) => !rule.canStopIfTrue)
      || { index: 18, canStopIfTrue: false };
    const update83 = cfHooks83?.updateRequest(standard83, standard83.index, '$A$1:$B$9 D2:D4', true);
    const visualUpdate83 = cfHooks83?.updateRequest(visual83, visual83.index, 'C1:C8', true);
    const duplicate83 = cfHooks83?.actionRequest('duplicate', standard83.index);
    const cfSource83 = String(cfHooks83?.open || '');
    const cfContract83 = !!cfHooks83 && !!cfDialog83
      && !!cfDialog83.querySelector('#cf-rule-list') && !!cfDialog83.querySelector('.cf-create')
      && !!cfDialog83.querySelector('#cf-clear') && !!cfDialog83.querySelector('.cf-extension-note')
      && cfDialog83.querySelector('.cf-extension-note').textContent.includes('x14')
      && cfDialog83.querySelector('.cf-extension-note').textContent.includes('原样保留')
      && update83?.op === 'update' && update83?.index === Number(standard83.index)
      && update83?.range === '$A$1:$B$9 D2:D4' && update83?.stopIfTrue === true
      && visualUpdate83?.op === 'update' && !Object.prototype.hasOwnProperty.call(visualUpdate83, 'stopIfTrue')
      && duplicate83?.op === 'duplicate' && duplicate83?.index === Number(standard83.index)
      && cfHooks83.preserveOnlyBoundary(cfState83) === true
      && cfState83?.capabilities?.standardRuntime === true
      && cfState83?.capabilities?.updateAppliesTo === true
      && cfState83?.capabilities?.reorderPriority === true
      && apiPostMutates('/api/cf', { op: 'list' }) === false
      && apiPostMutates('/api/cf', update83) === true
      && cfSource83.includes('refreshCfRules');
    ok('T83 条件格式规则管理器：应用范围/优先级/停止/复制契约 + x14/未知扩展 preserve-only 边界',
      cfContract83,
      `state=${JSON.stringify(cfState83)} update=${JSON.stringify(update83)} visual=${JSON.stringify(visualUpdate83)}`);
    document.getElementById('cf-close')?.click();

    // T84 DrawingML 颜色：主题 token 只在显示时解析一次，颜色变换严格按 XML 顺序叠加。
    const colorRuntime84 = window.DrawingMLColor;
    const customTheme84 = {
      dk1: '#101010', lt1: '#FAFAFA', dk2: '#202020', lt2: '#EEEEEE',
      accent1: '#808080', accent2: '#112233', accent3: '#445566',
      accent4: '#778899', accent5: '#AABBCC', accent6: '#DDEEFF',
      hlink: '#0000FF', folHlink: '#800080',
    };
    const resolve84 = (value, spec) => colorRuntime84?.resolve(value, spec, customTheme84);
    const ordered84 = resolve84('accent1', { type: 'scheme', value: 'accent1', transforms: [
      { type: 'tint', value: 50000, raw: '50000' },
      { type: 'shade', value: 50000, raw: '50000' },
    ] });
    const luminance84 = resolve84('accent1', { type: 'scheme', value: 'accent1', transforms: [
      { type: 'lumMod', value: 50000, raw: '50000' },
      { type: 'lumOff', value: 10000, raw: '10000' },
    ] });
    const hue84 = resolve84('#FF0000', { type: 'srgb', value: '#FF0000', transforms: [
      { type: 'hueOff', value: 7200000, raw: '7200000' },
    ] });
    const alpha84 = resolve84('#336699', { type: 'srgb', value: '#336699', transforms: [
      { type: 'alpha', value: 80000, raw: '80000' },
      { type: 'alphaMod', value: 50000, raw: '50000' },
      { type: 'alphaOff', value: 10000, raw: '10000' },
    ] });
    const scrgb84 = resolve84('scrgb(50000,50000,50000)', {
      type: 'scrgb', r: 50000, g: 50000, b: 50000, transforms: [],
    });
    const hsl84 = resolve84('hsl(7200000,100000,50000)', {
      type: 'hsl', hue: 7200000, sat: 100000, lum: 50000, transforms: [],
    });
    const system84 = resolve84('#123456', {
      type: 'system', value: 'windowText', lastColor: '#123456', transforms: [],
    });
    const stale84 = resolve84('#FF0000', {
      type: 'scheme', value: 'accent1', transforms: [{ type: 'shade', value: 10000, raw: '10000' }],
    });
    const unknown84 = resolve84('vendorMysteryColour', {
      type: 'unknown', value: 'vendorMysteryColour', transforms: [],
    });
    const shape84 = {
      id: 't84-shape', sheet: S.sheet, type: 'svg', mode: 'abs', r: 2, c: 2,
      x: 20, y: 20, w: 240, h: 120,
      config: { nativeDrawing: { kind: 'shape', name: 'ThemeShape', theme: customTheme84, model: {
        text: '', paragraphs: [], geometry: 'rect', rotation: 0, flipH: false, flipV: false,
        fill: { kind: 'solid', color: 'accent1', colorSpec: { type: 'scheme', value: 'accent1', transforms: [{ type: 'shade', value: 50000, raw: '50000' }] }, alpha: 1, stops: [] },
        line: null, effects: { shadow: { enabled: false }, softEdge: 0 },
      } } },
    };
    openNativeDrawingEditor(shape84);
    const dialog84 = document.getElementById('native-drawing-dialog');
    const picker84 = dialog84?.querySelector('[data-f=fillColor]')?.value?.toUpperCase();
    const raw84 = dialog84?._nativeCollect?.()?.fill?.color;
    const diff84 = dialog84?._nativeDiff?.();
    dialog84?.querySelector('[data-close]')?.click();
    const drawingColor84 = !!colorRuntime84
      && ordered84?.color === '#606060' && luminance84?.color === '#5A5A5A'
      && hue84?.color === '#00FF00' && Math.abs(alpha84?.alpha - .5) < 1e-9
      && scrgb84?.color === '#BCBCBC' && hsl84?.color === '#00FF00'
      && system84?.color === '#123456' && stale84?.color === '#FF0000'
      && unknown84 == null && picker84 === '#404040' && raw84 === 'accent1'
      && diff84 === undefined;
    ok('T84 DrawingML主题/scRGB/HSL/颜色变换单次解析', drawingColor84,
      `ordered=${ordered84?.color} lum=${luminance84?.color} hue=${hue84?.color}`
        + ` alpha=${alpha84?.alpha} scRGB=${scrgb84?.color} HSL=${hsl84?.color}`
        + ` stale=${stale84?.color} unknown=${unknown84} picker=${picker84} raw=${raw84} diff=${JSON.stringify(diff84)}`);

    // T85 cross-sheet cut: switching sheets must retain the cut marker, and the paste request
    // remains an all-content cut even if the system clipboard only supplies plain text.  The
    // server then either consumes its internal cut range or rejects it atomically; it must never
    // reinterpret the operation as a copy.
    const clipboardHooks85 = window.__unicellClipboardTest;
    const cutMarker85 = { sheet: 0, r0: 1, c0: 1, r1: 2, c1: 2 };
    const beforeSwitch85 = { sheet: 0, cur: { r: 2, c: 3 }, cutPending: cutMarker85 };
    const afterSwitch85 = { ...beforeSwitch85, sheet: 1, cur: { r: 5, c: 6 } };
    const rich85 = {
      text: 'A\tB',
      unicell: { version: 1, kind: 'unicell-range', clip: 'internal', height: 1, width: 2 },
    };
    const richRequest85 = clipboardHooks85?.request(rich85, 'all', afterSwitch85);
    const textRequest85 = clipboardHooks85?.request({ text: 'plain fallback' }, 'all', afterSwitch85);
    const switchSource85 = String(switchSheet);
    const clipboardContract85 = !!clipboardHooks85
      && afterSwitch85.cutPending === cutMarker85
      && clipboardHooks85.valid(rich85.unicell)
      && richRequest85?.sheet === 1 && richRequest85?.row === 5 && richRequest85?.col === 6
      && richRequest85?.mode === 'cut' && richRequest85?.special === 'all'
      && richRequest85?.unicell === rich85.unicell
      && textRequest85?.mode === 'cut' && textRequest85?.special === 'all'
      && textRequest85?.text === 'plain fallback' && !('unicell' in textRequest85)
      && !/cutPending\s*=\s*null/.test(switchSource85)
      && !/delete\s+[^;\n]*cutPending/.test(switchSource85);
    ok('T85 跨表剪切保留标记且粘贴请求不降级', clipboardContract85,
      `marker=${afterSwitch85.cutPending === cutMarker85} rich=${JSON.stringify(richRequest85)}`
        + ` text=${JSON.stringify(textRequest85)} switch=${switchSource85}`);

    // T86 U AI must never write during preview. The exact confirmed batch is committed once,
    // reports formula errors structurally, and a single undo restores every changed cell.
    const aiHooks86 = window.__unicellAiTest;
    const aiOps86 = [
      { op: 'setValue', ref: `${S.sheets[S.sheet]}!Z200`, value: 'AI-preview' },
      { op: 'setFormula', ref: `${S.sheets[S.sheet]}!AA200`, formula: '=1/0' },
    ];
    const aiBeforeZ86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=26`);
    const aiBeforeAa86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=27`);
    const aiPreview86 = await aiHooks86?.previewOps(aiOps86);
    const aiPreviewZ86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=26`);
    const aiPreviewAa86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=27`);
    const aiApplied86 = await aiHooks86?.applyPending();
    const aiAfterZ86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=26`);
    const aiAfterAa86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=27`);
    await apiPost('/api/undo', {});
    const aiUndoZ86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=26`);
    const aiUndoAa86 = await api(`/api/cell?sheet=${S.sheet}&row=200&col=27`);
    const aiContract86 = !!aiHooks86
      && aiPreview86?.dryRun === true && aiPreview86?.applied === false
      && aiPreview86?.changedCells === 2 && aiPreview86?.errors?.some((item) => item.ref.endsWith('!AA200'))
      && aiPreviewZ86?.content === aiBeforeZ86?.content
      && aiPreviewAa86?.content === aiBeforeAa86?.content
      && aiApplied86?.applied === true && aiAfterZ86?.content === 'AI-preview'
      && aiAfterAa86?.content === '=1/0' && aiAfterAa86?.formatted === '#DIV/0!'
      && aiUndoZ86?.content === aiBeforeZ86?.content
      && aiUndoAa86?.content === aiBeforeAa86?.content
      && apiPostMutates('/api/ai/apply', { dryRun: true }) === false
      && apiPostMutates('/api/ai/apply', { dryRun: false }) === true;
    ok('T86 U AI：dry-run 零写入 + 结构化错误 + 确认批次一步撤销', aiContract86,
      `preview=${JSON.stringify(aiPreview86)} applied=${JSON.stringify(aiApplied86)}`
        + ` after=${aiAfterZ86?.content}/${aiAfterAa86?.formatted}`
        + ` undo=${aiUndoZ86?.content}/${aiUndoAa86?.content}`);

    // T87 U AI has a persistent, discoverable launcher as well as the no-match command-palette
    // handoff. Both open the same assistant and use the same live command catalog plus L0 metadata.
    const directAiButton87 = document.getElementById('btn-ai-assistant');
    directAiButton87?.click();
    const directAiOpen87 = await waitUntil(() => window.__unicellAiTest?.getState().open === true);
    await waitUntil(() => !!window.__unicellAiTest?.getState().digest);
    const directEntry87 = directAiOpen87 && directAiButton87?.offsetParent !== null
      && directAiButton87?.getAttribute('aria-expanded') === 'true';
    aiHooks86?.close();
    window.UniCellUI?.openPalette();
    const cmdInput87 = document.getElementById('cmdk-input');
    cmdInput87.value = '§§§分析当前利润异常§§§';
    cmdInput87.dispatchEvent(new Event('input', { bubbles: true }));
    await wait(30);
    const aiCommand87 = document.querySelector('#cmdk-list .cmdk-item');
    const aiCommandText87 = aiCommand87?.textContent || '';
    aiCommand87?.click();
    await waitUntil(() => window.__unicellAiTest?.getState().open === true);
    await waitUntil(() => !!window.__unicellAiTest?.getState().digest);
    const aiRequest87 = aiHooks86?.buildModelRequest();
    const toolCatalog87 = window.UniCellCommandRegistry?.list?.() || [];
    const digest87 = aiHooks86?.getState().digest;
    const aiContract87 = directEntry87
      && aiCommandText87.includes('问 U AI：§§§分析当前利润异常§§§')
      && document.getElementById('ai-assistant')?.hidden === false
      && document.getElementById('ai-prompt')?.value === '§§§分析当前利润异常§§§'
      && toolCatalog87.length > 20
      && toolCatalog87.some((command) => command.id === 'btn-ai-assistant')
      && aiRequest87?.availableCommands?.length === toolCatalog87.length
      && aiRequest87?.constraints?.neverTouchOOXML === true
      && aiRequest87?.constraints?.dryRunBeforeCommit === true
      && Array.isArray(digest87?.tables) && Array.isArray(digest87?.pivotTables)
      && Array.isArray(digest87?.definedNames) && Array.isArray(digest87?.conditionalFormats)
      && Array.isArray(digest87?.dataValidations);
    ok('T87 U AI：常驻显式入口 + 命令面板入口 + 同源工具目录 + 完整 L0 结构', aiContract87,
      `direct=${directEntry87} command=${aiCommandText87} tools=${toolCatalog87.length} digest=${JSON.stringify(digest87)}`);

    // T88 The live model gateway is server-configured and the browser only consumes a strict
    // read-tool/write-op envelope; credentials and the upstream URL never enter this contract.
    const aiConfig88 = aiHooks86?.getState().aiConfig;
    const envelope88 = aiHooks86?.parseAssistantEnvelope(JSON.stringify({
      message: '读取销售区域',
      toolCalls: [{ tool: 'slice', ref: `${S.sheets[S.sheet]}!A1:B4` }],
      ops: [],
    }));
    const opsEnvelope88 = aiHooks86?.parseAssistantEnvelope(JSON.stringify({
      message: '建议公式',
      toolCalls: [],
      ops: [{ op: 'setFormula', ref: `${S.sheets[S.sheet]}!C2`, formula: '=SUM(A2:B2)' }],
    }));
    const commandEnvelope88 = aiHooks86?.parseAssistantEnvelope(JSON.stringify({
      message: '可以打开数据验证管理器',
      toolCalls: [],
      ops: [],
      commandSuggestions: [{ id: toolCatalog87[0]?.id, reason: '测试稳定命令 id' }],
    }));
    const gatewayContract88 = typeof aiConfig88?.configured === 'boolean'
      && typeof aiConfig88?.model === 'string' && aiConfig88.model.length > 0
      && !Object.hasOwn(aiConfig88, 'key') && !Object.hasOwn(aiConfig88, 'base')
      && envelope88?.toolCalls?.[0]?.tool === 'slice' && envelope88?.ops?.length === 0
      && opsEnvelope88?.ops?.[0]?.op === 'setFormula'
      && toolCatalog87.every((command) => typeof command.id === 'string'
        && command.parameters?.type === 'object' && command.requiresUserGesture === true)
      && commandEnvelope88?.commandSuggestions?.[0]?.id === toolCatalog87[0]?.id
      && window.UniCellCommandRegistry?.run?.('__missing_ai_command__') === false
      && aiRequest87?.version === 2 && aiRequest87?.readTools?.detail?.tool === 'detail';
    ok('T88 U AI：服务端模型配置 + 严格读取工具/写入操作协议', gatewayContract88,
      `config=${JSON.stringify(aiConfig88)} envelope=${JSON.stringify(envelope88)}`);

    // T89 The default surface is chat-first for beginners, while UniDoc-style width resizing
    // and full-screen mode remain available without exposing a developer-tools card.
    const aiPanel89 = document.getElementById('ai-assistant');
    const aiResizer89 = document.getElementById('ai-resizer');
    const aiFullscreen89 = document.getElementById('ai-fullscreen');
    const internalTools89 = document.getElementById('ai-internal-tools');
    const suggestions89 = Array.from(document.querySelectorAll('.ai-suggestions [data-ai-prompt]'));
    const originalWidth89 = aiPanel89?.getBoundingClientRect().width || 460;
    const originalFull89 = !!aiHooks86?.getState().fullscreen;
    aiHooks86?.toggleFullscreen(false, false);
    const targetWidth89 = Math.min(680, Math.max(360, innerWidth - 24));
    const resizedWidth89 = aiHooks86?.setWidth(targetWidth89, false);
    const widthContract89 = innerWidth <= 720
      || Math.abs((aiPanel89?.getBoundingClientRect().width || 0) - resizedWidth89) < 2;
    const fullOn89 = aiHooks86?.toggleFullscreen(true, false);
    const fullscreenContract89 = fullOn89 === true
      && aiPanel89?.classList.contains('ai-full')
      && aiPanel89?.getAttribute('aria-modal') === 'true'
      && aiFullscreen89?.getAttribute('aria-pressed') === 'true';
    const fullOff89 = aiHooks86?.toggleFullscreen(false, false);
    suggestions89[1]?.click();
    const beginnerContract89 = !document.getElementById('ai-advanced')
      && internalTools89?.hidden === true
      && !document.getElementById('ai-assistant')?.textContent.includes('高级工具')
      && suggestions89.length >= 3
      && document.getElementById('ai-prompt')?.value.includes('公式错误')
      && document.querySelector('.ai-composer')
      && aiResizer89?.getAttribute('role') === 'separator'
      && typeof aiHooks86?.setWidth === 'function'
      && typeof aiHooks86?.toggleFullscreen === 'function';
    const aiLayoutContract89 = beginnerContract89 && widthContract89 && fullscreenContract89
      && fullOff89 === false && !aiPanel89?.classList.contains('ai-full');
    ok('T89 U AI：小白对话优先 + 左拖拉宽 + 全屏/还原 + 无开发者工具卡', aiLayoutContract89,
      `beginner=${beginnerContract89} width=${resizedWidth89} full=${fullscreenContract89}`);
    aiHooks86?.setWidth(originalWidth89, false);
    aiHooks86?.toggleFullscreen(originalFull89, false);

    // T90 A simple arithmetic request is deterministically reduced to one formula in the
    // active cell, and user-facing messages are derived from the actual operation/diff.
    const arithmeticTask90 = '使用公式计算 99x 99999991 等于多少';
    const activeRef90 = aiHooks86?.activeCellRef();
    const controlledOps90 = aiHooks86?.normalizeTaskOps(arithmeticTask90, [
      { op: 'setFormula', ref: activeRef90, formula: '=99*99999991' },
      { op: 'setValue', ref: `${S.sheets[S.sheet]}!B1`, value: '计算结果' },
    ]);
    const formulaDiff90 = {
      diff: [{ ref: activeRef90, after: { content: '=99*99999991', formatted: '9899999109' } }],
    };
    const plan90 = aiHooks86?.operationPlanMessage(controlledOps90 || [], formulaDiff90);
    const applied90 = aiHooks86?.appliedOperationsMessage(controlledOps90 || [], {
      ...formulaDiff90,
    });
    const formulaTruth90 = aiHooks86?.simpleArithmeticFormula(arithmeticTask90) === '=99*99999991'
      && controlledOps90?.length === 1
      && controlledOps90[0]?.op === 'setFormula'
      && controlledOps90[0]?.ref === activeRef90
      && plan90?.includes(activeRef90) && plan90?.includes('同一单元格')
      && plan90?.includes('9899999109') && !plan90?.includes('B1')
      && applied90?.includes(activeRef90) && applied90?.includes('9899999109') && !applied90?.includes('B1');
    ok('T90 U AI：纯数字公式单格写入 + 真实地址/结果回执', formulaTruth90,
      `ops=${JSON.stringify(controlledOps90)} plan=${plan90} applied=${applied90}`);

    // T91 The app chrome, browser tab, launcher, and assistant header share one logo asset.
    const favicon91 = document.querySelector('link[rel="icon"]');
    const brandLogo91 = document.querySelector('.rb-brand');
    const launcherLogo91 = document.querySelector('.rb-ai-logo');
    const assistantLogo91 = document.querySelector('.ai-logo');
    const normalizedLogo91 = (element, attribute = 'src') => {
      try { return new URL(element?.getAttribute(attribute) || '', location.href).pathname; }
      catch (_) { return ''; }
    };
    const logoPath91 = '/unicell-logo.svg';
    const unifiedBrand91 = favicon91?.getAttribute('type') === 'image/svg+xml'
      && normalizedLogo91(favicon91, 'href') === logoPath91
      && normalizedLogo91(brandLogo91) === logoPath91
      && normalizedLogo91(launcherLogo91) === logoPath91
      && normalizedLogo91(assistantLogo91) === logoPath91
      && brandLogo91?.tagName === 'IMG' && assistantLogo91?.tagName === 'IMG';
    ok('T91 UniCell 品牌：应用/标签页/U AI 共用同一 SVG 标识', unifiedBrand91,
      `favicon=${favicon91?.href} brand=${brandLogo91?.src} ai=${assistantLogo91?.src}`);

    // T92 CSV is a real one-sheet interchange path: BOM/Chinese/quoted delimiters/newlines
    // import into the workbook engine, formulas calculate there, and export emits visible values.
    const csvButton92 = document.getElementById('btn-file-exp-csv');
    const openAccept92 = document.getElementById('file-input')?.accept || '';
    const csvPicker92 = buildSavePickerOptions('数据.csv');
    const csvPickerExtensions92 = csvPicker92.types.flatMap((type) => Object.values(type.accept).flat());
    const backup92 = await createWorkbookBlob({ quiet: true });
    let csvRoundtrip92 = false;
    let csvDetail92 = '';
    try {
      const csvSource92 = '\uFEFF姓名,备注,数值,公式\r\n小明,"含,逗号\n和换行",7,=3*4\r\n';
      const imported92 = await api('/api/import-csv', {
        method: 'POST', body: new TextEncoder().encode(csvSource92),
      });
      await loadWorkbook();
      const view92 = await api('/api/view?sheet=0&r0=1&c0=1&r1=2&c1=4');
      const exported92 = await fetch('/api/export-csv?name=csv-selftest&sheet=0');
      const bytes92 = new Uint8Array(await exported92.arrayBuffer());
      const text92 = new TextDecoder().decode(bytes92);
      const cell92 = (row, column) => view92.cells?.find((cell) => cell.r === row && cell.c === column)?.v;
      csvRoundtrip92 = imported92.rows === 2 && imported92.columns === 4
        && cell92(2, 1) === '小明' && cell92(2, 2) === '含,逗号\n和换行'
        && cell92(2, 4) === '12'
        && exported92.ok && exported92.headers.get('Content-Type')?.includes('text/csv')
        && bytes92[0] === 0xEF && bytes92[1] === 0xBB && bytes92[2] === 0xBF
        && text92.includes('"含,逗号\n和换行"') && text92.includes(',12\r\n');
      csvDetail92 = `${JSON.stringify(imported92)}/${JSON.stringify(view92.cells)}/${text92}`;
    } finally {
      await api('/api/import', { method: 'POST', body: await backup92.arrayBuffer() });
      await loadWorkbook();
    }
    const csvUi92 = !!csvButton92 && openAccept92.split(',').includes('.csv')
      && csvPickerExtensions92.includes('.csv') && workbookFormatFromName('数据.csv') === 'csv';
    ok('T92 CSV：打开/另存为/导出入口 + 中文/引号/换行/公式值往返', csvUi92 && csvRoundtrip92,
      `ui=${csvUi92} roundtrip=${csvRoundtrip92} detail=${csvDetail92}`);

    // T93 Processing progress is one live region updated in place. Long phase text must stay
    // on one visual line and ellipsize instead of adding a wrapped status transcript.
    const statusFirst93 = aiHooks86?.setRunStatus('正在组织工作簿摘要、选区和活动单元格上下文…');
    const statusSecond93 = aiHooks86?.setRunStatus('正在校验公式、数据验证和修改范围，并准备安全预览…');
    const statusStyle93 = statusSecond93 ? getComputedStyle(statusSecond93) : null;
    const statusTextStyle93 = statusSecond93?.querySelector('.ai-status-text')
      ? getComputedStyle(statusSecond93.querySelector('.ai-status-text')) : null;
    const liveStatusContract93 = !!statusFirst93 && statusFirst93 === statusSecond93
      && document.querySelectorAll('#ai-thread .ai-message-status').length === 1
      && statusSecond93?.getAttribute('role') === 'status'
      && statusSecond93?.getAttribute('aria-atomic') === 'true'
      && statusSecond93?.textContent.includes('正在校验公式')
      && statusStyle93?.whiteSpace === 'nowrap' && statusStyle93?.overflow === 'hidden'
      && statusTextStyle93?.whiteSpace === 'nowrap'
      && statusTextStyle93?.textOverflow === 'ellipsis';
    aiHooks86?.clearRunStatus();
    const statusCleared93 = document.querySelectorAll('#ai-thread .ai-message-status').length === 0;
    ok('T93 U AI：处理阶段单行动态更新 + 长文本省略 + 完成后清理',
      liveStatusContract93 && statusCleared93,
      `same=${statusFirst93 === statusSecond93} style=${statusStyle93?.whiteSpace}/${statusStyle93?.overflow}`
        + ` text=${statusTextStyle93?.whiteSpace}/${statusTextStyle93?.textOverflow} cleared=${statusCleared93}`);

    // T94 Formatting, native charts and PivotTables share the same typed-op parser and preview
    // surface. They are no longer downgraded to command suggestions or hidden raw-XML actions.
    const typedOps94 = [
      { op: 'setFormat', ref: 'Sheet1!A1:C3', style: { 'font.bold': true, 'fill.color': '#E8F5E9' } },
      { op: 'setBorder', ref: 'Sheet1!A1:C3', type: 'outer', style: 'thin', color: '#111827' },
      { op: 'updateChart', sheet: 'Sheet1', chartId: 'chart-stable-1', patch: { title: '销售趋势' } },
      { op: 'updatePivotTable', part: 'xl/pivotTables/pivotTable1.xml', patch: { display: { compact: false } } },
    ];
    let parsedTyped94 = null;
    let rejectsMissingPatch94 = false;
    try { parsedTyped94 = aiHooks86?.parseOps(JSON.stringify(typedOps94)); } catch (_) { /* asserted below */ }
    try {
      aiHooks86?.parseOps(JSON.stringify([{ op: 'updateChart', chartId: 'chart-stable-1' }]));
    } catch (_) { rejectsMissingPatch94 = true; }
    const plan94 = aiHooks86?.operationPlanMessage(typedOps94, { diff: [], objectDiff: [] });
    const applied94 = aiHooks86?.appliedOperationsMessage(typedOps94, { diff: [], objectDiff: [] });
    aiHooks86?.renderDiff({
      changedCells: 1,
      changedObjects: 1,
      diff: [{ ref: 'Sheet1!A1', before: { content: '收入', formatted: '收入', style: {} },
        after: { content: '收入', formatted: '收入', style: { font: { b: true } } } }],
      objectDiff: [{ target: 'chart:0:chart-stable-1', kind: 'chart',
        before: { model: { title: '旧标题' } }, after: { model: { title: '销售趋势' } } }],
      errors: [], validationViolations: [],
    });
    const request94 = aiHooks86?.buildModelRequest();
    const previewText94 = document.getElementById('ai-preview-card')?.textContent || '';
    const typedContract94 = parsedTyped94?.length === 4 && rejectsMissingPatch94
      && request94?.controlledWorkbookOps?.setFormat?.op === 'setFormat'
      && request94?.controlledWorkbookOps?.updateChart?.op === 'updateChart'
      && request94?.controlledWorkbookOps?.updatePivotTable?.op === 'updatePivotTable'
      && request94?.constraints?.capabilityFirstNoTokenSaving === true
      && request94?.constraints?.mixedTypedOpsAreAtomic === true
      && plan94?.includes('图表') && plan94?.includes('数据透视表')
      && applied94?.includes('一步撤销')
      && previewText94.includes('1 个单元格 + 1 个对象')
      && previewText94.includes('格式：') && previewText94.includes('销售趋势');
    ok('T94 U AI：格式/图表/透视表统一 typed ops + 对象级预览 + 一步撤销协议', typedContract94,
      `parsed=${JSON.stringify(parsedTyped94)} plan=${plan94} preview=${previewText94.slice(0, 400)}`);
    // T96 UniDoc-compatible font loading keeps the manifest fresh and every font content-addressed.
    const fontHooks96 = window.__UniCellFontRuntimeTest;
    const emptyMd596 = await fontHooks96?.md5(new Uint8Array());
    const fontManifestResponse96 = await fetch('/fonts/manifest.json', { cache: 'no-cache' });
    const fontManifestRaw96 = await fontManifestResponse96.json();
    const fontManifest96 = fontHooks96?.normalizeManifest(fontManifestRaw96);
    const fontHeaders96 = `${fontManifestResponse96.headers.get('cache-control') || ''} ${fontManifestResponse96.headers.get('permissions-policy') || ''}`;
    const safeUrls96 = fontManifest96?.files?.every((file) =>
      file.url.startsWith('/fonts/') && file.url.includes(`v=${file.sha256}`));
    const noRemote96 = fontHooks96?.normalizeManifest({
      schemaVersion: 1,
      files: [{ path: 'evil.ttf', sha256: 'a'.repeat(64), md5: 'b'.repeat(32), url: 'https://evil.example/evil.ttf' }],
      faces: [{ family: 'Evil', file: 'evil.ttf' }],
    });
    const fontContract96 = !!window.UniCellFonts && emptyMd596 === 'd41d8cd98f00b204e9800998ecf8427e'
      && fontManifestResponse96.ok && fontManifest96?.schemaVersion === 1
      && Array.isArray(fontManifest96.files) && Array.isArray(fontManifest96.faces)
      && safeUrls96 && noRemote96?.files.length === 0 && noRemote96?.faces.length === 0
      && /no-cache/i.test(fontHeaders96) && /local-fonts=\(self\)/i.test(fontHeaders96);
    ok('T96 字体：SHA-256/MD5 清单、本地优先边界、同源内容寻址回退', fontContract96,
      `md5=${emptyMd596} files=${fontManifest96?.files?.length} faces=${fontManifest96?.faces?.length} headers=${fontHeaders96}`);
    aiHooks86?.close();
  } catch (e) {
    out.push('FAIL exception :: ' + (e && e.message));
  }
  const passed = out.filter((l) => l.startsWith('PASS')).length;
  document.title = `TESTS ${passed}/${out.length} ${out.length - passed === 0 ? 'ALLPASS' : 'HASFAIL'}`;
  const pre = document.createElement('pre');
  pre.id = 'test-results';
  pre.textContent = out.join('\n');
  pre.style.cssText = 'position:fixed;right:8px;top:60px;z-index:999;background:#fff;border:1px solid #888;padding:8px;font-size:11px;max-width:480px;white-space:pre-wrap';
  document.body.appendChild(pre);
}
