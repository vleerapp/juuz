(() => {
  try {
    const root = document.documentElement;
    const saved = JSON.parse(localStorage.getItem("juuz.grid"));
    if (saved && saved.vw === root.clientWidth) {
      root.style.setProperty("--cols", saved.cols);
      root.style.setProperty("--n", saved.n);
      root.style.setProperty("--w", saved.w);
      root.setAttribute("data-n", saved.n);
      root.setAttribute("data-grid", "");
    }
  } catch {}

  const seat = () => {
    const ua = navigator.userAgent;
    const os = /android/i.test(ua)
      ? "Android"
      : /iphone|ipad|ipod/i.test(ua)
        ? "iOS"
        : /cros/i.test(ua)
          ? "ChromeOS"
          : /mac os/i.test(ua)
            ? "macOS"
            : /windows/i.test(ua)
              ? "Windows"
              : /linux|x11/i.test(ua)
                ? "Linux"
                : "Unknown";
    const kind =
      /ipad|tablet/i.test(ua) || (/android/i.test(ua) && !/mobile/i.test(ua))
        ? "tablet"
        : /mobi|iphone|ipod/i.test(ua)
          ? "phone"
          : "desktop";
    const name = /firefox|fxios/i.test(ua)
      ? "firefox"
      : /edg/i.test(ua)
        ? "edge"
        : /opr|opera/i.test(ua)
          ? "opera"
          : /chrome|chromium|crios/i.test(ua)
            ? "chrome"
            : /safari/i.test(ua)
              ? "safari"
              : "browser";
    const shown = { firefox: "Firefox", edge: "Edge", opera: "Opera", chrome: "Chrome", safari: "Safari" }[name] || "Browser";
    const fresh = () => `${name}-${Math.floor(Math.random() * 0xffffff).toString(16).padStart(6, "0")}`;
    let id;
    try {
      id = localStorage.getItem("juuz.browser");
      if (!id || !/^[a-z0-9-]{1,32}$/.test(id)) {
        id = fresh();
        localStorage.setItem("juuz.browser", id);
      }
    } catch {
      id = id || fresh();
    }
    return { id, udn: `browser:${id}`, info: `${os}, ${shown}`, kind };
  };
  window.juuzBrowser = seat();

  const startTerm = () => {
    const root = document.documentElement;
    const TEXT = "input:not([type=checkbox]):not([type=radio]):not([type=hidden]), textarea";
    const FOCUSABLE =
      "a[href], button:not(:disabled), input:not(:disabled):not([type=hidden]), summary, [data-copy], [data-rename-open], .slider";
    const MAX_COLS = 2000;
    const gauge = (css) => {
      let el = null;
      return () => {
        if (!el || !el.isConnected) {
          el = document.createElement("div");
          el.style.cssText = `position:fixed;left:0;top:0;visibility:hidden;${css}`;
          document.body.appendChild(el);
        }
        return el;
      };
    };
    const cellProbe = gauge("width:var(--ch);height:1px");
    const rowProbe = gauge("width:1px;height:1rlh");
    const rowHeight = () => rowProbe().getBoundingClientRect().height || 24;

    let cw = 8;
    const measure = () => {
      cw = cellProbe().getBoundingClientRect().width || cw;
      const avail = Math.max(24, Math.min(MAX_COLS, Math.floor(document.body.clientWidth / cw) - 2));
      const n = avail >= 130 ? 3 : avail >= 84 ? 2 : 1;
      const w = Math.floor((avail - 4 * (n - 1)) / n);
      root.style.setProperty("--n", n);
      root.style.setProperty("--w", w);
      root.style.setProperty("--cols", n * w + 4 * (n - 1));
      root.setAttribute("data-n", n);
      root.setAttribute("data-grid", "");
      try {
        localStorage.setItem(
          "juuz.grid",
          JSON.stringify({ vw: root.clientWidth, cols: n * w + 4 * (n - 1), n, w }),
        );
      } catch {}
      place();
    };

    const focusables = () => {
      const open = document.querySelector(".select[data-open]");
      const popup = document.querySelector(".popup:not([hidden])");
      const scope = open && open.contains(document.activeElement) ? open : popup || document;
      return [...scope.querySelectorAll(FOCUSABLE)].filter((el) => {
        const folded = el.closest("details:not([open])");
        if (folded && el !== folded.firstElementChild) return false;
        const box = el.getBoundingClientRect();
        return box.width > 0 || box.height > 0;
      });
    };

    const copy = async (el) => {
      const text = el.textContent.trim();
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        const range = document.createRange();
        range.selectNodeContents(el);
        const selection = getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        document.execCommand("copy");
        selection.removeAllRanges();
      }
      el.dataset.copied = "";
      setTimeout(() => delete el.dataset.copied, 1500);
    };

    document.addEventListener("click", (e) => {
      const el = e.target.closest?.("[data-copy]");
      if (el) copy(el);
    });

    const stepper = (el) => !!el && el.matches("input[data-stepper]");
    const editing = (el) => stepper(el) && el.hasAttribute("data-editing");
    const typing = (el) => !!el && el.matches(TEXT) && (!stepper(el) || editing(el));

    const adjust = (el, direction, big) => {
      const step = (Number(el.step) || 1) * (big ? 10 : 1);
      const min = el.min === "" ? -Infinity : Number(el.min);
      const max = el.max === "" ? Infinity : Number(el.max);
      const next = Math.min(max, Math.max(min, (Number(el.value) || 0) + direction * step));
      el.value = String(next);
      el.dispatchEvent(new Event("input", { bubbles: true }));
    };

    let pointerAt = -1e9;
    document.addEventListener("pointerdown", () => (pointerAt = performance.now()), true);
    document.addEventListener("focusin", (e) => {
      const el = e.target;
      if (stepper(el) && performance.now() - pointerAt < 500) {
        el.setAttribute("data-editing", "");
        el.select();
      }
    });

    document.addEventListener("focusout", (e) => {
      if (stepper(e.target)) e.target.removeAttribute("data-editing");
    });

    document.addEventListener("beforeinput", (e) => {
      if (!stepper(e.target) || !e.inputType.startsWith("insert")) return;
      const text = e.data ?? e.dataTransfer?.getData("text") ?? "";
      if (!/^-?\d*$/.test(text)) e.preventDefault();
    });

    const columnOf = (el) => {
      const holder = el.closest("main > section, main > .live > section, main > nav");
      if (!holder) return 0;
      const n = Number(root.dataset.n) || 1;
      const base = document.querySelector(".screen").getBoundingClientRect().left;
      const pitch = (Number(root.style.getPropertyValue("--w")) + 4) * cw;
      return Math.min(n - 1, Math.max(0, Math.round((holder.getBoundingClientRect().left - base) / pitch)));
    };

    let anchor = null;
    let trail = [];
    let goalX = null;
    let expected = null;
    let last = null;

    document.addEventListener("focusin", (e) => {
      anchor = null;
      if (e.target !== expected && e.target !== last) {
        trail = [];
        goalX = null;
      }
      expected = null;
      last = e.target;
    });

    const focusTo = (target) => {
      expected = target;
      target.focus();
      const popup = target.closest(".popup");
      if (!popup) return;
      const list = focusables();
      if (list[0] === target) popup.scrollTop = 0;
      else if (list[list.length - 1] === target) popup.scrollTop = popup.scrollHeight;
    };

    const sameLine = (a, b, ea, eb) => {
      const rowA = ea?.closest(".row");
      if (rowA && rowA === eb?.closest(".row")) return true;
      const ma = a.top + a.height / 2;
      const mb = b.top + b.height / 2;
      const overlap = Math.min(a.bottom, b.bottom) - Math.max(a.top, b.top);
      return (
        Math.abs(ma - mb) < rowHeight() / 2 ||
        overlap > 0.25 * Math.min(a.height, b.height) ||
        (mb > a.top && mb < a.bottom) ||
        (ma > b.top && ma < b.bottom)
      );
    };

    const go = (dir) => {
      const list = focusables();
      if (!list.includes(document.activeElement)) {
        const contained = !!document.querySelector(".popup:not([hidden])");
        const start =
          (anchor && list.includes(anchor) ? anchor : null) ||
          (contained ? null : document.querySelector(".tabs a.current")) ||
          list[0];
        if (!start) return;
        start.focus();
      }
      const el = document.activeElement;
      const vertical = dir === "up" || dir === "down";
      if (vertical) {
        const back = trail[trail.length - 1];
        if (back && back.to === el && back.dir !== dir && list.includes(back.from)) {
          trail.pop();
          focusTo(back.from);
          return;
        }
      } else {
        trail = [];
        goalX = null;
      }
      const half = rowHeight() / 2;
      const from = el.getBoundingClientRect();
      const mid = from.top + from.height / 2;
      const x = goalX ?? from.left;
      const col = columnOf(el);
      let best = null;
      let bestScore = Infinity;
      const consider = (cand, score) => {
        if (score < bestScore) {
          best = cand;
          bestScore = score;
        }
      };
      const ahead = [];
      for (const cand of list) {
        if (cand === el) continue;
        const to = cand.getBoundingClientRect();
        const dy = to.top - from.top;
        const dx = to.left - from.left;
        if (vertical) {
          const beyond = dir === "down" ? dy > half : dy < -half;
          if (beyond && columnOf(cand) === col && !sameLine(from, to, el, cand)) ahead.push({ cand, to, dy: Math.abs(dy) });
        } else if (sameLine(from, to, el, cand)) {
          const gap = dir === "right" ? dx : -dx;
          if (gap > 1) consider(cand, gap * 1e3 + Math.abs(to.top + to.height / 2 - mid));
        }
      }
      if (ahead.length) {
        ahead.sort((a, b) => a.dy - b.dy);
        const near = ahead[0];
        for (const { cand, to } of ahead) {
          if (!sameLine(near.to, to, near.cand, cand)) continue;
          const miss = x >= to.left && x <= to.right ? 0 : Math.min(Math.abs(to.left - x), Math.abs(to.right - x));
          consider(cand, miss * 1e4 + Math.abs(to.left - x));
        }
      }
      if (!best && !vertical) {
        const beyond = list.filter((cand) => (dir === "right" ? columnOf(cand) > col : columnOf(cand) < col));
        if (beyond.length) {
          const target = dir === "right"
            ? Math.min(...beyond.map(columnOf))
            : Math.max(...beyond.map(columnOf));
          for (const cand of beyond) {
            if (columnOf(cand) !== target) continue;
            const to = cand.getBoundingClientRect();
            consider(cand, Math.abs(to.top + to.height / 2 - mid) * 1e3 + (dir === "right" ? to.left : -to.left));
          }
        }
      }
      if (!best) return;
      if (vertical) {
        goalX = x;
        trail.push({ from: el, to: best, dir });
      }
      focusTo(best);
    };

    const jump = (step) => {
      const links = [...document.querySelectorAll(".tabs a")];
      const at = links.findIndex((a) => a.classList.contains("current"));
      if (at < 0) return;
      links[(at + step + links.length) % links.length].click();
    };

    const MOVES = { ArrowDown: "down", ArrowUp: "up", ArrowRight: "right", ArrowLeft: "left" };
    const VIM = { j: "down", k: "up", l: "right", h: "left" };

    document.addEventListener("keydown", (e) => {
      if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
      const el = document.activeElement;
      const text = typing(el);
      if (stepper(el)) {
        if (e.key === "Enter") {
          e.preventDefault();
          if (editing(el)) el.removeAttribute("data-editing");
          else {
            el.setAttribute("data-editing", "");
            el.select();
          }
          requestAnimationFrame(place);
          return;
        }
        if (e.key === "Escape" && editing(el)) {
          e.preventDefault();
          e.stopImmediatePropagation();
          el.removeAttribute("data-editing");
          requestAnimationFrame(place);
          return;
        }
        if (editing(el) && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
          e.preventDefault();
          adjust(el, e.key === "ArrowUp" ? 1 : -1, e.shiftKey);
          return;
        }
        if (!editing(el) && /^[0-9-]$/.test(e.key)) {
          el.setAttribute("data-editing", "");
          el.select();
          requestAnimationFrame(place);
          return;
        }
      }
      if (e.key === "Escape") {
        if (el && el !== document.body) {
          e.preventDefault();
          el.blur();
        }
        return;
      }
      if ((e.key === "PageDown" || e.key === "PageUp") && !e.shiftKey) {
        e.preventDefault();
        const rh = rowHeight();
        const page = Math.max(rh, window.innerHeight - 3 * rh);
        window.scrollBy({ top: e.key === "PageDown" ? page : -page });
        return;
      }
      const move = MOVES[e.key];
      if (move && !e.shiftKey) {
        if (text && move === "left" && (el.selectionStart !== 0 || el.selectionEnd !== 0)) return;
        if (text && move === "right" && el.selectionStart !== el.value.length) return;
        e.preventDefault();
        go(move);
        return;
      }
      if (text) return;
      if ((e.key === "Enter" || e.key === "c") && el && el.matches("[data-copy]")) {
        e.preventDefault();
        copy(el);
        return;
      }
      if (e.key === "Enter" && el && el.matches("[data-rename-open]")) {
        e.preventDefault();
        el.click();
        return;
      }
      if (e.key === "Enter" && el && (el.type === "checkbox" || el.type === "radio")) {
        e.preventDefault();
        el.click();
        return;
      }
      if (VIM[e.key]) {
        e.preventDefault();
        go(VIM[e.key]);
        return;
      }
      if ((e.key === "Home" || e.key === "End") && el && el !== document.body) {
        e.preventDefault();
        const list = focusables();
        const target = list[e.key === "Home" ? 0 : list.length - 1];
        if (target) focusTo(target);
        return;
      }
      if (e.key === "[" || e.key === "]") {
        e.preventDefault();
        jump(e.key === "]" ? 1 : -1);
        return;
      }
      if (e.key === "b") {
        const toggle = document.querySelector('.player:not([hidden]) [data-act="toggle"]');
        if (toggle) {
          e.preventDefault();
          focusTo(toggle);
        }
        return;
      }
      if (/^[0-9]$/.test(e.key)) {
        const link = document.querySelector(`.tabs a[data-key="${e.key}"]`);
        if (link) {
          e.preventDefault();
          link.click();
        }
      }
    });

    const native = CSS.supports("caret-shape", "block");
    let caret = null;
    function place() {
      if (native) return;
      const el = document.activeElement;
      const screen = document.querySelector(".screen");
      const host = el?.closest?.(".popup") || screen;
      if (!caret) {
        caret = document.createElement("div");
        caret.className = "caret";
        caret.hidden = true;
      }
      if (caret.parentElement !== host) host.appendChild(caret);
      root.setAttribute("data-caret", "");
      if (!typing(el)) {
        caret.hidden = true;
        return;
      }
      const base = host.getBoundingClientRect();
      const box = el.getBoundingClientRect();
      const pad = parseFloat(getComputedStyle(el).paddingLeft) || 0;
      const index = el.selectionEnd ?? el.value.length;
      const x = box.left + pad + index * cw - el.scrollLeft;
      caret.hidden = x + cw > box.right + 1;
      const inPopup = host !== screen;
      caret.style.mixBlendMode = inPopup ? "normal" : "";
      caret.toggleAttribute("data-inverse", inPopup && !!el.closest("[data-picked]"));
      caret.style.left = `${x - base.left + host.scrollLeft}px`;
      caret.style.top = `${box.top - base.top + host.scrollTop + (box.height - rowHeight()) / 2 + 1}px`;
      caret.style.animation = "none";
      void caret.offsetWidth;
      caret.style.animation = "";
    }

    if (!native) {
      for (const type of ["focusin", "focusout", "input", "keydown", "keyup", "click"]) {
        document.addEventListener(type, () => requestAnimationFrame(place));
      }
      document.addEventListener("selectionchange", place);
      window.addEventListener("scroll", place, true);
    }

    const KEY = "juuz.focus";
    const hiddenValues = (form) =>
      form ? [...form.querySelectorAll("input[type=hidden]")].map((i) => i.value).join("|") : "";
    const sortKey = (el) => el.getAttribute("aria-label") || el.textContent.trim();

    const describe = (el) =>
      el.matches("a.sort")
        ? { sort: sortKey(el) }
        : el.matches("a[href]")
        ? { href: el.getAttribute("href"), text: el.textContent.trim() }
        : {
            text: el.textContent.trim(),
            action: el.form?.getAttribute("action") ?? "",
            hidden: hiddenValues(el.form),
          };

    let keyAt = -1e9;
    document.addEventListener("keydown", () => (keyAt = performance.now()), true);

    document.addEventListener("click", (e) => {
      const target = e.target.closest?.("a[href], button");
      if (!target || !e.isTrusted) return;
      const pointer = performance.now() - keyAt > 400 && e.detail !== 0;
      try {
        sessionStorage.setItem(KEY, JSON.stringify({ ...describe(target), at: Date.now(), pointer }));
      } catch {}
    });

    const restore = () => {
      let saved = null;
      try {
        saved = JSON.parse(sessionStorage.getItem(KEY));
        sessionStorage.removeItem(KEY);
      } catch {}
      if (!saved || Date.now() - saved.at > 4000 || document.activeElement !== document.body) return;
      const match = focusables().find((el) => {
        if (saved.sort !== undefined) return el.matches("a.sort") && sortKey(el) === saved.sort;
        if (saved.href !== undefined) {
          return (
            el.matches("a[href]") &&
            el.getAttribute("href") === saved.href &&
            el.textContent.trim() === saved.text
          );
        }
        return (
          el.matches("button") &&
          el.textContent.trim() === saved.text &&
          (el.form?.getAttribute("action") ?? "") === saved.action &&
          hiddenValues(el.form) === saved.hidden
        );
      });
      if (!match) return;
      if (saved.pointer) anchor = match;
      else focusTo(match);
    };

    const settle = () => {
      const reveal = document.querySelector("[data-copy]");
      if (reveal) focusTo(reveal);
      else restore();
    };

    const arrived = () => {
      trail = [];
      goalX = null;
      measure();
      settle();
      window.dispatchEvent(new Event("juuz:navigated"));
    };

    for (const method of ["pushState", "replaceState"]) {
      const original = history[method];
      history[method] = function (...args) {
        const result = original.apply(this, args);
        queueMicrotask(arrived);
        return result;
      };
    }
    window.addEventListener("popstate", () => queueMicrotask(arrived));

    measure();
    if (document.fonts) document.fonts.ready.then(measure);
    new ResizeObserver(measure).observe(root);
    document.fonts?.addEventListener("loadingdone", measure);
    settle();
  };

  const startLive = () => {
    const offset = -new Date().getTimezoneOffset();
    const known = document.cookie.match(/(?:^|; )juuz_tz=(-?\d+)/);
    const stale = !known || Number(known[1]) !== offset;
    if (stale) {
      document.cookie = `juuz_tz=${offset}; path=/; max-age=31536000; samesite=lax`;
    }
    const paged = () => !!document.querySelector("[data-live]");
    const wants = (mask) => [...document.querySelectorAll("[data-live]")].some((el) => (el.dataset.topic ? mask !== 255 && Number(el.dataset.topic) & mask : true));
    const live = () => paged() || document.querySelector(".screen")?.dataset.jukebox === "1";
    const { id, info, kind } = window.juuzBrowser;
    const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/events?b=${encodeURIComponent(id)}&d=${encodeURIComponent(info)}&k=${kind}`;
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
          if (!fresh || fresh.innerHTML === el.innerHTML) continue;
          const active = document.activeElement;
          const at = active && active !== el && el.contains(active) ? [...el.querySelectorAll("*")].indexOf(active) : -1;
          const label = at >= 0 ? active.tagName + active.textContent.trim() : "";
          el.innerHTML = fresh.innerHTML;
          const again = at >= 0 ? el.querySelectorAll("*")[at] : null;
          if (again && again.tagName + again.textContent.trim() === label) again.focus({ preventScroll: true });
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
    const samples = [];
    let lastPcm = 0;
    const syncWanted = () => !!window.juuzSyncWanted?.() || Date.now() - lastPcm < 20000;
    const sendSync = () => {
      if (syncWanted() && socket && socket.readyState === WebSocket.OPEN) socket.send(`sync:${performance.now()}`);
    };
    const burst = () => {
      for (let i = 0; i < 15; i++) setTimeout(sendSync, i * 100);
    };
    window.addEventListener("juuz:syncwanted", burst);

    const connect = () => {
      clearTimeout(timer);
      if (!live()) return;
      if (socket && socket.readyState <= WebSocket.OPEN) return;
      socket = new WebSocket(url);
      const mine = socket;
      mine.binaryType = "arraybuffer";
      mine.addEventListener("open", () => {
        if (delay > 500 && paged()) refresh();
        delay = 500;
        samples.length = 0;
        burst();
      });
      mine.addEventListener("message", (e) => {
        if (typeof e.data !== "string") {
          if (Date.now() - lastPcm > 20000) {
            lastPcm = Date.now();
            burst();
          }
          lastPcm = Date.now();
          window.dispatchEvent(new CustomEvent("juuz:pcm", { detail: e.data }));
          return;
        }
        if (e.data === "pulse" || e.data.startsWith("pulse:")) {
          const mask = e.data === "pulse" ? 255 : Number(e.data.slice(6)) || 255;
          if (mask !== 255 && mask & 1) window.dispatchEvent(new CustomEvent("juuz:pulse", { detail: mask }));
          if (wants(mask)) refresh();
          return;
        }
        try {
          const msg = JSON.parse(e.data);
          if (msg.t === "sync") {
            const rtt = performance.now() - Number(msg.c);
            if (Number.isFinite(rtt) && rtt >= 0) {
              samples.push({ rtt, wall: msg.s + rtt / 2 - Date.now(), perf: msg.s + rtt / 2 - performance.now() });
              if (samples.length > 20) samples.shift();
            }
            return;
          }
          window.dispatchEvent(new CustomEvent("juuz:push", { detail: msg }));
        } catch {}
      });
      mine.addEventListener("close", () => {
        if (socket === mine) socket = null;
        if (!live()) return;
        timer = setTimeout(connect, delay);
        delay = Math.min(delay * 2, 15000);
      });
    };

    setInterval(sendSync, 5000);

    const resume = () => {
      if (document.visibilityState !== "visible" || !live()) return;
      if (socket && socket.readyState === WebSocket.OPEN) {
        if (paged()) refresh();
        return;
      }
      if (socket) socket.close();
      socket = null;
      delay = 500;
      connect();
      if (paged()) refresh();
    };

    document.addEventListener("visibilitychange", resume);
    window.addEventListener("pageshow", (e) => e.persisted && resume());
    window.addEventListener("online", resume);

    window.addEventListener("juuz:navigated", () => {
      if (live()) {
        connect();
        if (paged()) refresh();
      } else if (socket) {
        socket.close();
      }
    });

    const settled = (key) => {
      if (!samples.length) return null;
      const best = [...samples].sort((a, b) => a.rtt - b.rtt).slice(0, 5);
      const values = best.map((s) => s[key]).sort((a, b) => a - b);
      return values[Math.floor(values.length / 2)];
    };
    window.juuzLive = {
      open: () => !!socket && socket.readyState === WebSocket.OPEN,
      offset: () => settled("wall"),
      perfOffset: () => settled("perf"),
      rtt: () => (samples.length ? Math.min(...samples.map((s) => s.rtt)) : null),
    };
    connect();
    if (stale && paged()) refresh();
  };

  const startAuto = () => {
    const timers = new WeakMap();
    const sent = new WeakMap();
    const body = (form) => new URLSearchParams(new FormData(form)).toString();
    let chain = Promise.resolve();
    const send = (form) => {
      clearTimeout(timers.get(form));
      const payload = body(form);
      if (sent.get(form) === payload) return;
      sent.set(form, payload);
      chain = chain.then(async () => {
        try {
          const res = await fetch(form.action, {
            method: "POST",
            credentials: "same-origin",
            headers: { "content-type": "application/x-www-form-urlencoded" },
            body: payload,
          });
          const doc = new DOMParser().parseFromString(await res.text(), "text/html");
          const notice = doc.querySelector(".notice");
          document.querySelectorAll(".notice").forEach((n) => n.remove());
          if (notice) document.querySelector("main")?.before(document.importNode(notice, true));
          if (res.ok) {
            form.querySelectorAll("input[type=password]").forEach((i) => (i.value = ""));
            sent.set(form, body(form));
          } else sent.delete(form);
        } catch {
          sent.delete(form);
        }
      });
    };
    const auto = (e) => e.target.closest?.("form[data-auto]");
    document.addEventListener("focusin", (e) => {
      const form = auto(e);
      if (form && !sent.has(form)) sent.set(form, body(form));
    });
    document.addEventListener("change", (e) => {
      const form = auto(e);
      if (form) send(form);
    });
    document.addEventListener("input", (e) => {
      const form = auto(e);
      if (!form || !e.target.matches("input[type=text], input:not([type])")) return;
      clearTimeout(timers.get(form));
      timers.set(form, setTimeout(() => send(form), 600));
    });
    document.addEventListener("submit", (e) => {
      const form = e.target;
      if (!form.matches?.("form[data-auto]")) return;
      e.preventDefault();
      send(form);
    });
  };

  const start = () => {
    startTerm();
    startLive();
    startAuto();
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
