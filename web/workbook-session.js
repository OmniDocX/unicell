/* Establish one local workbook session before parallel editor requests. */
(() => {
  const nativeFetch = window.fetch.bind(window);
  let bootstrap;
  const ensureSession = () => bootstrap ||= nativeFetch('/api/session', {
    credentials: 'same-origin', cache: 'no-store', signal: AbortSignal.timeout(20000)
  }).then(response => { if (!response.ok) throw new Error('无法创建本机工作簿会话'); return response.json(); })
    .catch(error => { bootstrap = null; throw error; });
  window.UniCellSessionReady = ensureSession();
  window.fetch = async (input, init) => {
    const url = new URL(input instanceof Request ? input.url : input, location.href);
    if (url.origin === location.origin && url.pathname.startsWith('/api/') && url.pathname !== '/api/session') await ensureSession();
    return nativeFetch(input, init);
  };
})();
