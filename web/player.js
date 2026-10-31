(() => {
  const icons = new Map();
  const icon = (name) => {
    if (!icons.has(name)) {
      const holder = document.getElementById("juuz-icons")?.content.querySelector(`[data-icon="${name}"]`);
      icons.set(name, holder ? holder.innerHTML : "");
    }
    return icons.get(name);
  };

  const gain = (v) => {
    let a = v;
    if (a > 0 && a < 1) {
      a = Math.exp(6.908 * v) / 1000;
      if (v < 0.1) a *= v * 10;
    }
    return a;
  };
  const volumeIcon = (v) =>
    v === 0 ? "mute" : v <= 0.25 ? "volume1" : v <= 0.5 ? "volume2" : v <= 0.75 ? "volume3" : "volume4";

  const audio = new Audio();
  audio.preload = "auto";
  let queue = [];
  let index = -1;
  let bar = null;
  let els = {};
  let volume = 0.5;
  let beforeMute = 0.5;
  let seeking = false;
  let counted = false;
  let open = null;
  let paused = false;
  let loading = false;
  let ctx = null;
  let normNode = null;
  let volNode = null;

  const clock = (s) => {
    if (!Number.isFinite(s) || s < 0) return "0:00";
    const m = Math.floor(s / 60);
    return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
  };
  const current = () => queue[index];
  const artUrl = (song, size) => `/art?id=${encodeURIComponent(song.art || song.id)}&size=${size}`;

  const slider = (label, step, onInput) => {
    const el = document.createElement("div");
    el.className = "slider";
    el.tabIndex = 0;
    el.setAttribute("role", "slider");
    el.setAttribute("aria-label", label);
    el.setAttribute("aria-valuemin", "0");
    el.setAttribute("aria-valuemax", "100");
    const rail = document.createElement("span");
    rail.className = "rail";
    const fill = document.createElement("span");
    fill.className = "fill";
    el.append(rail, fill);
    let value = 0;
    const set = (v) => {
      value = Math.max(0, Math.min(1, v));
      fill.style.width = `${value * 100}%`;
      el.setAttribute("aria-valuenow", String(Math.round(value * 100)));
    };
    const at = (e) => {
      const box = el.getBoundingClientRect();
      return box.width > 0 ? (e.clientX - box.left) / box.width : 0;
    };
    let drag = false;
    el.addEventListener("pointerdown", (e) => {
      if (e.button !== 0) return;
      drag = true;
      el.setPointerCapture(e.pointerId);
      set(at(e));
      onInput(value, true);
    });
    el.addEventListener("pointermove", (e) => {
      if (!drag) return;
      set(at(e));
      onInput(value, true);
    });
    const release = (e) => {
      if (!drag) return;
      drag = false;
      set(at(e));
      onInput(value, false);
    };
    el.addEventListener("pointerup", release);
    el.addEventListener("pointercancel", release);
    el.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        el.toggleAttribute("data-adjust");
        return;
      }
      if (!el.hasAttribute("data-adjust")) return;
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        el.removeAttribute("data-adjust");
        return;
      }
      const keys = { ArrowRight: step, ArrowLeft: -step };
      let next = null;
      if (keys[e.key] !== undefined) next = value + keys[e.key] * (e.shiftKey ? 5 : 1);
      if (e.key === "Home") next = 0;
      if (e.key === "End") next = 1;
      if (next === null) return;
      e.preventDefault();
      e.stopPropagation();
      set(next);
      onInput(value, false);
    });
    el.addEventListener("blur", () => el.removeAttribute("data-adjust"));
    return { el, set, get: () => value };
  };

  const build = () => {
    bar = document.createElement("div");
    bar.className = "player";
    bar.hidden = true;
    const inner = document.createElement("div");
    inner.className = "inner";
    inner.innerHTML =
      '<div class="now"><img class="cover" alt=""><span class="cover blank"></span>' +
      '<div class="text"><strong class="title"></strong><span class="meta muted"></span></div></div>' +
      '<div class="controls">' +
      `<button class="icon" data-act="previous" aria-label="previous">${icon("previous")}</button>` +
      '<button class="icon" data-act="toggle" aria-label="play or pause"></button>' +
      `<button class="icon" data-act="next" aria-label="next">${icon("next")}</button>` +
      "</div>" +
      '<small class="elapsed muted">0:00</small>' +
      '<small class="total muted">0:00</small>' +
      '<div class="vol"><button class="icon" data-act="mute" aria-label="mute"></button></div>';
    const seek = slider("seek", 0.02, (v, live) => {
      seeking = live;
      if (Number.isFinite(audio.duration)) audio.currentTime = v * audio.duration;
    });
    seek.el.classList.add("seek");
    const vol = slider("volume", 0.05, (v) => setVolume(v, false));
    vol.el.classList.add("volume");
    inner.insertBefore(seek.el, inner.querySelector(".total"));
    inner.querySelector(".vol").append(vol.el);
    bar.append(inner);
    els = {
      art: inner.querySelector("img.cover"),
      blank: inner.querySelector(".cover.blank"),
      title: inner.querySelector(".title"),
      meta: inner.querySelector(".meta"),
      elapsed: inner.querySelector(".elapsed"),
      total: inner.querySelector(".total"),
      toggle: inner.querySelector('[data-act="toggle"]'),
      mute: inner.querySelector('[data-act="mute"]'),
      seek,
      vol,
    };
    els.art.addEventListener("error", () => showCover(false));
    try {
      const saved = Number(localStorage.getItem("juuz.volume"));
      if (localStorage.getItem("juuz.volume") !== null && saved >= 0 && saved <= 1) volume = saved;
    } catch {}
    setVolume(volume, true);
    bar.addEventListener("click", (e) => {
      const act = e.target.closest("[data-act]")?.dataset.act;
      if (act === "toggle") toggle();
      if (act === "next") step(1);
      if (act === "previous") previous();
      if (act === "mute") {
        if (volume > 0) {
          beforeMute = volume;
          setVolume(0, true);
        } else {
          setVolume(beforeMute || 0.5, true);
        }
      }
    });
    return bar;
  };

  const setVolume = (v, syncSlider) => {
    volume = Math.max(0, Math.min(1, v));
    if (volNode) volNode.gain.setTargetAtTime(gain(volume), ctx.currentTime, 0.004);
    else audio.volume = Math.min(1, gain(volume));
    if (syncSlider && els.vol) els.vol.set(volume);
    if (els.mute) els.mute.innerHTML = icon(volumeIcon(volume));
    try {
      localStorage.setItem("juuz.volume", String(volume));
    } catch {}
  };

  const showCover = (on) => {
    if (!els.art) return;
    els.art.hidden = !on;
    els.blank.hidden = on;
  };

  const mount = () => {
    if (!bar) build();
    if (!bar.isConnected) document.body.appendChild(bar);
  };

  const markPlaying = () => {
    const song = current();
    document.querySelectorAll("[data-play]").forEach((el) => {
      el.classList.toggle("playing", !!song && el.dataset.play === song.id);
    });
  };

  const render = () => {
    const song = current();
    if (!song) return;
    mount();
    bar.hidden = false;
    els.title.textContent = song.title;
    els.meta.textContent = song.artist;
    els.toggle.innerHTML = icon(audio.paused ? "play" : "pause");
    if (song.cover) {
      showCover(true);
      const src = artUrl(song, 128);
      if (!els.art.src.endsWith(src)) els.art.src = src;
    } else {
      showCover(false);
    }
    document.title = `${song.title} - Juuz`;
    if ("mediaSession" in navigator) {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: song.title,
        artist: song.artist,
        album: song.album,
        artwork: song.cover ? [{ src: artUrl(song, 300) }] : [],
      });
    }
    markPlaying();
  };

  const position = () => Math.max(0, Math.round(audio.currentTime * 1000));

  const emit = (type, song, at, beacon) => {
    const url = `/playback?type=${type}&id=${encodeURIComponent(song.id)}&position=${at}`;
    if (beacon && navigator.sendBeacon) {
      navigator.sendBeacon(url);
      return;
    }
    fetch(url, { method: "POST", credentials: "same-origin", keepalive: true }).catch(() => {});
  };

  const closeOpen = (at, beacon) => {
    if (!open) return;
    emit("STOP", open, at, beacon);
    open = null;
  };

  const report = (song, submission) => {
    fetch(`/scrobble?id=${encodeURIComponent(song.id)}&submission=${submission}`, {
      method: "POST",
      credentials: "same-origin",
      keepalive: true,
    }).catch(() => {});
  };

  const load = (at) => {
    if (at < 0 || at >= queue.length) return;
    closeOpen(position(), false);
    loading = true;
    paused = false;
    index = at;
    counted = false;
    graph();
    if (normNode) normNode.gain.value = normalization(queue[at].gain);
    audio.src = `/play?id=${encodeURIComponent(queue[at].id)}`;
    audio.play().catch(() => {});
    open = queue[at];
    emit("PLAY", open, 0, false);
    render();
    els.seek.set(0);
  };

  const toggle = () => {
    if (!current()) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  };

  const step = (by) => {
    if (index + by >= queue.length) {
      audio.pause();
      return;
    }
    load(Math.max(0, index + by));
  };

  const previous = () => {
    if (audio.currentTime > 3) audio.currentTime = 0;
    else step(-1);
  };

  const seekBy = (seconds) => {
    if (Number.isFinite(audio.duration)) {
      audio.currentTime = Math.max(0, Math.min(audio.duration, audio.currentTime + seconds));
    }
  };

  const normalization = (rg) => {
    if (rg === null || !Number.isFinite(rg)) return 1;
    const db = Math.max(-12, Math.min(12, rg + 4));
    return Math.pow(10, db / 20);
  };

  const graph = () => {
    if (ctx) {
      if (ctx.state === "suspended") ctx.resume().catch(() => {});
      return;
    }
    const Context = window.AudioContext || window.webkitAudioContext;
    if (!Context) return;
    try {
      ctx = new Context();
      const source = ctx.createMediaElementSource(audio);
      normNode = ctx.createGain();
      volNode = ctx.createGain();
      source.connect(normNode);
      normNode.connect(volNode);
      volNode.connect(ctx.destination);
      audio.volume = 1;
      volNode.gain.value = gain(volume);
    } catch {
      ctx = normNode = volNode = null;
    }
  };

  const describe = (el) => ({
    id: el.dataset.play,
    title: el.dataset.title || "unknown",
    artist: el.dataset.artist || "",
    album: el.dataset.album || "",
    art: el.dataset.art || "",
    cover: el.dataset.cover === "1",
    gain: el.dataset.gain ? Number(el.dataset.gain) : null,
  });

  const playAlbum = async (id) => {
    try {
      const res = await fetch(`/library?view=songs&status=all&album=${encodeURIComponent(id)}`, {
        credentials: "same-origin",
      });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const items = [...doc.querySelectorAll("[data-queue] [data-play]")];
      if (!items.length) return;
      queue = items.map(describe);
      load(0);
    } catch {}
  };

  document.addEventListener("click", (e) => {
    const album = e.target.closest("[data-play-album]");
    if (album) {
      e.preventDefault();
      playAlbum(album.dataset.playAlbum);
      return;
    }
    const all = e.target.closest("[data-playall]");
    if (all) {
      e.preventDefault();
      document.querySelector("[data-queue] [data-play]")?.click();
      return;
    }
    const el = e.target.closest("[data-play]");
    if (!el) return;
    e.preventDefault();
    const scope = el.closest("[data-queue]") || document;
    if (scope._virtual) {
      const rows = scope._virtual.loaded();
      const at = rows.findIndex((row) => row.contains(el));
      queue = rows.map((row) => describe(row.querySelector("[data-play]"))).filter(Boolean);
      load(Math.max(0, at));
      scope._virtual.fill().then(() => {
        if (!scope.isConnected || !current()) return;
        const id = current().id;
        queue = scope._virtual
          .loaded()
          .map((row) => describe(row.querySelector("[data-play]")))
          .filter(Boolean);
        const now = queue.findIndex((song) => song.id === id);
        if (now >= 0) index = now;
      });
      return;
    }
    const items = [...scope.querySelectorAll("[data-play]")];
    queue = items.map(describe);
    load(items.indexOf(el));
  });

  const ownsSpace = (el) =>
    !!el && el !== document.body && el.matches("input, textarea, select, summary, [role=option], .select-trigger");

  document.addEventListener("keyup", (e) => {
    if (e.key === " " && current() && !ownsSpace(document.activeElement)) e.preventDefault();
  });

  document.addEventListener("keydown", (e) => {
    if (e.defaultPrevented || e.ctrlKey || e.metaKey || e.altKey) return;
    const el = document.activeElement;
    if (el && el.matches("input:not([type=checkbox]), textarea, select")) return;
    if (e.key === " ") {
      if (current() && !ownsSpace(el)) {
        e.preventDefault();
        toggle();
      }
      return;
    }
    if (!current()) return;
    const keys = {
      n: () => step(1),
      p: previous,
      ",": () => seekBy(-10),
      ".": () => seekBy(10),
      m: () => els.mute.click(),
    };
    if (keys[e.key]) {
      e.preventDefault();
      keys[e.key]();
    }
  });

  audio.addEventListener("timeupdate", () => {
    if (!bar) return;
    const d = audio.duration;
    els.elapsed.textContent = clock(audio.currentTime);
    els.total.textContent = clock(d);
    if (!seeking && Number.isFinite(d) && d > 0) els.seek.set(audio.currentTime / d);
    const song = current();
    if (song && !counted && Number.isFinite(d) && d > 0 && audio.currentTime >= Math.min(d / 2, 240)) {
      counted = true;
      report(song, true);
    }
  });
  audio.addEventListener("seeked", () => {
    seeking = false;
  });
  audio.addEventListener("play", () => {
    if (open && paused) {
      paused = false;
      emit("RESUME", open, position(), false);
    }
    render();
  });
  audio.addEventListener("pause", () => {
    if (open && !loading && !audio.ended && !paused) {
      paused = true;
      emit("PAUSE", open, position(), false);
    }
    render();
  });
  audio.addEventListener("playing", () => {
    loading = false;
  });
  audio.addEventListener("seeked", () => {
    if (open && !loading) emit("SEEK", open, position(), false);
  });
  audio.addEventListener("ended", () => {
    closeOpen(Number.isFinite(audio.duration) ? Math.round(audio.duration * 1000) : position(), false);
    step(1);
  });
  audio.addEventListener("error", () => {
    loading = false;
    closeOpen(position(), false);
    if (index + 1 < queue.length) step(1);
  });
  window.addEventListener("pagehide", () => closeOpen(position(), true));

  if ("mediaSession" in navigator) {
    navigator.mediaSession.setActionHandler("play", toggle);
    navigator.mediaSession.setActionHandler("pause", toggle);
    navigator.mediaSession.setActionHandler("previoustrack", previous);
    navigator.mediaSession.setActionHandler("nexttrack", () => step(1));
  }

  let searchSeq = 0;
  let controller = null;
  const search = (form) => {
    const seq = ++searchSeq;
    controller?.abort();
    controller = new AbortController();
    const label = form.querySelector(".search");
    label?.setAttribute("data-busy", "");
    const params = new URLSearchParams(new FormData(form));
    fetch(`${form.action}?${params}`, { credentials: "same-origin", signal: controller.signal })
      .then((r) => r.text())
      .then((html) => {
        if (seq !== searchSeq) return;
        const doc = new DOMParser().parseFromString(html, "text/html");
        for (const key of ["nav", "results"]) {
          const now = document.querySelector(`[data-swap="${key}"]`);
          const next = doc.querySelector(`[data-swap="${key}"]`);
          if (now && next && next.innerHTML !== now.innerHTML) now.innerHTML = next.innerHTML;
        }
        markPlaying();
        fixCovers();
      })
      .catch(() => {})
      .finally(() => {
        if (seq === searchSeq) label?.removeAttribute("data-busy");
      });
  };

  document.addEventListener("input", (e) => {
    const form = e.target.closest?.("form[data-search]");
    if (form && e.target.matches("input[name=q]")) search(form);
  });
  document.addEventListener(
    "submit",
    (e) => {
      if (!e.target.matches?.("form[data-search]")) return;
      e.preventDefault();
      e.stopImmediatePropagation();
      search(e.target);
    },
    true,
  );

  document.addEventListener("submit", async (e) => {
    const form = e.target;
    if (e.defaultPrevented || !current() || !form.matches?.("form")) return;
    if (form.method.toLowerCase() !== "post") return;
    const path = new URL(form.action, location.href).pathname;
    if (path === "/login" || path === "/logout") return;
    e.preventDefault();
    try {
      const res = await fetch(form.action, {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams(new FormData(form)),
      });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const next = doc.querySelector(".screen");
      const now = document.querySelector(".screen");
      if (!next || !now) {
        location.href = res.url;
        return;
      }
      now.replaceWith(next);
      document.title = doc.title || document.title;
      history.pushState(null, "", res.url);
      markPlaying();
    } catch {
      form.submit();
    }
  });

  const closeSelects = (except) => {
    document.querySelectorAll(".select[data-open]").forEach((el) => {
      if (el === except) return;
      el.removeAttribute("data-open");
      el.querySelector(".options").hidden = true;
      el.querySelector(".select-trigger").setAttribute("aria-expanded", "false");
    });
  };

  document.addEventListener("click", (e) => {
    const select = e.target.closest(".select");
    closeSelects(select);
    if (!select) return;
    const option = e.target.closest("[role=option]");
    const trigger = e.target.closest(".select-trigger");
    if (option) {
      select.querySelector("input[type=hidden]").value = option.dataset.value;
      select.querySelector(".value").textContent = option.textContent;
      select.querySelectorAll("[role=option]").forEach((o) => {
        o.setAttribute("aria-selected", o === option ? "true" : "false");
      });
      closeSelects(null);
      select.querySelector(".select-trigger").focus();
    } else if (trigger) {
      const open = !select.hasAttribute("data-open");
      closeSelects(open ? select : null);
      select.toggleAttribute("data-open", open);
      select.querySelector(".options").hidden = !open;
      trigger.setAttribute("aria-expanded", String(open));
      if (open) select.querySelector("[aria-selected=true]")?.focus();
    }
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && document.querySelector(".select[data-open]")) {
      const open = document.querySelector(".select[data-open]");
      closeSelects(null);
      open.querySelector(".select-trigger").focus();
    }
  });

  const blankCover = (img) => {
    if (img.closest(".player")) return;
    const blank = document.createElement("span");
    blank.className = "cover";
    img.replaceWith(blank);
  };

  const fixCovers = () => {
    document.querySelectorAll("img.cover").forEach((img) => {
      if (img.complete && img.naturalWidth === 0) blankCover(img);
    });
  };

  document.addEventListener(
    "error",
    (e) => {
      if (e.target instanceof HTMLImageElement && e.target.matches("img.cover")) blankCover(e.target);
    },
    true,
  );
  document.addEventListener("DOMContentLoaded", fixCovers);
  window.addEventListener("load", fixCovers);

  const rowsOf = (html, selector) => {
    const doc = new DOMParser().parseFromString(html, "text/html");
    const host = doc.querySelector(selector);
    return host ? [...host.children] : [];
  };

  const mountVirtual = (host) => {
    if (host._virtual && host._virtual.src === host.dataset.src) return;
    const total = Number(host.dataset.total) || 0;
    const src = host.dataset.src;
    const chunk = Number(host.dataset.chunk) || 60;
    const rows = new Array(total);
    const seeded = [...host.children].filter((el) => el.classList.contains("row"));
    const first = Number(host.dataset.offset) || 0;
    seeded.forEach((el, i) => {
      rows[first + i] = el;
    });
    const gap = parseFloat(getComputedStyle(host).rowGap) || 0;
    const pitch = (seeded[0]?.getBoundingClientRect().height || 2 * 22.4) + gap;
    const above = document.createElement("div");
    const below = document.createElement("div");
    above.setAttribute("aria-hidden", "true");
    below.setAttribute("aria-hidden", "true");
    const requested = new Set();
    let range = [-1, -1];
    let frame = 0;

    const ghost = () => {
      const el = document.createElement("div");
      el.className = "row ghost";
      el.innerHTML =
        '<span class="c-cover"><span class="cover"></span></span>' +
        '<span class="main"><i class="bar"></i><i class="bar short"></i></span>';
      return el;
    };

    const load = (base) => {
      if (requested.has(base)) return Promise.resolve();
      requested.add(base);
      return fetch(`${src}&offset=${base}&limit=${chunk}`, { credentials: "same-origin" })
        .then((res) => res.text())
        .then((html) => {
          if (!alive()) return;
          rowsOf(html, "[data-virtual]")
            .filter((el) => el.classList.contains("row"))
            .forEach((el, i) => {
              rows[base + i] = el;
            });
          render(true);
        })
        .catch(() => requested.delete(base));
    };

    const alive = () =>
      host.isConnected && host.hasAttribute("data-virtual") && host.dataset.src === src;

    const render = (force) => {
      if (!alive()) {
        window.removeEventListener("scroll", schedule);
        window.removeEventListener("resize", schedule);
        return;
      }
      const origin = host.getBoundingClientRect().top + window.scrollY;
      const start = Math.max(0, Math.floor((window.scrollY - origin) / pitch) - 6);
      const end = Math.min(total, Math.ceil((window.scrollY + window.innerHeight - origin) / pitch) + 6);
      if (!force && range[0] === start && range[1] === end) return;
      range = [start, end];
      above.hidden = start === 0;
      above.style.height = `${Math.max(0, start * pitch - gap)}px`;
      below.hidden = end >= total;
      below.style.height = `${Math.max(0, (total - end) * pitch - gap)}px`;
      const view = [above];
      for (let i = start; i < end; i++) {
        if (rows[i]) {
          view.push(rows[i]);
        } else {
          view.push(ghost());
          load(Math.floor(i / chunk) * chunk);
        }
      }
      view.push(below);
      host.replaceChildren(...view);
      const ahead = Math.min(total - 1, end + chunk);
      for (let i = Math.max(0, start - chunk / 2); i <= ahead; i += chunk) {
        if (!rows[Math.floor(i / chunk) * chunk]) load(Math.floor(i / chunk) * chunk);
      }
      markPlaying();
    };

    function schedule() {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => render(false));
    }

    host._virtual = {
      src,
      loaded: () => rows.filter(Boolean),
      fill: async () => {
        for (let base = 0; base < total; base += chunk) {
          if (!alive()) return;
          if (!rows[base]) await load(base);
        }
      },
    };
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    render(true);
  };

  const mountMore = (host) => {
    if (host._more === host.dataset.src) return;
    host._more = host.dataset.src;
    const total = Number(host.dataset.total) || 0;
    const src = host.dataset.src;
    const chunk = Number(host.dataset.chunk) || 60;
    let busy = false;

    const check = () => {
      if (!host.isConnected || !host.hasAttribute("data-more") || host.dataset.src !== src) {
        window.removeEventListener("scroll", check);
        window.removeEventListener("resize", check);
        return;
      }
      const loaded = host.children.length;
      if (busy || loaded >= total) return;
      if (host.getBoundingClientRect().bottom - window.innerHeight > 800) return;
      busy = true;
      fetch(`${src}&offset=${loaded}&limit=${chunk}`, { credentials: "same-origin" })
        .then((res) => res.text())
        .then((html) => {
          if (host.isConnected && host.dataset.src === src) host.append(...rowsOf(html, "[data-more]"));
        })
        .catch(() => {})
        .finally(() => {
          busy = false;
          check();
        });
    };

    window.addEventListener("scroll", check, { passive: true });
    window.addEventListener("resize", check);
    check();
  };

  const mountLists = () => {
    document.querySelectorAll("[data-virtual]").forEach(mountVirtual);
    document.querySelectorAll("[data-more]").forEach(mountMore);
  };

  let mounting = 0;
  new MutationObserver(() => {
    cancelAnimationFrame(mounting);
    mounting = requestAnimationFrame(mountLists);
  }).observe(document.body, { childList: true, subtree: true });
  document.addEventListener("DOMContentLoaded", mountLists);
  window.addEventListener("load", mountLists);

  window.addEventListener("popstate", async () => {
    if (!current()) return;
    try {
      const res = await fetch(location.pathname + location.search, { credentials: "same-origin" });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const next = doc.querySelector(".screen");
      const now = document.querySelector(".screen");
      if (next && now && next.innerHTML !== now.innerHTML) {
        now.replaceWith(next);
        document.title = doc.title || document.title;
        markPlaying();
      }
    } catch {}
  });

  new MutationObserver(() => {
    if (bar && !bar.isConnected && current()) {
      mount();
      render();
    }
  }).observe(document.body, { childList: true });

  window.addEventListener("juuz:navigated", () => {
    if (current()) {
      mount();
      render();
    }
  });
})();
