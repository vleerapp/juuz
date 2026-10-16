(() => {
  const root = document.documentElement;
  const TEXT = "input:not([type=checkbox]):not([type=radio]):not([type=hidden]), textarea";
  const FOCUSABLE =
    "a[href], button:not(:disabled), input:not(:disabled):not([type=hidden]), summary, [data-copy]";
  const MAX_COLS = 160;
  const probe = document.createElement("div");
  probe.style.cssText = "position:fixed;left:0;top:0;width:var(--ch);height:1px;visibility:hidden";
  document.body.appendChild(probe);
  const row = document.createElement("div");
  row.style.cssText = "position:fixed;left:0;top:0;height:1rlh;width:1px;visibility:hidden";
  document.body.appendChild(row);
  const rowHeight = () => row.getBoundingClientRect().height || 24;

  let cw = 8;
  const measure = () => {
    cw = probe.getBoundingClientRect().width || cw;
    const avail = Math.max(24, Math.min(MAX_COLS, Math.floor(document.body.clientWidth / cw) - 2));
    const n = avail >= 130 ? 3 : avail >= 84 ? 2 : 1;
    const w = Math.floor((avail - 4 * (n - 1)) / n);
    root.style.setProperty("--n", n);
    root.style.setProperty("--w", w);
    root.style.setProperty("--cols", n * w + 4 * (n - 1));
    root.setAttribute("data-n", n);
    root.setAttribute("data-grid", "");
    place();
  };

  const focusables = () =>
    [...document.querySelectorAll(FOCUSABLE)].filter((el) => {
      const folded = el.closest("details:not([open])");
      if (folded && el !== folded.firstElementChild) return false;
      const box = el.getBoundingClientRect();
      return box.width > 0 || box.height > 0;
    });

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

  const typing = (el) => !!el && el.matches(TEXT);

  const columnOf = (el) => {
    const holder = el.closest("main > section, main > .live > section, main > nav");
    if (!holder) return 0;
    const n = Number(root.dataset.n) || 1;
    const base = document.querySelector(".screen").getBoundingClientRect().left;
    const pitch = (Number(root.style.getPropertyValue("--w")) + 4) * cw;
    return Math.min(n - 1, Math.max(0, Math.round((holder.getBoundingClientRect().left - base) / pitch)));
  };

  let trail = [];
  let goalX = null;
  let expected = null;

  document.addEventListener("focusin", (e) => {
    if (e.target !== expected) {
      trail = [];
      goalX = null;
    }
    expected = null;
  });

  const focusTo = (target) => {
    expected = target;
    target.focus();
  };

  const go = (dir) => {
    const list = focusables();
    if (!list.includes(document.activeElement)) {
      const start = document.querySelector(".tabs a.on") || list[0];
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
    for (const cand of list) {
      if (cand === el) continue;
      const to = cand.getBoundingClientRect();
      const dy = to.top - from.top;
      const dx = to.left - from.left;
      if (dir === "down" && dy > half && columnOf(cand) === col) consider(cand, dy * 1e4 + Math.abs(to.left - x));
      else if (dir === "up" && dy < -half && columnOf(cand) === col) consider(cand, -dy * 1e4 + Math.abs(to.left - x));
      else if (dir === "right" && Math.abs(dy) < half && dx > 1) consider(cand, dx);
      else if (dir === "left" && Math.abs(dy) < half && dx < -1) consider(cand, -dx);
    }
    if (!best && vertical) {
      const other = list.filter((cand) => (dir === "down" ? columnOf(cand) > col : columnOf(cand) < col));
      if (other.length) {
        const target = dir === "down"
          ? Math.min(...other.map(columnOf))
          : Math.max(...other.map(columnOf));
        for (const cand of other) {
          if (columnOf(cand) !== target) continue;
          const to = cand.getBoundingClientRect();
          const row = dir === "down" ? to.top : -to.top;
          consider(cand, row * 1e4 + Math.abs(to.left - x));
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
    const at = links.findIndex((a) => a.classList.contains("on"));
    if (at < 0) return;
    location.href = links[(at + step + links.length) % links.length].getAttribute("href");
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
    if (e.key === "Enter" && el && el.type === "checkbox") {
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
    if (/^[0-9]$/.test(e.key)) {
      const link = document.querySelector(`.tabs a[data-key="${e.key}"]`);
      if (link) {
        e.preventDefault();
        location.href = link.getAttribute("href");
      }
    }
  });

  const native = CSS.supports("caret-shape", "block");
  let caret = null;
  function place() {
    if (native) return;
    const el = document.activeElement;
    const screen = document.querySelector(".screen");
    if (!caret) {
      caret = document.createElement("div");
      caret.className = "caret";
      caret.hidden = true;
      screen.appendChild(caret);
      root.classList.add("fake-caret");
    }
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
    if (!target || e.detail !== 0) return;
    try {
      sessionStorage.setItem(KEY, JSON.stringify({ ...describe(target), at: Date.now() }));
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
    if (match) focusTo(match);
  };

  measure();
  if (document.fonts) document.fonts.ready.then(measure);
  new ResizeObserver(measure).observe(root);

  document.fonts?.addEventListener("loadingdone", measure);
  const reveal = document.querySelector("[data-copy]");
  if (reveal) focusTo(reveal);
  else restore();
})();
