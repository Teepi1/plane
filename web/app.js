// Plane on your phone. Talks to planeweb.py on the laptop; everything runs there.
"use strict";

const $ = (s, el = document) => el.querySelector(s);
const esc = s => String(s ?? "").replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const app = $("#app");

// ---------------------------------------------------------------- helpers

async function api(path, body) {
  const opts = body === undefined ? {} : {
    method: "POST", headers: { "Content-Type": "application/json", "X-Plane": "1" }, body: JSON.stringify(body),
  };
  let r;
  try { r = await fetch(path, opts); }
  catch { throw new Error("Can't reach your laptop. It may be asleep or off Tailscale."); }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(j.error || `Something went wrong (${r.status}).`);
  return j;
}

function store(key, value) {
  try {
    if (value === undefined) return JSON.parse(localStorage.getItem(key) || "null");
    localStorage.setItem(key, JSON.stringify(value));
  } catch { return null; }
}

function clock(ts) {
  return new Date(ts).toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

function resetLabel(secs) {
  if (!secs) return "";
  const d = new Date(secs * 1000);
  return d - Date.now() < 20 * 3600e3 ? clock(d) : d.toLocaleDateString([], { weekday: "short" });
}

function ago(ms) {
  if (!ms) return "";
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

const cap = s => s ? s[0].toUpperCase() + s.slice(1) : s;

function size(n) {
  if (!n && n !== 0) return "";
  return n < 1e3 ? `${n} B` : n < 1e6 ? `${Math.round(n / 1e3)} KB` : `${(n / 1e6).toFixed(1)} MB`;
}

// Photos: iPhone HEIC becomes JPEG (Claude can't read HEIC), and big photos shrink to 2400 px
// on the long side so they upload fast. Everything else goes up as it is.
async function prepareFile(file) {
  const heic = /image\/hei[cf]/i.test(file.type) || /\.hei[cf]$/i.test(file.name || "");
  if (!heic && !/^image\/(jpeg|png|webp)$/i.test(file.type)) return file;
  const url = URL.createObjectURL(file);
  try {
    const img = await new Promise((ok, bad) => { const im = new Image(); im.onload = () => ok(im); im.onerror = bad; im.src = url; });
    const scale = Math.min(1, 2400 / Math.max(img.naturalWidth, img.naturalHeight));
    if (scale === 1 && !heic) return file;
    const c = document.createElement("canvas");
    c.width = Math.round(img.naturalWidth * scale); c.height = Math.round(img.naturalHeight * scale);
    c.getContext("2d").drawImage(img, 0, 0, c.width, c.height);
    const png = file.type === "image/png";
    const blob = await new Promise(ok => c.toBlob(ok, png ? "image/png" : "image/jpeg", 0.88));
    if (!blob) return file;
    const base = (file.name || "photo").replace(/\.[^.]+$/, "");
    return new File([blob], `${base}.${png ? "png" : "jpg"}`, { type: blob.type });
  } catch { return file; }
  finally { URL.revokeObjectURL(url); }
}

// "Attached file: /path" lines in your messages show as a preview or a file chip
function youBody(text) {
  const files = [], rest = [];
  for (const l of String(text).split("\n")) {
    const m = /^Attached file: (\/.+)$/.exec(l.trim());
    m ? files.push(m[1]) : rest.push(l);
  }
  const words = rest.join("\n").trim();
  return esc(words) + (files.length ? `<span class="files">${files.map(fileChip).join("")}</span>` : "");
}

function fileChip(p) {
  const name = p.split("/").pop().replace(/^\d{6}-[0-9a-f]{4}-/, "");
  const dir = S.snap && S.snap.uploadsDir;
  const url = dir && p.startsWith(dir + "/") ? "/api/file/" + p.slice(dir.length + 1).split("/").map(encodeURIComponent).join("/") : "";
  if (url && /\.(jpe?g|png|gif|webp)$/i.test(p)) return `<a class="fchip img" href="${url}" target="_blank" rel="noopener"><img src="${url}" alt="${esc(name)}" loading="lazy"></a>`;
  return `<span class="fchip">File · ${esc(name)}</span>`;
}

// A small, safe markdown: escape first, then code, emphasis, links, lists, headings.
function md(src) {
  const blocks = [];
  let text = String(src ?? "").replace(/```[^\n]*\n([\s\S]*?)(```|$)/g, (_, code) => {
    blocks.push(`<div class="codeblock"><pre><code>${esc(code.replace(/\n$/, ""))}</code></pre><button class="copy" data-act="copy" aria-label="Copy this code"><span>Copy</span></button></div>`);
    return `\u0000${blocks.length - 1}\u0000`;
  });
  const inline = t => esc(t)
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[\s(])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>")
    .replace(/\[([^\]]+)\]\((https?:\/\/[^)\s]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>');
  const out = [];
  for (const para of text.split(/\n{2,}/)) {
    const lines = para.split("\n");
    if (/^\u0000\d+\u0000$/.test(para.trim())) { out.push(blocks[+para.trim().slice(1, -1)]); continue; }
    if (lines.every(l => /^\s*([-*•]|\d+\.)\s+/.test(l) || /^\s{2,}\S/.test(l))) {
      const ordered = /^\s*\d+\./.test(lines[0]);
      const items = [];
      for (const l of lines) {
        if (/^\s*([-*•]|\d+\.)\s+/.test(l)) items.push(l.replace(/^\s*([-*•]|\d+\.)\s+/, ""));
        else if (items.length) items[items.length - 1] += " " + l.trim();
      }
      out.push(`<${ordered ? "ol" : "ul"}>${items.map(i => `<li>${inline(i)}</li>`).join("")}</${ordered ? "ol" : "ul"}>`);
      continue;
    }
    if (/^#{1,6}\s/.test(lines[0]) && lines.length === 1) { out.push(`<h4>${inline(lines[0].replace(/^#+\s*/, ""))}</h4>`); continue; }
    if (lines.every(l => /^\s*\|/.test(l))) { out.push(`<pre><code>${esc(para)}</code></pre>`); continue; }
    out.push(`<p>${lines.map(l => /^#{1,6}\s/.test(l) ? `<strong>${inline(l.replace(/^#+\s*/, ""))}</strong>` : inline(l)).join("<br>")}</p>`);
  }
  return out.join("").replace(/\u0000(\d+)\u0000/g, (_, i) => blocks[+i]);
}

// Copy a code block's text. The clipboard API first; the old select-and-copy where it's refused.
async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* fall through */ }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.cssText = "position:fixed;top:0;left:0;opacity:0";
  document.body.append(ta);
  ta.select();
  ta.setSelectionRange(0, text.length);
  let ok = false;
  try { ok = document.execCommand("copy"); } catch { ok = false; }
  ta.remove();
  return ok;
}

async function copyBlock(btn) {
  const code = btn.closest(".codeblock")?.querySelector("code");
  const label = btn.querySelector("span");
  if (!code || !label) return;
  const ok = await copyText(code.textContent);
  label.textContent = ok ? "Copied" : "Couldn't copy";
  btn.classList.toggle("done", ok);
  if (!ok) {  // leave it selected, so press-and-hold › Copy works
    const r = document.createRange();
    r.selectNodeContents(code);
    const sel = getSelection();
    sel.removeAllRanges();
    sel.addRange(r);
  }
  clearTimeout(btn._t);
  btn._t = setTimeout(() => { label.textContent = "Copy"; btn.classList.remove("done"); }, ok ? 2000 : 4000);
}

let toastTimer;
function toast(text) {
  let el = $(".toast");
  if (!el) { el = document.createElement("div"); el.className = "toast"; el.setAttribute("role", "status"); document.body.append(el); }
  el.textContent = text;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, 2600);
}

// Server-sent events that come back after iOS suspends the app.
function stream(url, handlers) {
  let es = null, closed = false, retry = 1000, timer = null;
  const open = () => {
    if (closed) return;
    es = new EventSource(url);
    for (const [kind, fn] of Object.entries(handlers)) {
      if (kind === "open" || kind === "error") continue;
      es.addEventListener(kind, e => { retry = 1000; fn(JSON.parse(e.data)); });
    }
    es.onopen = () => { retry = 1000; handlers.open && handlers.open(); };
    es.onerror = () => {
      handlers.error && handlers.error();
      if (es.readyState === EventSource.CLOSED) { clearTimeout(timer); timer = setTimeout(open, retry); retry = Math.min(retry * 2, 15000); }
    };
  };
  // Leaving the app closes the connection at once: iOS can keep a backgrounded app's
  // connection open, and the laptop would think you're still looking (no notifications).
  const wake = () => {
    if (document.hidden) { clearTimeout(timer); es && es.close(); return; }
    if (es && es.readyState === EventSource.CLOSED) { clearTimeout(timer); open(); }
  };
  document.addEventListener("visibilitychange", wake);
  open();
  return {
    close() { closed = true; clearTimeout(timer); es && es.close(); document.removeEventListener("visibilitychange", wake); },
    restart() { es && es.close(); open(); },
  };
}

// Keep the layout above the iOS keyboard.
function fitViewport() {
  const vv = window.visualViewport;
  if (!vv) return;
  // Only while the keyboard is up: otherwise the CSS fills the whole screen (sizing from the
  // viewport left the app short by the status bar's height, a blank band at the bottom).
  const keyboard = window.innerHeight - vv.height > 80;
  app.classList.toggle("kb", keyboard);
  app.style.height = keyboard ? `${vv.height}px` : "";
  app.style.top = keyboard ? `${vv.offsetTop}px` : "";
}
window.visualViewport && visualViewport.addEventListener("resize", fitViewport);
window.visualViewport && visualViewport.addEventListener("scroll", fitViewport);

// ---------------------------------------------------------------- the session list, live

const S = { snap: store("plane:snap"), online: null, gotAt: 0, view: null };
const STATES = {
  "needs you": { ico: "◆", cls: "st-needs" },
  "your turn": { ico: "●", cls: "st-turn" },
  working: { ico: '<span class="spin">◌</span>', cls: "st-work" },
  paused: { ico: "‖", cls: "st-muted" },
  failed: { ico: "↯", cls: "st-bad" },
  standby: { ico: "◐", cls: "st-muted" },
  "cut off": { ico: "↯", cls: "st-bad" },
};

let offTimer = null;
stream("/api/events", {
  state(snap) {
    S.snap = snap; S.gotAt = Date.now(); setOnline(true);
    store("plane:snap", { ...snap, cachedAt: Date.now() });
    S.view && S.view.update && S.view.update();
  },
  tick() { S.gotAt = Date.now(); setOnline(true); },
  error() { clearTimeout(offTimer); offTimer = setTimeout(() => setOnline(false), 4000); },
});

function setOnline(on) {
  clearTimeout(offTimer);
  if (on === S.online) return;
  const was = S.online;
  S.online = on;
  if (on && was === false && S.everOnline) toast("Connected");
  if (on) S.everOnline = true;
  S.view && S.view.update && S.view.update();
}
setTimeout(() => { if (!S.online) { S.online = null; setOnline(false); } }, 5000);

const sessionById = id => (S.snap?.sessions || []).find(s => s.id === id);

// ---------------------------------------------------------------- views

function sheet(html) {
  const el = document.createElement("div");
  el.className = "shade";
  el.innerHTML = `<div class="sheet" role="dialog">${html}</div>`;
  el.addEventListener("click", e => { if (e.target === el || e.target.closest("[data-shut]")) el.remove(); });
  document.body.append(el);
  return el;
}

function confirmSheet(title, sub, label, fn) {
  const el = sheet(`<span class="t">${esc(title)}</span><span class="s">${esc(sub)}</span>
    <span class="inline-err" hidden></span>
    <button class="btn danger solid" data-go>${esc(label)}</button>
    <button class="btn quiet" data-shut>Cancel</button>`);
  $("[data-go]", el).onclick = async () => {
    const b = $("[data-go]", el);
    b.disabled = true; b.textContent = `${label}…`;
    try { await fn(); el.remove(); }
    catch (e) { b.disabled = false; b.textContent = label; const er = $(".inline-err", el); er.hidden = false; er.textContent = e.message; }
  };
}

// ---- home

function rowHtml(kind, o) {
  let st, ask = "", sub;
  if (kind === "handoff") {
    st = { ico: "→", cls: o.ready ? "st-turn" : "st-muted" };
    st.word = o.ready ? "ready" : "being written";
    sub = `${esc(o.folder)}${o.from ? ` · from “${esc(o.from)}”` : ""}`;
  } else if (kind === "lost") {
    const word = o.cause === "standby" ? "standby" : "cut off";
    st = { ...STATES[word], word };
    sub = o.cause === "standby" ? `${esc(o.folder)} · ${ago(o.lastActive * 1000)}`
      : `${esc(o.folder)} · cut off by a ${o.cause === "reboot" ? "reboot" : "crash"}${o.lastActive ? ` at ${clock(o.lastActive * 1000)}` : ""}`;
    if (o.cause === "standby" && o.needs) ask = `<span class="ask">${esc(o.needs)}</span>`;
  } else {
    st = { ...(STATES[o.state] || STATES["your turn"]), word: o.state };
    if (!o.bg) st.word = "in a laptop window";
    if (o.state === "needs you" && o.needs) ask = `<span class="ask">“${esc(o.needs)}”</span>`;
    else if (o.state === "working" && o.detail) ask = `<span class="sub">${esc(o.detail)}</span>`;
    sub = `${esc(o.folder)}${o.branch ? " · " + esc(o.branch) : ""}${o.startedAt ? " · " + ago(o.startedAt) : ""}`;
  }
  const href = kind === "handoff" ? `#h/${o.id}` : kind === "lost" ? `#l/${o.id}` : `#s/${o.id}`;
  return `<button class="row" data-href="${href}">
    <span class="ico ${st.cls}" aria-hidden="true">${st.ico}</span>
    <span class="name">${esc(o.name || o.title)}</span>
    <span class="tag ${st.cls}">${esc(st.word)}</span>${ask}<span class="sub">${sub}</span></button>`;
}

const isStandalone = () => navigator.standalone === true || matchMedia("(display-mode: standalone)").matches;
const isIOS = () => /iPhone|iPad|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);

function tipHtml() {
  if (store("plane:tipOff")) return "";
  if (isIOS() && !isStandalone()) {
    return `<div class="tip"><span>Add Plane to your Home Screen: tap Share, then Add to Home Screen. Notifications need it.</span><span></span>
      <button class="x" data-act="tipOff">Not now</button></div>`;
  }
  if (!("PushManager" in window) || !("Notification" in window) || Notification.permission !== "default") return "";
  return `<div class="tip"><span>Get a buzz when a session needs you</span><button class="small" data-act="notifOn">Turn on</button>
    <span class="err" hidden></span><button class="x" data-act="tipOff">Not now</button></div>`;
}

// The notifications row under the session list: whether they're on, and a test that
// follows one push all the way to this phone. N survives the list being redrawn.
const N = { msg: "", bad: false, busy: "", reset: false };

function notifHtml() {
  if (!("serviceWorker" in navigator) || !("PushManager" in window) || !("Notification" in window)) return "";
  if (isIOS() && !isStandalone()) return "";
  const perm = Notification.permission;
  if (perm === "default" && !store("plane:tipOff")) return "";  // the tip at the top asks
  let text, btn = "";
  if (perm === "denied") {
    text = "Notifications are blocked for Plane. Turn them on in Settings › Notifications › Plane.";
  } else if (perm === "default") {
    text = "Notifications are off.";
    btn = `<button class="small" data-act="notifOn">Turn on</button>`;
  } else {
    text = N.msg || "Notifications are on.";
    btn = N.busy ? `<button class="small" disabled>${esc(N.busy)}</button>` : `<button class="small" data-act="notifTest">Send test</button>`;
  }
  return `<div class="notif" role="status"><span class="${N.bad ? "st-bad" : ""}">${N.bad ? "⚠ " : ""}${esc(text)}</span>${btn}
    <span class="err" hidden></span>${N.reset && !N.busy ? `<button class="x" data-act="notifReset">Set up again</button>` : ""}</div>`;
}

function notifSet(o) { Object.assign(N, o); S.view && S.view.update && S.view.update(); }

async function testPush() {
  notifSet({ busy: "Sending…", bad: false, reset: false,
    msg: "A test is coming in 5 seconds. Go to the Home Screen or lock the phone to see it arrive." });
  try {
    const reg = await navigator.serviceWorker.ready;
    const sub = await reg.pushManager.getSubscription();
    if (!sub) return notifSet({ busy: "", bad: true, reset: true, msg: "This phone isn't signed up for notifications." });
    await api("/api/push/subscribe", { subscription: sub.toJSON() });  // in case the laptop dropped it
    const { id } = await api("/api/push/test", { endpoint: sub.endpoint, delay: 5 });
    notifSet({ busy: "Waiting…" });
    const until = Date.now() + 30000;
    let r = {};
    while (Date.now() < until) {
      await new Promise(res => setTimeout(res, 1500));
      r = await api(`/api/push/result?id=${id}`).catch(() => r);
      if (r.seen) break;
    }
    const refused = (r.apple || []).filter(s => s !== 201);
    if (r.seen && r.shown) {
      notifSet({ busy: "", msg: "It arrived: this phone got the test and showed it. Didn't see it? Check Focus, and Settings › Notifications › Plane." });
    } else if (r.seen) {
      notifSet({ busy: "", bad: true, msg: `This phone got the test but couldn't show it${r.error ? ` (${r.error})` : ""}.` });
    } else if (!r.sent) {
      notifSet({ busy: "", bad: true, msg: "The laptop didn't send it. Try again." });
    } else if (refused.length) {
      notifSet({ busy: "", bad: true, reset: true, msg: `Apple turned it away (${refused.join(", ")}). Set up again to sign this phone up fresh.` });
    } else {
      notifSet({ busy: "", bad: true, reset: true, msg: "Apple took it, but it never reached this phone. Set up again to sign this phone up fresh." });
    }
  } catch (e) {
    notifSet({ busy: "", bad: true, reset: /signed up/.test(e.message), msg: e.message });
  }
}

async function resetPush() {
  notifSet({ busy: "Setting up…", bad: false, reset: false, msg: "Signing this phone up again…" });
  try {
    const reg = await navigator.serviceWorker.ready;
    const old = await reg.pushManager.getSubscription();
    if (old) {
      await old.unsubscribe().catch(() => {});
      api("/api/push/unsubscribe", { endpoint: old.endpoint }).catch(() => {});
    }
    await subscribe();
    notifSet({ busy: "", msg: "Signed up again. Send a test to check." });
  } catch (e) {
    notifSet({ busy: "", bad: true, reset: true, msg: e.message });
  }
}

function homeView() {
  app.innerHTML = `<div class="top"><div class="top-row"><h1>Plane</h1><span class="live" id="live"></span></div><div class="usage" id="usage"></div></div>
    <div class="scroll" id="list"></div>
    <div class="dock"><button class="btn primary" data-href="#new" id="newBtn">+ New session</button></div>`;
  const list = $("#list");
  let tick;
  function update() {
    const snap = S.snap, off = S.online === false;
    $("#live").className = `live ${off ? "off" : ""}`;
    $("#live").textContent = off ? "Offline" : S.online ? "Live" : "Connecting…";
    const u = snap?.usage || {};
    const bar = (label, x) => x && x.pct != null
      ? `<span>${label}</span><span class="meter"><i class="${x.pct >= 85 ? "hot" : ""}" style="width:${Math.min(100, x.pct)}%"></i></span><span>${Math.round(x.pct)}%${x.resetsAt ? " · " + resetLabel(x.resetsAt) : ""}</span>` : "";
    $("#usage").innerHTML = bar("5-hour", u.five_hour) + bar("Week", u.seven_day);
    $("#newBtn").disabled = off;
    if (!snap) {
      list.innerHTML = off ? offlineHtml() : `<div class="center"><span class="big spin" aria-hidden="true">◌</span><span>Loading your sessions…</span></div>`;
      return;
    }
    const ss = snap.sessions || [];
    const bg = ss.filter(s => s.bg);
    const waiting = bg.filter(s => !s.stopped && (s.state === "needs you" || s.state === "your turn"))
      .sort((a, b) => (a.state === "needs you" ? 0 : 1) - (b.state === "needs you" ? 0 : 1));
    const working = bg.filter(s => !s.stopped && s.state === "working");
    const parked = bg.filter(s => s.stopped);
    const windows = ss.filter(s => !s.bg);
    const cutoff = (snap.lost || []).filter(r => r.cause !== "standby");
    const standby = (snap.lost || []).filter(r => r.cause === "standby");
    const sec = (title, items, kind) => items.length ? `<div class="sec">${title}</div>${items.map(o => rowHtml(kind, o)).join("")}` : "";
    const nothing = !ss.length && !(snap.handoffs || []).length && !(snap.lost || []).length;
    const scroll = list.scrollTop;
    list.innerHTML = (off ? offlineHtml() : tipHtml())
      + (nothing ? `<div class="empty"><b>No sessions running</b><span>Start one here and it runs on your laptop. It shows up in Plane on the laptop too.</span></div>` : "")
      + `<div class="${off ? "dim" : ""}">`
      + sec("Cut off by crash or reboot", cutoff, "lost") + sec("Waiting on you", waiting, "session") + sec("Working", working, "session")
      + (parked.length || standby.length ? `<div class="sec">Paused &amp; standby</div>${parked.map(o => rowHtml("session", o)).join("")}${standby.map(o => rowHtml("lost", o)).join("")}` : "")
      + sec("Handoffs", snap.handoffs || [], "handoff")  // after paused, as in the sidebar
      + sec("In laptop windows", windows, "session")
      + (off ? "" : notifHtml())
      + `<div class="updated" id="updated"></div></div>`;
    list.scrollTop = scroll;
    stamp();
  }
  function stamp() {
    const el = $("#updated");
    if (!el) return;
    if (S.online === false) {
      const at = S.snap?.cachedAt || (S.snap?.updatedAt || 0) * 1000;
      el.textContent = at ? `Last updated ${clock(at)}` : "";
    } else if (S.gotAt) {
      const s = Math.round((Date.now() - S.gotAt) / 1000);
      el.textContent = `Updated ${s < 2 ? "just now" : s + "s ago"}`;
    }
  }
  tick = setInterval(stamp, 1000);
  update();
  return { update, cleanup() { clearInterval(tick); } };
}

function offlineHtml() {
  return `<div class="offline" role="alert"><b>Can't reach your laptop</b>It may be asleep or off Tailscale.${S.snap ? " The list below is the last one Plane saw." : ""}
    <button class="btn primary" data-act="retry">Try again</button></div>`;
}

// ---- Claude is asking (a menu on Claude's screen, as buttons)

function promptCard(p, opts = {}) {
  const ctx = (p.context || []).filter(c => !/^Tip:|^Security guide$/i.test(c.s));
  const labels = p.options.map(o => o.label);
  const trust = labels.some(l => /trust this folder/i.test(l));
  let q, rest;
  if (trust) {
    q = "Do you trust the files in this folder?";
    const path = ctx.find(c => c.s.startsWith("/") || c.s.startsWith("~"));
    rest = path ? [{ t: "code", s: path.s }, { t: "text", s: "Claude will be able to read, edit and run files here." }] : [];
  } else {
    const texts = ctx.filter(c => c.t === "text");
    const qi = [...texts].reverse().find(c => /\?\s*$/.test(c.s)) || texts[texts.length - 1] || { s: "Claude is waiting for an answer" };
    q = qi.s;
    rest = ctx.filter(c => c !== qi);
  }
  let firstYes = true;
  const optHtml = p.options.map((o, i) => {
    let cls = "";
    if (/^yes\b/i.test(o.label)) { if (firstYes) cls = "yes"; firstYes = false; }
    else if (/^no\b/i.test(o.label)) cls = "no";
    return `<button class="opt ${cls}" data-choose="${i}"><b>${o.n ?? ""}</b><span>${esc(o.label)}</span>${o.desc ? `<small>${esc(o.desc)}</small>` : ""}</button>`;
  }).join("");
  const plan = opts.plan ? `<button class="linkbtn" data-act="togglePlan">${opts.planOpen ? "Hide the plan" : "Read the plan"}</button>
    ${opts.planOpen ? `<div class="plan"><div class="md">${md(opts.plan)}</div></div>` : ""}` : "";
  return `<div class="card" role="dialog" aria-label="Claude is asking">
    <span class="k">Claude is asking</span><span class="q">${esc(q)}</span>
    ${rest.map(c => c.t === "code" ? `<div class="code">${esc(c.s)}</div>` : `<span class="c">${esc(c.s)}</span>`).join("")}
    ${plan}${optHtml}<span class="inline-err" role="alert" hidden></span>
    ${opts.terminal === false ? "" : `<div class="subbar"><button data-act="terminal">Terminal</button></div>`}</div>`;
}

async function choose(card, url, i, sig) {
  const btn = card.querySelector(`[data-choose="${i}"]`);
  card.querySelectorAll("[data-choose]").forEach(b => { b.disabled = true; });
  const span = btn && btn.querySelector("span");
  const was = span ? span.textContent : "";
  if (span) span.textContent = "Sending…";
  const undo = () => { card.querySelectorAll("[data-choose]").forEach(b => { b.disabled = false; }); if (span && span.isConnected) span.textContent = was; };
  try {
    await api(url, { index: i, sig });
    setTimeout(() => { if (span && span.isConnected) undo(); }, 8000);  // the card normally goes away before this
  } catch (e) {
    undo();
    const er = card.querySelector(".inline-err");
    if (er) { er.hidden = false; er.textContent = e.message; }
  }
}

// ---- one session: chat

function itemHtml(it, idx, open) {
  if (it.k === "you") return `<div class="msg you"><span class="who">You${it.ts ? " · " + clock(it.ts) : ""}</span><div class="body">${youBody(it.text)}</div></div>`;
  if (it.k === "claude") return `<div class="msg"><span class="who">Claude</span><div class="md">${md(it.text)}</div></div>`;
  if (it.k === "note") return `<div class="note">${esc(it.text)}</div>`;
  if (it.k === "plan") {
    const o = open.has(idx);
    return `<div class="plan ${o ? "" : "clip"}"><span class="k">Plan</span><div class="md">${md(it.text)}</div>
      <button class="linkbtn" data-toggle="${idx}">${o ? "Show less" : "Read the whole plan"}</button></div>`;
  }
  if (it.k === "tools") {
    const o = open.has(idx);
    const bad = it.tools.some(t => t.err);
    return `<div class="msg"><button class="tools" data-toggle="${idx}" aria-expanded="${o}">${o ? "▾" : "▸"} ${esc(it.summary)}${bad ? " · <span class=\"st-bad\">something failed</span>" : ""}</button>
      ${o ? `<div class="toollist">${it.tools.map(t => `<div><div class="l">${esc(t.label)}</div>${t.out ? `<div class="o ${t.err ? "bad" : ""}">${esc(t.out)}</div>` : ""}</div>`).join("")}</div>` : ""}</div>`;
  }
  return "";
}

function chatView(id, wake) {
  let items = [], base = 0, pending = [], prompt = null, info = "", planOpen = false, atts = [];
  const open = new Set();
  const openedAt = Date.now();
  const late = setTimeout(() => header(), 4500);
  app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span></span><button id="actBtn">Actions</button></div>
    <div class="head"><h2 id="sname"></h2><span class="meta" id="smeta"></span></div>
    <div class="scroll" id="chat"><div class="center"><span class="big spin" aria-hidden="true">◌</span></div></div>
    <div id="bottom" class="bottom"></div>`;
  const chat = $("#chat"), bottom = $("#bottom");
  let draft = store(`plane:draft:${id}`) || "";

  function header() {
    const s = sessionById(id);
    $("#actBtn").hidden = !(s && s.bg);
    if (!s) {
      const ended = S.snap && Date.now() - openedAt > 4000;
      $("#sname").textContent = ended ? "Session ended" : "…";
      $("#smeta").textContent = ended ? "It isn't running any more. Its conversation is kept." : "";
      return;
    }
    const st = STATES[s.state] || STATES["your turn"];
    $("#sname").textContent = s.name;
    $("#smeta").innerHTML = `<span class="${st.cls}">${st.ico} ${esc(cap(s.state))}</span> · ${esc(s.folder)}${s.branch ? " · " + esc(s.branch) : ""}`;
  }

  function renderChat() {
    const nearBottom = chat.scrollHeight - chat.scrollTop - chat.clientHeight < 120;
    const seen = new Set(items.filter(i => i.k === "you").map(i => i.text.trim()));
    pending = pending.filter(p => !seen.has(p.text.trim()) && Date.now() - p.at < 60000);
    chat.innerHTML = `<div class="msgs">${items.length ? "" : `<div class="note">Nothing said yet.</div>`}
      ${items.map((it, i) => itemHtml(it, i, open)).join("")}
      ${pending.map(p => `<div class="msg you pending"><span class="who">You · ${p.failed ? "not sent" : "sending…"}</span><div class="body">${youBody(p.text)}</div></div>`).join("")}</div>`;
    if (nearBottom) chat.scrollTop = chat.scrollHeight;
  }

  function lastPlan() {
    for (let i = items.length - 1; i >= Math.max(0, items.length - 4); i--) if (items[i].k === "plan") return items[i].text;
    return "";
  }

  function renderBottom() {
    const s = sessionById(id);
    if (prompt) {
      const isPlan = prompt.context.some(c => /plan/i.test(c.s)) || prompt.options.some(o => /plan/i.test(o.label));
      bottom.innerHTML = promptCard(prompt, { plan: isPlan ? lastPlan() : "", planOpen });
      return;
    }
    if (s && !s.bg) {
      bottom.innerHTML = `<div class="readonly">This session is in a window on your laptop. You can read it here; reply in that window.</div>`;
      return;
    }
    const focused = document.activeElement && document.activeElement.id === "draft";
    if (focused && $("#draft")) { // don't rebuild the box you're typing in
      const sb = $("#subbar");
      if (sb) sb.innerHTML = subbar(s);
      return;
    }
    bottom.innerHTML = `<div class="composer" id="composer">${info ? `<span class="inline-err">${esc(info)}</span>` : ""}
      <div class="atts" id="atts">${attsHtml()}</div>
      <div class="inrow three"><button class="add" id="addBtn" aria-label="Add a photo or file">Add</button>
      <textarea class="field" id="draft" rows="1" placeholder="Message" aria-label="Message">${esc(draft)}</textarea>
      <button class="send" id="sendBtn">Send</button></div>
      <input type="file" id="fileIn" multiple hidden>
      <div class="subbar" id="subbar">${subbar(s)}</div></div>`;
    grow(); renderAtts();
  }

  function subbar(s) {
    const working = s && s.state === "working" && !s.stopped;
    const parked = (s && s.stopped) || !s;
    return `<button data-act="terminal">Terminal</button>${working ? `<button class="stop" data-act="stop">Stop</button>` : ""}
      ${parked ? `<span>Sending a message continues it on the laptop.</span>` : ""}
      <button class="kbd" data-kbd>Hide keyboard</button>`;
  }

  // -- photos and files: uploaded to the laptop as soon as they're picked, sent as paths
  function attsHtml() {
    return atts.map(a => `<div class="att ${a.status}">
      <span class="thumb">${a.thumb ? `<img src="${a.thumb}" alt="">` : "File"}</span>
      <span class="an"><span>${esc(a.name)}</span><small>${a.status === "uploading" ? "Uploading…" : a.status === "error" ? esc(a.error) : esc(size(a.size))}</small></span>
      <button class="rm" data-rm="${a.id}" aria-label="Remove ${esc(a.name)}">×</button></div>`).join("");
  }

  function renderAtts() {
    const box = $("#atts"), b = $("#sendBtn");
    if (box) { box.innerHTML = attsHtml(); box.hidden = !atts.length; }
    const busy = atts.some(a => a.status === "uploading");
    if (b) { b.disabled = busy; b.textContent = busy ? "Uploading…" : "Send"; }
  }

  async function addFiles(files) {
    for (const file of files) {
      const a = { id: Math.random().toString(36).slice(2), name: file.name || "photo", status: "uploading", size: file.size };
      atts.push(a);
      renderAtts();
      try {
        const f = await prepareFile(file);
        a.name = f.name; a.size = f.size;
        if (/^image\//.test(f.type)) a.thumb = URL.createObjectURL(f);
        renderAtts();
        const r = await fetch(`/api/upload?name=${encodeURIComponent(f.name)}`, {
          method: "POST", headers: { "X-Plane": "1", "Content-Type": f.type || "application/octet-stream" }, body: f });
        const j = await r.json().catch(() => ({}));
        if (!r.ok) throw new Error(j.error || `Upload failed (${r.status})`);
        a.path = j.path; a.status = "done";
      } catch (e) {
        a.status = "error";
        a.error = /fetch|network|load/i.test(e.message) ? "Upload failed: can't reach your laptop" : e.message;
      }
      renderAtts();
    }
  }

  function grow() {
    const d = $("#draft");
    if (!d) return;
    d.style.height = "auto";
    d.style.height = Math.min(d.scrollHeight + 2, 152) + "px";
  }

  async function send() {
    const d = $("#draft");
    const words = (d ? d.value : "").trim();
    if (atts.some(a => a.status === "uploading")) return;
    const sent = atts.filter(a => a.status === "done");
    if (!words && !sent.length) { d && d.focus(); return; }
    const text = [words, sent.map(a => `Attached file: ${a.path}`).join("\n")].filter(Boolean).join("\n\n");
    const p = { text, at: Date.now() };
    pending.push(p);
    draft = ""; store(`plane:draft:${id}`, "");
    d.value = ""; grow();
    const kept = atts;
    atts = atts.filter(a => a.status === "error");
    renderAtts();
    renderChat();
    chat.scrollTop = chat.scrollHeight;
    try { await api(`/api/session/${id}/send`, { text }); info = ""; }
    catch (e) {
      p.failed = true; info = e.message; draft = words; store(`plane:draft:${id}`, words); atts = kept;
      renderChat(); d.value = words; renderBottom();
    }
  }

  const st = stream(`/api/session/${id}/stream${wake ? "?wake=1" : ""}`, {
    chat(v) {
      // v.start is the server's index of v.items[0]; base is the server index of items[0]
      if (!items.length || v.start < base || v.start > base + items.length) { items = v.items.slice(); base = v.start; open.clear(); }
      else items = items.slice(0, v.start - base).concat(v.items);
      renderChat();
    },
    screen(v) {
      const before = prompt && prompt.sig;
      prompt = v.prompt;
      if ((prompt && prompt.sig) !== before) { planOpen = false; renderBottom(); }
    },
    info(v) { info = v.error || ""; renderBottom(); },
  });

  app.addEventListener("input", onInput);
  function onInput(e) { if (e.target.id === "draft") { draft = e.target.value; store(`plane:draft:${id}`, draft); grow(); } }
  // the keyboard: "Hide keyboard" while typing; tapping the conversation hides it too; Send keeps it up
  const onFocus = e => { if (e.target.id === "draft") { const c = $("#composer"); c && c.classList.toggle("typing", e.type === "focusin"); } };
  const onPress = e => {
    if (e.target.closest("[data-kbd]")) { e.preventDefault(); $("#draft") && $("#draft").blur(); }
    else if (e.target.closest("#sendBtn") && document.activeElement && document.activeElement.id === "draft") e.preventDefault();
  };
  const onPick = e => { if (e.target.id === "fileIn" && e.target.files.length) { addFiles([...e.target.files]); e.target.value = ""; } };
  app.addEventListener("focusin", onFocus);
  app.addEventListener("focusout", onFocus);
  app.addEventListener("pointerdown", onPress);
  app.addEventListener("change", onPick);
  chat.addEventListener("click", () => { const d = $("#draft"); if (d && document.activeElement === d) d.blur(); });
  app.onclick = async e => {
    if (e.target.closest("#sendBtn")) return send();
    if (e.target.closest("#addBtn")) { $("#fileIn").click(); return; }
    const rm = e.target.closest("[data-rm]");
    if (rm) {
      const a = atts.find(x => x.id === rm.dataset.rm);
      if (a && a.thumb) URL.revokeObjectURL(a.thumb);
      atts = atts.filter(x => x !== a); renderAtts(); return;
    }
    const tg = e.target.closest("[data-toggle]");
    if (tg) { const i = +tg.dataset.toggle; open.has(i) ? open.delete(i) : open.add(i); renderChat(); return; }
    const ch = e.target.closest("[data-choose]");
    if (ch && prompt) return choose(bottom, `/api/session/${id}/choose`, +ch.dataset.choose, prompt.sig);
    const a = e.target.closest("[data-act]");
    if (!a) return;
    if (a.dataset.act === "terminal") location.hash = `#t/${id}`;
    if (a.dataset.act === "togglePlan") { planOpen = !planOpen; renderBottom(); }
    if (a.dataset.act === "stop") {
      a.disabled = true; a.textContent = "Stopping…";
      try { await api(`/api/session/${id}/key`, { key: "esc" }); } catch (err) { toast(err.message); }
    }
  };
  $("#actBtn").onclick = () => actionsSheet(id);

  header(); renderBottom();
  return {
    update() { header(); if (!prompt) renderBottom(); },
    cleanup() {
      st.close(); clearTimeout(late); app.onclick = null;
      app.removeEventListener("input", onInput); app.removeEventListener("focusin", onFocus);
      app.removeEventListener("focusout", onFocus); app.removeEventListener("pointerdown", onPress);
      app.removeEventListener("change", onPick);
    },
  };
}

function actionsSheet(id) {
  const s = sessionById(id);
  if (!s) return;
  const el = sheet(`<span class="t">${esc(s.name)}</span>
    ${s.stopped ? `<button class="btn" data-a="continue">Continue</button>` : `<button class="btn" data-a="pause">Pause</button>`}
    ${s.fromHandoff ? `<button class="btn" data-a="requeue">Put back in queue</button>` : ""}
    <span class="inline-err" hidden></span>
    <div class="gap"></div>
    <button class="btn danger" data-a="close">Close session</button>
    <button class="btn quiet" data-shut>Cancel</button>`);
  el.addEventListener("click", async e => {
    const b = e.target.closest("[data-a]");
    if (!b) return;
    const act = b.dataset.a;
    const fail = err => { const er = $(".inline-err", el); er.hidden = false; er.textContent = err.message; b.disabled = false; };
    if (act === "close") {
      el.remove();
      return confirmSheet(`Close “${s.name}”?`, "It ends and leaves the list. The conversation is kept, so claude --resume can bring it back.",
        "Close session", async () => { await api(`/api/session/${id}/close`, {}); location.hash = "#"; toast(`Closed “${s.name}”. Conversation kept.`); });
    }
    if (act === "requeue") {
      el.remove();
      return confirmSheet(`Close and requeue “${s.name}”?`, "The session ends and its handoff goes back in the queue, ready to start again.",
        "Put back in queue", async () => { await api(`/api/session/${id}/requeue`, {}); location.hash = "#"; toast("Closed. The handoff is back in the queue."); });
    }
    b.disabled = true;
    try {
      if (act === "pause") { b.textContent = "Pausing…"; await api(`/api/session/${id}/pause`, {}); el.remove(); location.hash = "#"; toast(`Paused “${s.name}”. Open it to continue.`); }
      if (act === "continue") { b.textContent = "Continuing…"; await api(`/api/session/${id}/wake`, {}); el.remove(); toast("Continuing on your laptop"); }
    } catch (err) { fail(err); }
  });
}

// ---- one session: terminal

let xtermLoaded = null;
function loadXterm() {
  if (xtermLoaded) return xtermLoaded;
  const css = document.createElement("link");
  css.rel = "stylesheet"; css.href = "vendor/xterm.css";
  document.head.append(css);
  const script = src => new Promise((ok, bad) => { const s = document.createElement("script"); s.src = src; s.onload = ok; s.onerror = bad; document.head.append(s); });
  xtermLoaded = script("vendor/xterm.js").then(() => script("vendor/addon-fit.js"));
  return xtermLoaded;
}

function termView(id) {
  const s = sessionById(id);
  app.innerHTML = `<div class="nav"><button data-href="#s/${id}">‹ Chat</button><span class="t">${esc(s ? s.name : "Terminal")}</span><span style="width:60px"></span></div>
    <div class="termwrap" id="term"></div>
    <div class="composer"><div class="keys">${[["esc", "Esc"], ["tab", "Tab"], ["stab", "⇧Tab"], ["up", "↑"], ["down", "↓"], ["enter", "Enter"]]
      .map(([k, l]) => `<button data-key="${k}" aria-label="${k === "stab" ? "Shift Tab" : l}">${l}</button>`).join("")}</div>
      <div class="inrow"><input class="field mono" id="tin" placeholder="Type, then Send" aria-label="Type into the terminal" autocapitalize="off" autocorrect="off" spellcheck="false" enterkeyhint="send">
      <button class="send" id="tsend">Send</button></div></div>`;
  let term, fit, st, ro;
  loadXterm().then(() => {
    if (!$("#term")) return;
    term = new window.Terminal({ fontSize: 11, fontFamily: getComputedStyle(document.body).getPropertyValue("--mono"), disableStdin: true,
      scrollback: 3000, convertEol: false, cursorBlink: false,
      theme: { background: "#0A0E16", foreground: "#F2F5F9", cursor: "#5CCFF0", selectionBackground: "#1F4A73",
        black: "#151C28", red: "#FF5C6C", green: "#4ED6A0", yellow: "#F5C86A", blue: "#3A9BE8", magenta: "#E58AB8", cyan: "#5CCFF0", white: "#C9D2DE",
        brightBlack: "#5A6880", brightRed: "#FF7F8B", brightGreen: "#72E3B5", brightYellow: "#FFD88A", brightBlue: "#62B2F0",
        brightMagenta: "#F0A6CB", brightCyan: "#86DDF5", brightWhite: "#F2F5F9" } });
    fit = new window.FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open($("#term"));
    fit.fit();
    const connect = () => {
      st && st.close();
      term.reset();
      st = stream(`/api/session/${id}/stream?term=1&wake=1&cols=${term.cols}&rows=${term.rows}`, {
        term(v) { const bin = atob(v.b64); const bytes = new Uint8Array(bin.length); for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i); term.write(bytes); },
        info(v) { if (v.error) term.write(`\r\n\x1b[31m${v.error}\x1b[0m\r\n`); },
      });
    };
    connect();
    let last = `${term.cols}x${term.rows}`, rt;
    ro = new ResizeObserver(() => { clearTimeout(rt); rt = setTimeout(() => { fit.fit(); const now = `${term.cols}x${term.rows}`; if (now !== last) { last = now; connect(); } }, 250); });
    ro.observe($("#term"));
  }).catch(() => { $("#term").innerHTML = `<div class="center"><span>Couldn't load the terminal.</span></div>`; });

  const key = (k, extra) => api(`/api/session/${id}/key`, { key: k, ...extra }).catch(e => toast(e.message));
  app.onclick = e => {
    const k = e.target.closest("[data-key]");
    if (k) return key(k.dataset.key);
    if (e.target.closest("#tsend")) { const t = $("#tin"); if (t.value) { key("text", { text: t.value, enter: true }); t.value = ""; } else key("enter"); }
  };
  $("#tin").addEventListener("keydown", e => { if (e.key === "Enter") { e.preventDefault(); $("#tsend").click(); } });
  $("#term").addEventListener("pointerdown", () => $("#tin").blur());  // tap the screen to put the keyboard away
  return { cleanup() { st && st.close(); ro && ro.disconnect(); term && term.dispose(); app.onclick = null; } };
}

// ---- new session

function newView() {
  const form = store("plane:newform") || { folder: "", other: "", name: "", first: "" };
  let recent = [];
  app.innerHTML = `<div class="nav"><button data-href="#">Cancel</button><span class="t">New session</span><span style="width:70px"></span></div>
    <div class="scroll"><div class="form">
      <div class="group"><span class="label" id="flabel">Folder</span><div class="radios" role="radiogroup" aria-labelledby="flabel" id="radios">
        <div class="center"><span class="big spin" aria-hidden="true">◌</span></div></div></div>
      <div class="group" id="otherGroup" hidden><label class="label" for="opath">Folder path</label>
        <input class="field mono" id="opath" autocapitalize="off" autocorrect="off" spellcheck="false" placeholder="~/Documents/…" value="${esc(form.other)}">
        <div class="sugg" id="sugg" hidden></div></div>
      <div class="group"><label class="label" for="nname">Name <em>(optional)</em></label><input class="field" id="nname" value="${esc(form.name)}"></div>
      <div class="group"><label class="label" for="nfirst">First message <em>(optional)</em></label><textarea class="field" id="nfirst" rows="3">${esc(form.first)}</textarea></div>
      <span class="inline-err" id="nerr" role="alert" hidden></span>
    </div></div>
    <div class="dock"><button class="btn primary" id="startBtn">Start session</button></div>`;

  function radios() {
    const opts = recent.map(f => ({ k: f.path, n: f.name, p: f.path })).concat(
      [{ k: "~", n: "Home folder", p: "~" }, { k: "other", n: "Other folder…", p: "Type a path" }]);
    if (!form.folder || !opts.some(o => o.k === form.folder)) form.folder = opts[0].k;
    $("#radios").innerHTML = opts.map(o => `<button class="radio" role="radio" aria-checked="${o.k === form.folder}" data-folder="${esc(o.k)}">
      <span class="dot"></span><span class="n">${esc(o.n)}</span><span class="p">${esc(o.p)}</span></button>`).join("");
    $("#otherGroup").hidden = form.folder !== "other";
  }
  api("/api/folders").then(r => { recent = r.folders || []; radios(); })
    .catch(e => { recent = []; radios(); const er = $("#nerr"); er.hidden = false; er.textContent = e.message; });

  let st;
  async function suggest() {
    const q = $("#opath").value.trim();
    form.other = q; store("plane:newform", form);
    if (!q) { $("#sugg").hidden = true; return; }
    clearTimeout(st);
    st = setTimeout(async () => {
      try {
        const r = await api(`/api/folders?q=${encodeURIComponent(q)}`);
        const box = $("#sugg");
        if (!box) return;
        box.hidden = !r.folders.length;
        box.innerHTML = r.folders.map(f => `<button data-path="${esc(f.path)}">${esc(f.path)}</button>`).join("");
      } catch { /* typing on; the start button reports real errors */ }
    }, 150);
  }
  $("#opath").addEventListener("input", suggest);
  for (const fid of ["nname", "nfirst"]) $(`#${fid}`).addEventListener("input", e => { form[fid === "nname" ? "name" : "first"] = e.target.value; store("plane:newform", form); });

  app.onclick = async e => {
    const r = e.target.closest("[data-folder]");
    if (r) { form.folder = r.dataset.folder; store("plane:newform", form); radios(); if (form.folder === "other") $("#opath").focus(); return; }
    const sg = e.target.closest("[data-path]");
    if (sg) { $("#opath").value = sg.dataset.path; suggest(); $("#opath").focus(); return; }
    if (e.target.closest("#startBtn")) {
      const b = $("#startBtn"), er = $("#nerr");
      const cwd = form.folder === "other" ? $("#opath").value.trim() : form.folder;
      if (!cwd) { er.hidden = false; er.textContent = "Type the folder's path, or pick one above."; $("#opath").focus(); return; }
      b.disabled = true; b.textContent = "Starting…"; er.hidden = true;
      try {
        const res = await api("/api/start", { cwd, name: $("#nname").value.trim(), prompt: $("#nfirst").value.trim() });
        store("plane:newform", null);
        location.hash = `#starting/${res.starter}`;
      } catch (err) { b.disabled = false; b.textContent = "Start session"; er.hidden = false; er.textContent = err.message; }
    }
  };
  return { cleanup() { app.onclick = null; } };
}

// ---- starting (new session, handoff, resume)

function startingView(sid) {
  let state = null;
  app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span class="t">Starting</span><span style="width:90px"></span></div>
    <div class="scroll" id="body"></div><div id="bottom" class="bottom"></div>`;
  const body = $("#body"), bottom = $("#bottom");
  function render() {
    const v = state;
    if (!v) { body.innerHTML = `<div class="center"><span class="big spin" aria-hidden="true">◌</span><b>Starting…</b></div>`; bottom.innerHTML = ""; return; }
    if (v.status === "done") { location.replace(`#s/${v.result.jobId}/wake`); return; }
    if (v.status === "failed") {
      body.innerHTML = `<div class="center"><b>${esc(v.result.error)}</b>${v.result.detail ? `<pre>${esc(v.result.detail)}</pre>` : ""}</div>`;
      bottom.innerHTML = `<div class="dock"><button class="btn" data-href="#">Back to sessions</button></div>`;
      return;
    }
    body.innerHTML = `<div class="center"><span class="big spin" aria-hidden="true">◌</span><b>Starting in ${esc(v.folder)}…</b><span>It runs on your laptop.</span></div>`;
    bottom.innerHTML = v.prompt ? promptCard(v.prompt, { terminal: false }) : "";
  }
  const st = stream(`/api/start/${sid}/stream`, { screen(v) { state = v; render(); } });
  // a start that's over (the page was reloaded much later) answers 404: EventSource just errors
  const gone = setTimeout(() => { if (!state) { body.innerHTML = `<div class="center"><b>That start is over.</b><span>Check the session list.</span></div>`; } }, 6000);
  app.onclick = e => {
    const ch = e.target.closest("[data-choose]");
    if (ch && state && state.prompt) choose(bottom, `/api/start/${sid}/choose`, +ch.dataset.choose, state.prompt.sig);
  };
  render();
  return { cleanup() { st.close(); clearTimeout(gone); app.onclick = null; } };
}

// ---- handoff

function handoffView(hid) {
  function render() {
    const h = (S.snap?.handoffs || []).find(x => x.id === hid);
    if (!h) {
      app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span></span><span></span></div>
        <div class="center"><b>${S.snap ? "This handoff isn't in the queue any more." : "Loading…"}</b>${S.snap ? "<span>It was started or discarded.</span>" : ""}</div>`;
      return;
    }
    app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span></span><span></span></div>
      <div class="head"><h2>${esc(h.title)}</h2><span class="meta"><span class="${h.ready ? "st-turn" : "st-muted"}">→ ${h.ready ? "Ready" : "Being written"}</span>${h.from ? ` · from “${esc(h.from)}”` : ""}</span>
        <span class="meta">${esc(h.cwd)}</span></div>
      ${h.ready ? "" : `<div class="warn">Its session is still working on it.</div>`}
      <div class="scroll"><div class="prompt"><span class="label">Prompt</span><div class="md">${md(h.prompt)}</div></div></div>
      <div class="dock"><span class="inline-err" id="herr" role="alert" hidden></span>
        <button class="btn primary" id="hstart">${h.ready ? "Start this handoff" : "Start anyway"}</button>
        <button class="btn redtext" id="hdiscard">Discard</button></div>`;
    $("#hstart").onclick = async () => {
      const b = $("#hstart");
      b.disabled = true; b.textContent = "Starting…";
      try { const r = await api(`/api/handoff/${encodeURIComponent(hid)}/start`, {}); location.hash = `#starting/${r.starter}`; }
      catch (e) { b.disabled = false; b.textContent = "Start this handoff"; const er = $("#herr"); er.hidden = false; er.textContent = e.message; }
    };
    $("#hdiscard").onclick = () => confirmSheet(`Discard “${h.title}”?`, "It leaves the queue. A copy is kept for 14 days.", "Discard",
      async () => { await api(`/api/handoff/${encodeURIComponent(hid)}/discard`, {}); location.hash = "#"; toast("Discarded. A copy is kept for 14 days."); });
  }
  let shown = JSON.stringify((S.snap?.handoffs || []).find(x => x.id === hid) || null);
  render();
  return {
    update() { // redraw only when this handoff changed, so a tap isn't lost mid-redraw
      const now = JSON.stringify((S.snap?.handoffs || []).find(x => x.id === hid) || null);
      if (now !== shown) { shown = now; render(); }
    },
  };
}

// ---- cut off / standby

function lostView(sid) {
  function render() {
    const r = (S.snap?.lost || []).find(x => x.id === sid);
    if (!r) {
      app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span></span><span></span></div>
        <div class="center"><b>${S.snap ? "This session isn't waiting any more." : "Loading…"}</b></div>`;
      return;
    }
    const standby = r.cause === "standby";
    const when = r.lastActive ? new Date(r.lastActive * 1000) : null;
    app.innerHTML = `<div class="nav"><button data-href="#">‹ Sessions</button><span></span><span></span></div>
      <div class="head"><h2>${esc(r.name)}</h2><span class="meta ${standby ? "st-muted" : "st-bad"}">${standby ? "◐ On standby, waiting on you"
        : `↯ Cut off by a ${r.cause === "reboot" ? "reboot" : "crash"}${when ? ` at ${clock(when)}` : ""}`}</span></div>
      <dl class="info"><dt>Folder</dt><dd>${esc(r.cwd)}</dd>${when ? `<dt>Last active</dt><dd>${ago(when.getTime())} ago</dd>` : ""}
        <dt>What's kept</dt><dd>The whole conversation</dd>${r.needs ? `<dt>It asked</dt><dd>${esc(r.needs)}</dd>` : ""}</dl>
      <div class="scroll"></div>
      <div class="dock"><span class="inline-err" id="lerr" role="alert" hidden></span>
        <button class="btn primary" id="lgo">${standby ? "Continue" : "Resume"}</button><button class="btn danger" id="lclose">Close</button></div>`;
    $("#lgo").onclick = async () => {
      const b = $("#lgo");
      b.disabled = true; b.textContent = standby ? "Continuing…" : "Resuming…";
      try {
        const res = await api(`/api/lost/${sid}/resume`, {});
        location.hash = res.open ? `#s/${res.open}/wake` : `#starting/${res.starter}`;
      } catch (e) { b.disabled = false; b.textContent = standby ? "Continue" : "Resume"; const er = $("#lerr"); er.hidden = false; er.textContent = e.message; }
    };
    $("#lclose").onclick = () => confirmSheet(`Close “${r.name}”?`, "Plane stops offering it. The conversation is kept, so claude --resume can bring it back.",
      "Close", async () => { await api(`/api/lost/${sid}/close`, {}); location.hash = "#"; toast("Closed. The conversation is kept."); });
  }
  let shown = JSON.stringify((S.snap?.lost || []).find(x => x.id === sid) || null);
  render();
  return { update() { const now = JSON.stringify((S.snap?.lost || []).find(x => x.id === sid) || null); if (now !== shown) { shown = now; render(); } } };
}

// ---------------------------------------------------------------- routing and global taps

function route() {
  if (S.view && S.view.cleanup) S.view.cleanup();
  document.querySelectorAll(".shade").forEach(el => el.remove());
  const h = location.hash.replace(/^#\/?/, "");
  const [kind, id, extra] = h.split("/");
  app.className = kind === "s" || kind === "starting" ? "withbottom" : "";
  if (kind === "s" && id) S.view = chatView(id, extra === "wake");
  else if (kind === "t" && id) S.view = termView(id);
  else if (kind === "new") S.view = newView();
  else if (kind === "starting" && id) S.view = startingView(id);
  else if (kind === "h" && id) S.view = handoffView(decodeURIComponent(id));
  else if (kind === "l" && id) S.view = lostView(id);
  else S.view = homeView();
  fitViewport();
}
window.addEventListener("hashchange", route);

document.addEventListener("click", async e => {
  const link = e.target.closest("[data-href]");
  if (link) { location.hash = link.dataset.href; return; }
  const a = e.target.closest("[data-act]");
  if (!a) return;
  if (a.dataset.act === "tipOff") { store("plane:tipOff", true); S.view.update && S.view.update(); }
  if (a.dataset.act === "retry") { a.textContent = "Trying…"; location.reload(); }
  if (a.dataset.act === "copy") copyBlock(a);
  if (a.dataset.act === "notifTest" && !N.busy) testPush();
  if (a.dataset.act === "notifReset" && !N.busy) resetPush();
  if (a.dataset.act === "notifOn") {
    a.disabled = true; a.textContent = "Turning on…";
    try { await enablePush(); store("plane:tipOff", true); toast("Notifications are on"); S.view.update && S.view.update(); }
    catch (err) {
      a.disabled = false; a.textContent = "Turn on";
      const er = a.parentElement.querySelector(".err");
      if (er) { er.hidden = false; er.textContent = err.message; }
    }
  }
});

// ---------------------------------------------------------------- notifications

function keyBytes(b64) {
  const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - b64.length % 4) % 4));
  return Uint8Array.from(s, c => c.charCodeAt(0));
}

async function enablePush() {
  const perm = await Notification.requestPermission();  // iOS only allows this straight from a tap
  if (perm !== "granted") throw new Error("Notifications are off for Plane. Turn them on in Settings › Notifications › Plane.");
  await subscribe();
}

async function subscribe() {
  const { key, why } = await api("/api/push/key");
  if (!key) throw new Error(`Notifications aren't set up on the laptop yet (${why}).`);
  const reg = await navigator.serviceWorker.ready;
  let sub = await reg.pushManager.getSubscription();
  if (sub && sub.options && sub.options.applicationServerKey) {
    const have = new Uint8Array(sub.options.applicationServerKey);
    const want = keyBytes(key);
    if (have.length !== want.length || have.some((b, i) => b !== want[i])) { await sub.unsubscribe(); sub = null; }
  }
  if (!sub) sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) });
  await api("/api/push/subscribe", { subscription: sub.toJSON() });
}

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("sw.js").then(() => {
    if ("Notification" in window && Notification.permission === "granted") subscribe().catch(() => {});
  }).catch(() => {});
  navigator.serviceWorker.addEventListener("message", e => {
    if (e.data && e.data.open) location.hash = new URL(e.data.open, location.href).hash || "#";
  });
}

route();
