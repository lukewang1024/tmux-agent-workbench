export function monitorGateway(report) {
  let checking = false;

  async function check() {
    if (checking) return;
    checking = true;
    let session = null;
    try {
      const response = await fetch('/api/session', { cache: 'no-store', signal: AbortSignal.timeout(3000) });
      if (response.ok) {
        const data = await response.json();
        if (typeof data?.authenticated === 'boolean') session = data;
      }
    } catch {
      // The Web app can outlive its SSH tunnel or gateway.
    } finally { checking = false; }
    report(session);
  }

  function checkVisible() { if (!document.hidden) check(); }
  window.addEventListener('online', check);
  window.addEventListener('offline', () => { report(null); check(); });
  document.addEventListener('visibilitychange', checkVisible);
  setInterval(checkVisible, 3000);
  if (!navigator.onLine) report(null);
  check();
  return check;
}
