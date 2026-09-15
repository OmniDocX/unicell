/* UniCell verified fonts: exact local bytes first, content-addressed network fallback second. */
(() => {
  'use strict';
  const MANIFEST_URL = '/fonts/manifest.json';
  const state = {
    manifestPromise: null,
    localQueryPromise: null,
    localQueryState: 'idle',
    localFonts: [],
    installed: new Set(),
    requested: new Map(),
    epoch: 0,
    scheduled: false,
  };
  const mocks = () => window.__UNICELL_FONT_RUNTIME_MOCKS__ || {};
  const normalizeName = (value) => String(value || '').trim().replace(/^["']|["']$/g, '')
    .replace(/\s+/g, ' ').toLowerCase();
  const normalizeHash = (value, length) => {
    const hash = String(value || '').trim().toLowerCase();
    return new RegExp(`^[a-f0-9]{${length}}$`).test(hash) ? hash : '';
  };
  const bytesOf = (data) => data instanceof ArrayBuffer ? new Uint8Array(data)
    : ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : null;

  const MD5_SHIFTS = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
  ];
  const MD5_CONSTANTS = Array.from({ length: 64 }, (_, index) =>
    Math.floor(Math.abs(Math.sin(index + 1)) * 0x100000000) >>> 0);

  // MD5 is only a release-version identity. SHA-256 below is the mandatory integrity boundary.
  async function md5(data) {
    if (typeof mocks().digestMd5 === 'function') return normalizeHash(await mocks().digestMd5(data), 32);
    let source = bytesOf(data);
    if (!source && data?.arrayBuffer) source = new Uint8Array(await data.arrayBuffer());
    if (!source) return '';
    const length = source.byteLength;
    const paddedLength = Math.ceil((length + 9) / 64) * 64;
    const padded = new Uint8Array(paddedLength);
    padded.set(source); padded[length] = 0x80;
    const view = new DataView(padded.buffer);
    view.setUint32(paddedLength - 8, (length << 3) >>> 0, true);
    view.setUint32(paddedLength - 4, Math.floor(length / 0x20000000) >>> 0, true);
    let a0 = 0x67452301; let b0 = 0xefcdab89; let c0 = 0x98badcfe; let d0 = 0x10325476;
    const words = new Uint32Array(16);
    let sliceStarted = Date.now();
    for (let offset = 0; offset < paddedLength; offset += 64) {
      for (let index = 0; index < 16; index++) words[index] = view.getUint32(offset + index * 4, true);
      let a = a0; let b = b0; let c = c0; let d = d0;
      for (let index = 0; index < 64; index++) {
        let f; let wordIndex;
        if (index < 16) { f = (b & c) | (~b & d); wordIndex = index; }
        else if (index < 32) { f = (d & b) | (~d & c); wordIndex = (5 * index + 1) % 16; }
        else if (index < 48) { f = b ^ c ^ d; wordIndex = (3 * index + 5) % 16; }
        else { f = c ^ (b | ~d); wordIndex = (7 * index) % 16; }
        const sum = (a + f + MD5_CONSTANTS[index] + words[wordIndex]) >>> 0;
        const shift = MD5_SHIFTS[index];
        const rotated = ((sum << shift) | (sum >>> (32 - shift))) >>> 0;
        const previousD = d; d = c; c = b; b = (b + rotated) >>> 0; a = previousD;
      }
      a0 = (a0 + a) >>> 0; b0 = (b0 + b) >>> 0; c0 = (c0 + c) >>> 0; d0 = (d0 + d) >>> 0;
      if (offset && offset % (64 * 4096) === 0 && Date.now() - sliceStarted >= 12) {
        await new Promise((resolve) => setTimeout(resolve, 0));
        sliceStarted = Date.now();
      }
    }
    return [a0, b0, c0, d0].map((word) => [0, 8, 16, 24]
      .map((shift) => ((word >>> shift) & 0xff).toString(16).padStart(2, '0')).join('')).join('');
  }

  async function sha256(data) {
    if (typeof mocks().digestSha256 === 'function') return normalizeHash(await mocks().digestSha256(data), 64);
    const subtle = mocks().cryptoSubtle || globalThis.crypto?.subtle;
    const source = bytesOf(data);
    const buffer = source?.buffer.slice(source.byteOffset, source.byteOffset + source.byteLength)
      || (data?.arrayBuffer ? await data.arrayBuffer() : null);
    if (!subtle?.digest || !buffer) return '';
    const digest = await subtle.digest('SHA-256', buffer);
    return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
  }

  function safeManifestUrl(value, hash) {
    try {
      const base = typeof location === 'undefined' ? 'http://unicell.local/' : location.href;
      const url = new URL(String(value || ''), base);
      const origin = new URL(base).origin;
      if (url.origin !== origin || !url.pathname.startsWith('/fonts/')) return '';
      if (!url.searchParams.getAll('v').includes(hash)) url.searchParams.set('v', hash);
      return `${url.pathname}${url.search}${url.hash}`;
    } catch (_) { return ''; }
  }

  function normalizeManifest(raw) {
    const source = raw && typeof raw === 'object' ? raw : {};
    const files = new Map();
    for (const input of Array.isArray(source.files) ? source.files : []) {
      const path = String(input.path || '').replace(/\\/g, '/');
      const strong = normalizeHash(input.sha256, 64);
      const url = safeManifestUrl(input.url || `/fonts/${path}`, strong);
      if (!path || !strong || !url) continue;
      const file = { path, url, sha256: strong, md5: normalizeHash(input.md5, 32),
        bytes: Number(input.bytes) || 0, mime: String(input.mime || 'font/ttf') };
      files.set(path.toLowerCase(), file);
      files.set(path.split('/').pop().toLowerCase(), file);
    }
    const faces = [];
    for (const input of Array.isArray(source.faces) ? source.faces : []) {
      const file = files.get(String(input.file || '').replace(/\\/g, '/').toLowerCase())
        || files.get(String(input.file || '').split(/[\\/]/).pop().toLowerCase());
      const strong = normalizeHash(input.sha256 || file?.sha256, 64);
      const url = safeManifestUrl(file?.url || input.url, strong);
      const family = String(input.family || '').trim();
      if (!file || !family || !strong || !url) continue;
      const names = [family, input.fullName, input.postscriptName]
        .concat(input.families || [], input.aliases || []).map(normalizeName).filter(Boolean);
      faces.push({ family, names: [...new Set(names)], file: file.path, url,
        sha256: strong, md5: normalizeHash(input.md5 || file.md5, 32), bytes: file.bytes,
        mime: file.mime, postscriptName: String(input.postscriptName || ''),
        faceIndex: Number.isFinite(+input.faceIndex) ? +input.faceIndex : null,
        style: /italic/i.test(input.style) ? 'italic' : /oblique/i.test(input.style) ? 'oblique' : 'normal',
        weight: String(input.weight || 400), stretch: 'normal' });
    }
    return { schemaVersion: Number(source.schemaVersion) || 0, files: [...new Set(files.values())], faces };
  }

  async function manifest() {
    if (!state.manifestPromise) {
      const fetchFn = mocks().fetch || fetch;
      state.manifestPromise = fetchFn(MANIFEST_URL, {
        method: 'GET', credentials: 'same-origin', cache: 'no-cache',
      }).then(async (response) => {
        if (!response.ok) throw new Error(`字体清单 HTTP ${response.status}`);
        const value = normalizeManifest(await response.json());
        if (value.schemaVersion !== 1) throw new Error('字体清单版本不支持');
        return value;
      }).catch((error) => {
        console.warn('[fonts] manifest unavailable:', error?.message || error);
        return normalizeManifest({ schemaVersion: 1, files: [], faces: [] });
      });
    }
    return state.manifestPromise;
  }

  function secureContextForLocalFonts() {
    if (typeof mocks().isHttps === 'boolean') return mocks().isHttps;
    return location.protocol === 'https:';
  }

  function authorizeLocalFonts() {
    const query = mocks().queryLocalFonts || globalThis.queryLocalFonts;
    if (!secureContextForLocalFonts() || typeof query !== 'function') return Promise.resolve(false);
    if (state.localQueryPromise) return state.localQueryPromise;
    if (navigator.userActivation && !navigator.userActivation.isActive) {
      state.localQueryState = 'needs-gesture';
      return Promise.resolve(false);
    }
    state.localQueryState = 'pending';
    let result;
    try { result = query.call(globalThis); } catch (error) { result = Promise.reject(error); }
    state.localQueryPromise = Promise.resolve(result).then((records) => {
      state.localFonts = Array.from(records || []);
      state.localQueryState = 'granted';
      return true;
    }).catch((error) => {
      state.localFonts = [];
      state.localQueryState = error?.name === 'SecurityError' ? 'needs-gesture' : 'denied';
      if (error?.name === 'SecurityError') state.localQueryPromise = null;
      return false;
    });
    return state.localQueryPromise;
  }

  const localNames = (record) => [record?.family, record?.fullName, record?.postscriptName]
    .map(normalizeName).filter(Boolean);
  const isCollection = (group) => group.mime === 'font/collection' || /\.(ttc|otc)(?:[?#]|$)/i.test(group.url);
  const installKey = (group, face, family) => [group.sha256, normalizeName(family),
    face.postscriptName, face.faceIndex, face.style, face.weight].join('|');

  async function installGroup(group, buffer, epoch) {
    if (epoch !== state.epoch) return 0;
    const FontFaceCtor = mocks().FontFace || globalThis.FontFace;
    const fontSet = mocks().fontSet || document.fonts;
    if (typeof FontFaceCtor !== 'function' || !fontSet?.add) return 0;
    const collection = isCollection(group);
    const BlobCtor = mocks().Blob || Blob;
    const URLApi = mocks().URL || URL;
    let objectUrl = '';
    let count = 0;
    try {
      if (collection) objectUrl = URLApi.createObjectURL(new BlobCtor([buffer], { type: 'font/collection' }));
      for (const face of group.faces) {
        for (const family of face.installFamilies) {
          const key = installKey(group, face, family);
          if (state.installed.has(key)) continue;
          if (collection && !face.postscriptName) continue;
          const source = collection
            ? `url("${objectUrl}#${encodeURIComponent(face.postscriptName)}") format("collection")`
            : buffer.slice(0);
          try {
            const loaded = await new FontFaceCtor(family, source, {
              style: face.style, weight: face.weight, stretch: face.stretch,
            }).load();
            if (epoch !== state.epoch) return count;
            fontSet.add(loaded); state.installed.add(key); count++;
          } catch (error) { console.warn(`[fonts] ${family} install failed:`, error?.message || error); }
        }
      }
    } finally {
      if (objectUrl) URLApi.revokeObjectURL(objectUrl);
    }
    return count;
  }

  function groupsFor(manifestValue, families) {
    const requested = families.map((family) => ({ family, key: normalizeName(family) }));
    const groups = new Map();
    for (const face of manifestValue.faces) {
      const installFamilies = requested.filter((item) => face.names.includes(item.key)).map((item) => item.family);
      if (!installFamilies.length) continue;
      let group = groups.get(face.sha256);
      if (!group) {
        group = { sha256: face.sha256, md5: face.md5, bytes: face.bytes, url: face.url,
          mime: face.mime, names: new Set(), faces: [], invalid: false };
        groups.set(face.sha256, group);
      }
      if (group.md5 && face.md5 && group.md5 !== face.md5) group.invalid = true;
      face.names.forEach((name) => group.names.add(name));
      group.faces.push({ ...face, installFamilies });
    }
    return groups;
  }

  async function prepare(families) {
    const epoch = state.epoch;
    const manifestValue = await manifest();
    if (epoch !== state.epoch) return { stale: true };
    const groups = groupsFor(manifestValue, families);
    const unresolved = new Set(groups.keys());
    let localInstalled = 0;
    if (state.localQueryPromise) await state.localQueryPromise;
    const candidates = state.localFonts.filter((record) => {
      const names = localNames(record);
      return [...groups.values()].some((group) => names.some((name) => group.names.has(name)));
    });
    for (const record of candidates) {
      if (epoch !== state.epoch || typeof record?.blob !== 'function') break;
      try {
        const buffer = await (await record.blob()).arrayBuffer();
        const strong = await sha256(buffer);
        const group = groups.get(strong);
        if (!group || group.invalid || (group.md5 && await md5(buffer) !== group.md5)) continue;
        localInstalled += await installGroup(group, buffer, epoch);
        unresolved.delete(strong);
      } catch (_) { /* exact local candidate failed; verified network fallback remains */ }
    }
    let networkInstalled = 0;
    const fetchFn = mocks().fetch || fetch;
    for (const hash of unresolved) {
      if (epoch !== state.epoch) break;
      const group = groups.get(hash);
      try {
        if (group.invalid) throw new Error('manifest hash conflict');
        const response = await fetchFn(group.url, { method: 'GET', credentials: 'same-origin', cache: 'force-cache' });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const buffer = await response.arrayBuffer();
        if (group.bytes && buffer.byteLength !== group.bytes) throw new Error('size mismatch');
        if (await sha256(buffer) !== group.sha256) throw new Error('SHA-256 mismatch');
        if (group.md5 && await md5(buffer) !== group.md5) throw new Error('MD5 mismatch');
        networkInstalled += await installGroup(group, buffer, epoch);
      } catch (error) { console.warn('[fonts] verified fallback failed:', error?.message || error); }
    }
    const installed = localInstalled + networkInstalled;
    if (installed && epoch === state.epoch) {
      dispatchEvent(new CustomEvent('unicell-fonts-ready', { detail: { installed, localInstalled, networkInstalled } }));
    }
    return { requested: families.length, matched: [...groups.values()].reduce((sum, group) => sum + group.faces.length, 0),
      installed, localInstalled, networkInstalled };
  }

  function schedulePrepare() {
    if (state.scheduled) return;
    state.scheduled = true;
    const run = () => {
      state.scheduled = false;
      prepare([...state.requested.values()]).catch((error) => console.warn('[fonts] preparation failed:', error));
    };
    if (typeof requestIdleCallback === 'function') requestIdleCallback(run, { timeout: 800 });
    else setTimeout(run, 120);
  }

  function requestFamilies(values) {
    for (const raw of Array.isArray(values) ? values : []) {
      const family = String(raw || '').trim().replace(/^["']|["']$/g, '');
      const key = normalizeName(family);
      if (!key || ['serif', 'sans-serif', 'monospace', 'cursive', 'fantasy', 'system-ui'].includes(key)) continue;
      state.requested.set(key, family);
    }
    if (state.requested.size) schedulePrepare();
  }

  function beginWorkbook() {
    state.requested.clear();
    state.epoch++;
    state.scheduled = false;
  }

  for (const id of ['btn-file-open', 'sel-fontfamily']) {
    document.getElementById(id)?.addEventListener('click', () => { void authorizeLocalFonts(); }, { capture: true });
  }

  window.UniCellFonts = Object.freeze({ requestFamilies, authorizeLocalFonts, beginWorkbook });
  window.__UniCellFontRuntimeTest = Object.freeze({ normalizeManifest, normalizeName, md5, sha256,
    state: () => ({ localPermission: state.localQueryState, localRecordCount: state.localFonts.length,
      installedFaceCount: state.installed.size, requestedFamilyCount: state.requested.size }) });
})();
