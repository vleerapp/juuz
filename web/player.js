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
  let remote = false;
  let remoteState = null;
  let volumeSetAt = 0;

  const clock = (s) => {
    if (!Number.isFinite(s) || s < 0) return "0:00";
    const m = Math.floor(s / 60);
    return `${m}:${String(Math.floor(s % 60)).padStart(2, "0")}`;
  };
  const current = () => (remote ? remoteSong() : queue[index]);
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
      '<button class="ghost collapse" type="button" data-act="collapse" aria-label="minimize player">close</button>' +
      '<div class="now"><img class="cover" alt=""><span class="cover blank"></span>' +
      '<div class="text"><strong class="title"></strong><span class="meta muted"></span></div></div>' +
      '<div class="controls">' +
      `<button class="icon" data-act="previous" aria-label="previous">${icon("previous")}</button>` +
      '<button class="icon" data-act="toggle" aria-label="play or pause"></button>' +
      `<button class="icon" data-act="next" aria-label="next">${icon("next")}</button>` +
      "</div>" +
      '<small class="elapsed muted">0:00</small>' +
      '<small class="total muted">0:00</small>' +
      '<div class="vol"><button class="icon" data-act="mute" aria-label="mute"></button></div>' +
      '<div class="tools">' +
      `<button class="icon" data-act="queue" aria-label="queue">${icon("queue")}</button>` +
      (hasRemote() ? `<button class="icon" data-act="output" aria-label="output">${icon("output")}</button>` : "") +
      "</div>";
    const seek = slider("seek", 0.02, (v, live) => {
      seeking = live;
      if (remote) {
        if (!live) remoteSeek(v);
        return;
      }
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
      queue: inner.querySelector('[data-act="queue"]'),
      output: inner.querySelector('[data-act="output"]'),
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
      if (act === "collapse") collapsePlayer();
      else if (!act && !bar.hasAttribute("data-expanded") && narrow.matches && e.target.closest(".now")) expandPlayer();
      if (act === "queue") togglePanel("queue");
      if (act === "output") togglePanel("output");
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

  const narrow = window.matchMedia("(max-width: 560px)");
  const expandPlayer = () => {
    closePanels();
    bar.setAttribute("data-expanded", "");
    document.documentElement.setAttribute("data-player-open", "");
  };
  const collapsePlayer = () => {
    closePanels();
    bar?.removeAttribute("data-expanded");
    document.documentElement.removeAttribute("data-player-open");
  };
  narrow.addEventListener("change", () => {
    if (!narrow.matches) collapsePlayer();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && !panel && bar?.hasAttribute("data-expanded")) collapsePlayer();
  });

  const setVolume = (v, syncSlider) => {
    volume = Math.max(0, Math.min(1, v));
    if (remote) {
      volumeSetAt = Date.now();
      remoteVolume(volume);
      if (syncSlider && els.vol) els.vol.set(volume);
      if (els.mute) els.mute.innerHTML = icon(volumeIcon(volume));
      return;
    }
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
    if (remote) return renderRemote();
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

  const load = (at, offset = 0, autoplay = true) => {
    if (remote) return remoteLoad(at);
    if (at < 0 || at >= queue.length) return;
    closeOpen(position(), false);
    loading = true;
    paused = false;
    index = at;
    counted = false;
    graph();
    if (normNode) normNode.gain.value = normalization(queue[at].gain);
    audio.src = `/play?id=${encodeURIComponent(queue[at].id)}`;
    if (offset > 0) {
      audio.addEventListener(
        "loadedmetadata",
        () => {
          audio.currentTime = offset;
        },
        { once: true },
      );
    }
    open = queue[at];
    if (autoplay) {
      audio.play().catch(() => {});
      emit("PLAY", open, 0, false);
    } else {
      paused = true;
    }
    render();
    els.seek.set(0);
  };

  const toggle = () => {
    if (remote) {
      if (remoteState?.songs.length) remoteCmd({ action: remoteState.playing ? "stop" : "start" });
      return;
    }
    if (!current()) return;
    if (audio.paused) audio.play().catch(() => {});
    else audio.pause();
  };

  const step = (by) => {
    if (remote) {
      const next = (remoteState?.index ?? 0) + by;
      if (!remoteState || next < 0) return;
      if (next >= remoteState.songs.length) remoteCmd({ action: "stop" });
      else remoteCmd({ action: "skip", index: next });
      return;
    }
    if (index + by >= queue.length) {
      audio.pause();
      return;
    }
    load(Math.max(0, index + by));
  };

  const previous = () => {
    if (remote) {
      if (remotePosition() > 3) remoteCmd({ action: "skip", index: remoteState.index, offset: 0 });
      else step(-1);
      return;
    }
    if (audio.currentTime > 3) audio.currentTime = 0;
    else step(-1);
  };

  const seekBy = (seconds) => {
    if (remote) {
      const song = remoteSong();
      if (!song) return;
      const to = Math.max(0, Math.min(song.duration || 0, remotePosition() + seconds));
      remoteCmd({ action: "skip", index: remoteState.index, offset: Math.floor(to) });
      return;
    }
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

  const playFrom = async (url) => {
    try {
      const res = await fetch(url, { credentials: "same-origin" });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const items = [...doc.querySelectorAll("[data-queue] [data-play]")];
      if (!items.length) return;
      queue = items.map(describe);
      load(0);
    } catch {}
  };

  const hasRemote = () => document.querySelector(".screen")?.dataset.jukebox === "1";

  const post = async (url, data) => {
    const body = new URLSearchParams();
    for (const [key, value] of Object.entries(data || {})) {
      if (Array.isArray(value)) value.forEach((v) => body.append(key, v));
      else body.append(key, String(value));
    }
    const res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body,
    });
    if (!res.ok) {
      const failure = new Error(String(res.status));
      failure.detail = await res
        .json()
        .then((body) => body.error || "")
        .catch(() => "");
      throw failure;
    }
    return res.status === 204 ? null : res.json();
  };

  const fromRemote = (s) => ({
    id: s.id,
    title: s.title || "unknown",
    artist: s.artist || "",
    album: s.album || "",
    art: s.coverArt || "",
    cover: !!s.coverArt,
    gain: null,
    duration: Number(s.duration) || 0,
  });
  const remoteSong = () => remoteState?.songs[remoteState.index];
  const remotePosition = () => {
    if (!remoteState) return 0;
    const song = remoteSong();
    const moved = remoteState.playing ? (Date.now() - remoteState.at) / 1000 : 0;
    const at = (remoteState.positionMs !== undefined ? remoteState.positionMs / 1000 : remoteState.position) + moved;
    return song?.duration ? Math.min(at, song.duration) : at;
  };

  let remotePending = 0;
  let remoteChain = Promise.resolve();
  const optimistic = (data) => {
    const s = remoteState;
    if (!s) return;
    if (data.action === "start") s.playing = true;
    else if (data.action === "stop") s.playing = false;
    else if (data.action === "gain") s.gain = data.gain;
    else if (data.action === "skip") {
      if (data.index !== undefined) s.index = data.index;
      s.position = data.offset ?? 0;
      s.playing = true;
    } else if (data.action === "set") {
      s.songs = queue.map((song) => ({ ...song }));
      s.index = data.index ?? 0;
      s.position = data.offset ?? 0;
      s.playing = data.play !== 0;
    } else return;
    delete s.positionMs;
    delete s.clock;
    s.at = Date.now();
    renderRemote();
    tickRemote();
  };
  const remoteCmd = (data) => {
    optimistic(data);
    remotePending += 1;
    remoteChain = remoteChain
      .then(() => post("/queue", data))
      .catch(() => {})
      .finally(() => {
        remotePending -= 1;
        if (remotePending) return;
        if (pushed) applyRemote(pushed);
        else if (!liveOpen()) fetchRemote();
      });
    return remoteChain;
  };
  const remoteLoad = (at) => {
    if (at < 0 || at >= queue.length) return;
    remoteCmd({ action: "set", index: at, id: queue.map((song) => song.id) });
  };
  const remoteSeek = (v) => {
    const song = remoteSong();
    if (!song || !song.duration) return;
    remoteCmd({ action: "skip", index: remoteState.index, offset: Math.floor(v * song.duration) });
  };
  let gainTimer = null;
  const remoteVolume = (v) => {
    clearTimeout(gainTimer);
    gainTimer = setTimeout(() => remoteCmd({ action: "gain", gain: v }), 120);
  };

  const idleBar = (meta) => {
    mount();
    bar.hidden = false;
    els.title.textContent = "nothing playing";
    els.meta.textContent = meta || "";
    els.toggle.innerHTML = icon("play");
    showCover(false);
    els.elapsed.textContent = "0:00";
    els.total.textContent = "0:00";
    els.seek.set(0);
  };

  const renderRemote = () => {
    const song = remoteSong();
    if (!song) {
      idleBar(remoteState?.output?.name);
      markPlaying();
      return;
    }
    mount();
    bar.hidden = false;
    els.title.textContent = song.title;
    els.meta.textContent = song.artist;
    els.toggle.innerHTML = icon(remoteState.playing ? "pause" : "play");
    if (song.cover) {
      showCover(true);
      const src = artUrl(song, 128);
      if (!els.art.src.endsWith(src)) els.art.src = src;
    } else {
      showCover(false);
    }
    document.title = `${song.title} - Juuz`;
    markPlaying();
  };

  const tickRemote = () => {
    if (!remote || !bar || !remoteState) return;
    const song = remoteSong();
    const at = remotePosition();
    els.elapsed.textContent = clock(at);
    els.total.textContent = clock(song?.duration || 0);
    if (!seeking && song?.duration) els.seek.set(at / song.duration);
  };

  const showOutputState = () => {
    els.output?.classList.toggle("on", remote);
  };

  const setRemote = (on) => {
    if (on === remote) return;
    remote = on;
    if (on) {
      audio.pause();
      closeOpen(position(), false);
    } else {
      remoteState = null;
      if (queue.length && current()) render();
      else if (bar && !hasRemote()) bar.hidden = true;
      else if (hasRemote()) idleBar("");
    }
    showOutputState();
  };

  const liveOpen = () => !!window.juuzLive?.open();
  let pushed = null;
  const applyRemote = (state) => {
    if (!hasRemote() || remotePending) return;
    state.at = Date.now();
    state.songs = (state.queue || []).map(fromRemote);
    remoteState = state;
    setRemote(!!state.output);
    if (remote) {
      renderRemote();
      if (Date.now() - volumeSetAt > 1500 && els.vol) {
        volume = state.gain;
        els.vol.set(volume);
        if (els.mute) els.mute.innerHTML = icon(volumeIcon(volume));
      }
      tickRemote();
    }
    if (panel === "queue") renderQueue();
    pcmVolumeTo(state.gain);
  };
  const fetchRemote = async () => {
    if (!hasRemote()) return;
    try {
      const res = await fetch("/queue", { credentials: "same-origin" });
      if (!res.ok) return;
      pushed = null;
      remoteState = null;
      applyRemote(await res.json());
    } catch {}
  };

  setInterval(tickRemote, 250);

  let panel = null;
  const node = (tag, cls, text) => {
    const el = document.createElement(tag);
    if (cls) el.className = cls;
    if (text !== undefined) el.textContent = text;
    return el;
  };
  const panels = {};
  const panelEl = (name) => {
    if (!panels[name]) {
      const el = node("div", "popup");
      el.dataset.panel = name;
      el.hidden = true;
      panels[name] = el;
    }
    mount();
    if (!panels[name].isConnected) bar.append(panels[name]);
    return panels[name];
  };
  const closePanels = () => {
    panel = null;
    for (const el of Object.values(panels)) el.hidden = true;
    for (const b of [els.queue, els.output]) b?.removeAttribute("aria-expanded");
  };
  const togglePanel = (name) => {
    if (panel === name) return closePanels();
    closePanels();
    panel = name;
    panelEl(name).hidden = false;
    (name === "queue" ? els.queue : els.output)?.setAttribute("aria-expanded", "true");
    if (name === "queue") renderQueue(true);
    else if (outputs) renderOutputs();
    else loadOutputs();
  };
  document.addEventListener("click", (e) => {
    if (panel && bar && !bar.contains(e.target)) closePanels();
  });
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && panel) {
      closePanels();
      e.stopPropagation();
    }
  });

  const item = (label, current, attrs) => {
    const b = node("button", "item");
    b.type = "button";
    b.append(node("span", "name", label));
    if (current) b.setAttribute("aria-current", "true");
    Object.assign(b.dataset, attrs);
    return b;
  };

  let outputs = null;
  let draft = null;
  const draftFrom = (combo) => ({
    id: combo?.id || "",
    name: combo?.name || "",
    members: new Map((combo?.members || []).map((m) => [m.id, { name: m.name, delay: m.delayMs }])),
    delays: new Map((combo?.members || []).map((m) => [m.id, String(m.delayMs)])),
  });
  const mineUdn = window.juuzBrowser.udn;
  const SECTION = { local: "this device", group: "groups", browser: "browsers", snapcast: "snapcast", airplay: "airplay", dlna: "dlna" };
  const glyphFor = (protocol) => (protocol in SECTION && protocol !== "local" ? protocol : "dlna");
  const ghost = (label, attrs, cls) => {
    const b = node("button", cls ? `ghost ${cls}` : "ghost", label);
    b.type = "button";
    Object.assign(b.dataset, attrs);
    return b;
  };
  const tileEl = (label, glyph, attrs, { current = false, offline = false, meta = "", foot = null, extra = null, cls = "", detail = "" } = {}) => {
    const t = node("div", cls ? `tile ${cls}` : "tile");
    if (offline) t.setAttribute("data-offline", "");
    const face = node("button", "face");
    face.type = "button";
    Object.assign(face.dataset, attrs);
    if (current) face.setAttribute("aria-current", "true");
    const mark = node("span", "glyph");
    mark.innerHTML = icon(glyph);
    if (detail) {
      const text = node("span", "text");
      text.append(node("span", "name", label), node("span", "detail", detail));
      face.append(mark, text);
    } else {
      face.append(mark, node("span", "name", label));
    }
    const bottom = node("div", "foot");
    if (foot) bottom.append(foot);
    else bottom.append(node("span", "meta", meta));
    t.append(face, bottom);
    if (extra) t.append(extra);
    return t;
  };
  const section = (el, key) => {
    const label = node("div", "label", SECTION[key] || key);
    const grid = node("div", "tiles");
    el.append(label, grid);
    return grid;
  };
  const pool = () => {
    const found = new Map();
    found.set(mineUdn, { name: "this browser", protocol: "browser", detail: window.juuzBrowser.info });
    for (const group of outputs?.groups || [])
      for (const d of group.devices) if (d.id !== mineUdn) found.set(d.id, { name: d.name, protocol: group.protocol, detail: d.detail || "" });
    if (draft)
      for (const [id, m] of draft.members) if (!found.has(id)) found.set(id, { name: m.name, protocol: id.split(":")[0] });
    return found;
  };
  const delayField = (id) => {
    const wrap = node("label", "delay");
    const input = node("input");
    input.type = "number";
    input.step = "10";
    input.min = "-900";
    input.max = "3000";
    input.placeholder = "0";
    input.value = draft.delays.get(id) ?? "";
    input.dataset.gdelay = id;
    input.dataset.stepper = "1";
    input.title = "enter to edit, up and down to change, esc to leave";
    input.setAttribute("aria-label", "delay in ms");
    wrap.append(input, node("span", "", "ms"));
    return wrap;
  };
  const renderDraft = (el) => {
    const head = node("div", "head editor");
    const name = node("input", "gname");
    name.type = "text";
    name.maxLength = 64;
    name.placeholder = "group name";
    name.value = draft.name;
    name.dataset.gname = "1";
    head.append(name);
    if (draft.id) head.append(ghost("delete", { grp: "remove" }));
    head.append(ghost("done", { grp: "done" }));
    el.append(head);
    const byProtocol = new Map();
    for (const [id, d] of pool()) {
      if (!byProtocol.has(d.protocol)) byProtocol.set(d.protocol, []);
      byProtocol.get(d.protocol).push([id, d]);
    }
    if (!byProtocol.size) el.append(node("div", "note muted", "no outputs to group yet"));
    for (const [protocol, devices] of [...byProtocol].sort((x, y) => x[0].localeCompare(y[0]))) {
      const grid = section(el, protocol);
      for (const [id, d] of devices) {
        const member = draft.members.get(id);
        const tile = tileEl(d.name, glyphFor(protocol), { gcheck: id }, { foot: delayField(id), detail: d.detail || "" });
        tile.dataset.tile = id;
        tile.toggleAttribute("data-picked", !!member);
        grid.append(tile);
      }
    }
  };
  let authView = null;
  const findDevice = (id) => {
    for (const group of outputs?.groups || []) {
      const device = group.devices.find((d) => d.id === id);
      if (device) return device;
    }
    return null;
  };
  const openAuth = (id) => {
    const device = findDevice(id);
    if (!device) return;
    authView = { id, name: device.name, lock: device.lock || {}, step: "idle", password: "", pin: "", message: "" };
  };
  const renderAuth = (el) => {
    const lock = outputs ? findDevice(authView.id)?.lock || authView.lock : authView.lock;
    authView.lock = lock;
    const head = node("div", "head");
    head.append(node("strong", "", `airplay · ${authView.name}`), ghost("back", { auth: "back" }));
    el.append(head);
    const hint = lock.needs === "password" ? "this device needs its password" : lock.needs === "pin" ? "this device wants a pairing code" : "this device accepts any sender";
    const status = lock.paired ? "paired with a code" : lock.configured ? "password saved" : "";
    el.append(node("div", "note", status ? `${hint} · ${status}` : hint));
    const form = node("div", "authform");
    const row = (label, ...children) => {
      const r = node("div", "authrow");
      r.append(node("span", "label", label), ...children);
      form.append(r);
    };
    const password = node("input");
    password.type = "password";
    password.placeholder = "password";
    password.autocomplete = "off";
    password.value = authView.password;
    password.dataset.apw = "1";
    row("password", password, ghost("save", { auth: "password" }));
    if (authView.step === "pin") {
      const pin = node("input");
      pin.type = "text";
      pin.inputMode = "numeric";
      pin.placeholder = "code shown on the device";
      pin.autocomplete = "off";
      pin.value = authView.pin;
      pin.dataset.apin = "1";
      row("pairing code", pin, ghost("pair", { auth: "finish" }));
    } else {
      row("pairing code", node("span", "hint", "shows a code on the device"), ghost("start pairing", { auth: "pair" }));
    }
    if (lock.configured) row("saved", node("span", "hint", status), ghost("forget", { auth: "forget" }));
    el.append(form);
    if (authView.message) el.append(node("div", "note", authView.message));
  };
  const focusSelector = (node) => {
    if (!node || node === document.body) return null;
    const d = node.dataset;
    if (d.out !== undefined) return `[data-out="${CSS.escape(d.out)}"]`;
    if (d.gcheck !== undefined) return `[data-gcheck="${CSS.escape(d.gcheck)}"]`;
    if (d.gdelay !== undefined) return `[data-gdelay="${CSS.escape(d.gdelay)}"]`;
    if (d.gname !== undefined) return "[data-gname]";
    if (d.grp !== undefined) return `[data-grp="${CSS.escape(d.grp)}"]${d.gid ? `[data-gid="${CSS.escape(d.gid)}"]` : ""}`;
    if (d.auth !== undefined) return `[data-auth="${CSS.escape(d.auth)}"]${d.gid ? `[data-gid="${CSS.escape(d.gid)}"]` : ""}`;
    return null;
  };
  const renderOutputs = () => {
    const el = panelEl("output");
    const focus = el.contains(document.activeElement) ? focusSelector(document.activeElement) : null;
    const scroll = el.scrollTop;
    paintOutputs(el);
    el.scrollTop = scroll;
    if (focus) el.querySelector(focus)?.focus({ preventScroll: true });
  };
  const paintOutputs = (el) => {
    el.classList.add("outputs");
    el.replaceChildren();
    if (authView) {
      renderAuth(el);
      return;
    }
    if (draft) {
      el.append(node("div", "head", draft.id ? "edit group" : "new group"));
      if (!outputs) return;
      renderDraft(el);
      return;
    }
    const head = node("div", "head");
    head.append(node("strong", "", "output"), ghost("scan", { out: "scan" }));
    el.append(head);
    if (!outputs) return;
    const down = (current) => current && outputs.current && !outputs.current.online;
    const here = outputs.current?.id === mineUdn || (!outputs.current && (localPicked || (!remote && !!current())));
    const local = section(el, "local");
    local.append(tileEl("this browser", "browser", { out: "" }, { current: here, meta: window.juuzBrowser.info }));
    let found = 0;
    const paintGroup = (group, into) => {
      const devices = group.devices.filter((d) => d.id !== mineUdn);
      if (!devices.length) return;
      const grid = into || section(el, group.protocol);
      for (const device of devices) {
        if (group.protocol !== "browser") found += 1;
        grid.append(
          tileEl(device.name, glyphFor(group.protocol), { out: device.id }, {
            current: device.current,
            offline: down(device.current),
            meta: down(device.current)
              ? "offline"
              : device.lock?.needs && !device.lock.configured
                ? `${device.lock.needs} needed`
                : device.detail || group.protocol,
            extra: device.lock?.needs || device.lock?.configured
              ? ghost(device.lock.needs && !device.lock.configured ? "unlock" : "auth", { auth: "open", gid: device.id }, "edit")
              : null,
          }),
        );
      }
    };
    const others = outputs.groups.filter((g) => g.protocol === "browser");
    others.forEach((group) => paintGroup(group, local));
    const groups = section(el, "group");
    for (const combo of outputs.combos || [])
      groups.append(
        tileEl(combo.name, "group", { out: combo.id }, {
          current: combo.current,
          offline: down(combo.current),
          meta: down(combo.current) ? "offline" : combo.members.map((m) => m.name).join(", "),
          extra: ghost("edit", { grp: "edit", gid: combo.id }, "edit"),
        }),
      );
    groups.append(tileEl("new group", "plus", { grp: "new" }, { meta: "combine outputs", cls: "add" }));
    for (const group of [...outputs.groups].filter((g) => g.protocol !== "browser").sort((x, y) => x.protocol.localeCompare(y.protocol)))
      paintGroup(group, null);
    if (!found) el.append(node("div", "note muted", "no speakers found yet, scan again"));
  };
  let outputsSeen = "";
  let localPicked = false;
  const loadOutputs = async () => {
    try {
      const res = await fetch("/outputs", { credentials: "same-origin" });
      if (res.ok) takeOutputs(await res.json());
    } catch {}
  };
  const takeOutputs = (next) => {
    const sig = JSON.stringify(next);
    const changed = sig !== outputsSeen;
    outputsSeen = sig;
    outputs = next;
    if (panel !== "output" || !changed) return;
    if (draft) {
      if (adoptRemote()) renderOutputs();
    } else if (!authView) renderOutputs();
  };

  const adoptRemote = () => {
    if (!draft?.id || livePending || liveJob) return false;
    const combo = (outputs?.combos || []).find((c) => c.id === draft.id);
    if (!combo) {
      draft = null;
      return true;
    }
    const keeping = document.activeElement;
    const typingName = keeping?.dataset?.gname !== undefined;
    const same =
      (typingName || combo.name === draft.name) &&
      combo.members.length === draft.members.size &&
      combo.members.every((m) => {
        const mine = draft.members.get(m.id);
        const field = keeping?.dataset?.gdelay === m.id;
        return mine && (field || Number(mine.delay) === m.delayMs);
      });
    if (same) return false;
    if (!typingName) draft.name = combo.name;
    const delays = new Map(draft.delays);
    const members = new Map();
    for (const m of combo.members) {
      const field = keeping?.dataset?.gdelay === m.id;
      const delay = field ? draft.delays.get(m.id) ?? String(m.delayMs) : String(m.delayMs);
      delays.set(m.id, delay);
      members.set(m.id, { name: m.name, delay });
    }
    draft.members = members;
    draft.delays = delays;
    return true;
  };

  let pcmCtx = null;
  let pcmGain = null;
  let pcmVolume = 1;
  let pcmNext = 0;
  let pcmSkew = 0;
  const pcmLive = new Set();
  const pcmEnsure = () => {
    if (!pcmCtx) {
      const Context = window.AudioContext || window.webkitAudioContext;
      try {
        pcmCtx = new Context({ sampleRate: 48000, latencyHint: "interactive" });
      } catch {
        pcmCtx = new Context();
      }
      pcmGain = pcmCtx.createGain();
      pcmGain.gain.value = pcmVolume;
      pcmGain.connect(pcmCtx.destination);
    }
    if (pcmCtx.state !== "running") pcmCtx.resume().catch(() => {});
    return pcmCtx;
  };
  const pcmFlush = () => {
    for (const source of pcmLive) {
      try {
        source.stop();
      } catch {}
    }
    pcmLive.clear();
    pcmNext = 0;
    pcmSkew = 0;
  };
  const pcmVolumeTo = (v) => {
    pcmVolume = Math.min(1, gain(v ?? 1));
    if (pcmGain) pcmGain.gain.value = pcmVolume;
  };
  const unlockPcm = () => {
    pcmEnsure();
    if (pcmCtx.state !== "running") return;
    for (const type of ["pointerdown", "touchend", "keydown"]) document.removeEventListener(type, unlockPcm, true);
  };
  for (const type of ["pointerdown", "touchend", "keydown"]) document.addEventListener(type, unlockPcm, true);

  window.addEventListener("juuz:pcm", (e) => {
    const buf = e.detail;
    if (!(buf instanceof ArrayBuffer) || buf.byteLength < 16) return;
    const view = new DataView(buf);
    if (view.getUint8(0) === 2) {
      pcmFlush();
      return;
    }
    const offset = window.juuzLive?.perfOffset();
    if (!pcmCtx || pcmCtx.state !== "running" || offset == null) {
      pcmEnsure();
      return;
    }
    const rate = view.getUint16(1, true) * 100;
    const latencyMs = view.getInt32(4, true);
    const dueUs = Number(view.getBigInt64(8, true));
    const frames = (buf.byteLength - 16) >> 2;
    if (!frames) return;
    const heard = dueUs / 1000 + latencyMs - offset;
    const stamp = pcmCtx.getOutputTimestamp ? pcmCtx.getOutputTimestamp() : null;
    let at;
    if (stamp && stamp.performanceTime > 0 && stamp.contextTime > 0) {
      at = stamp.contextTime + (heard - stamp.performanceTime) / 1000;
    } else {
      const hardware = pcmCtx.outputLatency || pcmCtx.baseLatency || 0;
      at = pcmCtx.currentTime + (heard - performance.now()) / 1000 - hardware;
    }
    let stretch = 1;
    if (pcmNext && Math.abs(at - pcmNext) < 0.05) {
      pcmSkew = pcmSkew * 0.9 + (at - pcmNext) * 0.1;
      stretch = 1 - Math.max(-0.005, Math.min(0.005, pcmSkew * 0.25));
      at = pcmNext;
    } else {
      pcmSkew = 0;
    }
    if (at < pcmCtx.currentTime) {
      if (pcmCtx.currentTime - at > 0.04) {
        pcmNext = 0;
        return;
      }
      at = pcmCtx.currentTime;
    }
    const samples = new Int16Array(buf, 16, frames * 2);
    const audio = pcmCtx.createBuffer(2, frames, rate);
    const left = audio.getChannelData(0);
    const right = audio.getChannelData(1);
    for (let i = 0; i < frames; i++) {
      left[i] = samples[2 * i] / 32768;
      right[i] = samples[2 * i + 1] / 32768;
    }
    const source = pcmCtx.createBufferSource();
    source.buffer = audio;
    source.playbackRate.value = stretch;
    source.connect(pcmGain);
    source.onended = () => pcmLive.delete(source);
    pcmLive.add(source);
    source.start(at);
    pcmNext = at + frames / rate / stretch;
  });

  window.addEventListener("juuz:push", (e) => {
    const { t, d } = e.detail || {};
    if (t === "queue") {
      pushed = d;
      applyRemote(d);
    } else if (t === "outputs") takeOutputs(d);
  });

  const switchOutput = async (id) => {
    let handoff = null;
    let back = null;
    if (!remote && id && queue.length && current()) {
      handoff = {
        ids: queue.map((song) => song.id),
        index,
        offset: Math.floor(audio.currentTime || 0),
        play: !audio.paused,
      };
    }
    if (remote && !id) {
      await fetchRemote();
      if (remoteState?.songs.length) {
        back = {
          songs: remoteState.songs,
          index: remoteState.index,
          offset: Math.floor(remotePosition()),
          play: remoteState.playing,
        };
      }
    }
    const result = await post(`/output?id=${encodeURIComponent(id)}`);
    if (handoff) {
      audio.pause();
      closeOpen(position(), false);
      await post("/queue", {
        action: "set",
        index: handoff.index,
        offset: handoff.offset,
        play: handoff.play ? 1 : 0,
        id: handoff.ids,
      });
      queue = [];
      index = -1;
    }
    await fetchRemote();
    if (back) {
      queue = back.songs.map((song) => ({ ...song }));
      index = back.index;
      load(back.index, back.offset, back.play);
    }
    return result;
  };

  const queueView = () => (remote ? { songs: remoteState?.songs || [], at: remoteState?.index ?? -1 } : { songs: queue, at: index });
  const queueAct = async (what, i, j) => {
    if (remote) {
      const data = { remove: { action: "remove", index: i }, move: { action: "move", from: i, to: j }, clear: { action: "clear" }, jump: { action: "skip", index: i } }[what];
      return remoteCmd(data);
    }
    if (what === "jump") load(i);
    if (what === "remove") {
      if (i < 0 || i >= queue.length) return;
      const wasCurrent = i === index;
      queue.splice(i, 1);
      if (i < index) index--;
      if (wasCurrent) {
        if (queue.length) load(Math.min(i, queue.length - 1));
        else dropLocal();
      }
    }
    if (what === "move") {
      if (i < 0 || j < 0 || i >= queue.length || j >= queue.length || i === j) return;
      const [song] = queue.splice(i, 1);
      queue.splice(j, 0, song);
      if (index === i) index = j;
      else if (i < index && j >= index) index--;
      else if (i > index && j <= index) index++;
    }
    if (what === "clear") {
      queue = [];
      dropLocal();
    }
    markPlaying();
    renderQueue();
  };
  const dropLocal = () => {
    closeOpen(position(), false);
    audio.pause();
    audio.removeAttribute("src");
    audio.load();
    index = -1;
    if (hasRemote()) idleBar("");
    else if (bar) bar.hidden = true;
  };

  let queueSeen = "";
  const renderQueue = (force = false) => {
    if (qdrag) return;
    const el = panelEl("queue");
    const view = queueView();
    const sig = JSON.stringify(view.songs.map((song) => song.id));
    if (!force && sig === queueSeen && el.firstElementChild) {
      el.querySelectorAll("[data-i][aria-current]").forEach((row) => row.removeAttribute("aria-current"));
      el.querySelector(`[data-i="${view.at}"]`)?.setAttribute("aria-current", "true");
      return;
    }
    queueSeen = sig;
    const keep = el.scrollTop;
    const held = el.contains(document.activeElement) ? document.activeElement : null;
    const heldQ = held?.dataset?.q;
    const heldRow = held?.closest?.("[data-i]")?.dataset.i;
    const { songs, at } = queueView();
    el.replaceChildren();
    const head = node("div", "head");
    head.append(node("strong", "", songs.length ? `queue, ${songs.length} songs` : "queue"));
    if (songs.length) {
      const clear = node("button", "ghost", "clear");
      clear.type = "button";
      clear.dataset.q = "clear";
      head.append(clear);
    }
    el.append(head);
    if (!songs.length) {
      el.append(node("div", "note muted", "nothing queued"));
      return;
    }
    const list = node("div", "qlist");
    const from = Math.max(0, at - 5);
    const to = Math.min(songs.length, from + 200);
    for (let i = from; i < to; i++) {
      const song = songs[i];
      const row = node("div", "qrow");
      row.dataset.i = String(i);
      row.dataset.q = "jump";
      if (i === at) row.setAttribute("aria-current", "true");
      if (song.cover) {
        const img = node("img", "cover");
        img.alt = "";
        img.draggable = false;
        img.loading = "lazy";
        img.src = artUrl(song, 64);
        row.append(img);
      } else {
        row.append(node("span", "cover blank"));
      }
      const text = node("div", "text");
      text.append(node("strong", "title", song.title), node("span", "meta muted", song.artist));
      row.append(text);
      if (song.duration) row.append(node("small", "time muted", clock(song.duration)));
      const b = node("button", "icon");
      b.type = "button";
      b.dataset.q = "remove";
      b.setAttribute("aria-label", "remove");
      b.innerHTML = icon("remove");
      row.append(b);
      list.append(row);
    }
    el.append(list);
    if (to < songs.length) el.append(node("div", "note muted", `${songs.length - to} more`));
    if (keep) el.scrollTop = keep;
    else el.querySelector("[aria-current]")?.scrollIntoView({ block: "nearest" });
    if (heldQ) {
      const selector = heldRow !== undefined ? `[data-i="${heldRow}"] [data-q="${heldQ}"]` : `[data-q="${heldQ}"]`;
      el.querySelector(selector)?.focus({ preventScroll: true });
    }
  };

  let qpress = null;
  let qdrag = null;
  let qquiet = 0;
  const qClear = () => {
    if (qpress) clearTimeout(qpress.timer);
    qpress = null;
  };
  const qPlace = () => {
    const d = qdrag;
    const box = d.panel.getBoundingClientRect();
    if (d.y < box.top + 24) d.panel.scrollTop -= 10;
    else if (d.y > box.bottom - 24) d.panel.scrollTop += 10;
    const listTop = d.top0 - d.panel.scrollTop;
    const centre = d.y - d.grab + d.height / 2;
    d.over = Math.max(0, Math.min(d.rows.length - 1, Math.floor((centre - listTop) / d.pitch)));
    d.rows.forEach((row, i) => {
      let shift = 0;
      if (d.from < d.over && i > d.from && i <= d.over) shift = -d.pitch;
      if (d.from > d.over && i >= d.over && i < d.from) shift = d.pitch;
      row.style.transform = shift ? `translateY(${shift}px)` : "";
    });
    d.clone.style.transform = `translateY(${d.y - d.grab}px)`;
    d.raf = requestAnimationFrame(qPlace);
  };
  const beginQDrag = (row, x, y) => {
    const panelNode = row.closest(".popup");
    const rows = [...row.parentElement.querySelectorAll(".qrow[data-i]")];
    const rects = rows.map((r) => r.getBoundingClientRect());
    const box = row.getBoundingClientRect();
    const clone = row.cloneNode(true);
    clone.classList.add("floating");
    Object.assign(clone.style, {
      position: "fixed",
      left: `${box.left}px`,
      top: "0",
      width: `${box.width}px`,
      height: `${box.height}px`,
      transform: `translateY(${box.top}px)`,
      zIndex: "60",
      pointerEvents: "none",
    });
    panelNode.append(clone);
    row.classList.add("lifted");
    rows.forEach((r) => r.classList.add("sliding"));
    const from = rows.indexOf(row);
    qdrag = {
      row,
      rows,
      from,
      over: from,
      clone,
      panel: panelNode,
      y,
      grab: y - box.top,
      height: box.height,
      pitch: rows.length > 1 ? rects[1].top - rects[0].top : box.height,
      top0: rects[0].top + panelNode.scrollTop,
      raf: 0,
    };
    document.documentElement.classList.add("reordering");
    qPlace();
  };
  const endQDrag = (commit) => {
    const d = qdrag;
    if (!d) return;
    cancelAnimationFrame(d.raf);
    qquiet = performance.now() + 400;
    const moved = commit && d.over !== d.from;
    const from = Number(d.row.dataset.i);
    const to = Number(d.rows[d.over].dataset.i);
    d.clone.remove();
    d.row.classList.remove("lifted");
    d.rows.forEach((r) => {
      r.classList.remove("sliding");
      r.style.transform = "";
    });
    document.documentElement.classList.remove("reordering");
    qdrag = null;
    if (moved) queueAct("move", from, to);
  };
  document.addEventListener("pointerdown", (e) => {
    const row = e.target.closest?.(".qrow[data-i]");
    if (!row || qdrag || e.button > 0 || e.target.closest("button")) return;
    qClear();
    const touch = e.pointerType === "touch";
    qpress = { row, x: e.clientX, y: e.clientY, touch, timer: 0 };
    if (touch) {
      qpress.timer = setTimeout(() => {
        const p = qpress;
        qpress = null;
        if (p) beginQDrag(p.row, p.x, p.y);
      }, 350);
    }
  });
  document.addEventListener("pointermove", (e) => {
    if (qdrag) {
      qdrag.y = e.clientY;
      return;
    }
    if (!qpress) return;
    const far = Math.hypot(e.clientX - qpress.x, e.clientY - qpress.y);
    if (qpress.touch) {
      if (far > 8) qClear();
      return;
    }
    if (far > 4) {
      const p = qpress;
      qClear();
      beginQDrag(p.row, e.clientX, e.clientY);
    }
  });
  document.addEventListener("pointerup", () => {
    qClear();
    endQDrag(true);
  });
  document.addEventListener("pointercancel", () => {
    qClear();
    endQDrag(false);
  });
  document.addEventListener(
    "touchmove",
    (e) => {
      if (qdrag && e.cancelable) e.preventDefault();
    },
    { passive: false },
  );
  document.addEventListener(
    "click",
    (e) => {
      if (performance.now() < qquiet) {
        e.preventDefault();
        e.stopPropagation();
      }
    },
    true,
  );

  let liveTimer = null;
  const draftBody = (d = draft) => {
    const members = [...d.members.entries()];
    const data = {
      name: d.name.trim(),
      member: members.map(([id]) => id),
      delay: members.map(([, m]) => Math.round(Number(m.delay) || 0)),
    };
    if (d.id) data.id = d.id;
    return data;
  };
  let liveJob = null;
  let liveDirty = false;
  const adopt = (target, saved) => {
    target.id = saved.saved;
    if (draft !== target) return;
    const panelNode = panelEl("output");
    const editor = panelNode.querySelector(".head.editor");
    if (editor && !editor.querySelector('[data-grp="remove"]')) {
      editor.querySelector('[data-grp="done"]')?.before(ghost("delete", { grp: "remove" }));
    }
    const title = panelNode.querySelector(".head:not(.editor)");
    if (title) title.textContent = "edit group";
  };
  let liveTarget = null;
  let livePending = false;
  const flushLive = (target = draft) => {
    liveTarget = target;
    livePending = false;
    liveDirty = true;
    if (liveJob) return liveJob;
    liveJob = (async () => {
      await null;
      while (liveDirty) {
        liveDirty = false;
        const t = liveTarget;
        if (!t || !t.name.trim() || !t.members.size) continue;
        try {
          const saved = await post("/group", draftBody(t));
          if (!t.id) adopt(t, saved);
          outputs = saved;
        } catch {}
      }
      liveJob = null;
      if (!draft && panel === "output") renderOutputs();
    })();
    return liveJob;
  };
  const pushLive = () => {
    clearTimeout(liveTimer);
    livePending = true;
    liveTimer = setTimeout(() => flushLive(), 200);
  };
  document.addEventListener("input", (e) => {
    if (authView && bar?.contains(e.target)) {
      if (e.target.dataset.apw !== undefined) authView.password = e.target.value;
      if (e.target.dataset.apin !== undefined) authView.pin = e.target.value;
      return;
    }
    if (!draft || !bar?.contains(e.target)) return;
    const t = e.target;
    if (t.dataset.gname !== undefined) {
      draft.name = t.value;
      pushLive();
    } else if (t.dataset.gdelay !== undefined) {
      draft.delays.set(t.dataset.gdelay, t.value);
      const m = draft.members.get(t.dataset.gdelay);
      if (m) {
        m.delay = t.value;
        pushLive();
      }
    }
  });

  document.addEventListener("click", async (e) => {
    const pick = e.target.closest?.("[data-gcheck]");
    if (pick && draft && bar?.contains(pick)) {
      e.preventDefault();
      const id = pick.dataset.gcheck;
      const tile = pick.closest(".tile");
      if (draft.members.has(id)) {
        draft.members.delete(id);
        tile.removeAttribute("data-picked");
      } else {
        draft.members.set(id, { name: pool().get(id)?.name || "", delay: draft.delays.get(id) ?? 0 });
        tile.setAttribute("data-picked", "");
      }
      pushLive();
      return;
    }
    const grp = e.target.closest?.("[data-grp]");
    if (grp && bar?.contains(grp)) {
      e.preventDefault();
      const act = grp.dataset.grp;
      try {
        if (act === "new") draft = draftFrom(null);
        else if (act === "edit") draft = draftFrom((outputs?.combos || []).find((c) => c.id === grp.dataset.gid));
        else if (act === "done") {
          clearTimeout(liveTimer);
          const target = draft;
          draft = null;
          if (livePending) flushLive(target);
        } else if (act === "remove") {
          clearTimeout(liveTimer);
          await liveJob;
          outputs = await post("/group/delete", { id: draft.id });
          draft = null;
        } else return;
      } catch {}
      renderOutputs();
      return;
    }
    const auth = e.target.closest?.("[data-auth]");
    if (auth && bar?.contains(auth)) {
      e.preventDefault();
      const act = auth.dataset.auth;
      try {
        if (act === "open") openAuth(auth.dataset.gid);
        else if (act === "back") authView = null;
        else if (authView) {
          const id = authView.id;
          authView.message = "";
          if (act === "password") {
            outputs = await post("/airplay", { action: "password", id, password: authView.password });
            authView.message = authView.password ? "password saved" : "password cleared";
            authView.password = "";
          } else if (act === "pair") {
            outputs = await post("/airplay", { action: "pair", id });
            authView.step = "pin";
            authView.message = "enter the code the device is showing";
          } else if (act === "finish") {
            outputs = await post("/airplay", { action: "finish", id, pin: authView.pin });
            authView.step = "idle";
            authView.pin = "";
            authView.message = "paired";
          } else if (act === "forget") {
            outputs = await post("/airplay", { action: "forget", id });
            authView.step = "idle";
            authView.message = "forgotten";
          }
        }
      } catch (failure) {
        if (authView) authView.message = failure.detail || "that did not work";
      }
      renderOutputs();
      return;
    }
    const out = e.target.closest?.("[data-out]");
    if (out && bar?.contains(out)) {
      e.preventDefault();
      const locked = findDevice(out.dataset.out)?.lock;
      if (locked?.needs && !locked.configured) {
        openAuth(out.dataset.out);
        renderOutputs();
        return;
      }
      try {
        if (out.dataset.out === "scan") {
          outputs = await post("/outputs/scan");
        } else {
          localPicked = out.dataset.out === "";
          outputs = await switchOutput(out.dataset.out);
        }
      } catch {}
      renderOutputs();
      return;
    }
    const q = e.target.closest?.("[data-q]");
    if (q && bar?.contains(q)) {
      e.preventDefault();
      const i = Number(q.closest("[data-i]")?.dataset.i);
      const what = q.dataset.q;
      queueAct(what, i);
    }
  });

  if (hasRemote()) {
    mount();
    idleBar("");
    setVolume(volume, true);
    fetchRemote();
  }

  document.addEventListener("click", (e) => {
    const album = e.target.closest("[data-play-album]");
    if (album) {
      e.preventDefault();
      playFrom(`/library?view=songs&status=all&album=${encodeURIComponent(album.dataset.playAlbum)}`);
      return;
    }
    const list = e.target.closest("[data-play-list]");
    if (list) {
      e.preventDefault();
      playFrom(`/library?view=playlists&playlist=${encodeURIComponent(list.dataset.playList)}`);
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
    const target = e.submitter?.formAction || form.action;
    const path = new URL(target, location.href).pathname;
    if (path === "/login" || path === "/logout") return;
    e.preventDefault();
    try {
      const res = await fetch(target, {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body: new URLSearchParams(new FormData(form, e.submitter)),
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
      swapped = true;
      markPlaying();
    } catch {
      form.submit();
    }
  });

  let swapped = false;
  let navigation = 0;
  document.addEventListener("click", async (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    if (!bar || bar.hidden || !bar.isConnected) return;
    const link = e.target.closest?.("a[href]");
    if (!link || link.target || link.hasAttribute("download") || bar.contains(link)) return;
    const url = new URL(link.href, location.href);
    if (url.origin !== location.origin || url.hash) return;
    if (url.pathname === "/logout" || url.pathname === "/login") return;
    e.preventDefault();
    const mine = ++navigation;
    try {
      const res = await fetch(url, { credentials: "same-origin" });
      const kind = res.headers.get("content-type") || "";
      const dest = new URL(res.url);
      if (mine !== navigation) return;
      if (!res.ok || !kind.includes("text/html") || dest.pathname === "/login") {
        location.href = url.href;
        return;
      }
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const next = doc.querySelector(".screen");
      const now = document.querySelector(".screen");
      if (!next || !now || mine !== navigation) {
        if (mine === navigation) location.href = url.href;
        return;
      }
      closePanels();
      now.replaceWith(next);
      document.title = doc.title || document.title;
      history.pushState(null, "", res.url);
      swapped = true;
      window.scrollTo(0, 0);
      markPlaying();
    } catch {
      location.href = url.href;
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
      const hidden = select.querySelector("input[type=hidden]");
      const changed = hidden.value !== option.dataset.value;
      hidden.value = option.dataset.value;
      select.querySelector(".value").textContent = option.textContent;
      select.querySelectorAll("[role=option]").forEach((o) => {
        o.setAttribute("aria-selected", o === option ? "true" : "false");
      });
      closeSelects(null);
      select.querySelector(".select-trigger").focus();
      if (changed) hidden.dispatchEvent(new Event("change", { bubbles: true }));
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
      const want = new Set(view);
      for (const child of [...host.children]) if (!want.has(child)) child.remove();
      view.forEach((el, i) => {
        const at = host.children[i];
        if (at !== el) host.insertBefore(el, at || null);
      });
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
    if (!current() && !swapped) return;
    try {
      const res = await fetch(location.pathname + location.search, { credentials: "same-origin" });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const next = doc.querySelector(".screen");
      const now = document.querySelector(".screen");
      if (next && now && next.innerHTML !== now.innerHTML) {
        now.replaceWith(next);
        document.title = doc.title || document.title;
        markPlaying();
        window.dispatchEvent(new Event("juuz:navigated"));
      }
    } catch {}
  });

  new MutationObserver(() => {
    if (bar && !bar.isConnected && current()) {
      mount();
      render();
    }
  }).observe(document.body, { childList: true });

  let picked = [];
  let pickTarget = "";
  let picking = 0;
  const renderPicks = () => {
    const form = document.querySelector("form[data-pick]");
    const target = form ? form.elements.id.value : "";
    if (target !== pickTarget) {
      pickTarget = target;
      picked = [];
    }
    document.querySelectorAll("[data-pick]:not(form)").forEach((btn) => {
      const at = picked.indexOf(btn.dataset.pick);
      const pressed = at >= 0 ? "true" : "false";
      if (btn.getAttribute("aria-pressed") !== pressed) btn.setAttribute("aria-pressed", pressed);
      const n = btn.querySelector(".n");
      const text = at >= 0 ? String(at + 1) : "";
      if (n && n.textContent !== text) n.textContent = text;
    });
    if (!form) return;
    const label = `add ${picked.length} song${picked.length === 1 ? "" : "s"}`;
    const submit = form.querySelector("button");
    if (submit.textContent !== label) submit.textContent = label;
    submit.disabled = picked.length === 0;
  };
  const LONG_PRESS = 350;
  const TOUCH_SLOP = 8;
  const MOUSE_SLOP = 4;
  const EDGE = 64;
  let press = null;
  let drag = null;
  let quiet = 0;

  const reorderRow = (el) => {
    const row = el?.closest?.(".row[data-slot]");
    return row && row.dataset.slot !== "" ? row : null;
  };

  const place = () => {
    const d = drag;
    const listTop = d.top0 - window.scrollY;
    const centre = d.y - d.grab + d.height / 2;
    const over = Math.max(0, Math.min(d.rows.length - 1, Math.floor((centre - listTop) / d.pitch)));
    if (over !== d.over) {
      d.over = over;
      if (navigator.vibrate) navigator.vibrate(4);
    }
    d.rows.forEach((row, i) => {
      let shift = 0;
      if (d.from < d.over && i > d.from && i <= d.over) shift = -d.pitch;
      if (d.from > d.over && i >= d.over && i < d.from) shift = d.pitch;
      row.style.transform = shift ? `translateY(${shift}px)` : "";
    });
    d.clone.style.transform = `translateY(${d.y - d.grab}px)`;
  };

  const frame = () => {
    if (!drag) return;
    const speed = (EDGE - Math.min(drag.y, window.innerHeight - drag.y)) / EDGE;
    if (speed > 0) window.scrollBy(0, (drag.y < EDGE ? -1 : 1) * Math.min(1, speed) * 18);
    place();
    drag.raf = requestAnimationFrame(frame);
  };

  const beginDrag = (row, x, y) => {
    const rows = [...row.parentElement.children].filter((r) => r.matches(".row[data-slot]"));
    const rects = rows.map((r) => r.getBoundingClientRect());
    const box = row.getBoundingClientRect();
    const clone = row.cloneNode(true);
    clone.classList.add("floating");
    Object.assign(clone.style, {
      position: "fixed",
      left: `${box.left}px`,
      top: "0",
      width: `${box.width}px`,
      height: `${box.height}px`,
      transform: `translateY(${box.top}px)`,
      zIndex: "50",
      pointerEvents: "none",
    });
    document.body.append(clone);
    row.classList.add("lifted");
    rows.forEach((r) => r.classList.add("sliding"));
    const from = rows.indexOf(row);
    drag = {
      row,
      rows,
      from,
      over: from,
      clone,
      x,
      y,
      grab: y - box.top,
      height: box.height,
      pitch: rows.length > 1 ? rects[1].top - rects[0].top : box.height,
      top0: rects[0].top + window.scrollY,
      raf: 0,
    };
    document.documentElement.classList.add("reordering");
    if (navigator.vibrate) navigator.vibrate(12);
    frame();
  };

  const endDrag = (commit) => {
    const d = drag;
    if (!d) return;
    drag = null;
    cancelAnimationFrame(d.raf);
    quiet = performance.now() + 400;
    const done = () => {
      d.clone.remove();
      d.row.classList.remove("lifted");
      d.rows.forEach((r) => {
        r.classList.remove("sliding");
        r.style.transform = "";
      });
      document.documentElement.classList.remove("reordering");
    };
    const moved = commit && d.over !== d.from;
    const slot = moved ? d.over : d.from;
    d.clone.style.transition = "transform 160ms cubic-bezier(0.2, 0.8, 0.2, 1)";
    d.clone.style.transform = `translateY(${d.top0 - window.scrollY + slot * d.pitch}px)`;
    setTimeout(() => {
      if (!moved) return done();
      const anchor = d.rows[d.over];
      const index = Number(d.row.dataset.slot);
      const to = Number(anchor.dataset.slot);
      const body = new URLSearchParams({
        id: d.row.querySelector("input[name=id]").value,
        song: d.row.dataset.song,
        index: String(index),
        to: String(to),
      });
      d.rows.forEach((r) => {
        r.classList.remove("sliding");
        r.style.transform = "";
        const slot = Number(r.dataset.slot);
        let next = slot;
        if (r === d.row) next = to;
        else if (index < to && slot > index && slot <= to) next = slot - 1;
        else if (index > to && slot >= to && slot < index) next = slot + 1;
        r.dataset.slot = String(next);
        const hidden = r.querySelector("input[name=index]");
        if (hidden) hidden.value = String(next);
      });
      if (d.from < d.over) anchor.after(d.row);
      else anchor.before(d.row);
      done();
      fetch("/library/playlists/shift", {
        method: "POST",
        credentials: "same-origin",
        headers: { "content-type": "application/x-www-form-urlencoded" },
        body,
        redirect: "manual",
      })
        .then((res) => {
          if (res.status !== 204) location.reload();
        })
        .catch(() => location.reload());
    }, 170);
  };

  const clearPress = () => {
    if (press) clearTimeout(press.timer);
    press = null;
  };

  document.addEventListener("pointerdown", (e) => {
    if (drag || e.button > 0) return;
    const row = reorderRow(e.target);
    if (!row) return;
    const touch = e.pointerType === "touch";
    if (touch ? e.target.closest("form") : e.target.closest("a, button, input, select, textarea, label")) return;
    clearPress();
    press = { row, x: e.clientX, y: e.clientY, touch, timer: 0 };
    if (touch) {
      press.timer = setTimeout(() => {
        const p = press;
        press = null;
        if (p) beginDrag(p.row, p.x, p.y);
      }, LONG_PRESS);
    }
  });

  document.addEventListener("pointermove", (e) => {
    if (drag) {
      drag.x = e.clientX;
      drag.y = e.clientY;
      return;
    }
    if (!press) return;
    const far = Math.hypot(e.clientX - press.x, e.clientY - press.y);
    if (press.touch) {
      if (far > TOUCH_SLOP) clearPress();
      return;
    }
    if (far > MOUSE_SLOP) {
      const p = press;
      clearPress();
      beginDrag(p.row, e.clientX, e.clientY);
    }
  });

  document.addEventListener("pointerup", () => {
    clearPress();
    endDrag(true);
  });
  document.addEventListener("pointercancel", () => {
    clearPress();
    endDrag(false);
  });
  document.addEventListener(
    "touchmove",
    (e) => {
      if (drag && e.cancelable) e.preventDefault();
    },
    { passive: false },
  );
  document.addEventListener("contextmenu", (e) => {
    if (drag || press?.touch) e.preventDefault();
  });
  document.addEventListener("dragstart", (e) => {
    if (reorderRow(e.target) || e.target.closest?.(".qrow")) e.preventDefault();
  });
  document.addEventListener(
    "click",
    (e) => {
      if (performance.now() < quiet) {
        e.preventDefault();
        e.stopPropagation();
      }
    },
    true,
  );
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key === "Escape" && drag) {
        e.preventDefault();
        e.stopPropagation();
        endDrag(false);
      }
    },
    true,
  );

  const armed = new WeakMap();
  document.addEventListener(
    "submit",
    (e) => {
      const form = e.target;
      if (!form.matches?.("form[data-confirm]")) return;
      const btn = form.querySelector("button");
      if (form.dataset.armed) {
        clearTimeout(armed.get(form));
        delete form.dataset.armed;
        return;
      }
      e.preventDefault();
      e.stopImmediatePropagation();
      form.dataset.armed = "1";
      btn.dataset.label = btn.textContent;
      btn.textContent = "confirm delete";
      armed.set(
        form,
        setTimeout(() => {
          delete form.dataset.armed;
          btn.textContent = btn.dataset.label;
        }, 5000),
      );
    },
    true,
  );
  document.addEventListener("mousedown", (e) => {
    if (e.target.closest("[data-pick]:not(form)")) e.preventDefault();
  });
  document.addEventListener("click", (e) => {
    const btn = e.target.closest("[data-pick]:not(form)");
    if (!btn) return;
    e.preventDefault();
    const at = picked.indexOf(btn.dataset.pick);
    if (at >= 0) picked.splice(at, 1);
    else picked.push(btn.dataset.pick);
    renderPicks();
  });
  document.addEventListener(
    "submit",
    (e) => {
      const form = e.target;
      if (!form.matches?.("form[data-pick]")) return;
      if (!picked.length) {
        e.preventDefault();
        e.stopImmediatePropagation();
        return;
      }
      form.elements.songs.value = picked.join(",");
      picked = [];
    },
    true,
  );
  new MutationObserver(() => {
    cancelAnimationFrame(picking);
    picking = requestAnimationFrame(renderPicks);
  }).observe(document.body, { childList: true, subtree: true });

  document.addEventListener("click", (e) => {
    const name = e.target.closest("[data-rename-open]");
    if (!name) return;
    const form = name.closest("form[data-rename]");
    form.setAttribute("data-open", "");
    const input = form.elements.name;
    input.focus();
    input.select();
  });
  document.addEventListener(
    "keydown",
    (e) => {
      if (e.key !== "Escape") return;
      const form = e.target.closest?.("form[data-rename][data-open]");
      if (!form) return;
      e.preventDefault();
      e.stopPropagation();
      form.elements.name.value = form.elements.name.defaultValue;
      form.removeAttribute("data-open");
      form.querySelector("[data-rename-open]").focus();
    },
    true,
  );

  window.addEventListener("juuz:navigated", () => {
    if (current()) {
      mount();
      render();
    }
  });
})();
