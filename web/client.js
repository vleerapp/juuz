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
      const scope = open && open.contains(document.activeElement) ? open : document;
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

    const typing = (el) => !!el && el.matches(TEXT);

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
        const start =
          (anchor && list.includes(anchor) ? anchor : null) ||
          document.querySelector(".tabs a.current") ||
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
        list[e.key === "Home" ? 0 : list.length - 1]?.focus();
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
      if (!caret || !caret.isConnected) {
        caret = document.createElement("div");
        caret.className = "caret";
        caret.hidden = true;
        screen.appendChild(caret);
      }
      root.setAttribute("data-caret", "");
      if (!typing(el)) {
        caret.hidden = true;
        return;
      }
      const base = screen.getBoundingClientRect();
      const box = el.getBoundingClientRect();
      const pad = parseFloat(getComputedStyle(el).paddingLeft) || 0;
      const index = el.selectionEnd ?? el.value.length;
      const x = box.left + pad + index * cw - el.scrollLeft;
      caret.hidden = x + cw > box.right + 1;
      caret.style.left = `${x - base.left}px`;
      caret.style.top = `${box.top - base.top + (box.height - rowHeight()) / 2 + 1}px`;
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
    const describe = (el) =>
      el.matches("a[href]")
        ? { href: el.getAttribute("href"), text: el.textContent.trim() }
        : {
            text: el.textContent.trim(),
            action: el.form?.getAttribute("action") ?? "",
            hidden: hiddenValues(el.form),
          };

    document.addEventListener("click", (e) => {
      const target = e.target.closest?.("a[href], button");
      if (!target || !e.isTrusted) return;
      try {
        sessionStorage.setItem(KEY, JSON.stringify({ ...describe(target), at: Date.now(), pointer: e.detail !== 0 }));
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
    const live = () => !!document.querySelector("[data-live]");
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
      if (!live()) return;
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
        if (!live()) return;
        timer = setTimeout(connect, delay);
        delay = Math.min(delay * 2, 15000);
      });
    };

    setInterval(() => {
      if (socket && socket.readyState === WebSocket.OPEN) socket.send("ping");
    }, 20000);

    const resume = () => {
      if (document.visibilityState !== "visible" || !live()) return;
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

    window.addEventListener("juuz:navigated", () => {
      if (live()) {
        connect();
        refresh();
      } else if (socket) {
        socket.close();
      }
    });

    connect();
    if (stale && live()) refresh();
  };

  const start = () => {
    startTerm();
    startLive();
  };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
