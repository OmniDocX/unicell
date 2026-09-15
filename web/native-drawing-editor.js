// UniCell native DrawingML editor.
// Keeps the UI model separate from raw OOXML; the Rust exporter patches only explicitly edited
// chart/shape/SmartArt fields and preserves every unknown DrawingML child.
'use strict';

(() => {
  const objectUndo = [];
  const objectRedo = [];
  let historyArmed = false;

  const copy = (value) => value == null ? value : JSON.parse(JSON.stringify(value));
  const native = (o) => o && o.config && o.config.nativeDrawing;
  const drawingThemeFor = (o) => native(o)?.theme || S.workbookTheme || window.workbookTheme || DEFAULT_DRAWING_THEME;
  const clamp = (value, lo, hi) => Math.max(lo, Math.min(hi, Number(value) || 0));
  const xmlText = (value) => String(value ?? '')
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
  const DEFAULT_DRAWING_THEME = Object.freeze({
    dk1: '#000000', lt1: '#FFFFFF', dk2: '#44546A', lt2: '#E7E6E6',
    accent1: '#4472C4', accent2: '#ED7D31', accent3: '#A5A5A5',
    accent4: '#FFC000', accent5: '#5B9BD5', accent6: '#70AD47',
    hlink: '#0563C1', folHlink: '#954F72',
  });
  const cssColorCache = new Map();
  const systemColorFallback = {
    window: '#FFFFFF', windowText: '#000000', btnFace: '#F0F0F0', btnText: '#000000',
    highlight: '#3399FF', highlightText: '#FFFFFF', grayText: '#6D6D6D',
    menu: '#F0F0F0', menuText: '#000000', infoBk: '#FFFFE1', infoText: '#000000',
    activeCaption: '#99B4D1', inactiveCaption: '#BFCDDB', captionText: '#000000',
    inactiveCaptionText: '#434E54', appWorkspace: '#ABABAB', btnHighlight: '#FFFFFF',
    btnShadow: '#A0A0A0', threeDDkShadow: '#696969', threeDLight: '#E3E3E3',
    hotLight: '#0066CC', scrollBar: '#C8C8C8', background: '#000000',
  };
  const systemCssName = {
    window: 'Canvas', windowText: 'CanvasText', btnFace: 'ButtonFace', btnText: 'ButtonText',
    highlight: 'Highlight', highlightText: 'HighlightText', grayText: 'GrayText',
    menu: 'Canvas', menuText: 'CanvasText', infoBk: 'Canvas', infoText: 'CanvasText',
    activeCaption: 'ActiveCaption', inactiveCaption: 'InactiveCaption',
    captionText: 'CaptionText', inactiveCaptionText: 'InactiveCaptionText',
  };
  const commonPresetFallback = {
    black: '#000000', white: '#FFFFFF', red: '#FF0000', green: '#008000', blue: '#0000FF',
    yellow: '#FFFF00', cyan: '#00FFFF', aqua: '#00FFFF', magenta: '#FF00FF', fuchsia: '#FF00FF',
    gray: '#808080', grey: '#808080', silver: '#C0C0C0', maroon: '#800000', olive: '#808000',
    lime: '#00FF00', teal: '#008080', navy: '#000080', purple: '#800080', orange: '#FFA500',
    transparent: '#000000', dkSeaGreen: '#8FBC8F', ltBlue: '#ADD8E6', medBlue: '#0000CD',
  };
  const normalizedTheme = (theme) => {
    const source = theme && typeof theme === 'object' ? theme : {};
    const result = { ...DEFAULT_DRAWING_THEME, ...source };
    result.folHlink = source.folHlink || source.fol_hlink || result.folHlink;
    result.tx1 = result.dk1; result.bg1 = result.lt1;
    result.tx2 = result.dk2; result.bg2 = result.lt2;
    return result;
  };
  const validHex = (value) => /^#[0-9a-f]{6}$/i.test(String(value || ''));
  const hexRgb = (value) => validHex(value) ? [
    parseInt(String(value).slice(1, 3), 16) / 255,
    parseInt(String(value).slice(3, 5), 16) / 255,
    parseInt(String(value).slice(5, 7), 16) / 255,
  ] : null;
  const rgbHex = (rgb) => `#${rgb.map((component) => Math.round(clamp(component, 0, 1) * 255)
    .toString(16).padStart(2, '0')).join('').toUpperCase()}`;
  const rgbToHsl = ([r, g, b]) => {
    const max = Math.max(r, g, b), min = Math.min(r, g, b);
    const lightness = (max + min) / 2;
    if (max === min) return [0, 0, lightness];
    const delta = max - min;
    const saturation = lightness > .5 ? delta / (2 - max - min) : delta / (max + min);
    let hue = max === r ? (g - b) / delta + (g < b ? 6 : 0)
      : max === g ? (b - r) / delta + 2 : (r - g) / delta + 4;
    hue /= 6;
    return [hue, saturation, lightness];
  };
  const hslToRgb = ([hue, saturation, lightness]) => {
    const h = ((hue % 1) + 1) % 1, s = clamp(saturation, 0, 1), l = clamp(lightness, 0, 1);
    if (s === 0) return [l, l, l];
    const q = l < .5 ? l * (1 + s) : l + s - l * s, p = 2 * l - q;
    const channel = (offset) => {
      let t = h + offset;
      if (t < 0) t += 1; if (t > 1) t -= 1;
      if (t < 1 / 6) return p + (q - p) * 6 * t;
      if (t < 1 / 2) return q;
      if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
      return p;
    };
    return [channel(1 / 3), channel(0), channel(-1 / 3)];
  };
  const percentage = (transform) => {
    const raw = String(transform?.raw ?? '');
    const number = Number(transform?.value ?? raw.replace(/%$/, ''));
    if (!Number.isFinite(number)) return 0;
    return raw.trim().endsWith('%') ? number / 100 : number / 100000;
  };
  const angleFraction = (transform) => {
    const raw = String(transform?.raw ?? '');
    const number = Number(transform?.value ?? raw.replace(/deg$/, ''));
    if (!Number.isFinite(number)) return 0;
    return (raw.trim().endsWith('deg') ? number : number / 60000) / 360;
  };
  const linearToSrgb = (value) => value <= .0031308 ? 12.92 * value : 1.055 * value ** (1 / 2.4) - .055;
  const srgbToLinear = (value) => value <= .04045 ? value / 12.92 : ((value + .055) / 1.055) ** 2.4;
  const expandedPresetName = (token) => String(token || '')
    .replace(/^dk(?=[A-Z])/, 'dark').replace(/^lt(?=[A-Z])/, 'light').replace(/^med(?=[A-Z])/, 'medium')
    .replace('Goldenrod', 'GoldenRod');
  const browserColorHex = (name) => {
    const key = String(name || '');
    if (cssColorCache.has(key)) return cssColorCache.get(key);
    let result = null;
    try {
      const context = document.createElement('canvas').getContext('2d');
      context.fillStyle = '#010203';
      context.fillStyle = key;
      const normalized = String(context.fillStyle || '');
      if (validHex(normalized)) result = normalized.toUpperCase();
      else {
        const match = normalized.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)/i);
        if (match) result = rgbHex(match.slice(1, 4).map((part) => Number(part) / 255));
      }
    } catch {}
    cssColorCache.set(key, result);
    return result;
  };
  const colorSpecMatches = (raw, spec) => {
    if (!spec || typeof spec !== 'object') return false;
    const value = String(raw ?? '').trim();
    if (!value) return true;
    if (spec.type === 'srgb') return value.toUpperCase() === String(spec.value || '').toUpperCase();
    if (spec.type === 'scheme') return value.replace(/^scheme:/i, '') === String(spec.value || '');
    if (spec.type === 'preset') return value.replace(/^preset:/i, '') === String(spec.value || '');
    if (spec.type === 'system') return value.replace(/^system:|^sys:/i, '') === String(spec.value || '')
      || value.toUpperCase() === String(spec.lastColor || '').toUpperCase();
    if (spec.type === 'scrgb') return /^scrgb\(/i.test(value);
    if (spec.type === 'hsl') return /^hsl\(/i.test(value);
    return false;
  };
  const baseColorFromSpec = (spec, theme) => {
    if (!spec || typeof spec !== 'object') return null;
    if (spec.type === 'srgb') return hexRgb(spec.value);
    if (spec.type === 'scheme') return hexRgb(normalizedTheme(theme)[spec.value]);
    if (spec.type === 'system') {
      const saved = hexRgb(spec.lastColor);
      if (saved) return saved;
      const resolved = browserColorHex(systemCssName[spec.value]) || systemColorFallback[spec.value];
      return hexRgb(resolved);
    }
    if (spec.type === 'preset') {
      const resolved = browserColorHex(expandedPresetName(spec.value)) || commonPresetFallback[spec.value];
      return hexRgb(resolved);
    }
    if (spec.type === 'scrgb') return ['r', 'g', 'b'].map((key) => linearToSrgb(clamp(Number(spec[key]) / 100000, 0, 1)));
    if (spec.type === 'hsl') return hslToRgb([
      (Number(spec.hue) / 60000) / 360, Number(spec.sat) / 100000, Number(spec.lum) / 100000,
    ]);
    return null;
  };
  const applyColorTransforms = (base, transforms) => {
    let rgb = [...base], alpha = 1;
    for (const transform of transforms || []) {
      const type = transform?.type, amount = percentage(transform);
      if (type === 'tint') rgb = rgb.map((component) => component * amount + 1 - amount);
      else if (type === 'shade') rgb = rgb.map((component) => component * amount);
      else if (type === 'alpha') alpha = amount;
      else if (type === 'alphaMod') alpha *= amount;
      else if (type === 'alphaOff') alpha += amount;
      else if (type === 'red') rgb[0] = amount;
      else if (type === 'redMod') rgb[0] *= amount;
      else if (type === 'redOff') rgb[0] += amount;
      else if (type === 'green') rgb[1] = amount;
      else if (type === 'greenMod') rgb[1] *= amount;
      else if (type === 'greenOff') rgb[1] += amount;
      else if (type === 'blue') rgb[2] = amount;
      else if (type === 'blueMod') rgb[2] *= amount;
      else if (type === 'blueOff') rgb[2] += amount;
      else if (['hue', 'hueMod', 'hueOff', 'sat', 'satMod', 'satOff', 'lum', 'lumMod', 'lumOff'].includes(type)) {
        const hsl = rgbToHsl(rgb);
        if (type === 'hue') hsl[0] = angleFraction(transform);
        else if (type === 'hueMod') hsl[0] *= amount;
        else if (type === 'hueOff') hsl[0] += angleFraction(transform);
        else if (type === 'sat') hsl[1] = amount;
        else if (type === 'satMod') hsl[1] *= amount;
        else if (type === 'satOff') hsl[1] += amount;
        else if (type === 'lum') hsl[2] = amount;
        else if (type === 'lumMod') hsl[2] *= amount;
        else if (type === 'lumOff') hsl[2] += amount;
        rgb = hslToRgb(hsl);
      } else if (type === 'comp') {
        const hsl = rgbToHsl(rgb); hsl[0] += .5; rgb = hslToRgb(hsl);
      } else if (type === 'inv') rgb = rgb.map((component) => 1 - component);
      else if (type === 'gray') { const gray = .2126 * rgb[0] + .7152 * rgb[1] + .0722 * rgb[2]; rgb = [gray, gray, gray]; }
      else if (type === 'gamma') rgb = rgb.map((component) => linearToSrgb(clamp(component, 0, 1)));
      else if (type === 'invGamma') rgb = rgb.map((component) => srgbToLinear(clamp(component, 0, 1)));
      rgb = rgb.map((component) => clamp(component, 0, 1)); alpha = clamp(alpha, 0, 1);
    }
    return { color: rgbHex(rgb), alpha };
  };
  const specFromToken = (value) => {
    const raw = String(value || '').trim();
    if (validHex(raw)) return { type: 'srgb', value: raw.toUpperCase(), transforms: [] };
    if (/^scheme:/i.test(raw)) return { type: 'scheme', value: raw.slice(7), transforms: [] };
    if (/^preset:/i.test(raw)) return { type: 'preset', value: raw.slice(7), transforms: [] };
    if (/^(?:system|sys):/i.test(raw)) return { type: 'system', value: raw.replace(/^(?:system|sys):/i, ''), transforms: [] };
    if (Object.hasOwn(normalizedTheme({}), raw)) return { type: 'scheme', value: raw, transforms: [] };
    return null;
  };
  const resolveDrawingColor = (value, spec, theme) => {
    const source = colorSpecMatches(value, spec) ? spec : specFromToken(value);
    const base = baseColorFromSpec(source, theme);
    return base ? applyColorTransforms(base, source.transforms) : null;
  };
  const color = (value, fallback = null, spec = null, theme = null) =>
    resolveDrawingColor(value, spec, theme)?.color || (validHex(fallback) ? String(fallback).toUpperCase() : 'transparent');
  const drawingOpacity = (item, theme, fallback = 1) => {
    const resolved = resolveDrawingColor(item?.color, item?.colorSpec, theme);
    return clamp(resolved ? resolved.alpha : (item?.alpha ?? fallback), 0, 1);
  };
  window.DrawingMLColor = Object.freeze({
    defaultTheme: DEFAULT_DRAWING_THEME, normalizedTheme, resolve: resolveDrawingColor,
    colorSpecMatches, applyColorTransforms,
  });
  const equal = (a, b) => JSON.stringify(a) === JSON.stringify(b);
  const mergeModel = (base, patch) => {
    if (patch == null) return copy(base);
    if (Array.isArray(patch) || typeof patch !== 'object') return copy(patch);
    const result = base && typeof base === 'object' && !Array.isArray(base) ? copy(base) : {};
    Object.keys(patch).forEach((key) => { result[key] = mergeModel(result[key], patch[key]); });
    return result;
  };
  const modelDiff = (base, current) => {
    if (equal(base, current)) return undefined;
    if (Array.isArray(current) || current == null || typeof current !== 'object') return copy(current);
    const result = {};
    Object.keys(current).forEach((key) => {
      const difference = modelDiff(base && typeof base === 'object' ? base[key] : undefined, current[key]);
      if (difference !== undefined) result[key] = difference;
    });
    return Object.keys(result).length ? result : undefined;
  };
  const seriesSourceIndex = (baseSeries, current, fallback, used = new Set()) => {
    const candidates = (baseSeries || []).map((series, index) => ({ series, index })).filter(({ index }) => !used.has(index));
    const score = (series, index) => {
      let value = index === fallback ? 5 : 0;
      if (current.valueFormula && current.valueFormula === series.valueFormula) value += 120;
      if (current.categoryFormula && current.categoryFormula === series.categoryFormula) value += 70;
      if (current.name && current.name === series.name) value += 30;
      return value;
    };
    const best = candidates.sort((left, right) => score(right.series, right.index) - score(left.series, left.index))[0];
    return best && score(best.series, best.index) > 0 ? best.index : (fallback < (baseSeries || []).length ? fallback : null);
  };
  const indexedChartArrayDiff = (base, current, identityFields = ['index']) => {
    if (equal(base, current)) return undefined;
    return (current || []).map((entry, position) => {
      const identityKey = identityFields.find((key) => entry?.[key] != null);
      const baseline = identityKey
        ? (base || []).find((candidate) => candidate?.[identityKey] === entry?.[identityKey])
        : (base || [])[position];
      if (!baseline) return copy(entry);
      const difference = modelDiff(baseline, entry) || {};
      identityFields.forEach((key) => { if (entry?.[key] != null) difference[key] = copy(entry[key]); });
      return difference;
    });
  };
  const mergeIndexedChartArray = (base, patch, identityFields = ['index']) => {
    if (!Array.isArray(patch)) return copy(base || []);
    const used = new Set();
    return patch.map((entry, position) => {
      const identityKey = identityFields.find((key) => entry?.[key] != null);
      let sourceIndex = identityKey
        ? (base || []).findIndex((candidate, index) => !used.has(index) && candidate?.[identityKey] === entry?.[identityKey])
        : -1;
      if (sourceIndex < 0 && position < (base || []).length && !used.has(position)) sourceIndex = position;
      if (sourceIndex >= 0) used.add(sourceIndex);
      return mergeModel(sourceIndex >= 0 ? base[sourceIndex] : {}, entry);
    });
  };
  const dataLabelsDiff = (base, current) => {
    if (equal(base, current)) return undefined;
    if (current == null || base == null) return copy(current);
    const plainCurrent = copy(current); delete plainCurrent.labels;
    const plainBase = copy(base); delete plainBase.labels;
    const result = modelDiff(plainBase, plainCurrent) || {};
    const labels = indexedChartArrayDiff(base.labels || [], current.labels || [], ['index']);
    if (labels) result.labels = labels;
    return result;
  };
  const mergeDataLabels = (base, patch) => {
    if (patch == null) return copy(patch);
    if (base == null) return copy(patch);
    const plainPatch = copy(patch); delete plainPatch.labels;
    const result = mergeModel(base, plainPatch);
    if (Object.hasOwn(patch, 'labels')) result.labels = mergeIndexedChartArray(base.labels || [], patch.labels || [], ['index']);
    return result;
  };
  const chartSeriesEntryDiff = (base, current) => {
    if (!base) return copy(current);
    const plainCurrent = copy(current);
    const plainBase = copy(base);
    ['dataLabels', 'trendlines', 'errorBars'].forEach((key) => { delete plainCurrent[key]; delete plainBase[key]; });
    const result = modelDiff(plainBase, plainCurrent) || {};
    const labels = dataLabelsDiff(base.dataLabels, current.dataLabels);
    const trendlines = indexedChartArrayDiff(base.trendlines || [], current.trendlines || [], ['index']);
    const errorBars = indexedChartArrayDiff(base.errorBars || [], current.errorBars || [], ['direction', 'index']);
    if (labels !== undefined) result.dataLabels = labels;
    if (trendlines !== undefined) result.trendlines = trendlines;
    if (errorBars !== undefined) result.errorBars = errorBars;
    return result;
  };
  const mergeChartSeriesEntry = (base, patch) => {
    const plainPatch = copy(patch || {});
    ['dataLabels', 'trendlines', 'errorBars', '__sourceIndex'].forEach((key) => { delete plainPatch[key]; });
    const result = mergeModel(base || {}, plainPatch);
    if (Object.hasOwn(patch || {}, 'dataLabels')) result.dataLabels = mergeDataLabels(base?.dataLabels, patch.dataLabels);
    if (Object.hasOwn(patch || {}, 'trendlines')) result.trendlines = mergeIndexedChartArray(base?.trendlines || [], patch.trendlines || [], ['index']);
    if (Object.hasOwn(patch || {}, 'errorBars')) result.errorBars = mergeIndexedChartArray(base?.errorBars || [], patch.errorBars || [], ['direction', 'index']);
    return result;
  };
  const keyedChartArrayDiff = (base, current, key, deepKind = '') => {
    if (equal(base, current)) return undefined;
    return (current || []).map((entry) => {
      if (entry?.$delete) return { [key]: entry[key], $delete: true };
      const baseline = (base || []).find((candidate) => Number(candidate?.[key]) === Number(entry?.[key]));
      if (!baseline) return copy(entry);
      const plainEntry = copy(entry), plainBaseline = copy(baseline);
      if (deepKind === 'plot') { delete plainEntry.dataLabels; delete plainBaseline.dataLabels; }
      const difference = modelDiff(plainBaseline, plainEntry) || {};
      if (deepKind === 'plot') {
        const labels = dataLabelsDiff(baseline.dataLabels, entry.dataLabels);
        if (labels !== undefined) difference.dataLabels = labels;
      }
      return { [key]: entry[key], ...difference };
    }).filter((entry) => Object.keys(entry).length > 1 || !(base || []).some((candidate) => Number(candidate?.[key]) === Number(entry?.[key])));
  };
  const chartModelDiff = (base, current) => {
    if (equal(base, current)) return undefined;
    const result = {};
    Object.keys(current || {}).forEach((key) => {
      if (['axes', 'plots', 'series'].includes(key)) return;
      const difference = modelDiff(base?.[key], current[key]);
      if (difference !== undefined) result[key] = difference;
    });
    const axes = keyedChartArrayDiff(base?.axes || [], current?.axes || [], 'id');
    const plots = keyedChartArrayDiff(base?.plots || [], current?.plots || [], 'index', 'plot');
    if (axes?.length) result.axes = axes;
    if (plots?.length) result.plots = plots;
    if (!equal(base?.series || [], current?.series || [])) {
      const used = new Set();
      const needsPlotAssignment = (current?.plots || []).filter((plot) => !plot?.$delete).length > 1;
      result.series = (current?.series || []).map((series, position) => {
        const sourceIndex = seriesSourceIndex(base?.series || [], series, position, used);
        if (sourceIndex != null) used.add(sourceIndex);
        const baseline = sourceIndex == null ? undefined : base.series[sourceIndex];
        const difference = baseline ? chartSeriesEntryDiff(baseline, series) : copy(series);
        const identity = {
          __sourceIndex: sourceIndex,
          name: series.name ?? '',
          categoryFormula: series.categoryFormula ?? '',
          valueFormula: series.valueFormula ?? '',
        };
        if (needsPlotAssignment) Object.assign(identity, {
          plotIndex: series.plotIndex ?? 0,
          plotType: series.plotType ?? '',
          axisIds: copy(series.axisIds || []),
          axisGroup: series.axisGroup ?? 'primary',
        });
        else ['plotIndex', 'plotType', 'axisIds', 'axisGroup'].forEach((key) => { delete difference[key]; });
        return { ...identity, ...difference };
      });
    }
    return Object.keys(result).length ? result : undefined;
  };
  const mergeKeyedChartArray = (base, patch, key, deepKind = '') => {
    if (!Array.isArray(patch)) return copy(base || []);
    const result = copy(base || []);
    const tombstones = [];
    patch.forEach((entry) => {
      const index = result.findIndex((candidate) => Number(candidate?.[key]) === Number(entry?.[key]));
      if (entry?.$delete) {
        if (index >= 0) result.splice(index, 1);
        tombstones.push(copy(entry));
      } else if (index >= 0) {
        if (deepKind === 'plot' && Object.hasOwn(entry, 'dataLabels')) {
          const baselineLabels = result[index]?.dataLabels;
          const plainEntry = copy(entry); delete plainEntry.dataLabels;
          result[index] = mergeModel(result[index], plainEntry);
          result[index].dataLabels = mergeDataLabels(baselineLabels, entry.dataLabels);
        } else result[index] = mergeModel(result[index], entry);
      }
      else result.push(copy(entry));
    });
    return [...result, ...tombstones];
  };
  const mergeChartSeries = (base, patch) => {
    if (!Array.isArray(patch)) return copy(base || []);
    const used = new Set();
    return patch.map((entry, position) => {
      const explicit = Number.isInteger(entry?.__sourceIndex) ? entry.__sourceIndex : null;
      const sourceIndex = explicit != null ? explicit : seriesSourceIndex(base || [], entry || {}, position, used);
      if (sourceIndex != null) used.add(sourceIndex);
      return mergeChartSeriesEntry(sourceIndex == null ? {} : base[sourceIndex], entry);
    });
  };
  const mergeChartModel = (base, patch) => {
    if (!patch || typeof patch !== 'object') return copy(base);
    const plainPatch = copy(patch);
    delete plainPatch.axes; delete plainPatch.plots; delete plainPatch.series;
    const result = mergeModel(base, plainPatch);
    if (Object.hasOwn(patch, 'axes')) result.axes = mergeKeyedChartArray(base?.axes, patch.axes, 'id');
    if (Object.hasOwn(patch, 'plots')) result.plots = mergeKeyedChartArray(base?.plots, patch.plots, 'index', 'plot');
    if (Object.hasOwn(patch, 'series')) result.series = mergeChartSeries(base?.series, patch.series);
    return result;
  };
  const replaceObject = (target, source) => {
    Object.keys(target).forEach((key) => { delete target[key]; });
    Object.assign(target, copy(source));
  };
  const notifyHistory = () => window.updateUndoRedoButtons?.();
  const queueObjectWrite = (payload) => window.queueObjectWrite
    ? window.queueObjectWrite(payload)
    : apiPost('/api/objects', payload);
  const commitNativeObject = async (candidate) => {
    const object = copy(candidate);
    const payload = { op: 'update', sheet: object.sheet, id: object.id, object };
    if (window.queueObjectMutation) {
      return window.queueObjectMutation(async () => {
        await apiPost('/api/native-drawing/validate', { object });
        return apiPost('/api/objects', payload);
      });
    }
    await window.flushObjectWrites?.();
    await apiPost('/api/native-drawing/validate', { object });
    return queueObjectWrite(payload);
  };
  const setColorInput = (input, raw, fallback = '#808080', spec = null, theme = null) => {
    input.value = color(raw, fallback, spec, theme);
    input._nativeRawColor = copy(raw);
    input.dataset.colorChanged = '0';
    input.addEventListener('input', () => { input.dataset.colorChanged = '1'; });
  };
  const getColorInput = (input) => input.dataset.colorChanged === '1' ? input.value : copy(input._nativeRawColor);
  const preserveSelectValue = (select, value, label = '') => {
    const raw = value == null ? '' : String(value);
    if (![...select.options].some((option) => option.value === raw)) {
      const option = document.createElement('option');
      option.value = raw;
      option.textContent = label || `${raw || '（无）'}（保留原值）`;
      option.dataset.preserve = '1';
      select.appendChild(option);
    }
    select.value = raw;
  };
  const parseSlotList = (value) => {
    const raw = String(value ?? '');
    if (raw === '') return [];
    return raw.split(/[\n,，]/).map((item) => item.trim());
  };
  const parseNumberSlots = (value) => parseSlotList(value).map((item, index) => {
    if (item === '') return null;
    const number = Number(item);
    if (!Number.isFinite(number)) throw new Error(`数值数据第 ${index + 1} 项“${item}”不是有效数字`);
    return number;
  });
  const formatPointOverrides = (points) => (points || []).map((point) => [
    point.index,
    point.color || '',
    point.explosion == null ? '' : point.explosion,
    point.markerSymbol || '',
    point.markerSize == null ? '' : point.markerSize,
  ].join(' | ')).join('\n');
  const parsePointOverrides = (value) => {
    const rows = String(value ?? '').split(/\r?\n/).map((row) => row.trim()).filter(Boolean);
    const seen = new Set();
    return rows.map((row, rowIndex) => {
      const fields = row.split('|').map((field) => field.trim());
      if (fields.length > 5) throw new Error(`单点覆盖第 ${rowIndex + 1} 行字段过多`);
      while (fields.length < 5) fields.push('');
      const index = Number(fields[0]);
      if (!Number.isInteger(index) || index < 0) throw new Error(`单点覆盖第 ${rowIndex + 1} 行索引必须是非负整数`);
      if (seen.has(index)) throw new Error(`单点覆盖索引 ${index} 重复`);
      seen.add(index);
      const explosion = fields[2] === '' ? null : Number(fields[2]);
      if (explosion != null && (!Number.isInteger(explosion) || explosion < 0 || explosion > 400)) {
        throw new Error(`单点覆盖第 ${rowIndex + 1} 行分离比例必须是 0–400 的整数`);
      }
      const markerSize = fields[4] === '' ? null : Number(fields[4]);
      if (markerSize != null && (!Number.isInteger(markerSize) || markerSize < 2 || markerSize > 72)) {
        throw new Error(`单点覆盖第 ${rowIndex + 1} 行标记大小必须是 2–72 的整数`);
      }
      if (markerSize != null && !fields[3]) throw new Error(`单点覆盖第 ${rowIndex + 1} 行设置大小时必须填写标记类型`);
      return {
        index,
        color: fields[1],
        explosion,
        markerSymbol: fields[3],
        markerSize,
      };
    });
  };
  const chartFamily = (kind) => {
    if (['bar', 'line', 'area', 'radar'].includes(kind)) return 'category';
    if (['pie', 'doughnut'].includes(kind)) return 'pie';
    if (['scatter', 'bubble'].includes(kind)) return 'xy';
    if (kind === 'stock') return 'stock';
    if (kind === 'combo') return 'combo';
    return `unknown:${kind || ''}`;
  };

  function setObjectState(sheet, id, value) {
    if (Number(sheet) !== Number(S.sheet)) return;
    const index = (S.objects || []).findIndex((item) => item.id === id);
    if (value == null) {
      if (index >= 0) S.objects.splice(index, 1);
      return;
    }
    const restored = copy(value);
    restored.sheet = sheet;
    if (index >= 0) S.objects[index] = restored;
    else (S.objects = S.objects || []).push(restored);
  }

  async function applyHistoryState(entry, value, operation) {
    const payload = { op: operation, sheet: entry.sheet, id: entry.id };
    if (value != null) {
      payload.object = copy(value);
      payload.object.sheet = entry.sheet;
    }
    await queueObjectWrite(payload);
    if (Number(entry.sheet) !== Number(S.sheet)) return;
    setObjectState(entry.sheet, entry.id, value);
    if (value == null && S.selObj === entry.id) S.selObj = null;
    if (value != null) S.selObj = entry.id;
    renderObjects();
    selectObject(value == null ? null : entry.id);
  }

  window.recordObjectHistory = (entry) => {
    if (!entry || !entry.id) return;
    if (equal(entry.before, entry.after)) return;
    const op = entry.op || (entry.before == null ? 'add' : entry.after == null ? 'delete' : 'update');
    objectUndo.push({ ...entry, op, before: copy(entry.before), after: copy(entry.after) });
    if (objectUndo.length > 100) objectUndo.shift();
    objectRedo.length = 0;
    historyArmed = true;
    notifyHistory();
  };
  window.armObjectHistory = () => { historyArmed = true; notifyHistory(); };
  window.disarmObjectHistory = () => { historyArmed = false; notifyHistory(); };
  window.clearObjectHistory = () => {
    objectUndo.length = 0;
    objectRedo.length = 0;
    historyArmed = false;
    notifyHistory();
  };
  window.nativeObjectCanUndo = () => historyArmed && objectUndo.length > 0;
  window.nativeObjectCanRedo = () => historyArmed && objectRedo.length > 0;
  window.nativeObjectUndo = async () => {
    if (!window.nativeObjectCanUndo()) return false;
    const entry = objectUndo[objectUndo.length - 1];
    const operation = entry.op === 'delete' ? 'add' : entry.op === 'add' ? 'delete' : 'update';
    try {
      await applyHistoryState(entry, entry.before, operation);
    } catch (error) {
      setStatus(`撤销对象失败：${error?.message || error}`);
      notifyHistory();
      return true;
    }
    objectUndo.pop();
    objectRedo.push(entry);
    setStatus(`已撤销：${entry.label || '对象操作'}`);
    historyArmed = true;
    notifyHistory();
    return true;
  };
  window.nativeObjectRedo = async () => {
    if (!window.nativeObjectCanRedo()) return false;
    const entry = objectRedo[objectRedo.length - 1];
    const operation = entry.op === 'add' ? 'add' : entry.op === 'delete' ? 'delete' : 'update';
    try {
      await applyHistoryState(entry, entry.after, operation);
    } catch (error) {
      setStatus(`重做对象失败：${error?.message || error}`);
      notifyHistory();
      return true;
    }
    objectRedo.pop();
    objectUndo.push(entry);
    setStatus(`已重做：${entry.label || '对象操作'}`);
    historyArmed = true;
    notifyHistory();
    return true;
  };

  window.prepareNativeDrawingClone = (source, target) => {
    const descriptor = native(target);
    if (!descriptor) return target;
    const originalToken = native(source).cloneOfToken || native(source).token;
    descriptor.clone = true;
    descriptor.cloneOfToken = originalToken;
    descriptor.token = `native-clone-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
    descriptor.nonVisualId = native(source).nonVisualId;
    return target;
  };

  const chartPaint = (value, spec, fallback, theme) => {
    const resolved = resolveDrawingColor(value, spec, theme);
    if (resolved) return resolved.color;
    // A missing colour inherits the chart palette. An opaque vendor colour token stays intact in
    // OOXML and gets a neutral preview rather than being misrepresented as Office accent1.
    return value || spec ? '#808080' : fallback;
  };
  const chartSeriesPaint = (series, index, theme) => chartPaint(
    series?.color,
    series?.colorSpec,
    normalizedTheme(theme)[`accent${index % 6 + 1}`],
    theme,
  );

  function chartPreview(model, theme) {
    const series = Array.isArray(model.series) ? model.series : [];
    const W = 800, H = 450, L = 70, R = 24, T = 58, B = 58;
    const pw = W - L - R, ph = H - T - B;
    const all = series.flatMap((s) => (s.values || []).map(Number).filter(Number.isFinite));
    const min = Math.min(0, ...(all.length ? all : [0]));
    const max = Math.max(1, ...(all.length ? all : [1]));
    const span = max - min || 1;
    const y = (v) => T + ph - (Number(v) - min) / span * ph;
    const n = Math.max(1, ...series.map((s) => (s.values || []).length));
    let marks = '';
    for (let i = 0; i <= 4; i++) {
      const yy = T + ph * i / 4;
      marks += `<line x1="${L}" y1="${yy}" x2="${W - R}" y2="${yy}" stroke="#E5E7EB"/>`;
      marks += `<text x="${L - 8}" y="${yy + 4}" text-anchor="end" font-size="11" fill="#667085">${xmlText((max - span * i / 4).toFixed(1).replace(/\.0$/, ''))}</text>`;
    }
    const type = model.chartType || 'bar';
    if (type === 'pie' || type === 'doughnut') {
      const values = (series[0] && series[0].values || []).map((v) => Math.max(0, Number(v) || 0));
      const sum = values.reduce((a, b) => a + b, 0) || 1;
      let angle = -Math.PI / 2;
      const cx = W / 2, cy = T + ph / 2, radius = Math.min(pw, ph) * .38;
      values.forEach((value, i) => {
        const next = angle + value / sum * Math.PI * 2;
        const x1 = cx + Math.cos(angle) * radius, y1 = cy + Math.sin(angle) * radius;
        const x2 = cx + Math.cos(next) * radius, y2 = cy + Math.sin(next) * radius;
        const large = next - angle > Math.PI ? 1 : 0;
        const selected = series[i] || series[0] || {};
        const point = (series[0]?.pointOverrides || []).find((entry) => Number(entry.index) === i);
        const fill = point?.color || point?.colorSpec
          ? chartPaint(point.color, point.colorSpec, chartSeriesPaint(selected, i, theme), theme)
          : chartSeriesPaint(selected, i, theme);
        marks += `<path d="M ${cx} ${cy} L ${x1} ${y1} A ${radius} ${radius} 0 ${large} 1 ${x2} ${y2} Z" fill="${fill}"/>`;
        angle = next;
      });
      if (type === 'doughnut') marks += `<circle cx="${cx}" cy="${cy}" r="${radius * .52}" fill="white"/>`;
    } else if (type === 'bar' || type === 'column') {
      const group = pw / n;
      const bw = Math.max(3, group * .72 / Math.max(1, series.length));
      series.forEach((s, si) => (s.values || []).forEach((value, i) => {
        const top = y(Math.max(0, Number(value) || 0));
        const base = y(Math.min(0, Number(value) || 0));
        marks += `<rect x="${L + i * group + group * .14 + si * bw}" y="${Math.min(top, base)}" width="${bw}" height="${Math.max(1, Math.abs(base - top))}" fill="${chartSeriesPaint(s, si, theme)}"/>`;
      }));
    } else {
      series.forEach((s, seriesIndex) => {
        const points = (s.values || []).map((value, i) => `${L + (n === 1 ? pw / 2 : i * pw / (n - 1))},${y(Number(value) || 0)}`).join(' ');
        const paint = chartSeriesPaint(s, seriesIndex, theme);
        if (type === 'area') marks += `<polygon points="${points} ${L + pw},${y(0)} ${L},${y(0)}" fill="${paint}" opacity=".25"/>`;
        marks += `<polyline points="${points}" fill="none" stroke="${paint}" stroke-width="3"/>`;
        if (type === 'scatter') marks += (s.values || []).map((value, i) => `<circle cx="${L + (n === 1 ? pw / 2 : i * pw / (n - 1))}" cy="${y(Number(value) || 0)}" r="4" fill="${paint}"/>`).join('');
      });
    }
    const labels = ((series[0] && series[0].categories) || []).map((label, i) => {
      if (i >= n || (n > 10 && i % Math.ceil(n / 10))) return '';
      const x = L + (n === 1 ? pw / 2 : i * pw / Math.max(1, n - 1));
      return `<text x="${x}" y="${H - 28}" text-anchor="middle" font-size="11" fill="#475467">${xmlText(label)}</text>`;
    }).join('');
    const legend = model.legend && model.legend.show === false ? '' : series.map((s, i) => `<g transform="translate(${L + i * 150},${H - 8})"><rect width="12" height="8" y="-8" fill="${chartSeriesPaint(s, i, theme)}"/><text x="18" font-size="11" fill="#344054">${xmlText(s.name || `系列 ${i + 1}`)}</text></g>`).join('');
    return `<!doctype html><html><body style="margin:0;overflow:hidden;background:#fff"><svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="100%" height="100%"><rect width="100%" height="100%" fill="white"/><text x="${W / 2}" y="30" text-anchor="middle" font-family="Segoe UI,Arial" font-size="20" font-weight="600" fill="#101828">${xmlText(model.title || 'Excel 图表')}</text>${marks}<line x1="${L}" y1="${y(0)}" x2="${W - R}" y2="${y(0)}" stroke="#98A2B3"/>${labels}${legend}</svg></body></html>`;
  }

  function shapePreview(model, theme) {
    const fill = model.fill || {};
    const line = model.line || {};
    const effects = model.effects || {};
    const stops = (fill.stops || [{ position: 0, color: fill.color || 'scheme:accent1', alpha: 1 }, { position: 100, color: '#FFFFFF', alpha: 1 }])
      .map((s) => `<stop offset="${clamp(s.position, 0, 100)}%" stop-color="${color(s.color, null, s.colorSpec, theme)}" stop-opacity="${drawingOpacity(s, theme)}"/>`).join('');
    const paint = fill.kind === 'none' ? 'none' : fill.kind === 'gradient' ? 'url(#g)' : color(fill.color, null, fill.colorSpec, theme);
    const shadow = effects.shadow && effects.shadow.enabled ? `<filter id="fx" x="-30%" y="-30%" width="160%" height="160%"><feDropShadow dx="${Math.cos((effects.shadow.angle || 45) * Math.PI / 180) * (effects.shadow.distance || 6)}" dy="${Math.sin((effects.shadow.angle || 45) * Math.PI / 180) * (effects.shadow.distance || 6)}" stdDeviation="${Math.max(0, (effects.shadow.blur || 6) / 2)}" flood-color="${color(effects.shadow.color, '#000000', effects.shadow.colorSpec, theme)}" flood-opacity="${drawingOpacity(effects.shadow, theme, .35)}"/></filter>` : '';
    const attrs = `fill="${paint}" fill-opacity="${drawingOpacity(fill, theme)}" stroke="${color(line.color, '#667085', line.colorSpec, theme)}" stroke-opacity="${drawingOpacity(line, theme)}" stroke-width="${Math.max(0, Number(line.width) || 1) * 4}" ${line.dash && line.dash !== 'solid' ? 'stroke-dasharray="22 14"' : ''} filter="${shadow ? 'url(#fx)' : ''}"`;
    const geometry = model.geometry || 'rect';
    const shape = geometry === 'ellipse' ? `<ellipse cx="500" cy="300" rx="450" ry="250" ${attrs}/>`
      : geometry.includes('Connector') || geometry === 'line' ? `<line x1="60" y1="540" x2="940" y2="60" ${attrs}/>`
      : geometry === 'roundRect' ? `<rect x="50" y="50" width="900" height="500" rx="70" ${attrs}/>`
      : geometry === 'triangle' ? `<path d="M500 45 L955 555 L45 555 Z" ${attrs}/>`
      : `<rect x="50" y="50" width="900" height="500" ${attrs}/>`;
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 600" preserveAspectRatio="none"><defs><linearGradient id="g" gradientTransform="rotate(${Number(fill.angle) || 0} .5 .5)">${stops}</linearGradient>${shadow}</defs><g transform="rotate(${Number(model.rotation) || 0} 500 300) scale(${model.flipH ? -1 : 1} ${model.flipV ? -1 : 1}) translate(${model.flipH ? -1000 : 0} ${model.flipV ? -600 : 0})">${shape}<text x="500" y="310" text-anchor="middle" dominant-baseline="middle" font-family="Calibri,Segoe UI" font-size="54" fill="#1F2937">${xmlText(model.text || '')}</text></g></svg>`;
  }

  function smartArtPreview(model, theme) {
    const nodes = Array.isArray(model.nodes) ? [...model.nodes] : [];
    nodes.sort((a, b) => (Number(a.order) || 0) - (Number(b.order) || 0));
    const byParent = new Map();
    nodes.forEach((node) => {
      const key = node.parentId || '';
      if (!byParent.has(key)) byParent.set(key, []);
      byParent.get(key).push(node);
    });
    const roots = nodes.filter((node) => !node.parentId || !nodes.some((p) => p.id === node.parentId));
    const positions = new Map();
    const levels = [];
    let queue = roots.map((node) => ({ node, level: 0 }));
    const seen = new Set();
    while (queue.length) {
      const { node, level } = queue.shift();
      if (seen.has(node.id)) continue;
      seen.add(node.id);
      (levels[level] ||= []).push(node);
      (byParent.get(node.id) || []).forEach((child) => queue.push({ node: child, level: level + 1 }));
    }
    nodes.filter((n) => !seen.has(n.id)).forEach((n) => (levels[0] ||= []).push(n));
    levels.forEach((items, level) => items.forEach((node, index) => positions.set(node.id, {
      x: 80 + (index + 1) * 840 / (items.length + 1), y: 65 + level * Math.min(150, 330 / Math.max(1, levels.length - 1)),
    })));
    const edges = nodes.filter((n) => n.parentId && positions.has(n.parentId)).map((n) => {
      const a = positions.get(n.parentId), b = positions.get(n.id);
      return `<path d="M${a.x} ${a.y + 35} L${b.x} ${b.y - 35}" stroke="#98A2B3" stroke-width="4" fill="none"/>`;
    }).join('');
    const boxes = nodes.map((n, i) => {
      const p = positions.get(n.id) || { x: 500, y: 300 };
      const nodePaint = color(`scheme:accent${i % 6 + 1}`, null, null, theme);
      return `<g><rect x="${p.x - 100}" y="${p.y - 35}" width="200" height="70" rx="16" fill="${nodePaint}" fill-opacity=".22" stroke="${nodePaint}" stroke-width="3"/><text x="${p.x}" y="${p.y + 5}" text-anchor="middle" font-family="Calibri,Segoe UI" font-size="25" fill="${color('scheme:tx1', '#1F2937', null, theme)}">${xmlText(n.text || '节点')}</text></g>`;
    }).join('');
    return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1000 600" preserveAspectRatio="none"><rect width="100%" height="100%" fill="#fff"/>${edges}${boxes}</svg>`;
  }

  function updatePreview(o, kind, model) {
    const theme = drawingThemeFor(o);
    if (kind === 'chart') o.config.code = chartPreview(model, theme);
    else if (kind === 'smartart') { o.type = 'svg'; o.config.svg = smartArtPreview(model, theme); }
    else { o.type = 'svg'; o.config.svg = shapePreview(model, theme); }
  }

  function staticInput(type, cls, title) {
    const input = document.createElement('input');
    input.type = type;
    if (cls) input.className = cls;
    if (title) input.title = title;
    return input;
  }

  const nullableBooleanValue = (value) => value == null ? '' : value ? 'true' : 'false';
  const readNullableBoolean = (control) => control.value === '' ? null : control.value === 'true';
  const readNullableNumber = (control, label) => {
    const raw = String(control.value ?? '').trim();
    if (!raw) return null;
    const value = Number(raw);
    if (!Number.isFinite(value)) throw new Error(`${label}必须是有效数字`);
    return value;
  };
  const markChartControl = (control, onChange, eventName) => {
    control.addEventListener(eventName || (control.tagName === 'SELECT' || control.type === 'checkbox' ? 'change' : 'input'), () => {
      control.dataset.nativeChanged = '1';
      onChange();
    });
  };
  const setNullableBoolean = (control, value) => {
    preserveSelectValue(control, nullableBooleanValue(value), '原布尔值（保留）');
  };
  const chartTypeOptions = '<option value="bar">柱形/条形</option><option value="line">折线</option><option value="area">面积</option><option value="pie">饼图</option><option value="doughnut">圆环</option><option value="scatter">散点</option><option value="bubble">气泡</option><option value="radar">雷达</option><option value="stock">股价</option>';
  const dataLabelPositionOptions = '<option value="">自动/未设置</option><option value="center">居中</option><option value="insideBase">内部基准</option><option value="insideEnd">内部末端</option><option value="outsideEnd">外部末端</option><option value="bestFit">最佳匹配</option><option value="top">顶部</option><option value="bottom">底部</option><option value="left">左侧</option><option value="right">右侧</option>';
  const triStateOptions = '<option value="">继承/未设置</option><option value="true">是</option><option value="false">否</option>';

  function buildDataLabelsEditor(host, originalValue, onChange, caption = '数据标签') {
    const original = originalValue && typeof originalValue === 'object' ? copy(originalValue) : null;
    let touched = false;
    let labelsTouched = false;
    host.innerHTML = `<details class="native-chart-subeditor"><summary>${caption}</summary>
      <div class="native-chart-subeditor-body">
        <label class="native-check native-inline-check"><input data-dl="enabled" type="checkbox">启用数据标签</label>
        <div class="native-grid native-chart-label-grid" data-dl-panel>
          <label>标签位置<select data-dl="position">${dataLabelPositionOptions}</select></label>
          <label>分隔符<input data-dl="separator" placeholder="自动"></label>
          <label class="native-check"><input data-dl="numFmtEnabled" type="checkbox">自定义数字格式</label>
          <label>格式代码<input data-dl="numFmtCode" placeholder="0.00%"></label>
          <label>格式链接源<select data-dl="numFmtSource">${triStateOptions}</select></label>
          <label>隐藏标签<select data-dl="delete">${triStateOptions}</select></label>
          <label>显示图例项<select data-dl="showLegendKey">${triStateOptions}</select></label>
          <label>显示值<select data-dl="showValue">${triStateOptions}</select></label>
          <label>显示分类名<select data-dl="showCategoryName">${triStateOptions}</select></label>
          <label>显示系列名<select data-dl="showSeriesName">${triStateOptions}</select></label>
          <label>显示百分比<select data-dl="showPercent">${triStateOptions}</select></label>
          <label>显示气泡大小<select data-dl="showBubbleSize">${triStateOptions}</select></label>
          <label>显示引导线<select data-dl="showLeaderLines">${triStateOptions}</select></label>
          <label>显示单元格标签范围<select data-dl="showDataLabelsRange">${triStateOptions}</select></label>
        </div>
        <div class="native-chart-list-title"><span>逐数据点覆盖</span><button type="button" data-add-label>＋ 标签</button></div>
        <div class="native-chart-card-list" data-label-list></div>
      </div>
    </details>`;
    const enabled = host.querySelector('[data-dl=enabled]');
    const panel = host.querySelector('[data-dl-panel]');
    const labelList = host.querySelector('[data-label-list]');
    enabled.checked = !!original;
    const setTouched = () => { touched = true; onChange(); };
    const syncEnabled = () => {
      panel.querySelectorAll('input,select').forEach((control) => { control.disabled = !enabled.checked; });
      host.querySelector('[data-add-label]').disabled = !enabled.checked;
      labelList.classList.toggle('native-disabled', !enabled.checked);
    };
    const field = (name) => host.querySelector(`[data-dl=${name}]`);
    preserveSelectValue(field('position'), original?.position ?? '', '原标签位置（保留）');
    field('separator').value = original?.separator ?? '';
    field('numFmtEnabled').checked = !!original?.numberFormat;
    field('numFmtCode').value = original?.numberFormat?.code ?? '';
    setNullableBoolean(field('numFmtSource'), original?.numberFormat?.sourceLinked ?? null);
    ['delete', 'showLegendKey', 'showValue', 'showCategoryName', 'showSeriesName', 'showPercent',
      'showBubbleSize', 'showLeaderLines', 'showDataLabelsRange'].forEach((name) => {
      setNullableBoolean(field(name), original?.[name] ?? null);
    });
    enabled.addEventListener('change', () => { enabled.dataset.nativeChanged = '1'; setTouched(); syncEnabled(); });
    panel.querySelectorAll('input,select').forEach((control) => markChartControl(control, setTouched));

    const addLabel = (label = {}, isNew = false) => {
      const card = document.createElement('details');
      card.className = 'native-chart-card native-data-label-card';
      card.open = isNew;
      card._nativeOriginal = copy(label || {});
      card._nativeNew = isNew;
      card.innerHTML = `<summary><span data-label-caption></span><button type="button" data-delete-label>删除</button></summary>
        <div class="native-grid native-chart-detail-grid">
          <label>数据点索引<input data-k="index" type="number" min="0" step="1"></label>
          <label class="native-wide">自定义文字<input data-k="text" placeholder="留空使用自动标签"></label>
          <label>位置<select data-k="position">${dataLabelPositionOptions}</select></label>
          <label>分隔符<input data-k="separator"></label>
          <label class="native-check"><input data-k="numFmtEnabled" type="checkbox">自定义数字格式</label>
          <label>格式代码<input data-k="numFmtCode"></label>
          <label>格式链接源<select data-k="numFmtSource">${triStateOptions}</select></label>
          <label>隐藏<select data-k="delete">${triStateOptions}</select></label>
          <label>图例项<select data-k="showLegendKey">${triStateOptions}</select></label>
          <label>值<select data-k="showValue">${triStateOptions}</select></label>
          <label>分类名<select data-k="showCategoryName">${triStateOptions}</select></label>
          <label>系列名<select data-k="showSeriesName">${triStateOptions}</select></label>
          <label>百分比<select data-k="showPercent">${triStateOptions}</select></label>
          <label>气泡大小<select data-k="showBubbleSize">${triStateOptions}</select></label>
          <label>引导线<select data-k="showLeaderLines">${triStateOptions}</select></label>
        </div>`;
      const item = (name) => card.querySelector(`[data-k=${name}]`);
      item('index').value = label.index ?? Math.max(0, ...[...labelList.children].map((entry) => Number(entry.querySelector('[data-k=index]')?.value) || 0)) + (labelList.children.length ? 1 : 0);
      item('text').value = label.text ?? '';
      preserveSelectValue(item('position'), label.position ?? '', '原标签位置（保留）');
      item('separator').value = label.separator ?? '';
      item('numFmtEnabled').checked = !!label.numberFormat;
      item('numFmtCode').value = label.numberFormat?.code ?? '';
      setNullableBoolean(item('numFmtSource'), label.numberFormat?.sourceLinked ?? null);
      ['delete', 'showLegendKey', 'showValue', 'showCategoryName', 'showSeriesName', 'showPercent',
        'showBubbleSize', 'showLeaderLines'].forEach((name) => setNullableBoolean(item(name), label[name] ?? null));
      const updateCaption = () => { card.querySelector('[data-label-caption]').textContent = `数据点 ${item('index').value || 0}`; };
      card.querySelectorAll('input,select').forEach((control) => markChartControl(control, () => {
        labelsTouched = true; setTouched(); updateCaption();
      }));
      card.querySelector('[data-delete-label]').onclick = (event) => {
        event.preventDefault(); event.stopPropagation(); card.remove(); labelsTouched = true; setTouched();
      };
      labelList.appendChild(card);
      updateCaption();
      if (isNew) { labelsTouched = true; setTouched(); }
    };
    (original?.labels || []).forEach((label) => addLabel(label));
    host.querySelector('[data-add-label]').onclick = () => addLabel({ index: labelList.children.length }, true);
    syncEnabled();

    const collectLabel = (card) => {
      const result = copy(card._nativeOriginal || {});
      const item = (name) => card.querySelector(`[data-k=${name}]`);
      const assign = (name, value) => {
        if (card._nativeNew || item(name).dataset.nativeChanged === '1') result[name] = value;
      };
      const index = readNullableNumber(item('index'), '数据标签索引');
      if (!Number.isInteger(index) || index < 0) throw new Error('数据标签索引必须是非负整数');
      assign('index', index);
      assign('text', item('text').value);
      assign('position', item('position').value || null);
      assign('separator', item('separator').value || null);
      ['delete', 'showLegendKey', 'showValue', 'showCategoryName', 'showSeriesName', 'showPercent',
        'showBubbleSize', 'showLeaderLines'].forEach((name) => assign(name, readNullableBoolean(item(name))));
      if (card._nativeNew || item('numFmtEnabled').dataset.nativeChanged === '1'
        || item('numFmtCode').dataset.nativeChanged === '1' || item('numFmtSource').dataset.nativeChanged === '1') {
        result.numberFormat = item('numFmtEnabled').checked
          ? { ...(result.numberFormat || {}), code: item('numFmtCode').value, sourceLinked: readNullableBoolean(item('numFmtSource')) ?? true }
          : null;
      }
      return result;
    };
    return {
      touched: () => touched,
      collect: () => {
        if (!enabled.checked) return null;
        const result = copy(original || {});
        const assign = (name, value) => {
          if (!original || field(name).dataset.nativeChanged === '1') result[name] = value;
        };
        assign('position', field('position').value || null);
        assign('separator', field('separator').value || null);
        ['delete', 'showLegendKey', 'showValue', 'showCategoryName', 'showSeriesName', 'showPercent',
          'showBubbleSize', 'showLeaderLines', 'showDataLabelsRange'].forEach((name) => assign(name, readNullableBoolean(field(name))));
        if (!original || field('numFmtEnabled').dataset.nativeChanged === '1'
          || field('numFmtCode').dataset.nativeChanged === '1' || field('numFmtSource').dataset.nativeChanged === '1') {
          result.numberFormat = field('numFmtEnabled').checked
            ? { ...(result.numberFormat || {}), code: field('numFmtCode').value, sourceLinked: readNullableBoolean(field('numFmtSource')) ?? true }
            : null;
        }
        if (labelsTouched || !original) result.labels = [...labelList.children].map(collectLabel);
        return result;
      },
    };
  }

  function buildTrendlinesEditor(host, originalValue, onChange) {
    const original = Array.isArray(originalValue) ? copy(originalValue) : [];
    let touched = false;
    host.innerHTML = '<details class="native-chart-subeditor"><summary>趋势线</summary><div class="native-chart-subeditor-body"><div class="native-chart-list-title"><span>Excel 原生趋势线</span><button type="button" data-add-trendline>＋ 趋势线</button></div><div class="native-chart-card-list" data-trendline-list></div></div></details>';
    const list = host.querySelector('[data-trendline-list]');
    const setTouched = () => { touched = true; onChange(); };
    const add = (trendline = {}, isNew = false) => {
      const card = document.createElement('details');
      card.className = 'native-chart-card native-trendline-card';
      card.open = isNew;
      card._nativeOriginal = copy(trendline || {});
      card._nativeNew = isNew;
      card.innerHTML = `<summary><span data-trendline-caption></span><button type="button" data-delete-trendline>删除</button></summary>
        <div class="native-grid native-chart-detail-grid">
          <label>名称<input data-k="name"></label>
          <label>类型<select data-k="type"><option value="linear">线性</option><option value="exp">指数</option><option value="log">对数</option><option value="movingAvg">移动平均</option><option value="poly">多项式</option><option value="power">幂</option></select></label>
          <label>多项式阶数<input data-k="order" type="number" min="2" max="255" step="1"></label>
          <label>移动平均周期<input data-k="period" type="number" min="2" max="255" step="1"></label>
          <label>向前预测<input data-k="forward" type="number" step="0.1"></label>
          <label>向后预测<input data-k="backward" type="number" step="0.1"></label>
          <label>截距<input data-k="intercept" type="number" step="any"></label>
          <label>显示 R²<select data-k="displayRSquared">${triStateOptions}</select></label>
          <label>显示公式<select data-k="displayEquation">${triStateOptions}</select></label>
          <label class="native-wide">趋势线标签<input data-k="label"></label>
        </div>`;
      const item = (name) => card.querySelector(`[data-k=${name}]`);
      item('name').value = trendline.name ?? '';
      preserveSelectValue(item('type'), trendline.type ?? 'linear', '原趋势线类型（保留）');
      ['order', 'period', 'forward', 'backward', 'intercept'].forEach((name) => { item(name).value = trendline[name] ?? ''; });
      setNullableBoolean(item('displayRSquared'), trendline.displayRSquared ?? null);
      setNullableBoolean(item('displayEquation'), trendline.displayEquation ?? null);
      item('label').value = trendline.label ?? '';
      const updateCaption = () => {
        const labels = { linear: '线性', exp: '指数', log: '对数', movingAvg: '移动平均', poly: '多项式', power: '幂' };
        card.querySelector('[data-trendline-caption]').textContent = item('name').value || labels[item('type').value] || item('type').value;
      };
      card.querySelectorAll('input,select').forEach((control) => markChartControl(control, () => { setTouched(); updateCaption(); }));
      card.querySelector('[data-delete-trendline]').onclick = (event) => {
        event.preventDefault(); event.stopPropagation(); card.remove(); setTouched();
      };
      list.appendChild(card);
      updateCaption();
      if (isNew) setTouched();
    };
    original.forEach((trendline) => add(trendline));
    host.querySelector('[data-add-trendline]').onclick = () => {
      const nextIndex = Math.max(-1, ...[...list.children].map((card) => Number(card._nativeOriginal?.index ?? -1))) + 1;
      add({ index: nextIndex, type: 'linear', name: '' }, true);
    };
    return {
      touched: () => touched,
      collect: () => [...list.children].map((card, position) => {
        const result = copy(card._nativeOriginal || {});
        const item = (name) => card.querySelector(`[data-k=${name}]`);
        const assign = (name, value) => {
          if (card._nativeNew || item(name).dataset.nativeChanged === '1') result[name] = value;
        };
        if (result.index == null) result.index = position;
        assign('name', item('name').value || null);
        assign('type', item('type').value);
        ['order', 'period', 'forward', 'backward', 'intercept'].forEach((name) => assign(name, readNullableNumber(item(name), `趋势线${name}`)));
        assign('displayRSquared', readNullableBoolean(item('displayRSquared')));
        assign('displayEquation', readNullableBoolean(item('displayEquation')));
        assign('label', item('label').value || null);
        return result;
      }),
    };
  }

  function buildErrorBarsEditor(host, originalValue, onChange) {
    const original = Array.isArray(originalValue) ? copy(originalValue) : [];
    let touched = false;
    host.innerHTML = '<details class="native-chart-subeditor"><summary>误差线</summary><div class="native-chart-subeditor-body"><div class="native-chart-list-title"><span>X/Y 方向、固定/百分比/标准差/自定义值</span><button type="button" data-add-errorbar>＋ 误差线</button></div><div class="native-chart-card-list" data-errorbar-list></div></div></details>';
    const list = host.querySelector('[data-errorbar-list]');
    const setTouched = () => { touched = true; onChange(); };
    const add = (errorBar = {}, isNew = false) => {
      const card = document.createElement('details');
      card.className = 'native-chart-card native-errorbar-card';
      card.open = isNew;
      card._nativeOriginal = copy(errorBar || {});
      card._nativeNew = isNew;
      card.innerHTML = `<summary><span data-errorbar-caption></span><button type="button" data-delete-errorbar>删除</button></summary>
        <div class="native-grid native-chart-detail-grid">
          <label>方向<select data-k="direction"><option value="x">X</option><option value="y">Y</option></select></label>
          <label>误差方向<select data-k="barType"><option value="both">正负</option><option value="plus">正</option><option value="minus">负</option></select></label>
          <label>误差量<select data-k="valueType"><option value="fixedVal">固定值</option><option value="percentage">百分比</option><option value="stdDev">标准差</option><option value="stdErr">标准误差</option><option value="cust">自定义</option></select></label>
          <label>数值<input data-k="value" type="number" step="any"></label>
          <label>无端帽<select data-k="noEndCap">${triStateOptions}</select></label>
        </div>
        <div class="native-error-amounts">
          <fieldset data-amount="plus"><legend><label class="native-check"><input data-k="enabled" type="checkbox">正误差自定义值</label></legend><div class="native-grid"><label>绑定<select data-k="bindingMode"><option value="embedded">内嵌</option><option value="reference">单元格引用</option><option value="unknown">保留原绑定</option></select></label><label class="native-wide">公式<input data-k="formula"></label><label class="native-wide">缓存值<textarea data-k="values" rows="2"></textarea></label></div></fieldset>
          <fieldset data-amount="minus"><legend><label class="native-check"><input data-k="enabled" type="checkbox">负误差自定义值</label></legend><div class="native-grid"><label>绑定<select data-k="bindingMode"><option value="embedded">内嵌</option><option value="reference">单元格引用</option><option value="unknown">保留原绑定</option></select></label><label class="native-wide">公式<input data-k="formula"></label><label class="native-wide">缓存值<textarea data-k="values" rows="2"></textarea></label></div></fieldset>
        </div>`;
      const item = (name) => card.querySelector(`[data-k=${name}]`);
      preserveSelectValue(item('direction'), errorBar.direction ?? 'y', '原方向（保留）');
      preserveSelectValue(item('barType'), errorBar.barType ?? 'both', '原误差方向（保留）');
      preserveSelectValue(item('valueType'), errorBar.valueType ?? 'fixedVal', '原误差量（保留）');
      item('value').value = errorBar.value ?? '';
      setNullableBoolean(item('noEndCap'), errorBar.noEndCap ?? null);
      const amountControllers = {};
      ['plus', 'minus'].forEach((name) => {
        const fieldset = card.querySelector(`[data-amount=${name}]`);
        const amount = errorBar[name] && typeof errorBar[name] === 'object' ? errorBar[name] : null;
        const amountItem = (key) => fieldset.querySelector(`[data-k=${key}]`);
        amountItem('enabled').checked = !!amount;
        preserveSelectValue(amountItem('bindingMode'), amount?.bindingMode ?? 'embedded', '原绑定（保留）');
        amountItem('formula').value = amount?.formula ?? '';
        amountItem('values').value = (amount?.values || []).map((value) => value == null ? '' : value).join('\n');
        const sync = () => fieldset.querySelectorAll('input:not([data-k=enabled]),select,textarea').forEach((control) => { control.disabled = !amountItem('enabled').checked; });
        fieldset.querySelectorAll('input,select,textarea').forEach((control) => markChartControl(control, () => { setTouched(); sync(); }));
        sync();
        amountControllers[name] = { fieldset, amount, item: amountItem };
      });
      const updateCaption = () => { card.querySelector('[data-errorbar-caption]').textContent = `${item('direction').value.toUpperCase()} 方向误差线`; };
      card.querySelectorAll(':scope > .native-grid input, :scope > .native-grid select').forEach((control) => markChartControl(control, () => { setTouched(); updateCaption(); }));
      card.querySelector('[data-delete-errorbar]').onclick = (event) => {
        event.preventDefault(); event.stopPropagation(); card.remove(); setTouched();
      };
      card._nativeAmounts = amountControllers;
      list.appendChild(card);
      updateCaption();
      if (isNew) setTouched();
    };
    original.forEach((errorBar) => add(errorBar));
    host.querySelector('[data-add-errorbar]').onclick = () => {
      const nextIndex = Math.max(-1, ...[...list.children].map((card) => Number(card._nativeOriginal?.index ?? -1))) + 1;
      add({ index: nextIndex, direction: 'y', barType: 'both', valueType: 'fixedVal', value: 1, noEndCap: false }, true);
    };
    return {
      touched: () => touched,
      collect: () => [...list.children].map((card, position) => {
        const result = copy(card._nativeOriginal || {});
        const item = (name) => card.querySelector(`[data-k=${name}]`);
        const assign = (name, value) => {
          if (card._nativeNew || item(name).dataset.nativeChanged === '1') result[name] = value;
        };
        if (result.index == null) result.index = position;
        assign('direction', item('direction').value);
        assign('barType', item('barType').value);
        assign('valueType', item('valueType').value);
        assign('value', readNullableNumber(item('value'), '误差线数值'));
        assign('noEndCap', readNullableBoolean(item('noEndCap')));
        ['plus', 'minus'].forEach((name) => {
          const controller = card._nativeAmounts[name];
          const amountItem = controller.item;
          const amountTouched = card._nativeNew || [...controller.fieldset.querySelectorAll('input,select,textarea')]
            .some((control) => control.dataset.nativeChanged === '1');
          if (!amountTouched) return;
          if (!amountItem('enabled').checked) { result[name] = null; return; }
          const amount = copy(controller.amount || {});
          amount.bindingMode = amountItem('bindingMode').value;
          amount.formula = amountItem('formula').value.trim();
          amount.values = parseNumberSlots(amountItem('values').value);
          if (amount.bindingMode === 'reference' && !amount.formula) throw new Error(`${name === 'plus' ? '正' : '负'}误差引用必须填写公式`);
          result[name] = amount;
        });
        return result;
      }),
    };
  }

  function seriesRow(series, tbody, onChange, plotProvider, theme, isNew = false) {
    const original = copy(series || {});
    const tr = document.createElement('tr');
    tr._nativeOriginal = original;
    tr._nativeNew = isNew;
    tr.dataset.seriesRow = '1';
    tr.innerHTML = '<td><input data-k="name"></td><td><select data-k="bindingMode"><option value="reference">引用单元格</option><option value="embedded">内嵌数据</option></select></td><td><textarea data-k="categories" rows="2"></textarea></td><td><textarea data-k="values" rows="2"></textarea></td><td><input data-k="categoryFormula"></td><td><input data-k="valueFormula"></td><td><textarea data-k="pointOverrides" rows="2" placeholder="0 | #FF0000 | 20 | circle | 7" title="每行：点索引 | 颜色 | 分离比例 | 标记类型 | 标记大小"></textarea></td><td><input data-k="color" type="color"></td><td><input data-k="plotIndex" type="number" min="0" step="1"></td><td><select data-k="axisGroup"><option value="primary">主坐标轴</option><option value="secondary">次坐标轴</option></select></td><td><button type="button" data-a="advanced">高级…</button></td><td class="native-row-actions"><button data-a="up" title="上移">↑</button><button data-a="down" title="下移">↓</button><button data-a="delete" title="删除">×</button></td>';
    tr.querySelector('[data-k=name]').value = series.name || '';
    tr.querySelector('[data-k=categories]').value = (series.categories || []).join('\n');
    tr.querySelector('[data-k=values]').value = (series.values || []).map((v) => v == null ? '' : v).join('\n');
    tr.querySelector('[data-k=categoryFormula]').value = series.categoryFormula || '';
    tr.querySelector('[data-k=valueFormula]').value = series.valueFormula || '';
    tr.querySelector('[data-k=pointOverrides]').value = formatPointOverrides(series.pointOverrides);
    tr.querySelector('[data-k=plotIndex]').value = series.plotIndex ?? 0;
    preserveSelectValue(tr.querySelector('[data-k=axisGroup]'), series.axisGroup ?? 'primary', '原坐标轴组（保留）');
    const binding = tr.querySelector('[data-k=bindingMode]');
    const inferredBinding = series.bindingMode
      || ((series.categoryFormula || series.valueFormula) ? 'reference' : 'embedded');
    preserveSelectValue(binding, inferredBinding, inferredBinding === 'mixed' ? '混合绑定（分别保持）' : '未知绑定（保留）');
    setColorInput(tr.querySelector('[data-k=color]'), series.color, '#808080', series.colorSpec, theme);
    tr.querySelectorAll('input,textarea').forEach((input) => {
      input.addEventListener('input', () => { input.dataset.nativeChanged = '1'; onChange(); });
    });
    const syncBinding = () => {
      const mode = binding.value;
      const categories = tr.querySelector('[data-k=categories]');
      const values = tr.querySelector('[data-k=values]');
      const categoryFormula = tr.querySelector('[data-k=categoryFormula]');
      const valueFormula = tr.querySelector('[data-k=valueFormula]');
      categories.disabled = mode === 'reference'
        || (mode !== 'embedded' && !!categoryFormula.value.trim());
      values.disabled = mode === 'reference'
        || (mode !== 'embedded' && !!valueFormula.value.trim());
      categoryFormula.disabled = mode === 'embedded';
      valueFormula.disabled = mode === 'embedded';
      categories.title = categories.disabled ? '引用数据由源单元格决定；请清空/修改公式或切换为内嵌数据。' : '';
      values.title = values.disabled ? '引用数据由源单元格决定；请清空/修改公式或切换为内嵌数据。' : '';
    };
    tr.querySelector('[data-k=categoryFormula]').addEventListener('input', syncBinding);
    tr.querySelector('[data-k=valueFormula]').addEventListener('input', syncBinding);
    binding.addEventListener('change', () => {
      binding.dataset.nativeChanged = '1';
      if (binding.value === 'embedded') {
        ['categoryFormula', 'valueFormula'].forEach((name) => {
          const field = tr.querySelector(`[data-k=${name}]`);
          field.value = '';
          field.dataset.nativeChanged = '1';
        });
      }
      syncBinding();
      onChange();
    });
    tr.querySelector('[data-k=axisGroup]').addEventListener('change', () => {
      tr.querySelector('[data-k=axisGroup]').dataset.nativeChanged = '1';
      onChange();
    });
    syncBinding();
    const advanced = document.createElement('tr');
    advanced.className = 'native-series-advanced-row';
    advanced.dataset.seriesAdvanced = '1';
    advanced.hidden = true;
    advanced.innerHTML = `<td colspan="12"><div class="native-series-advanced-panel">
      <div class="native-section-title">系列绘图区与坐标轴</div>
      <div class="native-grid">
        <label>绘图类型<select data-series-advanced="plotType">${chartTypeOptions}</select></label>
        <label class="native-wide">坐标轴 ID（逗号分隔）<input data-series-advanced="axisIds" placeholder="10, 20"></label>
      </div>
      <div data-series-labels></div><div data-series-trendlines></div><div data-series-errorbars></div>
    </div></td>`;
    const advancedField = (name) => advanced.querySelector(`[data-series-advanced=${name}]`);
    preserveSelectValue(advancedField('plotType'), series.plotType ?? 'bar', '原绘图类型（保留）');
    advancedField('axisIds').value = (series.axisIds || []).join(', ');
    advanced.querySelectorAll('input,select').forEach((control) => markChartControl(control, onChange));
    const dataLabelsEditor = buildDataLabelsEditor(advanced.querySelector('[data-series-labels]'), series.dataLabels, onChange, '系列数据标签');
    const trendlinesEditor = buildTrendlinesEditor(advanced.querySelector('[data-series-trendlines]'), series.trendlines, onChange);
    const errorBarsEditor = buildErrorBarsEditor(advanced.querySelector('[data-series-errorbars]'), series.errorBars, onChange);
    tr._nativeAdvancedRow = advanced;
    tr._nativeCollect = () => {
      const next = copy(tr._nativeOriginal || {});
      const field = (name) => tr.querySelector(`[data-k=${name}]`);
      if (tr._nativeNew || field('name').dataset.nativeChanged === '1') next.name = field('name').value;
      if (tr._nativeNew || field('bindingMode').dataset.nativeChanged === '1') next.bindingMode = field('bindingMode').value;
      if (tr._nativeNew || field('categories').dataset.nativeChanged === '1') next.categories = parseSlotList(field('categories').value);
      if (tr._nativeNew || field('values').dataset.nativeChanged === '1') next.values = parseNumberSlots(field('values').value);
      if (tr._nativeNew || field('categoryFormula').dataset.nativeChanged === '1') next.categoryFormula = field('categoryFormula').value.trim();
      if (tr._nativeNew || field('valueFormula').dataset.nativeChanged === '1') next.valueFormula = field('valueFormula').value.trim();
      if (tr._nativeNew || field('pointOverrides').dataset.nativeChanged === '1') next.pointOverrides = parsePointOverrides(field('pointOverrides').value);
      if (tr._nativeNew || field('color').dataset.nativeChanged === '1') {
        next.color = getColorInput(field('color'));
        delete next.colorSpec;
      }
      if (next.bindingMode === 'reference') {
        if (!next.categoryFormula || !next.valueFormula) throw new Error(`系列“${next.name || ''}”使用引用模式时必须填写分类公式和数值公式`);
        if (field('categories').dataset.nativeChanged === '1' || field('values').dataset.nativeChanged === '1') {
          throw new Error(`系列“${next.name || ''}”的引用数据由源单元格决定；请修改公式/源单元格，或切换为内嵌数据`);
        }
      } else if (next.bindingMode === 'embedded') {
        next.categoryFormula = '';
        next.valueFormula = '';
      } else {
        if (field('categories').dataset.nativeChanged === '1' && field('categoryFormula').dataset.nativeChanged !== '1') next.categoryFormula = '';
        if (field('values').dataset.nativeChanged === '1' && field('valueFormula').dataset.nativeChanged !== '1') next.valueFormula = '';
        next.bindingMode = next.categoryFormula && next.valueFormula ? 'reference'
          : (!next.categoryFormula && !next.valueFormula ? 'embedded' : 'mixed');
      }
      const assignmentChanged = tr._nativeNew || field('plotIndex').dataset.nativeChanged === '1'
        || field('axisGroup').dataset.nativeChanged === '1' || advancedField('plotType').dataset.nativeChanged === '1'
        || advancedField('axisIds').dataset.nativeChanged === '1';
      if (assignmentChanged) {
        const plotIndex = readNullableNumber(field('plotIndex'), '绘图区索引');
        if (!Number.isInteger(plotIndex) || plotIndex < 0) throw new Error('绘图区索引必须是非负整数');
        const requestedIds = parseSlotList(advancedField('axisIds').value).filter(Boolean).map((value) => {
          const id = Number(value);
          if (!Number.isInteger(id) || id < 0) throw new Error(`坐标轴 ID“${value}”不是非负整数`);
          return id;
        });
        const plots = (plotProvider?.() || []).filter((plot) => !plot.$delete);
        let target = plots.find((plot) => Number(plot.index) === plotIndex);
        if (field('axisGroup').dataset.nativeChanged === '1' || advancedField('plotType').dataset.nativeChanged === '1'
          || advancedField('axisIds').dataset.nativeChanged === '1') {
          target = plots.find((plot) => (field('axisGroup').dataset.nativeChanged !== '1' || plot.axisGroup === field('axisGroup').value)
            && (advancedField('plotType').dataset.nativeChanged !== '1' || plot.chartType === advancedField('plotType').value)
            && (advancedField('axisIds').dataset.nativeChanged !== '1' || equal(plot.axisIds || [], requestedIds)));
        }
        if (!target) throw new Error(`系列“${next.name || ''}”没有匹配的绘图区；请先在“绘图区”中建立相应主/次坐标轴绘图区`);
        next.plotIndex = Number(target.index);
        next.plotType = target.chartType;
        next.axisIds = copy(target.axisIds || []);
        next.axisGroup = target.axisGroup || field('axisGroup').value;
      }
      if (dataLabelsEditor.touched()) next.dataLabels = dataLabelsEditor.collect();
      if (trendlinesEditor.touched()) next.trendlines = trendlinesEditor.collect();
      if (errorBarsEditor.touched()) next.errorBars = errorBarsEditor.collect();
      return next;
    };
    tr.addEventListener('click', (event) => {
      const action = event.target.closest('button')?.dataset.a;
      if (!action) return;
      if (action === 'advanced') { advanced.hidden = !advanced.hidden; return; }
      if (action === 'delete') { advanced.remove(); tr.remove(); }
      if (action === 'up') {
        const previous = tr.previousElementSibling?.dataset.seriesAdvanced === '1'
          ? tr.previousElementSibling.previousElementSibling : tr.previousElementSibling;
        if (previous?.dataset.seriesRow === '1') tbody.insertBefore(advanced, previous), tbody.insertBefore(tr, advanced);
      }
      if (action === 'down') {
        const next = advanced.nextElementSibling;
        if (next?.dataset.seriesRow === '1') {
          const after = next._nativeAdvancedRow?.nextElementSibling || null;
          tbody.insertBefore(tr, after); tbody.insertBefore(advanced, after);
        }
      }
      onChange();
    });
    tbody.append(tr, advanced);
    return tr;
  }

  function plotAxisGroup(plot, axes) {
    const ids = new Set((plot.axisIds || []).map(Number));
    return (axes || []).some((axis) => ids.has(Number(axis.id)) && ['top', 'right'].includes(axis.position))
      ? 'secondary' : 'primary';
  }

  function buildPlotCard(host, plot, onChange, axesProvider, onDelete, isNew = false) {
    const original = copy(plot || {});
    const card = document.createElement('details');
    card.className = 'native-chart-card native-plot-card';
    card.open = isNew;
    card._nativeOriginal = original;
    card._nativeNew = isNew;
    card.innerHTML = `<summary><span data-plot-caption></span><span class="native-chart-summary-actions"><em data-plot-axis-group></em><button type="button" data-delete-plot>删除绘图区</button></span></summary>
      <div class="native-grid native-chart-detail-grid">
        <label>绘图区索引<input data-k="index" type="number" readonly></label>
        <label>图表类型<select data-k="chartType">${chartTypeOptions}</select></label>
        <label class="native-wide">坐标轴 ID（逗号分隔）<input data-k="axisIds"></label>
        <label>分组方式<input data-k="grouping" placeholder="clustered / stacked / standard"></label>
        <label>条形方向<select data-k="barDirection"><option value="">未设置</option><option value="col">柱形</option><option value="bar">条形</option></select></label>
        <label>间隙宽度<input data-k="gapWidth" type="number" step="1"></label>
        <label>系列重叠<input data-k="overlap" type="number" step="1"></label>
        <label>平滑线<select data-k="smooth">${triStateOptions}</select></label>
        <label>逐点变色<select data-k="varyColors">${triStateOptions}</select></label>
      </div><div data-plot-labels></div>`;
    const field = (name) => card.querySelector(`[data-k=${name}]`);
    field('index').value = plot.index ?? 0;
    preserveSelectValue(field('chartType'), plot.chartType ?? 'bar', '原绘图类型（保留）');
    field('axisIds').value = (plot.axisIds || []).join(', ');
    field('grouping').value = plot.grouping ?? '';
    preserveSelectValue(field('barDirection'), plot.barDirection ?? '', '原条形方向（保留）');
    field('gapWidth').value = plot.gapWidth ?? '';
    field('overlap').value = plot.overlap ?? '';
    setNullableBoolean(field('smooth'), plot.smooth ?? null);
    setNullableBoolean(field('varyColors'), plot.varyColors ?? null);
    const setTouched = () => { onChange(); refreshCaption(); };
    card.querySelectorAll('input:not([readonly]),select').forEach((control) => markChartControl(control, setTouched));
    const labels = buildDataLabelsEditor(card.querySelector('[data-plot-labels]'), plot.dataLabels, onChange, '绘图区数据标签');
    const compatibleFamily = chartFamily(plot.chartType || 'bar');
    [...field('chartType').options].forEach((option) => { option.disabled = chartFamily(option.value) !== compatibleFamily; });
    const parseIds = () => parseSlotList(field('axisIds').value).filter(Boolean).map((value) => {
      const id = Number(value);
      if (!Number.isInteger(id) || id < 0) throw new Error(`绘图区坐标轴 ID“${value}”不是非负整数`);
      return id;
    });
    function refreshCaption() {
      const index = field('index').value || 0;
      card.querySelector('[data-plot-caption]').textContent = `绘图区 ${index + 1} · ${field('chartType').selectedOptions[0]?.textContent || field('chartType').value}`;
      let group = original.axisGroup || 'primary';
      try { group = plotAxisGroup({ axisIds: parseIds() }, axesProvider?.() || []); } catch (_) { /* keep prior label while typing */ }
      card.querySelector('[data-plot-axis-group]').textContent = group === 'secondary' ? '次坐标轴' : '主坐标轴';
    }
    card.querySelector('[data-delete-plot]').onclick = (event) => {
      event.preventDefault(); event.stopPropagation(); onDelete(card); onChange();
    };
    card._nativeCollect = () => {
      const result = copy(original);
      const assign = (name, value) => {
        if (isNew || field(name).dataset.nativeChanged === '1') result[name] = value;
      };
      result.index = Number(field('index').value);
      assign('chartType', field('chartType').value);
      assign('axisIds', parseIds());
      assign('grouping', field('grouping').value || null);
      assign('barDirection', field('barDirection').value || null);
      assign('gapWidth', readNullableNumber(field('gapWidth'), '间隙宽度'));
      assign('overlap', readNullableNumber(field('overlap'), '系列重叠'));
      assign('smooth', readNullableBoolean(field('smooth')));
      assign('varyColors', readNullableBoolean(field('varyColors')));
      if (labels.touched()) result.dataLabels = labels.collect();
      result.axisGroup = plotAxisGroup(result, axesProvider?.() || []);
      return result;
    };
    host.appendChild(card);
    refreshCaption();
    return card;
  }

  function buildAxisCard(host, axis, onChange, onDelete, isNew = false) {
    const original = copy(axis || {});
    const card = document.createElement('details');
    card.className = 'native-chart-card native-axis-card';
    card.open = isNew;
    card._nativeOriginal = original;
    card._nativeNew = isNew;
    card.innerHTML = `<summary><span data-axis-caption></span><button type="button" data-delete-axis>删除轴</button></summary>
      <div class="native-grid native-chart-detail-grid">
        <label>轴 ID<input data-k="id" type="number" readonly></label>
        <label>轴类型<select data-k="axisType"><option value="category">分类轴</option><option value="value">数值轴</option><option value="date">日期轴</option><option value="series">系列轴</option></select></label>
        <label>位置<select data-k="position"><option value="bottom">底部</option><option value="top">顶部</option><option value="left">左侧</option><option value="right">右侧</option></select></label>
        <label>标题<input data-k="title"></label>
        <label>隐藏轴<select data-k="delete">${triStateOptions}</select></label>
        <label>方向<select data-k="orientation"><option value="">未设置</option><option value="minMax">正向</option><option value="maxMin">反向</option></select></label>
        <label>最小值<input data-k="min" type="number" step="any"></label>
        <label>最大值<input data-k="max" type="number" step="any"></label>
        <label>对数底<input data-k="logBase" type="number" min="2" max="1000" step="any"></label>
        <label>主要单位<input data-k="majorUnit" type="number" step="any"></label>
        <label>次要单位<input data-k="minorUnit" type="number" step="any"></label>
        <label class="native-check"><input data-k="majorGridlines" type="checkbox">主要网格线</label>
        <label class="native-check"><input data-k="minorGridlines" type="checkbox">次要网格线</label>
        <label>主要刻度<select data-k="majorTickMark"><option value="">未设置</option><option value="none">无</option><option value="in">内部</option><option value="out">外部</option><option value="cross">交叉</option></select></label>
        <label>次要刻度<select data-k="minorTickMark"><option value="">未设置</option><option value="none">无</option><option value="in">内部</option><option value="out">外部</option><option value="cross">交叉</option></select></label>
        <label>标签位置<select data-k="tickLabelPosition"><option value="">未设置</option><option value="nextTo">轴旁</option><option value="high">高位</option><option value="low">低位</option><option value="none">无</option></select></label>
        <label>交叉轴 ID<input data-k="crossAxisId" type="number" min="0" step="1"></label>
        <label>交叉方式<select data-k="crosses"><option value="">未设置</option><option value="autoZero">自动/零</option><option value="min">最小值</option><option value="max">最大值</option></select></label>
        <label>交叉于数值<input data-k="crossesAt" type="number" step="any"></label>
        <label>交叉位置<select data-k="crossBetween"><option value="">未设置</option><option value="between">刻度之间</option><option value="midCat">刻度线上</option></select></label>
        <label>自动分类轴<select data-k="auto">${triStateOptions}</select></label>
        <label>标签对齐<select data-k="labelAlignment"><option value="">未设置</option><option value="ctr">居中</option><option value="l">左</option><option value="r">右</option></select></label>
        <label>标签偏移<input data-k="labelOffset" type="number" step="any"></label>
        <label>标签跳过<input data-k="tickLabelSkip" type="number" min="1" step="1"></label>
        <label>刻度跳过<input data-k="tickMarkSkip" type="number" min="1" step="1"></label>
        <label>禁用多级标签<select data-k="noMultiLevelLabels">${triStateOptions}</select></label>
        <label>基础时间单位<select data-k="baseTimeUnit"><option value="">未设置</option><option value="days">天</option><option value="months">月</option><option value="years">年</option></select></label>
        <label>主要时间单位<select data-k="majorTimeUnit"><option value="">未设置</option><option value="days">天</option><option value="months">月</option><option value="years">年</option></select></label>
        <label>次要时间单位<select data-k="minorTimeUnit"><option value="">未设置</option><option value="days">天</option><option value="months">月</option><option value="years">年</option></select></label>
      </div>
      <div class="native-section-title">数字格式</div><div class="native-grid">
        <label class="native-check"><input data-k="numFmtEnabled" type="checkbox">自定义轴数字格式</label><label>格式代码<input data-k="numFmtCode"></label><label>链接源<select data-k="numFmtSource">${triStateOptions}</select></label>
      </div>
      <div class="native-section-title">显示单位</div><div class="native-grid">
        <label class="native-check"><input data-k="displayUnitsEnabled" type="checkbox">启用显示单位</label>
        <label>内置单位<select data-k="displayBuiltIn"><option value="">自定义/无</option><option value="hundreds">百</option><option value="thousands">千</option><option value="tenThousands">万</option><option value="hundredThousands">十万</option><option value="millions">百万</option><option value="tenMillions">千万</option><option value="hundredMillions">亿</option><option value="billions">十亿</option><option value="trillions">万亿</option></select></label>
        <label>自定义单位<input data-k="displayCustom" type="number" step="any"></label>
        <label class="native-check"><input data-k="displayShowLabel" type="checkbox">显示单位标签</label>
        <label class="native-wide">单位标签文字<input data-k="displayLabel"></label>
      </div>`;
    const field = (name) => card.querySelector(`[data-k=${name}]`);
    field('id').value = axis.id ?? 0;
    preserveSelectValue(field('axisType'), axis.axisType ?? 'value', '原轴类型（保留）');
    preserveSelectValue(field('position'), axis.position ?? (axis.axisType === 'value' ? 'left' : 'bottom'), '原轴位置（保留）');
    field('title').value = axis.title ?? '';
    setNullableBoolean(field('delete'), axis.delete ?? null);
    preserveSelectValue(field('orientation'), axis.scaling?.orientation ?? '', '原方向（保留）');
    ['min', 'max', 'logBase'].forEach((name) => { field(name).value = axis.scaling?.[name] ?? ''; });
    ['majorUnit', 'minorUnit', 'crossAxisId', 'crossesAt', 'labelOffset', 'tickLabelSkip', 'tickMarkSkip'].forEach((name) => { field(name).value = axis[name] ?? ''; });
    field('majorGridlines').checked = axis.majorGridlines === true;
    field('minorGridlines').checked = axis.minorGridlines === true;
    ['majorTickMark', 'minorTickMark', 'tickLabelPosition', 'crosses', 'crossBetween', 'labelAlignment',
      'baseTimeUnit', 'majorTimeUnit', 'minorTimeUnit'].forEach((name) => preserveSelectValue(field(name), axis[name] ?? '', `原 ${name} 值（保留）`));
    setNullableBoolean(field('auto'), axis.auto ?? null);
    setNullableBoolean(field('noMultiLevelLabels'), axis.noMultiLevelLabels ?? null);
    field('numFmtEnabled').checked = !!axis.numberFormat;
    field('numFmtCode').value = axis.numberFormat?.code ?? '';
    setNullableBoolean(field('numFmtSource'), axis.numberFormat?.sourceLinked ?? null);
    field('displayUnitsEnabled').checked = !!axis.displayUnits;
    preserveSelectValue(field('displayBuiltIn'), axis.displayUnits?.builtIn ?? '', '原显示单位（保留）');
    field('displayCustom').value = axis.displayUnits?.custom ?? '';
    field('displayShowLabel').checked = axis.displayUnits?.showLabel === true;
    field('displayLabel').value = axis.displayUnits?.label ?? '';
    const refreshCaption = () => {
      const names = { category: '分类轴', value: '数值轴', date: '日期轴', series: '系列轴' };
      const group = ['top', 'right'].includes(field('position').value) ? '次' : '主';
      card.querySelector('[data-axis-caption]').textContent = `${group}${names[field('axisType').value] || '坐标轴'} · ID ${field('id').value}`;
    };
    card.querySelectorAll('input:not([readonly]),select').forEach((control) => markChartControl(control, () => { onChange(); refreshCaption(); }));
    card.querySelector('[data-delete-axis]').onclick = (event) => {
      event.preventDefault(); event.stopPropagation(); onDelete(card); onChange();
    };
    card._nativeCollect = () => {
      const result = copy(original);
      const assign = (name, value) => { if (isNew || field(name).dataset.nativeChanged === '1') result[name] = value; };
      result.id = Number(field('id').value);
      assign('axisType', field('axisType').value);
      assign('position', field('position').value);
      assign('title', field('title').value);
      assign('delete', readNullableBoolean(field('delete')));
      const scalingChanged = isNew || ['orientation', 'min', 'max', 'logBase'].some((name) => field(name).dataset.nativeChanged === '1');
      if (scalingChanged) {
        result.scaling = copy(result.scaling || {});
        if (isNew || field('orientation').dataset.nativeChanged === '1') result.scaling.orientation = field('orientation').value || null;
        ['min', 'max', 'logBase'].forEach((name) => {
          if (isNew || field(name).dataset.nativeChanged === '1') result.scaling[name] = readNullableNumber(field(name), `坐标轴${name}`);
        });
      }
      assign('majorGridlines', field('majorGridlines').checked);
      assign('minorGridlines', field('minorGridlines').checked);
      ['majorTickMark', 'minorTickMark', 'tickLabelPosition', 'crosses', 'crossBetween', 'labelAlignment',
        'baseTimeUnit', 'majorTimeUnit', 'minorTimeUnit'].forEach((name) => assign(name, field(name).value || null));
      assign('auto', readNullableBoolean(field('auto')));
      assign('noMultiLevelLabels', readNullableBoolean(field('noMultiLevelLabels')));
      ['majorUnit', 'minorUnit', 'crossesAt', 'labelOffset', 'tickLabelSkip', 'tickMarkSkip'].forEach((name) => assign(name, readNullableNumber(field(name), `坐标轴${name}`)));
      if (isNew || field('crossAxisId').dataset.nativeChanged === '1') {
        const crossId = readNullableNumber(field('crossAxisId'), '交叉轴 ID');
        if (crossId != null && (!Number.isInteger(crossId) || crossId < 0)) throw new Error('交叉轴 ID 必须是非负整数');
        result.crossAxisId = crossId;
      }
      if (isNew || ['numFmtEnabled', 'numFmtCode', 'numFmtSource'].some((name) => field(name).dataset.nativeChanged === '1')) {
        result.numberFormat = field('numFmtEnabled').checked
          ? { ...(result.numberFormat || {}), code: field('numFmtCode').value, sourceLinked: readNullableBoolean(field('numFmtSource')) ?? true }
          : null;
      }
      if (isNew || ['displayUnitsEnabled', 'displayBuiltIn', 'displayCustom', 'displayShowLabel', 'displayLabel']
        .some((name) => field(name).dataset.nativeChanged === '1')) {
        result.displayUnits = field('displayUnitsEnabled').checked ? {
          ...(result.displayUnits || {}),
          builtIn: field('displayBuiltIn').value || null,
          custom: readNullableNumber(field('displayCustom'), '自定义显示单位'),
          showLabel: field('displayShowLabel').checked,
          label: field('displayLabel').value || null,
        } : null;
        if (result.displayUnits?.builtIn) result.displayUnits.custom = null;
      }
      return result;
    };
    host.appendChild(card);
    refreshCaption();
    return card;
  }

  function buildChart(body, model, baseModel, theme) {
    body.innerHTML = `<div class="native-grid native-chart-basics"><label>标题<input data-f="title"></label><label>图表类型<select data-f="chartType">${chartTypeOptions}<option value="combo">组合图</option></select></label><label class="native-check"><input data-f="legendShow" type="checkbox">显示图例</label><label>图例位置<select data-f="legendPosition"><option value="right">右侧</option><option value="left">左侧</option><option value="top">顶部</option><option value="bottom">底部</option><option value="topRight">右上</option></select></label></div>
      <div class="native-section-title native-chart-section-head"><span>绘图区</span><span><button type="button" data-add-plot>＋ 绘图区</button><button type="button" data-add-secondary>＋ 次坐标轴组合图</button></span></div><div class="native-chart-card-list" data-plots></div>
      <div class="native-section-title native-chart-section-head"><span>坐标轴</span><small>主轴通常在底部/左侧，次轴在顶部/右侧；ID 与交叉轴必须成对有效。</small></div><div class="native-chart-card-list native-axis-list" data-axes></div>
      <div class="native-section-title">系列与数据绑定</div><div class="native-table-wrap"><table class="native-table native-chart-series-table"><thead><tr><th>系列名</th><th>绑定模式</th><th>分类数据</th><th>数值数据</th><th>分类公式</th><th>数值公式</th><th title="每行：点索引 | 颜色 | 分离比例 | 标记类型 | 标记大小">单点覆盖</th><th>颜色</th><th>绘图区</th><th>坐标轴</th><th>深层编辑</th><th></th></tr></thead><tbody data-series></tbody></table></div><button class="native-add" data-add-series>＋ 添加系列</button>`;
    const draft = copy(model);
    const changed = new Set();
    const title = body.querySelector('[data-f=title]');
    const type = body.querySelector('[data-f=chartType]');
    const legendShow = body.querySelector('[data-f=legendShow]');
    const legendPosition = body.querySelector('[data-f=legendPosition]');
    title.value = model.title ?? '';
    preserveSelectValue(type, model.chartType ?? 'bar');
    legendShow.checked = !model.legend || model.legend.show !== false;
    preserveSelectValue(legendPosition, model.legend?.position ?? 'right');
    const baseType = baseModel.chartType || model.chartType || 'bar';
    const baseFamily = chartFamily(baseType);
    [...type.options].forEach((option) => {
      option.disabled = chartFamily(option.value) !== baseFamily
        || (['combo', 'stock'].includes(baseType) && option.value !== baseType);
    });
    title.addEventListener('input', () => changed.add('title'));
    type.addEventListener('change', () => changed.add('chartType'));
    legendShow.addEventListener('change', () => changed.add('legendShow'));
    legendPosition.addEventListener('change', () => changed.add('legendPosition'));

    const axesHost = body.querySelector('[data-axes]');
    const plotHost = body.querySelector('[data-plots]');
    const deletedAxes = (model.axes || []).filter((axis) => axis?.$delete).map(copy);
    const deletedPlots = (model.plots || []).filter((plot) => plot?.$delete).map(copy);
    const axisCards = [];
    const plotCards = [];
    const activeAxes = () => axisCards.filter((card) => card.isConnected).map((card) => card._nativeCollect());
    const activePlots = () => plotCards.filter((card) => card.isConnected).map((card) => card._nativeCollect());
    const touchAxes = () => changed.add('axes');
    const touchPlots = () => changed.add('plots');
    const deleteAxis = (card) => {
      const id = Number(card.querySelector('[data-k=id]').value);
      if (activePlots().some((plot) => (plot.axisIds || []).map(Number).includes(id))) {
        throw new Error(`坐标轴 ${id} 仍被绘图区引用，请先修改绘图区的坐标轴 ID`);
      }
      deletedAxes.push({ id, $delete: true });
      card.remove(); touchAxes();
    };
    const addAxisCard = (axis, isNew = false) => {
      const card = buildAxisCard(axesHost, axis, touchAxes, (target) => {
        try { deleteAxis(target); } catch (error) { alert(error?.message || error); }
      }, isNew);
      axisCards.push(card);
      return card;
    };
    (model.axes || []).filter((axis) => !axis?.$delete).forEach((axis) => addAxisCard(axis));
    const deletePlot = (card) => {
      const index = Number(card.querySelector('[data-k=index]').value);
      const max = Math.max(...plotCards.filter((candidate) => candidate.isConnected).map((candidate) => Number(candidate.querySelector('[data-k=index]').value)));
      if (index !== max) throw new Error('为保持 OOXML 绘图区索引稳定，只能从最后一个绘图区开始删除');
      const assigned = [...body.querySelectorAll('[data-series-row="1"]')]
        .some((row) => Number(row.querySelector('[data-k=plotIndex]').value) === index);
      if (assigned) throw new Error(`绘图区 ${index} 仍有系列，请先把这些系列切换到其他绘图区`);
      deletedPlots.push({ index, $delete: true });
      card.remove(); touchPlots();
    };
    const addPlotCard = (plot, isNew = false) => {
      const card = buildPlotCard(plotHost, plot, touchPlots, activeAxes, (target) => {
        try { deletePlot(target); } catch (error) { alert(error?.message || error); }
      }, isNew);
      plotCards.push(card);
      return card;
    };
    (model.plots || []).filter((plot) => !plot?.$delete).forEach((plot) => addPlotCard(plot));
    body.querySelector('[data-add-plot]').onclick = () => {
      const plots = activePlots();
      if (!plots.length) return alert('当前图表没有可克隆的绘图区');
      const source = plots[0];
      if (!['category', 'xy'].includes(chartFamily(source.chartType))) return alert('当前图表类型不支持安全创建组合绘图区');
      const index = Math.max(-1, ...plots.map((plot) => Number(plot.index))) + 1;
      const nextType = chartFamily(source.chartType) === 'xy' ? (source.chartType === 'scatter' ? 'bubble' : 'scatter') : (source.chartType === 'line' ? 'bar' : 'line');
      addPlotCard({ index, cloneFrom: Number(source.index), chartType: nextType, axisIds: copy(source.axisIds || []), axisGroup: source.axisGroup || 'primary', dataLabels: null }, true);
      touchPlots();
    };
    body.querySelector('[data-add-secondary]').onclick = () => {
      const axes = activeAxes();
      const plots = activePlots();
      if (!plots.length || axes.length < 2) return alert('创建次坐标轴需要一个已有的有轴绘图区');
      if (plots.some((plot) => plotAxisGroup(plot, axes) === 'secondary')) return alert('当前图表已经包含次坐标轴绘图区');
      const source = plots[0];
      if (chartFamily(source.chartType) !== 'category') return alert('一键次坐标轴目前用于柱形/折线/面积/雷达系列');
      const sourceAxes = (source.axisIds || []).map((id) => axes.find((axis) => Number(axis.id) === Number(id))).filter(Boolean);
      const category = sourceAxes.find((axis) => ['category', 'date'].includes(axis.axisType));
      const value = sourceAxes.find((axis) => axis.axisType === 'value');
      if (!category || !value) return alert('已有绘图区缺少可配对的分类轴和数值轴');
      const maxId = Math.max(0, ...axes.map((axis) => Number(axis.id) || 0));
      const categoryId = maxId + 1, valueId = maxId + 2;
      addAxisCard({ ...copy(category), id: categoryId, position: 'top', crossAxisId: valueId, delete: false }, true);
      addAxisCard({ ...copy(value), id: valueId, position: 'right', crossAxisId: categoryId, delete: false }, true);
      const index = Math.max(-1, ...plots.map((plot) => Number(plot.index))) + 1;
      addPlotCard({ index, cloneFrom: Number(source.index), chartType: source.chartType === 'line' ? 'bar' : 'line', axisIds: [categoryId, valueId], axisGroup: 'secondary', dataLabels: null }, true);
      touchAxes(); touchPlots();
    };

    const tbody = body.querySelector('[data-series]');
    const touchSeries = () => changed.add('series');
    (model.series || []).forEach((series) => seriesRow(series, tbody, touchSeries, activePlots, theme));
    body.querySelector('[data-add-series]').onclick = () => {
      changed.add('series');
      const plots = activePlots();
      const plot = plots[0] || { index: 0, chartType: model.chartType || 'bar', axisIds: [], axisGroup: 'primary' };
      seriesRow({ name: `系列 ${tbody.querySelectorAll('[data-series-row="1"]').length + 1}`, bindingMode: 'embedded', categories: [], values: [], pointOverrides: [], color: '', plotIndex: plot.index, plotType: plot.chartType, axisIds: copy(plot.axisIds || []), axisGroup: plot.axisGroup || 'primary', dataLabels: null, trendlines: [], errorBars: [] }, tbody, touchSeries, activePlots, theme, true);
    };
    return () => {
      if (changed.has('title')) draft.title = title.value;
      if (changed.has('chartType')) draft.chartType = type.value;
      if (changed.has('chartType') && (chartFamily(draft.chartType) !== baseFamily
        || (['combo', 'stock'].includes(baseType) && draft.chartType !== baseType))) {
        throw new Error(`当前图表只能在兼容类型族内转换，不能从 ${baseType} 转为 ${draft.chartType}`);
      }
      if (changed.has('legendShow') || changed.has('legendPosition')) {
        draft.legend = copy(draft.legend || {});
        if (changed.has('legendShow')) draft.legend.show = legendShow.checked;
        if (changed.has('legendPosition')) draft.legend.position = legendPosition.value;
      }
      if (changed.has('axes')) draft.axes = [...activeAxes(), ...deletedAxes];
      if (changed.has('plots')) {
        const currentPlots = activePlots();
        if (!currentPlots.length) throw new Error('图表至少需要一个绘图区');
        draft.plots = [...currentPlots, ...deletedPlots.sort((left, right) => Number(right.index) - Number(left.index))];
        draft.chartType = currentPlots.length > 1 ? 'combo' : currentPlots[0].chartType;
      }
      const axes = changed.has('axes') ? draft.axes.filter((axis) => !axis.$delete) : activeAxes();
      const plots = changed.has('plots') ? draft.plots.filter((plot) => !plot.$delete) : activePlots();
      const axisIds = new Set(axes.map((axis) => Number(axis.id)));
      plots.forEach((plot) => (plot.axisIds || []).forEach((id) => {
        if (!axisIds.has(Number(id))) throw new Error(`绘图区 ${plot.index} 引用了不存在的坐标轴 ${id}`);
      }));
      axes.forEach((axis) => {
        if (axis.crossAxisId != null && !axisIds.has(Number(axis.crossAxisId))) throw new Error(`坐标轴 ${axis.id} 的交叉轴 ${axis.crossAxisId} 不存在`);
      });
      if (changed.has('series')) {
        draft.series = [...tbody.querySelectorAll(':scope > [data-series-row="1"]')].map((tr) => tr._nativeCollect());
      }
      return copy(draft);
    };
  }

  function stopRow(stop, tbody, onChange, theme, isNew = false) {
    const original = copy(stop || {});
    const tr = document.createElement('tr');
    tr._nativeOriginal = original;
    tr._nativeNew = isNew;
    tr.innerHTML = '<td><input data-k="position" type="number" min="0" max="100"></td><td><input data-k="color" type="color"></td><td><input data-k="alpha" type="number" min="0" max="1" step="0.05"></td><td class="native-row-actions"><button data-a="up">↑</button><button data-a="down">↓</button><button data-a="delete">×</button></td>';
    tr.querySelector('[data-k=position]').value = stop.position ?? 0;
    setColorInput(tr.querySelector('[data-k=color]'), stop.color, '#808080', stop.colorSpec, theme);
    tr.querySelector('[data-k=alpha]').value = stop.alpha ?? 1;
    tr.querySelectorAll('input').forEach((input) => {
      input.addEventListener('input', () => { input.dataset.nativeChanged = '1'; onChange(); });
    });
    tr.onclick = (event) => {
      const action = event.target.closest('button')?.dataset.a;
      if (!action) return;
      if (action === 'delete') tr.remove();
      if (action === 'up' && tr.previousElementSibling) tbody.insertBefore(tr, tr.previousElementSibling);
      if (action === 'down' && tr.nextElementSibling) tbody.insertBefore(tr.nextElementSibling, tr);
      onChange();
    };
    tbody.appendChild(tr);
  }

  function buildShape(body, model, theme) {
    const draft = copy(model);
    const fill = model.fill || {}, line = model.line || {}, effects = model.effects || {}, shadow = effects.shadow || {};
    const changed = new Set();
    body.innerHTML = '<div class="native-grid"><label class="native-wide">文字<textarea data-f="text" rows="3"></textarea></label><label>几何形状<select data-f="geometry"><option value="rect">矩形</option><option value="roundRect">圆角矩形</option><option value="ellipse">椭圆</option><option value="triangle">三角形</option><option value="line">直线</option><option value="straightConnector1">连接线</option><option value="custom">自定义几何（保留）</option></select></label><label>旋转角度<input data-f="rotation" type="number" step="1"></label><label class="native-check"><input data-f="flipH" type="checkbox">水平翻转</label><label class="native-check"><input data-f="flipV" type="checkbox">垂直翻转</label></div><div class="native-section-title">填充</div><div class="native-grid"><label>类型<select data-f="fillKind"><option value="none">无填充</option><option value="solid">纯色</option><option value="gradient">渐变</option><option value="other">其他填充（保留）</option></select></label><label>颜色<input data-f="fillColor" type="color"></label><label>透明度<input data-f="fillAlpha" type="number" min="0" max="1" step="0.05"></label><label>渐变角度<input data-f="fillAngle" type="number"></label></div><div class="native-table-wrap"><table class="native-table native-stop-table"><thead><tr><th>位置 %</th><th>颜色</th><th>不透明度</th><th></th></tr></thead><tbody data-stops></tbody></table></div><button class="native-add" data-add-stop>＋ 添加渐变光标</button><div class="native-section-title">轮廓</div><div class="native-grid"><label>颜色<input data-f="lineColor" type="color"></label><label>透明度<input data-f="lineAlpha" type="number" min="0" max="1" step="0.05"></label><label>宽度 pt<input data-f="lineWidth" type="number" min="0" step="0.25"></label><label>虚线<select data-f="lineDash"><option value="solid">实线</option><option value="dash">虚线</option><option value="dot">点线</option><option value="dashDot">点划线</option></select></label></div><div class="native-section-title">效果</div><div class="native-grid"><label class="native-check"><input data-f="shadowEnabled" type="checkbox">阴影</label><label>阴影颜色<input data-f="shadowColor" type="color"></label><label>阴影透明度<input data-f="shadowAlpha" type="number" min="0" max="1" step="0.05"></label><label>模糊 pt<input data-f="shadowBlur" type="number" min="0"></label><label>距离 pt<input data-f="shadowDistance" type="number" min="0"></label><label>角度<input data-f="shadowAngle" type="number"></label><label>柔化边缘 pt<input data-f="softEdge" type="number" min="0" step="0.25"></label></div>';
    let textStructureTouched = false;
    const textStructureTitle = document.createElement('div');
    textStructureTitle.className = 'native-section-title';
    textStructureTitle.textContent = '段落与文字片段（run）';
    const textStructure = document.createElement('div');
    textStructure.className = 'native-text-structure';
    const addParagraph = document.createElement('button');
    addParagraph.type = 'button';
    addParagraph.className = 'native-add';
    addParagraph.textContent = '＋ 添加段落';
    const firstSection = body.querySelector('.native-section-title');
    firstSection.before(textStructureTitle, textStructure, addParagraph);

    const nullableBoolean = (value) => value == null ? '' : value ? 'true' : 'false';
    const nullableNumber = (value) => value == null ? '' : value;
    const markTextControl = (control) => {
      const eventName = control.tagName === 'SELECT' ? 'change' : 'input';
      control.addEventListener(eventName, () => {
        control.dataset.nativeChanged = '1';
        textStructureTouched = true;
      });
    };
    const refreshParagraphLabels = () => {
      [...textStructure.children].forEach((card, index) => {
        card.querySelector('[data-paragraph-label]').textContent = `段落 ${index + 1}`;
        card.querySelector('[data-delete-paragraph]').disabled = textStructure.children.length <= 1;
      });
    };
    const addTextRunRow = (tbody, run = {}, isNew = false) => {
      const row = document.createElement('tr');
      row._nativeOriginal = copy(run || {});
      row._nativeNew = isNew;
      row.innerHTML = '<td><textarea data-k="text" rows="2"></textarea></td><td><input data-k="font" placeholder="继承"></td><td><input data-k="size" type="number" min="0" step="0.5" placeholder="继承"></td><td><select data-k="bold"><option value="">继承</option><option value="true">是</option><option value="false">否</option></select></td><td><select data-k="italic"><option value="">继承</option><option value="true">是</option><option value="false">否</option></select></td><td><select data-k="underline"><option value="">继承</option><option value="none">无</option><option value="sng">单线</option><option value="dbl">双线</option><option value="heavy">粗线</option></select></td><td><input data-k="color" placeholder="#RRGGBB / accent1 / 继承"></td><td><input data-k="alpha" type="number" min="0" max="1" step="0.05" placeholder="继承"></td><td><button type="button" data-delete-run title="删除 run">×</button></td>';
      row.querySelector('[data-k=text]').value = run.text ?? '';
      row.querySelector('[data-k=font]').value = run.font ?? '';
      row.querySelector('[data-k=size]').value = nullableNumber(run.size);
      row.querySelector('[data-k=bold]').value = nullableBoolean(run.bold);
      row.querySelector('[data-k=italic]').value = nullableBoolean(run.italic);
      preserveSelectValue(row.querySelector('[data-k=underline]'), run.underline ?? '', '原下划线值（保留）');
      row.querySelector('[data-k=color]').value = run.color ?? '';
      row.querySelector('[data-k=alpha]').value = nullableNumber(run.alpha);
      row.querySelectorAll('input,textarea,select').forEach(markTextControl);
      row.querySelector('[data-delete-run]').onclick = () => {
        row.remove();
        textStructureTouched = true;
      };
      tbody.appendChild(row);
      return row;
    };
    const addTextParagraph = (paragraph = { text: '', runs: [{ kind: 'r', text: '' }] }, isNew = false) => {
      const card = document.createElement('div');
      card.className = 'native-text-paragraph';
      card._nativeOriginal = copy(paragraph || {});
      card._nativeNew = isNew;
      card.innerHTML = '<div class="native-text-paragraph-head"><strong data-paragraph-label></strong><span><button type="button" data-add-run>＋ run</button><button type="button" data-delete-paragraph>删除段落</button></span></div><div class="native-table-wrap"><table class="native-table native-text-run-table"><thead><tr><th>文字</th><th>字体</th><th>字号 pt</th><th>粗体</th><th>斜体</th><th>下划线</th><th>颜色</th><th>Alpha</th><th></th></tr></thead><tbody></tbody></table></div>';
      const tbody = card.querySelector('tbody');
      (paragraph.runs || []).forEach((run) => addTextRunRow(tbody, run));
      card.querySelector('[data-add-run]').onclick = () => {
        addTextRunRow(tbody, { kind: 'r', text: '', font: null, size: null, bold: null, italic: null, underline: null, color: null, alpha: null }, true);
        textStructureTouched = true;
      };
      card.querySelector('[data-delete-paragraph]').onclick = () => {
        if (textStructure.children.length <= 1) return;
        card.remove();
        textStructureTouched = true;
        refreshParagraphLabels();
      };
      textStructure.appendChild(card);
      refreshParagraphLabels();
      return card;
    };
    const initialParagraphs = Array.isArray(model.paragraphs) && model.paragraphs.length
      ? model.paragraphs
      : String(model.text ?? '').split('\n').map((text) => ({ text, runs: [{ kind: 'r', text }] }));
    initialParagraphs.forEach((paragraph) => addTextParagraph(paragraph));
    addParagraph.onclick = () => {
      addTextParagraph({ text: '', runs: [{ kind: 'r', text: '', font: null, size: null, bold: null, italic: null, underline: null, color: null, alpha: null }] }, true);
      textStructureTouched = true;
    };

    const readNullableBoolean = (control) => control.value === '' ? null : control.value === 'true';
    const readNullableNumber = (control) => control.value === '' ? null : Number(control.value);
    const collectTextStructure = () => [...textStructure.children].map((card) => {
      const paragraph = copy(card._nativeOriginal || { text: '', runs: [] });
      paragraph.runs = [...card.querySelectorAll('tbody > tr')].map((row) => {
        const run = copy(row._nativeOriginal || { kind: 'r' });
        const field = (name) => row.querySelector(`[data-k=${name}]`);
        const assign = (name, value) => {
          if (row._nativeNew || field(name).dataset.nativeChanged === '1') run[name] = value;
        };
        assign('text', field('text').value);
        assign('font', field('font').value.trim() || null);
        assign('size', readNullableNumber(field('size')));
        assign('bold', readNullableBoolean(field('bold')));
        assign('italic', readNullableBoolean(field('italic')));
        assign('underline', field('underline').value || null);
        assign('color', field('color').value.trim() || null);
        if (row._nativeNew || field('color').dataset.nativeChanged === '1') delete run.colorSpec;
        assign('alpha', readNullableNumber(field('alpha')));
        if (!run.kind) run.kind = 'r';
        return run;
      });
      paragraph.text = paragraph.runs.map((run) => run.text || '').join('');
      return paragraph;
    });
    const advancedTitle = document.createElement('div');
    advancedTitle.className = 'native-section-title native-gradient-advanced';
    advancedTitle.textContent = '复杂渐变参数';
    const advanced = document.createElement('div');
    advanced.className = 'native-grid native-gradient-advanced';
    advanced.innerHTML = '<label>方向类型<select data-f="fillDirectionType"><option value="linear">线性</option><option value="path">路径</option></select></label><label class="native-check"><input data-f="fillScaled" type="checkbox">随边界缩放</label><label>路径形状<select data-f="fillPath"><option value="circle">圆形</option><option value="rect">矩形</option><option value="shape">形状</option></select></label><label>填充矩形 l,t,r,b<input data-f="fillToRect" placeholder="0, 0, 0, 0"></label><label>平铺矩形 l,t,r,b<input data-f="tileRect" placeholder="0, 0, 0, 0"></label><label>翻转<select data-f="fillFlip"><option value="none">无</option><option value="x">水平</option><option value="y">垂直</option><option value="xy">双向</option></select></label><label class="native-check"><input data-f="fillRotWithShape" type="checkbox">随形状旋转</label>';
    const stopsWrap = body.querySelector('.native-stop-table').closest('.native-table-wrap');
    stopsWrap.before(advancedTitle, advanced);
    const rectText = (value) => value && typeof value === 'object'
      ? ['l', 't', 'r', 'b'].map((key) => value[key] ?? '').join(', ') : '';
    const parseRect = (value, label) => {
      const raw = String(value ?? '').trim();
      if (!raw) return null;
      const parts = raw.split(/[,，\s]+/).filter(Boolean);
      if (parts.length !== 4 || parts.some((part) => !Number.isFinite(Number(part)))) {
        throw new Error(`${label}必须是四个数字：l, t, r, b`);
      }
      return Object.fromEntries(['l', 't', 'r', 'b'].map((key, index) => [key, Number(parts[index])]));
    };
    const colorSpecForField = (name) => name === 'fillColor' ? fill.colorSpec
      : name === 'lineColor' ? line.colorSpec : name === 'shadowColor' ? shadow.colorSpec : null;
    const set = (name, value) => { const el = body.querySelector(`[data-f=${name}]`); if (el.type === 'checkbox') el.checked = !!value; else if (el.type === 'color') setColorInput(el, value, name === 'shadowColor' ? '#000000' : name === 'lineColor' ? '#667085' : '#808080', colorSpecForField(name), theme); else el.value = value ?? ''; };
    set('text', model.text); set('rotation', model.rotation ?? 0); set('flipH', model.flipH); set('flipV', model.flipV);
    preserveSelectValue(body.querySelector('[data-f=geometry]'), model.geometry ?? '');
    preserveSelectValue(body.querySelector('[data-f=fillKind]'), fill.kind ?? 'none');
    set('fillColor', fill.color); set('fillAlpha', fill.alpha ?? 1); set('fillAngle', fill.angle);
    preserveSelectValue(body.querySelector('[data-f=fillDirectionType]'), fill.directionType ?? '');
    set('fillScaled', fill.scaled);
    preserveSelectValue(body.querySelector('[data-f=fillPath]'), fill.path ?? '');
    body.querySelector('[data-f=fillToRect]').value = rectText(fill.fillToRect);
    body.querySelector('[data-f=tileRect]').value = rectText(fill.tileRect);
    preserveSelectValue(body.querySelector('[data-f=fillFlip]'), fill.flip ?? '');
    set('fillRotWithShape', fill.rotWithShape);
    set('lineColor', line.color); set('lineAlpha', line.alpha ?? 1); set('lineWidth', line.width ?? 1);
    preserveSelectValue(body.querySelector('[data-f=lineDash]'), line.dash ?? 'solid');
    set('shadowEnabled', shadow.enabled); set('shadowColor', shadow.color); set('shadowAlpha', shadow.alpha ?? .35); set('shadowBlur', shadow.blur ?? 6); set('shadowDistance', shadow.distance ?? 6); set('shadowAngle', shadow.angle ?? 45); set('softEdge', effects.softEdge || 0);
    const tbody = body.querySelector('[data-stops]');
    const touchStops = () => changed.add('fill.stops');
    (fill.stops || []).forEach((stop) => stopRow(stop, tbody, touchStops, theme));
    body.querySelector('[data-add-stop]').onclick = () => {
      changed.add('fill.stops');
      stopRow({ position: tbody.children.length ? 100 : 0, color: normalizedTheme(theme).accent1, alpha: 1 }, tbody, touchStops, theme, true);
      updateFillControls();
    };
    const get = (name) => body.querySelector(`[data-f=${name}]`);
    const mark = (name, eventName = 'input') => get(name).addEventListener(eventName, () => changed.add(name));
    mark('text'); mark('geometry', 'change'); mark('rotation'); mark('flipH', 'change'); mark('flipV', 'change');
    mark('fillColor'); mark('fillAlpha'); mark('fillAngle');
    mark('fillDirectionType', 'change'); mark('fillScaled', 'change'); mark('fillPath', 'change');
    mark('fillToRect'); mark('tileRect'); mark('fillFlip', 'change'); mark('fillRotWithShape', 'change');
    mark('lineColor'); mark('lineAlpha'); mark('lineWidth'); mark('lineDash', 'change');
    mark('shadowColor'); mark('shadowAlpha'); mark('shadowBlur'); mark('shadowDistance'); mark('shadowAngle'); mark('softEdge');
    const fillOtherOption = [...get('fillKind').options].find((option) => option.value === 'other');
    if (fillOtherOption) fillOtherOption.disabled = fill.kind !== 'other';
    function updateFillControls() {
      const kind = get('fillKind').value;
      const preserveOnly = kind === 'other';
      const gradient = kind === 'gradient';
      const pathGradient = get('fillDirectionType').value === 'path';
      get('fillColor').disabled = preserveOnly || kind === 'none';
      get('fillAlpha').disabled = preserveOnly || kind === 'none';
      get('fillAngle').disabled = preserveOnly || !gradient || pathGradient;
      get('fillDirectionType').disabled = preserveOnly || !gradient;
      get('fillScaled').disabled = preserveOnly || !gradient || pathGradient;
      get('fillPath').disabled = preserveOnly || !gradient || !pathGradient;
      get('fillToRect').disabled = preserveOnly || !gradient || !pathGradient;
      get('tileRect').disabled = preserveOnly || !gradient;
      get('fillFlip').disabled = preserveOnly || !gradient;
      get('fillRotWithShape').disabled = preserveOnly || !gradient;
      body.querySelector('[data-add-stop]').disabled = preserveOnly || !gradient;
      tbody.querySelectorAll('input,button').forEach((control) => { control.disabled = preserveOnly || !gradient; });
      advancedTitle.hidden = !gradient;
      advanced.hidden = !gradient;
    }
    get('fillKind').addEventListener('change', () => {
      changed.add('fillKind');
      const kind = get('fillKind').value;
      if (kind === 'solid') { changed.add('fillColor'); changed.add('fillAlpha'); }
      if (kind === 'gradient') {
        if (!['linear', 'path'].includes(get('fillDirectionType').value)) {
          get('fillDirectionType').value = 'linear';
          changed.add('fillDirectionType');
        }
        if (get('fillDirectionType').value === 'linear' && get('fillAngle').value === '') {
          get('fillAngle').value = '0';
        }
        if (fill.scaled == null) get('fillScaled').checked = true;
        if (fill.rotWithShape == null) get('fillRotWithShape').checked = true;
        changed.add('fillScaled'); changed.add('fillRotWithShape');
        changed.add('fillAngle'); changed.add('fill.stops');
        if (!tbody.children.length) {
          stopRow({ position: 0, color: normalizedTheme(theme).accent1, alpha: 1 }, tbody, touchStops, theme, true);
          stopRow({ position: 100, color: '#FFFFFF', alpha: 1 }, tbody, touchStops, theme, true);
        }
      }
      updateFillControls();
    });
    get('fillDirectionType').addEventListener('change', () => {
      changed.add('fillDirectionType');
      if (get('fillDirectionType').value === 'path') {
        if (!get('fillPath').value) {
          get('fillPath').value = 'circle';
          changed.add('fillPath');
        }
      } else {
        if (get('fillAngle').value === '') get('fillAngle').value = '0';
        changed.add('fillAngle');
        changed.add('fillScaled');
      }
      updateFillControls();
    });
    const shadowFields = ['shadowColor', 'shadowAlpha', 'shadowBlur', 'shadowDistance', 'shadowAngle'];
    const updateShadowControls = () => shadowFields.forEach((name) => { get(name).disabled = !get('shadowEnabled').checked; });
    get('shadowEnabled').addEventListener('change', () => { changed.add('shadowEnabled'); updateShadowControls(); });
    updateFillControls();
    updateShadowControls();
    return () => {
      if (changed.has('text')) draft.text = get('text').value;
      if (textStructureTouched) {
        draft.paragraphs = collectTextStructure();
        draft.text = draft.paragraphs.map((paragraph) => paragraph.text || '').join('\n');
      }
      if (changed.has('geometry')) draft.geometry = get('geometry').value;
      if (changed.has('rotation')) draft.rotation = Number(get('rotation').value) || 0;
      if (changed.has('flipH')) draft.flipH = get('flipH').checked;
      if (changed.has('flipV')) draft.flipV = get('flipV').checked;
      if ([...changed].some((name) => name === 'fillKind' || name === 'tileRect' || name.startsWith('fill'))) {
        draft.fill = copy(draft.fill || {});
        if (changed.has('fillKind')) draft.fill.kind = get('fillKind').value;
        if (draft.fill.kind !== 'other') {
          if (changed.has('fillColor')) {
            draft.fill.color = changed.has('fillKind') && get('fillColor').dataset.colorChanged !== '1'
              ? get('fillColor').value : getColorInput(get('fillColor'));
            delete draft.fill.colorSpec;
          }
          if (changed.has('fillAlpha')) draft.fill.alpha = clamp(get('fillAlpha').value, 0, 1);
          if (changed.has('fillAngle')) draft.fill.angle = get('fillAngle').value === '' ? null : Number(get('fillAngle').value);
          if (draft.fill.kind === 'gradient') {
            if (changed.has('fillDirectionType')) draft.fill.directionType = get('fillDirectionType').value || null;
            if (changed.has('fillScaled')) draft.fill.scaled = get('fillScaled').checked;
            if (changed.has('fillPath')) draft.fill.path = get('fillPath').value || null;
            if (changed.has('fillToRect')) draft.fill.fillToRect = parseRect(get('fillToRect').value, '填充矩形');
            if (changed.has('tileRect')) draft.fill.tileRect = parseRect(get('tileRect').value, '平铺矩形');
            if (changed.has('fillFlip')) draft.fill.flip = get('fillFlip').value || null;
            if (changed.has('fillRotWithShape')) draft.fill.rotWithShape = get('fillRotWithShape').checked;
          }
          if (changed.has('fill.stops')) {
            draft.fill.stops = [...tbody.children].map((tr) => {
              const next = copy(tr._nativeOriginal || {});
              const field = (name) => tr.querySelector(`[data-k=${name}]`);
              if (tr._nativeNew || field('position').dataset.nativeChanged === '1') next.position = clamp(field('position').value, 0, 100);
              if (tr._nativeNew || field('color').dataset.nativeChanged === '1') {
                next.color = getColorInput(field('color'));
                delete next.colorSpec;
              }
              if (tr._nativeNew || field('alpha').dataset.nativeChanged === '1') next.alpha = clamp(field('alpha').value, 0, 1);
              return next;
            });
          }
        }
      }
      const lineTouched = ['lineColor', 'lineAlpha', 'lineWidth', 'lineDash'].some((name) => changed.has(name));
      if (lineTouched) {
        const hadLine = draft.line && typeof draft.line === 'object';
        draft.line = copy(hadLine ? draft.line : {});
        if (!hadLine || changed.has('lineColor')) draft.line.color = !hadLine && get('lineColor').dataset.colorChanged !== '1'
          ? get('lineColor').value : getColorInput(get('lineColor'));
        if (!hadLine || changed.has('lineColor')) delete draft.line.colorSpec;
        if (!hadLine || changed.has('lineAlpha')) draft.line.alpha = clamp(get('lineAlpha').value, 0, 1);
        if (!hadLine || changed.has('lineWidth')) draft.line.width = Math.max(0, Number(get('lineWidth').value) || 0);
        if (!hadLine || changed.has('lineDash')) draft.line.dash = get('lineDash').value;
      }
      const shadowTouched = changed.has('shadowEnabled') || shadowFields.some((name) => changed.has(name));
      if (shadowTouched || changed.has('softEdge')) draft.effects = copy(draft.effects || {});
      if (shadowTouched) {
        const hadShadow = !!draft.effects.shadow?.enabled;
        draft.effects.shadow = copy(draft.effects.shadow || {});
        draft.effects.shadow.enabled = get('shadowEnabled').checked;
        if (draft.effects.shadow.enabled && (!hadShadow || changed.has('shadowColor'))) {
          draft.effects.shadow.color = !hadShadow && get('shadowColor').dataset.colorChanged !== '1'
            ? get('shadowColor').value : getColorInput(get('shadowColor'));
          delete draft.effects.shadow.colorSpec;
        }
        if (draft.effects.shadow.enabled && (!hadShadow || changed.has('shadowAlpha'))) draft.effects.shadow.alpha = clamp(get('shadowAlpha').value, 0, 1);
        if (draft.effects.shadow.enabled && (!hadShadow || changed.has('shadowBlur'))) draft.effects.shadow.blur = Math.max(0, Number(get('shadowBlur').value) || 0);
        if (draft.effects.shadow.enabled && (!hadShadow || changed.has('shadowDistance'))) draft.effects.shadow.distance = Math.max(0, Number(get('shadowDistance').value) || 0);
        if (draft.effects.shadow.enabled && (!hadShadow || changed.has('shadowAngle'))) draft.effects.shadow.angle = Number(get('shadowAngle').value) || 0;
      }
      if (changed.has('softEdge')) draft.effects.softEdge = Math.max(0, Number(get('softEdge').value) || 0);
      return copy(draft);
    };
  }

  function smartNodeRow(node, tbody, onChange, allowedExternal, openTextDetails, isNew = false) {
    const tr = document.createElement('tr');
    tr._nativeOriginal = copy(node || {});
    tr._nativeNew = isNew;
    tr.dataset.id = node.id || `node-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
    tr.innerHTML = '<td><input data-k="text"></td><td><select data-k="parent"></select></td><td><select data-k="kind"><option value="node">节点</option><option value="asst">助理</option></select></td><td class="native-row-actions"><button type="button" data-a="format" title="编辑段落与 run 格式">格式</button><button type="button" data-a="up">↑</button><button type="button" data-a="down">↓</button><button type="button" data-a="delete">×</button></td>';
    tr.querySelector('[data-k=text]').value = node.text || '';
    tr.querySelector('[data-k=parent]').dataset.value = node.parentId || '';
    preserveSelectValue(tr.querySelector('[data-k=kind]'), node.kind || 'node');
    tr.querySelector('[data-k=text]').addEventListener('input', (event) => {
      event.target.dataset.nativeChanged = '1';
      onChange('field');
      refreshSmartParents(tbody, allowedExternal);
    });
    tr.querySelector('[data-k=parent]').addEventListener('change', (event) => { event.target.dataset.nativeChanged = '1'; onChange('field'); });
    tr.querySelector('[data-k=kind]').addEventListener('change', (event) => { event.target.dataset.nativeChanged = '1'; onChange('field'); });
    const handleAction = (action) => {
      if (action === 'format') { openTextDetails(tr); return; }
      if (action === 'delete') { openTextDetails(null, tr); tr.remove(); refreshSmartParents(tbody, allowedExternal); }
      const parentValue = () => tr.querySelector('[data-k="parent"]')?.value || '';
      const hasSameParent = (candidate) =>
        candidate?.querySelector('[data-k="parent"]')?.value === parentValue();
      if (action === 'up') {
        let target = tr.previousElementSibling;
        while (target && !hasSameParent(target)) target = target.previousElementSibling;
        if (target) tbody.insertBefore(tr, target);
      }
      if (action === 'down') {
        let target = tr.nextElementSibling;
        while (target && !hasSameParent(target)) target = target.nextElementSibling;
        if (target) tbody.insertBefore(tr, target.nextElementSibling);
      }
      onChange(action === 'up' || action === 'down' ? 'order' : 'delete');
    };
    tr.querySelectorAll('[data-a]').forEach((button) => {
      button.onclick = (event) => {
        event.preventDefault();
        event.stopPropagation();
        handleAction(button.dataset.a);
      };
    });
    tbody.appendChild(tr);
    return tr;
  }

  function refreshSmartParents(tbody, allowedExternal = new Set()) {
    const rows = [...tbody.children];
    rows.forEach((row) => {
      const select = row.querySelector('[data-k=parent]');
      const previous = select.value || select.dataset.value || '';
      select.textContent = '';
      const root = document.createElement('option'); root.value = ''; root.textContent = '（顶层）'; select.appendChild(root);
      rows.filter((candidate) => candidate !== row).forEach((candidate) => {
        const option = document.createElement('option'); option.value = candidate.dataset.id;
        option.textContent = candidate.querySelector('[data-k=text]').value || candidate.dataset.id;
        select.appendChild(option);
      });
      if (previous && ![...select.options].some((o) => o.value === previous) && allowedExternal.has(previous)) {
        const option = document.createElement('option');
        option.value = previous; option.textContent = `${previous}（外部父节点，保留）`;
        option.dataset.preserve = '1'; select.appendChild(option);
      }
      const nextValue = [...select.options].some((o) => o.value === previous) ? previous : '';
      select.value = nextValue;
      if (previous && nextValue !== previous) select.dataset.nativeChanged = '1';
      delete select.dataset.value;
    });
  }

  function validateSmartTree(nodes, allowedExternal) {
    const ids = new Set();
    nodes.forEach((node) => {
      if (!node.id || ids.has(node.id)) throw new Error(`SmartArt 节点 ID 缺失或重复：${node.id || '空 ID'}`);
      ids.add(node.id);
    });
    nodes.forEach((node) => {
      if (node.parentId && !ids.has(node.parentId) && !allowedExternal.has(node.parentId)) {
        throw new Error(`SmartArt 父节点不存在：${node.parentId}`);
      }
      const seen = new Set([node.id]);
      let parent = node.parentId;
      while (parent && ids.has(parent)) {
        if (seen.has(parent)) throw new Error(`SmartArt 层级存在循环：${node.id} ↔ ${parent}`);
        seen.add(parent);
        parent = nodes.find((item) => item.id === parent)?.parentId || null;
      }
    });
  }

  function buildSmartArt(body, model) {
    body.innerHTML = '<div class="native-grid"><label>布局标识<input data-f="layout" placeholder="保留原 SmartArt 布局"></label></div><div class="native-section-title">SmartArt 节点与层级</div><div class="native-table-wrap"><table class="native-table"><thead><tr><th>文字</th><th>父节点</th><th>类型</th><th></th></tr></thead><tbody data-nodes></tbody></table></div><button class="native-add" data-add-node>＋ 添加节点</button><section class="native-smart-text-details" data-smart-text-details hidden><div class="native-smart-text-title"><div><strong data-smart-text-title></strong><span>只修改触碰过的 run 字段；未知 DrawingML 保留</span></div><button type="button" data-smart-text-close>关闭</button></div><div data-smart-text-body></div><button type="button" class="native-add" data-smart-add-paragraph>＋ 添加段落</button></section>';
    const draft = copy(model);
    const layout = body.querySelector('[data-f=layout]');
    layout.value = model.layout || '';
    layout.readOnly = true;
    layout.title = '当前仅保留原 SmartArt 布局；布局类型暂不支持修改';
    const tbody = body.querySelector('[data-nodes]');
    const initialIds = new Set((model.nodes || []).map((node) => node.id));
    const allowedExternal = new Set((model.nodes || []).map((node) => node.parentId).filter((id) => id && !initialIds.has(id)));
    let nodesTouched = false;
    let orderTouched = false;
    const touchNodes = (reason) => { nodesTouched = true; if (reason === 'order') orderTouched = true; };
    const details = body.querySelector('[data-smart-text-details]');
    const detailsBody = body.querySelector('[data-smart-text-body]');
    const detailsTitle = body.querySelector('[data-smart-text-title]');
    let detailsRow = null;

    const fallbackParagraphs = (row) => {
      const original = row._nativeOriginal || {};
      if (Array.isArray(original.paragraphs) && original.paragraphs.length) return copy(original.paragraphs);
      return String(row.querySelector('[data-k=text]').value || '').split('\n')
        .map((text) => ({ text, runs: [{ kind: 'r', text, font: null, size: null, bold: null, italic: null, underline: null, color: null, alpha: null }] }));
    };
    const nullableBoolean = (value) => value == null ? '' : value ? 'true' : 'false';
    const readNullableBoolean = (control) => control.value === '' ? null : control.value === 'true';
    const readNullableNumber = (control) => control.value === '' ? null : Number(control.value);
    const paragraphsText = (paragraphs) => paragraphs.map((paragraph) => (paragraph.runs || []).map((run) => run.text || '').join('')).join('\n');
    const markDetailsTouched = (control) => {
      const eventName = control.tagName === 'SELECT' ? 'change' : 'input';
      control.addEventListener(eventName, () => {
        control.dataset.nativeChanged = '1';
        if (detailsRow) detailsRow._nativeParagraphsTouched = true;
        touchNodes('field');
      });
    };
    const addSmartRun = (tbodyElement, run = {}, isNew = false) => {
      const row = document.createElement('tr');
      row._nativeOriginal = copy(run || {});
      row._nativeNew = isNew;
      row.innerHTML = '<td><textarea data-k="runText" rows="2"></textarea></td><td><input data-k="font" placeholder="继承"></td><td><input data-k="size" type="number" min="0" step="0.5" placeholder="继承"></td><td><select data-k="bold"><option value="">继承</option><option value="true">是</option><option value="false">否</option></select></td><td><select data-k="italic"><option value="">继承</option><option value="true">是</option><option value="false">否</option></select></td><td><select data-k="underline"><option value="">继承</option><option value="none">无</option><option value="sng">单线</option><option value="dbl">双线</option><option value="heavy">粗线</option></select></td><td><input data-k="color" placeholder="#RRGGBB / accent1 / 继承"></td><td><input data-k="alpha" type="number" min="0" max="1" step="0.05" placeholder="继承"></td><td><button type="button" data-delete-smart-run>×</button></td>';
      row.querySelector('[data-k=runText]').value = run.text ?? '';
      row.querySelector('[data-k=font]').value = run.font ?? '';
      row.querySelector('[data-k=size]').value = run.size ?? '';
      row.querySelector('[data-k=bold]').value = nullableBoolean(run.bold);
      row.querySelector('[data-k=italic]').value = nullableBoolean(run.italic);
      preserveSelectValue(row.querySelector('[data-k=underline]'), run.underline ?? '', '原下划线值（保留）');
      row.querySelector('[data-k=color]').value = run.color ?? '';
      row.querySelector('[data-k=alpha]').value = run.alpha ?? '';
      row.querySelectorAll('input,textarea,select').forEach(markDetailsTouched);
      row.querySelector('[data-delete-smart-run]').onclick = () => {
        row.remove();
        if (detailsRow) detailsRow._nativeParagraphsTouched = true;
        touchNodes('field');
      };
      tbodyElement.appendChild(row);
    };
    const refreshDetailParagraphs = () => {
      [...detailsBody.children].forEach((card, index) => {
        card.querySelector('[data-smart-paragraph-label]').textContent = `段落 ${index + 1}`;
        card.querySelector('[data-delete-smart-paragraph]').disabled = detailsBody.children.length <= 1;
      });
    };
    const addSmartParagraph = (paragraph = { text: '', runs: [{ kind: 'r', text: '' }] }, isNew = false) => {
      const card = document.createElement('div');
      card.className = 'native-text-paragraph';
      card._nativeOriginal = copy(paragraph || {});
      card._nativeNew = isNew;
      card.innerHTML = '<div class="native-text-paragraph-head"><strong data-smart-paragraph-label></strong><span><button type="button" data-add-smart-run>＋ run</button><button type="button" data-delete-smart-paragraph>删除段落</button></span></div><div class="native-table-wrap"><table class="native-table native-text-run-table"><thead><tr><th>文字</th><th>字体</th><th>字号 pt</th><th>粗体</th><th>斜体</th><th>下划线</th><th>颜色</th><th>Alpha</th><th></th></tr></thead><tbody></tbody></table></div>';
      const runBody = card.querySelector('tbody');
      (paragraph.runs || []).forEach((run) => addSmartRun(runBody, run));
      card.querySelector('[data-add-smart-run]').onclick = () => {
        addSmartRun(runBody, { kind: 'r', text: '', font: null, size: null, bold: null, italic: null, underline: null, color: null, alpha: null }, true);
        if (detailsRow) detailsRow._nativeParagraphsTouched = true;
        touchNodes('field');
      };
      card.querySelector('[data-delete-smart-paragraph]').onclick = () => {
        if (detailsBody.children.length <= 1) return;
        card.remove();
        if (detailsRow) detailsRow._nativeParagraphsTouched = true;
        touchNodes('field');
        refreshDetailParagraphs();
      };
      detailsBody.appendChild(card);
      refreshDetailParagraphs();
    };
    const collectSmartParagraphs = () => [...detailsBody.children].map((card) => {
      const paragraph = copy(card._nativeOriginal || { text: '', runs: [] });
      paragraph.runs = [...card.querySelectorAll('tbody > tr')].map((row) => {
        const run = copy(row._nativeOriginal || { kind: 'r' });
        const field = (name) => row.querySelector(`[data-k=${name}]`);
        const assign = (name, controlName, value) => {
          if (row._nativeNew || field(controlName).dataset.nativeChanged === '1') run[name] = value;
        };
        assign('text', 'runText', field('runText').value);
        assign('font', 'font', field('font').value.trim() || null);
        assign('size', 'size', readNullableNumber(field('size')));
        assign('bold', 'bold', readNullableBoolean(field('bold')));
        assign('italic', 'italic', readNullableBoolean(field('italic')));
        assign('underline', 'underline', field('underline').value || null);
        assign('color', 'color', field('color').value.trim() || null);
        if (row._nativeNew || field('color').dataset.nativeChanged === '1') delete run.colorSpec;
        assign('alpha', 'alpha', readNullableNumber(field('alpha')));
        if (!run.kind) run.kind = 'r';
        return run;
      });
      paragraph.text = paragraph.runs.map((run) => run.text || '').join('');
      return paragraph;
    });
    const saveTextDetails = () => {
      if (!detailsRow || !detailsRow._nativeParagraphsTouched) return;
      detailsRow._nativeParagraphs = collectSmartParagraphs();
      const text = paragraphsText(detailsRow._nativeParagraphs);
      const textInput = detailsRow.querySelector('[data-k=text]');
      textInput.value = text;
      textInput.dataset.nativeChanged = '1';
    };
    const openTextDetails = (row, removing = null) => {
      saveTextDetails();
      if (!row || (removing && removing === detailsRow)) {
        detailsRow = null;
        details.hidden = true;
        detailsBody.textContent = '';
        return;
      }
      detailsRow = row;
      detailsTitle.textContent = `节点文字格式 · ${row.querySelector('[data-k=text]').value || row.dataset.id}`;
      detailsBody.textContent = '';
      details.hidden = false;
      try {
        const paragraphs = row._nativeParagraphs || fallbackParagraphs(row);
        paragraphs.forEach((paragraph) => addSmartParagraph(paragraph));
      } catch (error) {
        console.error('SmartArt rich-text editor failed to open', error);
        detailsBody.textContent = `无法读取该节点的富文本：${error?.message || error}`;
      }
      details.scrollIntoView({ block: 'nearest' });
    };
    const syncStructuredPlainText = (row) => {
      if (!row._nativeParagraphsTouched) return;
      const paragraphs = copy(row._nativeParagraphs || fallbackParagraphs(row));
      const lines = String(row.querySelector('[data-k=text]').value || '').split('\n');
      while (paragraphs.length < lines.length) paragraphs.push({ text: '', runs: [{ kind: 'r', text: '' }] });
      paragraphs.forEach((paragraph, index) => {
        if (!Array.isArray(paragraph.runs) || !paragraph.runs.length) paragraph.runs = [{ kind: 'r', text: '' }];
        paragraph.runs[0].text = lines[index] ?? '';
        paragraph.runs.slice(1).forEach((run) => { run.text = ''; });
        paragraph.text = paragraph.runs.map((run) => run.text || '').join('');
      });
      row._nativeParagraphs = paragraphs;
    };
    const addNodeRow = (node, isNew = false) => {
      const row = smartNodeRow(node, tbody, touchNodes, allowedExternal, openTextDetails, isNew);
      row.querySelector('[data-k=text]').addEventListener('input', () => syncStructuredPlainText(row));
      return row;
    };
    (model.nodes || []).forEach((node) => addNodeRow(node));
    refreshSmartParents(tbody, allowedExternal);
    body.querySelector('[data-add-node]').onclick = () => {
      nodesTouched = true;
      const documentRoot = (model.nodes || []).find((node) => node.kind === 'doc' && !node.parentId)
        || (model.nodes || []).find((node) => node.kind === 'doc');
      addNodeRow({ text: '新节点', kind: 'node', parentId: documentRoot?.id || null }, true);
      refreshSmartParents(tbody, allowedExternal);
    };
    body.querySelector('[data-smart-text-close]').onclick = () => openTextDetails(null);
    body.querySelector('[data-smart-add-paragraph]').onclick = () => {
      addSmartParagraph({ text: '', runs: [{ kind: 'r', text: '', font: null, size: null, bold: null, italic: null, underline: null, color: null, alpha: null }] }, true);
      if (detailsRow) detailsRow._nativeParagraphsTouched = true;
      touchNodes('field');
    };
    return () => {
      saveTextDetails();
      if (nodesTouched) {
        const siblingOrders = new Map();
        draft.nodes = [...tbody.children].map((tr) => {
          const next = copy(tr._nativeOriginal || {});
          const text = tr.querySelector('[data-k=text]');
          const parent = tr.querySelector('[data-k=parent]');
          const kind = tr.querySelector('[data-k=kind]');
          next.id = tr.dataset.id;
          if (tr._nativeNew || text.dataset.nativeChanged === '1') next.text = text.value;
          if (tr._nativeParagraphsTouched) {
            next.paragraphs = copy(tr._nativeParagraphs || fallbackParagraphs(tr));
            next.text = paragraphsText(next.paragraphs);
          }
          if (tr._nativeNew || parent.dataset.nativeChanged === '1' || next.parentId == null) next.parentId = parent.value || null;
          if (tr._nativeNew || kind.dataset.nativeChanged === '1') next.kind = kind.value || 'node';
          const parentKey = next.parentId || '';
          const siblingOrder = siblingOrders.get(parentKey) || 0;
          if (tr._nativeNew || orderTouched) next.order = siblingOrder;
          siblingOrders.set(parentKey, Math.max(siblingOrder + 1, (Number(next.order) || 0) + 1));
          return next;
        });
      }
      validateSmartTree(draft.nodes || [], allowedExternal);
      return copy(draft);
    };
  }

  function fallbackModel(o, kind) {
    return kind === 'chart'
      ? { chartType: o.config.chartType || 'bar', title: o.config.title || '', legend: { show: true, position: 'right' }, series: [] }
      : kind === 'smartart'
        ? { layout: '', nodes: [] }
        : { text: '', geometry: 'rect', rotation: 0, flipH: false, flipV: false, fill: { kind: 'solid', color: '#4472C4', alpha: 1, angle: 0, stops: [] }, line: { color: '#667085', alpha: 1, width: 1, dash: 'solid' }, effects: { shadow: { enabled: false }, softEdge: 0 } };
  }

  function rawModel(o, kind) {
    return copy(native(o).model || fallbackModel(o, kind));
  }

  function initialModel(o, kind) {
    const descriptor = native(o);
    const base = rawModel(o, kind);
    const patch = descriptor.edits && descriptor.edits[kind];
    return kind === 'chart' ? mergeChartModel(base, patch) : mergeModel(base, patch);
  }

  function editorModel(kind, model) {
    const result = copy(model);
    if (kind === 'shape' && result.fill && Array.isArray(result.fill.stops)) {
      result.fill.stops = result.fill.stops.map((stop) => ({
        ...stop, position: Number(stop.position) >= 0 && Number(stop.position) <= 1 ? Number(stop.position) * 100 : Number(stop.position) || 0,
      }));
    }
    return result;
  }

  window.openNativeDrawingEditor = (o) => {
    const descriptor = native(o);
    if (!descriptor) return openObjSourceEditor(o);
    const kind = descriptor.kind === 'chart' ? 'chart' : descriptor.kind === 'smartart' ? 'smartart'
      : ['shape', 'connector', 'group'].includes(descriptor.kind) ? 'shape' : null;
    if (!kind) { setStatus(`该原生 ${descriptor.kind || 'DrawingML'} 对象暂无内容字段，仍可移动、缩放、复制和删除。`); return; }
    if (descriptor.model?.error) {
      const message = `该原生对象的 DrawingML 模型无法安全解析：${descriptor.model.error}`;
      setStatus(message);
      alert(message);
      return;
    }
    document.getElementById('native-drawing-dialog')?.remove();
    const overlay = document.createElement('div');
    overlay.id = 'native-drawing-dialog';
    overlay.className = 'native-editor-overlay';
    overlay.innerHTML = '<div class="native-editor"><div class="native-editor-title"><div><strong data-title></strong><span data-subtitle></span></div><button data-close title="关闭">×</button></div><div class="native-editor-body"></div><div class="native-editor-footer"><span data-info>所有未显示的 DrawingML 节点会原样保留。</span><button data-cancel>取消</button><button data-apply>应用</button><button data-ok class="primary">确定</button></div></div>';
    const label = kind === 'chart' ? '编辑原生 Excel 图表' : kind === 'smartart' ? '编辑原生 SmartArt' : '编辑原生 DrawingML 形状';
    overlay.querySelector('[data-title]').textContent = label;
    overlay.querySelector('[data-subtitle]').textContent = descriptor.name ? ` · ${descriptor.name}` : '';
    const body = overlay.querySelector('.native-editor-body');
    const theme = drawingThemeFor(o);
    const baseModel = editorModel(kind, rawModel(o, kind));
    const model = editorModel(kind, initialModel(o, kind));
    const collect = kind === 'chart' ? buildChart(body, model, baseModel, theme) : kind === 'smartart' ? buildSmartArt(body, model) : buildShape(body, model, theme);
    // Kept on the dialog (instead of a global) so browser regression tests and accessibility
    // tooling can inspect the exact differential model without committing the object.
    overlay._nativeCollect = collect;
    overlay._nativeDiff = () => {
      const current = collect();
      return kind === 'chart' ? chartModelDiff(baseModel, current) : modelDiff(baseModel, current);
    };
    const close = () => overlay.remove();
    const applyButton = overlay.querySelector('[data-apply]');
    const okButton = overlay.querySelector('[data-ok]');
    let applying = false;
    const apply = async (closeAfter) => {
      if (applying) return;
      applying = true;
      applyButton.disabled = true;
      okButton.disabled = true;
      try {
        const before = copy(o);
        const next = collect();
        const candidate = copy(o);
        const candidateDescriptor = native(candidate);
        candidateDescriptor.edits ||= {};
        const difference = kind === 'chart' ? chartModelDiff(baseModel, next) : modelDiff(baseModel, next);
        if (difference === undefined) delete candidateDescriptor.edits[kind];
        else candidateDescriptor.edits[kind] = difference;
        if (!Object.keys(candidateDescriptor.edits).length) delete candidateDescriptor.edits;
        const previousDifference = native(o).edits?.[kind];
        const nextDifference = candidateDescriptor.edits?.[kind];
        if (equal(previousDifference, nextDifference)) {
          setStatus(`${label}没有更改。`);
          if (closeAfter) close();
          return;
        }
        updatePreview(candidate, kind, next);
        await commitNativeObject(candidate);
        replaceObject(o, candidate);
        window.recordObjectHistory({ id: o.id, sheet: o.sheet, before, after: o, label });
        renderObjects(); selectObject(o.id);
        setStatus(`${label}已保存；导出仍为原生可编辑 DrawingML。`);
        if (closeAfter) close();
      } catch (error) {
        const message = `${label}失败：${error?.message || error}`;
        setStatus(message);
        alert(message);
      } finally {
        applying = false;
        if (overlay.isConnected) {
          applyButton.disabled = false;
          okButton.disabled = false;
        }
      }
    };
    overlay.querySelector('[data-close]').onclick = close;
    overlay.querySelector('[data-cancel]').onclick = close;
    overlay.querySelector('[data-apply]').onclick = () => void apply(false);
    overlay.querySelector('[data-ok]').onclick = () => void apply(true);
    overlay.addEventListener('pointerdown', (event) => event.stopPropagation());
    overlay.addEventListener('keydown', (event) => {
      event.stopPropagation();
      if (event.key === 'Escape') { event.preventDefault(); close(); }
      if ((event.ctrlKey || event.metaKey) && event.key === 'Enter') { event.preventDefault(); void apply(true); }
    });
    document.body.appendChild(overlay);
    overlay.querySelector('input,textarea,select')?.focus();
    historyArmed = true;
    notifyHistory();
  };
})();
