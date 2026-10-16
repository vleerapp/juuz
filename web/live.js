(() => {
  const offset = -new Date().getTimezoneOffset();
  const known = document.cookie.match(/(?:^|; )juuz_tz=(-?\d+)/);
  const stale = !known || Number(known[1]) !== offset;
  if (stale) {
    document.cookie = `juuz_tz=${offset}; path=/; max-age=31536000; samesite=lax`;
  }
  if (!document.querySelector("[data-live]")) return;
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/events`;
  let busy = false;
  let queued = false;
  let delay = 500;

  const refresh = async () => {
    if (busy) {
      queued = true;
      return;
    }
    busy = true;
    try {
      const res = await fetch(location.pathname + location.search, {
        cache: "no-store",
        credentials: "same-origin",
        headers: { "x-live": "1" },
      });
      if (!res.ok) return;
      const next = new DOMParser().parseFromString(await res.text(), "text/html");
      for (const el of document.querySelectorAll("[data-live]")) {
        const fresh = next.querySelector(`[data-live="${el.dataset.live}"]`);
        if (fresh && fresh.innerHTML !== el.innerHTML) el.innerHTML = fresh.innerHTML;
      }
    } finally {
      busy = false;
      if (queued) {
        queued = false;
        refresh();
      }
    }
  };

  let socket = null;
  let timer = null;

  const connect = () => {
    clearTimeout(timer);
    if (socket && socket.readyState <= WebSocket.OPEN) return;
    socket = new WebSocket(url);
    const mine = socket;
    mine.addEventListener("open", () => {
      if (delay > 500) refresh();
      delay = 500;
    });
    mine.addEventListener("message", refresh);
    mine.addEventListener("close", () => {
      if (socket === mine) socket = null;
      timer = setTimeout(connect, delay);
      delay = Math.min(delay * 2, 15000);
    });
  };

  setInterval(() => {
    if (socket && socket.readyState === WebSocket.OPEN) socket.send("ping");
  }, 20000);

  const resume = () => {
    if (document.visibilityState !== "visible") return;
    if (socket && socket.readyState === WebSocket.OPEN) {
      refresh();
      return;
    }
    if (socket) socket.close();
    socket = null;
    delay = 500;
    connect();
    refresh();
  };

  document.addEventListener("visibilitychange", resume);
  window.addEventListener("pageshow", (e) => e.persisted && resume());
  window.addEventListener("online", resume);

  connect();
  if (stale) refresh();
})();
