// Loupe frontend, vanilla JS, talks only to the local /api layer.
"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  session: null,
  conn: null,
  selected: null,
  tab: "schema",
  offset: 0,
  limit: 50,
  lastResult: null,
  sort: { field: null, dir: 1 },
};

const THEME_KEY = "loupe-theme";
const CONNS_KEY = "loupe-connections";
const LAST_KEY = "loupe-last-connection";

/* theme */

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme;
  const glyph = theme === "dark" ? "☀" : "☾";
  ["btn-theme", "btn-theme-float"].forEach((id) => {
    const el = $(id);
    if (el) el.innerHTML = glyph;
  });
}

function initTheme() {
  const system = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  applyTheme(localStorage.getItem(THEME_KEY) || system);
}

function toggleTheme() {
  const next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
  localStorage.setItem(THEME_KEY, next);
  applyTheme(next);
}

/* saved connections, stored in this browser only */

function loadConns() {
  try {
    return JSON.parse(localStorage.getItem(CONNS_KEY)) || [];
  } catch {
    return [];
  }
}

function saveConns(conns) {
  localStorage.setItem(CONNS_KEY, JSON.stringify(conns));
}

function upsertConn(conn) {
  const conns = loadConns().filter((c) => c.id !== conn.id);
  conns.unshift(conn);
  saveConns(conns);
}

function connLabel(conn) {
  return conn.name || `${conn.host}:${conn.port}`;
}

/* api */

async function call(path, options = {}) {
  const headers = { "Content-Type": "application/json" };
  if (state.session) headers["X-Session"] = state.session;
  const response = await fetch(path, { headers, ...options });
  const body = await response.json().catch(() => ({}));
  if (response.status === 401) {
    state.session = null;
    showSignin();
    throw new Error(body.error || "session expired, sign in again");
  }
  if (!response.ok) throw new Error(body.error || `${response.status} ${response.statusText}`);
  return body;
}

function toast(message) {
  const el = $("toast");
  el.textContent = message;
  el.classList.remove("hidden");
  clearTimeout(el._timer);
  el._timer = setTimeout(() => el.classList.add("hidden"), 5000);
}

/* views */

function show(view) {
  $("signin-view").classList.toggle("hidden", view !== "signin");
  $("main-view").classList.toggle("hidden", view !== "main");
  $("btn-theme-float").classList.toggle("hidden", view !== "signin");
}

function showSignin() {
  renderSavedList();
  show("signin");
}

function renderSavedList() {
  const conns = loadConns();
  $("saved-block").classList.toggle("hidden", conns.length === 0);
  const list = $("saved-list");
  list.innerHTML = "";
  conns.forEach((conn) => {
    const li = document.createElement("li");
    const label = document.createElement("div");
    label.className = "saved-label";
    label.innerHTML = `<span class="saved-name">${connLabel(conn)}</span>
      <span class="saved-target">${conn.user}@${conn.host}:${conn.port}</span>`;
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "icon";
    remove.title = "Remove this saved connection";
    remove.innerHTML = "&times;";
    remove.onclick = (event) => {
      event.stopPropagation();
      saveConns(loadConns().filter((c) => c.id !== conn.id));
      renderSavedList();
    };
    li.appendChild(label);
    li.appendChild(remove);
    li.onclick = () => connectWith(conn).catch((e) => toast(e.message));
    list.appendChild(li);
  });
}

async function connectWith(conn) {
  const result = await call("/api/connect", {
    method: "POST",
    body: JSON.stringify({ host: conn.host, port: conn.port, user: conn.user, password: conn.password }),
  });
  state.session = result.session;
  state.conn = conn;
  state.selected = null;
  upsertConn(conn);
  localStorage.setItem(LAST_KEY, conn.id);
  $("conn-name").textContent = connLabel(conn);
  $("target").textContent = `${conn.user}@${conn.host}:${conn.port}`;
  $("detail").classList.add("hidden");
  $("empty-hint").classList.remove("hidden");
  show("main");
  await refreshCollections();
}

async function boot() {
  const lastId = localStorage.getItem(LAST_KEY);
  const last = loadConns().find((c) => c.id === lastId);
  if (last) {
    try {
      await connectWith(last);
      return;
    } catch (e) {
      toast(`auto reconnect to ${connLabel(last)} failed: ${e.message}`);
    }
  }
  const defaults = await call("/api/defaults");
  if (loadConns().length === 0) {
    $("in-host").value = defaults.host;
    $("in-port").value = defaults.port || "19530";
    $("in-user").value = defaults.user || "root";
    $("in-password").value = defaults.password;
  }
  showSignin();
}

/* collections */

async function refreshCollections() {
  const items = await call("/api/collections");
  const list = $("collection-list");
  list.innerHTML = "";
  items.forEach((item) => {
    const li = document.createElement("li");
    li.dataset.name = item.name;
    const loaded = item.loaded === "LoadStateLoaded";
    li.innerHTML = `<span class="cname" title="${item.name}">${item.name}</span>
      <span class="badge ${loaded ? "loaded" : ""}">${loaded ? "loaded" : "cold"} · ${item.rows}</span>`;
    li.onclick = () => selectCollection(item.name).catch((e) => toast(e.message));
    if (item.name === state.selected) li.classList.add("active");
    list.appendChild(li);
  });
}

async function selectCollection(name) {
  state.selected = name;
  state.offset = 0;
  state.sort = { field: null, dir: 1 };
  document.querySelectorAll("#collection-list li").forEach((li) => {
    li.classList.toggle("active", li.dataset.name === name);
  });
  $("empty-hint").classList.add("hidden");
  $("detail").classList.remove("hidden");
  await renderDetail();
  if (state.tab === "data") await runQuery();
}

/* rendering */

function cell(value) {
  const td = document.createElement("td");
  const text = typeof value === "object" && value !== null ? JSON.stringify(value) : String(value ?? "");
  td.title = text;
  if (typeof value === "number") td.classList.add("num");
  td.textContent = text.length > 160 ? text.slice(0, 160) + "…" : text;
  return td;
}

function renderTable(table, headers, rows, onSort) {
  table.innerHTML = "";
  const trh = document.createElement("tr");
  headers.forEach((h) => {
    const th = document.createElement("th");
    th.textContent = h;
    if (onSort) {
      th.classList.add("sortable");
      if (state.sort.field === h) th.dataset.dir = state.sort.dir === 1 ? "asc" : "desc";
      th.onclick = () => onSort(h);
    }
    trh.appendChild(th);
  });
  table.appendChild(trh);
  rows.forEach((row) => {
    const tr = document.createElement("tr");
    row.forEach((v) => tr.appendChild(cell(v)));
    table.appendChild(tr);
  });
}

async function renderDetail() {
  const d = await call(`/api/collections/${encodeURIComponent(state.selected)}`);
  $("col-name").textContent = d.name;
  const loaded = d.loaded === "LoadStateLoaded" ? "loaded" : "not loaded";
  $("col-meta").textContent = `${d.rows} rows · ${loaded}${d.description ? " · " + d.description : ""}`;

  const fields = (d.fields || []).map((f) => {
    const params = (f.params || []).map((p) => `${p.key}=${p.value}`).join(", ");
    return [f.name, f.type, params, f.primaryKey ? "yes" : "", f.autoId ? "yes" : "", f.nullable ? "yes" : "", f.description || ""];
  });
  renderTable($("fields-table"), ["name", "type", "params", "primary", "auto id", "nullable", "description"], fields);

  const indexes = (d.indexes || []).map((ix) => [ix.fieldName || "", ix.indexName || "", ix.metricType || ""]);
  renderTable($("indexes-table"), ["field", "index name", "metric"], indexes.length ? indexes : [["no indexes", "", ""]]);
}

/* data tab, sorting runs server side over the whole collection */

function renderData() {
  const result = state.lastResult;
  if (!result) return;
  const grid = result.rows.map((row) => result.fields.map((f) => row[f]));
  renderTable($("data-table"), result.fields, grid, toggleSort);
  const last = result.offset + result.rows.length;
  $("page-info").textContent = `rows ${result.rows.length ? result.offset + 1 : 0} to ${last} of ${result.total}`;
}

function toggleSort(field) {
  if (state.sort.field === field) {
    if (state.sort.dir === 1) state.sort.dir = -1;
    else state.sort = { field: null, dir: 1 };
  } else {
    state.sort = { field, dir: 1 };
  }
  state.offset = 0;
  runQuery().catch((e) => toast(e.message));
}

async function runQuery(refresh = false) {
  if (!state.selected) return;
  state.limit = parseInt($("in-limit").value, 10);
  $("page-info").textContent = state.sort.field ? "sorting the whole collection, first run can take a while..." : "loading...";
  const body = {
    filter: $("in-filter").value.trim(),
    limit: state.limit,
    offset: state.offset,
    sort_field: state.sort.field || "",
    sort_dir: state.sort.dir === -1 ? "desc" : "asc",
    refresh,
  };
  try {
    state.lastResult = await call(`/api/collections/${encodeURIComponent(state.selected)}/query`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  } catch (e) {
    $("page-info").textContent = "";
    throw e;
  }
  renderData();
}

function switchTab(tab) {
  state.tab = tab;
  document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  $("tab-schema").classList.toggle("hidden", tab !== "schema");
  $("tab-data").classList.toggle("hidden", tab !== "data");
  if (tab === "data") runQuery().catch((e) => toast(e.message));
}

/* events */

function guard(fn) {
  return (event) => {
    if (event) event.preventDefault();
    fn().catch((e) => toast(e.message));
  };
}

$("connect-form").onsubmit = guard(async () => {
  $("btn-connect").disabled = true;
  $("connect-error").classList.add("hidden");
  const conn = {
    id: `${Date.now()}`,
    name: $("in-name").value.trim(),
    host: $("in-host").value.trim(),
    port: $("in-port").value.trim() || "19530",
    user: $("in-user").value.trim(),
    password: $("in-password").value,
  };
  try {
    await connectWith(conn);
  } catch (e) {
    $("connect-error").textContent = e.message;
    $("connect-error").classList.remove("hidden");
  } finally {
    $("btn-connect").disabled = false;
  }
});

$("btn-switch").onclick = guard(async () => {
  await call("/api/disconnect", { method: "POST" }).catch(() => ({}));
  state.session = null;
  localStorage.removeItem(LAST_KEY);
  showSignin();
});

$("btn-refresh").onclick = guard(refreshCollections);
$("btn-load").onclick = guard(async () => {
  await call(`/api/collections/${encodeURIComponent(state.selected)}/load`, { method: "POST" });
  await renderDetail();
  await refreshCollections();
});
$("btn-release").onclick = guard(async () => {
  await call(`/api/collections/${encodeURIComponent(state.selected)}/release`, { method: "POST" });
  await renderDetail();
  await refreshCollections();
});
$("query-form").onsubmit = guard(async () => {
  state.offset = 0;
  await runQuery(true);
});
$("btn-prev").onclick = guard(async () => {
  state.offset = Math.max(0, state.offset - state.limit);
  await runQuery();
});
$("btn-next").onclick = guard(async () => {
  if (!state.lastResult || state.offset + state.limit >= state.lastResult.total) return;
  state.offset += state.limit;
  await runQuery();
});
document.querySelectorAll(".tab").forEach((b) => (b.onclick = () => switchTab(b.dataset.tab)));
$("btn-theme").onclick = toggleTheme;
$("btn-theme-float").onclick = toggleTheme;

initTheme();
boot().catch((e) => toast(e.message));
