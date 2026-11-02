// File: ballista/assets/console/console.js
(function () {
  "use strict";

  var SCOPES = { read: 1, run: 2, admin: 3 };
  var RUN_ID = /^r[0-9a-f]{24}$/;
  var BUNDLE_ID = /^[0-9a-f]{32}$/;
  var NAME = /^[A-Za-z0-9_.-]{1,64}$/;
  var DURATION = /^\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d)(\s*\d+(\.\d+)?\s*(ns|us|ms|s|sec|m|min|h|hr|d))*\s*$/;
  var SEGMENT = /^[A-Za-z0-9_-][A-Za-z0-9._-]{0,99}$/;
  var TERMINAL = { passed: 1, failed: 1, stopped: 1, cancelled: 1, error: 1 };
  var ARTIFACTS = [["report.html", "HTML report"], ["report.json", "JSON report"], ["stats.csv", "Stats CSV"], ["history.csv", "History CSV"], ["failures.csv", "Failures CSV"]];
  var METRICS = ["p50", "p95", "p99", "avg", "error_rate", "rps"];
  var MAX_FILE = 16 * 1024 * 1024;
  var MAX_TOTAL = 64 * 1024 * 1024;
  var MAX_TRACE_POINTS = 900;

  var session = null;
  var cleanup = [];

  // ------------------------------------------------------------------ helpers

  function $(id) { return document.getElementById(id); }

  /** Element builder. Text is always set with textContent; attributes are restricted to safe names. */
  function h(tag, attrs) {
    var el = document.createElement(tag);
    var a = attrs || {};
    Object.keys(a).forEach(function (k) {
      var v = a[k];
      if (v === null || v === undefined || v === false) { return; }
      if (k === "text") { el.textContent = String(v); }
      else if (k === "class") { el.className = v; }
      else if (k === "on") { Object.keys(v).forEach(function (ev) { el.addEventListener(ev, v[ev]); }); }
      else if (k === "dataset") { Object.keys(v).forEach(function (d) { el.dataset[d] = v[d]; }); }
      else if (k === "href") { el.setAttribute("href", safeHref(v)); }
      else if (/^(id|type|name|value|placeholder|for|title|role|min|max|step|maxlength|rows|colspan|scope|download|inputmode|autocomplete|spellcheck|checked|disabled|required|multiple|accept|hidden|novalidate|tabindex|aria-[a-z]+)$/.test(k)) {
        if (v === true) { el.setAttribute(k, ""); } else { el.setAttribute(k, String(v)); }
        if (k === "value") { el.value = String(v); }
        if (k === "checked") { el.checked = !!v; }
      }
    });
    for (var i = 2; i < arguments.length; i++) { append(el, arguments[i]); }
    return el;
  }

  function append(el, child) {
    if (child === null || child === undefined || child === false) { return; }
    if (Array.isArray(child)) { child.forEach(function (c) { append(el, c); }); return; }
    el.appendChild(child instanceof Node ? child : document.createTextNode(String(child)));
  }

  /** Only same-origin relative links and fragment routes. */
  function safeHref(v) {
    var s = String(v);
    if (/^#\//.test(s) || /^\/(api\/v1|console)\//.test(s)) { return s; }
    return "#/";
  }

  function fmt(n, digits) {
    if (typeof n !== "number" || !isFinite(n)) { return "-"; }
    return n.toLocaleString(undefined, { maximumFractionDigits: digits === undefined ? 1 : digits, minimumFractionDigits: 0 });
  }

  function pct(r) { return typeof r === "number" && isFinite(r) ? fmt(r * 100, 2) + "%" : "-"; }

  function bytes(n) {
    if (typeof n !== "number") { return "-"; }
    if (n < 1024) { return n + " B"; }
    if (n < 1048576) { return fmt(n / 1024, 1) + " KiB"; }
    return fmt(n / 1048576, 1) + " MiB";
  }

  function duration(secs) {
    if (typeof secs !== "number" || !isFinite(secs)) { return "-"; }
    secs = Math.max(0, Math.round(secs));
    var hr = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
    if (hr) { return hr + "h " + m + "m"; }
    if (m) { return m + "m " + s + "s"; }
    return s + "s";
  }

  function when(iso) {
    if (!iso) { return "-"; }
    var d = new Date(iso);
    if (isNaN(d.getTime())) { return String(iso); }
    var ago = (Date.now() - d.getTime()) / 1000;
    if (ago >= 0 && ago < 60) { return "just now"; }
    if (ago >= 0 && ago < 3600) { return Math.floor(ago / 60) + " min ago"; }
    if (ago >= 0 && ago < 86400) { return Math.floor(ago / 3600) + " h ago"; }
    return d.toLocaleString();
  }

  function elapsed(run) {
    if (!run.started_at) { return null; }
    var end = run.ended_at ? new Date(run.ended_at) : new Date();
    return (end.getTime() - new Date(run.started_at).getTime()) / 1000;
  }

  function can(scope) { return session && SCOPES[session.scope] >= SCOPES[scope]; }

  function stateMark(state) { return h("span", { class: "state", dataset: { state: state }, text: state }); }

  var toastTimer = 0;
  function toast(msg, isError) {
    var el = $("toast");
    el.textContent = msg;
    el.classList.toggle("error", !!isError);
    el.hidden = false;
    window.clearTimeout(toastTimer);
    toastTimer = window.setTimeout(function () { el.hidden = true; }, isError ? 8000 : 4000);
  }

  function uuid() {
    if (window.crypto && crypto.randomUUID) { return crypto.randomUUID(); }
    var b = new Uint8Array(16);
    crypto.getRandomValues(b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    var hex = Array.prototype.map.call(b, function (x) { return ("0" + x.toString(16)).slice(-2); }).join("");
    return hex.slice(0, 8) + "-" + hex.slice(8, 12) + "-" + hex.slice(12, 16) + "-" + hex.slice(16, 20) + "-" + hex.slice(20);
  }

  // ------------------------------------------------------------------ api

  function ApiError(status, code, message) {
    this.status = status;
    this.code = code;
    this.message = message;
  }

  function api(method, path, opts) {
    var o = opts || {};
    var url = "/api/v1" + path;
    if (o.query) {
      var q = Object.keys(o.query).filter(function (k) { return o.query[k] !== undefined && o.query[k] !== ""; })
        .map(function (k) { return encodeURIComponent(k) + "=" + encodeURIComponent(o.query[k]); });
      if (q.length) { url += "?" + q.join("&"); }
    }
    var headers = { "Accept": "application/json" };
    if (method !== "GET") { headers["X-CSRF-Token"] = session ? session.csrf : ""; }
    if (o.headers) { Object.keys(o.headers).forEach(function (k) { headers[k] = o.headers[k]; }); }
    var init = { method: method, credentials: "same-origin", cache: "no-store", redirect: "error", headers: headers };
    if (o.body !== undefined) {
      headers["Content-Type"] = "application/json";
      init.body = JSON.stringify(o.body);
    }
    return fetch(url, init).then(function (res) {
      if (res.status === 401) { toLogin(); throw new ApiError(401, "unauthorized", "Your session ended. Sign in again."); }
      if (res.status === 204) { return null; }
      return res.json().catch(function () { return null; }).then(function (data) {
        if (!res.ok) {
          var err = data && data.error ? data.error : {};
          throw new ApiError(res.status, err.code || "error", err.message || ("Request failed with status " + res.status + "."));
        }
        return data;
      });
    }, function () {
      throw new ApiError(0, "network", "The server could not be reached.");
    });
  }

  function failed(e) {
    if (e && e.status === 401) { return; }
    toast(e && e.message ? e.message : "Something went wrong.", true);
  }

  function toLogin() { window.location.replace("/console/login"); }

  // ------------------------------------------------------------------ trace chart

  function sentence(s) { s = String(s || ""); return s.charAt(0).toUpperCase() + s.slice(1) + (/[.!?]$/.test(s) ? "" : "."); }

  function cssVar(name) { return getComputedStyle(document.documentElement).getPropertyValue(name).trim(); }

  /** Draws p95 latency (left axis), throughput (right axis) and users (shaded) over time. */
  function drawTrace(canvas, points) {
    var rect = canvas.getBoundingClientRect();
    var dpr = window.devicePixelRatio || 1;
    var w = Math.max(200, Math.floor(rect.width)), ht = Math.max(160, Math.floor(rect.height));
    canvas.width = w * dpr;
    canvas.height = ht * dpr;
    var ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, ht);
    var pad = { l: 46, r: 46, t: 10, b: 24 };
    var pw = w - pad.l - pad.r, ph = ht - pad.t - pad.b;
    ctx.font = "11px system-ui, sans-serif";
    ctx.fillStyle = cssVar("--slate");
    ctx.strokeStyle = cssVar("--line-soft");
    ctx.lineWidth = 1;
    if (!points || points.length < 2) {
      ctx.fillText("Waiting for data", pad.l, pad.t + ph / 2);
      return;
    }
    var t0 = points[0].t, t1 = points[points.length - 1].t || 1;
    var maxLat = 1, maxRps = 1, maxUsers = 1;
    points.forEach(function (p) {
      maxLat = Math.max(maxLat, p.p95_ms || 0);
      maxRps = Math.max(maxRps, p.rps || 0);
      maxUsers = Math.max(maxUsers, p.users || 0, p.target || 0);
    });
    maxLat *= 1.1; maxRps *= 1.1; maxUsers *= 1.1;
    function x(t) { return pad.l + ((t - t0) / Math.max(t1 - t0, 1e-6)) * pw; }
    function yL(v) { return pad.t + ph - (v / maxLat) * ph; }
    function yR(v) { return pad.t + ph - (v / maxRps) * ph; }
    function yU(v) { return pad.t + ph - (v / maxUsers) * ph; }
    for (var i = 0; i < 4; i++) {
      var gy = pad.t + (ph * i) / 4;
      ctx.beginPath(); ctx.moveTo(pad.l, gy); ctx.lineTo(pad.l + pw, gy); ctx.stroke();
      ctx.textAlign = "right"; ctx.fillText(fmt(maxLat * (1 - i / 4), maxLat < 10 ? 1 : 0), pad.l - 6, gy + 4);
      ctx.textAlign = "left"; ctx.fillText(fmt(maxRps * (1 - i / 4), maxRps < 10 ? 1 : 0), pad.l + pw + 6, gy + 4);
    }
    ctx.textAlign = "left"; ctx.fillText(duration(0), pad.l, ht - 6);
    ctx.textAlign = "right"; ctx.fillText(duration(t1 - t0), pad.l + pw, ht - 6);
    ctx.textAlign = "right"; ctx.fillText("ms", pad.l - 6, pad.t + ph + 14);
    ctx.textAlign = "left"; ctx.fillText("req/s", pad.l + pw + 6, pad.t + ph + 14);

    ctx.beginPath();
    ctx.moveTo(x(points[0].t), yU(0));
    points.forEach(function (p) { ctx.lineTo(x(p.t), yU(p.users || 0)); });
    ctx.lineTo(x(points[points.length - 1].t), yU(0));
    ctx.closePath();
    ctx.globalAlpha = 0.35;
    ctx.fillStyle = cssVar("--trace-users");
    ctx.fill();
    ctx.globalAlpha = 1;

    function line(color, width, y, key) {
      ctx.beginPath();
      ctx.strokeStyle = color;
      ctx.lineWidth = width;
      ctx.lineJoin = "round";
      points.forEach(function (p, j) { var px = x(p.t), py = y(p[key] || 0); if (j) { ctx.lineTo(px, py); } else { ctx.moveTo(px, py); } });
      ctx.stroke();
    }
    line(cssVar("--slate"), 1.25, yR, "rps");
    line(cssVar("--accent"), 2.25, yL, "p95_ms");
    if (points.some(function (p) { return p.fail_rps > 0; })) { line(cssVar("--fail"), 1.25, yR, "fail_rps"); }
  }

  function traceBlock() {
    var canvas = h("canvas", { role: "img", "aria-label": "Latency and throughput over time" });
    var wrap = h("div", { class: "trace" }, canvas);
    var points = [];
    function redraw() { drawTrace(canvas, points); }
    var ro = new ResizeObserver(redraw);
    ro.observe(wrap);
    cleanup.push(function () { ro.disconnect(); });
    return {
      el: h("div", null, wrap, h("div", { class: "trace-key" },
        keyItem("--accent", "p95 latency"), keyItem("--slate", "requests per second"), keyItem("--trace-users", "users"), keyItem("--fail", "failures per second"))),
      set: function (timeline) { points = (timeline || []).slice(-MAX_TRACE_POINTS); redraw(); }
    };
  }

  function keyItem(varName, label) {
    var s = h("span", { text: label });
    s.style.setProperty("--k", "var(" + varName + ")");
    return s;
  }

  // ------------------------------------------------------------------ shared pieces

  function figure(value, unit, label, mid, bad) {
    return h("div", { class: "figure" + (bad ? " fail" : "") },
      h("div", { class: "value" + (mid ? " mid" : "") }, value, unit ? h("span", { class: "unit", text: unit }) : null),
      h("div", { class: "label", text: label }));
  }

  function figures(snap, run) {
    var t = (snap && snap.total) || {};
    var failing = (t.fail_ratio || 0) > 0;
    return h("div", { class: "figures" },
      figure(fmt(t.rps, 1), "req/s", "Throughput"),
      figure(fmt(t.p95_ms, 1), "ms", "p95 latency"),
      figure(fmt(snap ? snap.users : null, 0), null, "Users", true),
      figure(pct(t.fail_ratio), null, fmt(t.failures || 0, 0) + " failed requests", true, failing),
      figure(fmt(t.count || 0, 0), null, "Requests", true),
      figure(duration(snap ? snap.elapsed_secs : elapsed(run || {})), null, "Elapsed", true));
  }

  function table(headers, rows, emptyText) {
    var thead = h("thead", null, h("tr", null, headers.map(function (c) {
      return h("th", { class: c.num ? "num" : null, scope: "col", text: c.label });
    })));
    var tbody = h("tbody");
    if (!rows.length) { tbody.appendChild(h("tr", { class: "empty-row" }, h("td", { colspan: headers.length, text: emptyText || "Nothing here yet." }))); }
    rows.forEach(function (r) { tbody.appendChild(r); });
    return h("div", { class: "table-wrap" }, h("table", null, thead, tbody));
  }

  function td(text, cls) { return h("td", { class: cls || null, text: text }); }

  function statsRows(snap) {
    var rows = [];
    function row(r, cls) {
      rows.push(h("tr", { class: cls || null },
        td(r.method || ""), td(r.name || ""), td(fmt(r.count, 0), "num"), td(fmt(r.failures, 0), "num"),
        td(fmt(r.p50_ms), "num"), td(fmt(r.p95_ms), "num"), td(fmt(r.p99_ms), "num"), td(fmt(r.max_ms), "num"), td(fmt(r.rps)), td(bytes(r.avg_bytes), "num")));
    }
    (snap.rows || []).forEach(function (r) { row(r); });
    (snap.transactions || []).forEach(function (r) { row(r); });
    if (snap.total) { row(snap.total, "total"); }
    return table([{ label: "Method" }, { label: "Name" }, { label: "Requests", num: 1 }, { label: "Failures", num: 1 }, { label: "p50 ms", num: 1 },
      { label: "p95 ms", num: 1 }, { label: "p99 ms", num: 1 }, { label: "Max ms", num: 1 }, { label: "req/s" }, { label: "Avg size", num: 1 }], rows, "No requests recorded.");
  }

  function errorsTable(snap) {
    var rows = (snap.errors || []).slice(0, 200).map(function (e) {
      return h("tr", null, td(e.method || ""), td(e.name || ""), h("td", { class: "wrap", text: e.message || "" }), td(fmt(e.count, 0), "num"));
    });
    return table([{ label: "Method" }, { label: "Name" }, { label: "Error" }, { label: "Count", num: 1 }], rows, "No errors.");
  }

  function thresholdsTable(list) {
    var rows = (list || []).map(function (t) {
      var bound = t.max !== null && t.max !== undefined ? "at most " + fmt(t.max, 3) : "at least " + fmt(t.min, 3);
      return h("tr", { class: t.passed ? null : "regressed" }, td(t.metric), td(t.request), td(bound),
        td(t.actual === null || t.actual === undefined ? "no data" : fmt(t.actual, 3), "num"),
        h("td", { class: t.passed ? "verdict-pass" : "verdict-fail", text: t.passed ? "Passed" : "Failed" }));
    });
    return table([{ label: "Metric" }, { label: "Request" }, { label: "Limit" }, { label: "Actual", num: 1 }, { label: "Result" }], rows, "This plan has no thresholds.");
  }

  function comparisonBlock(c) {
    if (!c) { return null; }
    var head;
    if (!c.baseline) {
      head = h("p", { class: "notice", text: c.note ? sentence(c.note) : "No baseline to compare with yet." });
    } else if (c.passed) {
      head = h("p", { class: "notice pass" }, "No regressions against ", h("a", { href: "#/runs/" + c.baseline, text: c.baseline }), ".");
    } else {
      head = h("p", { class: "notice fail" }, c.regressions + " regression" + (c.regressions === 1 ? "" : "s") + " against ", h("a", { href: "#/runs/" + c.baseline, text: c.baseline }), ".");
    }
    var rows = (c.changes || []).map(function (ch) {
      var change = ch.change_pct === null || ch.change_pct === undefined ? "-" : (ch.change_pct > 0 ? "+" : "") + fmt(ch.change_pct, 1) + "%";
      var isRate = ch.metric === "error_rate";
      return h("tr", { class: ch.regressed ? "regressed" : null }, td(ch.kind), td(((ch.method || "") + " " + ch.name).trim()), td(ch.metric),
        td(isRate ? pct(ch.baseline) : fmt(ch.baseline, 2), "num"), td(isRate ? pct(ch.current) : fmt(ch.current, 2), "num"), td(change, "num"),
        h("td", { class: ch.regressed ? "verdict-fail" : "muted", text: ch.regressed ? "Regressed" : "Within limit" }));
    });
    var unmatched = c.unmatched && c.unmatched.length ? h("p", { class: "small muted", text: "Only in one of the runs: " + c.unmatched.join(", ") }) : null;
    return h("div", { class: "section" }, h("h2", { text: "Compared with baseline" }), head, c.baseline ? h("div", { class: "section" },
      table([{ label: "Kind" }, { label: "Request" }, { label: "Metric" }, { label: "Baseline", num: 1 }, { label: "This run", num: 1 }, { label: "Change", num: 1 }, { label: "Result" }], rows, "Not enough samples to compare.")) : null, unmatched);
  }

  /** Follows a run's event stream; EventSource sends the session cookie and reconnects on its own. */
  function follow(id, handlers) {
    var es = new EventSource("/api/v1/runs/" + encodeURIComponent(id) + "/events", { withCredentials: true });
    var closed = false;
    function close() { if (!closed) { closed = true; es.close(); } }
    es.addEventListener("run", function (ev) { try { handlers.run(JSON.parse(ev.data)); } catch (e) { /* ignore malformed */ } });
    es.addEventListener("stats", function (ev) { try { handlers.stats(JSON.parse(ev.data)); } catch (e) { /* ignore malformed */ } });
    es.addEventListener("end", function (ev) { close(); try { handlers.end(JSON.parse(ev.data)); } catch (e) { /* ignore malformed */ } });
    es.onerror = function () { if (handlers.error) { handlers.error(); } };
    cleanup.push(close);
    return close;
  }

  function every(ms, fn) {
    var t = window.setInterval(function () { if (!document.hidden) { fn(); } }, ms);
    cleanup.push(function () { window.clearInterval(t); });
  }

  // ------------------------------------------------------------------ overview

  function runRow(r) {
    var s = r.summary;
    var tr = h("tr", { class: "link", tabindex: "0" },
      h("td", null, stateMark(r.state)),
      h("td", null, h("a", { href: "#/runs/" + r.id, text: r.plan_name })),
      td(when(r.created_at)), td(duration(elapsed(r))),
      td(s ? fmt(s.requests, 0) : "-", "num"), td(s ? fmt(s.p95_ms) : "-", "num"), td(s ? pct(s.fail_ratio) : "-", "num"),
      td(r.comparison ? (r.comparison.baseline ? (r.comparison.passed ? "No regressions" : r.comparison.regressions + " regressions") : "First run") : ""),
      td(r.created_by));
    tr.addEventListener("click", function (ev) { if (ev.target.tagName !== "A") { location.hash = "#/runs/" + r.id; } });
    tr.addEventListener("keydown", function (ev) { if (ev.key === "Enter") { location.hash = "#/runs/" + r.id; } });
    return tr;
  }

  function viewOverview(root) {
    var stateFilter = h("select", { id: "state-filter", "aria-label": "Filter by state" },
      ["", "queued", "running", "passed", "failed", "stopped", "cancelled", "error"].map(function (s) { return h("option", { value: s, text: s ? s : "All states" }); }));
    var strip = h("section", { class: "strip empty", "aria-live": "polite" }, h("p", { text: "Loading server status." }));
    var listHost = h("div");
    var more = h("button", { class: "btn", type: "button", text: "Show older runs", hidden: true });
    var queueLine = h("p", { class: "sub muted" });
    var cursor = null;
    var rows = [];
    var liveId = null;
    var trace = null;
    var figuresHost = null;

    root.append(
      h("div", { class: "view-head" }, h("div", null, h("h1", { text: "Runs" }), queueLine),
        can("run") ? h("a", { class: "btn primary", href: "#/bundles", text: "Start a run" }) : null),
      strip,
      h("div", { class: "section" }, h("div", { class: "row" }, h("h2", { text: "History" }), stateFilter), h("div", { class: "section" }, listHost), h("div", { class: "row section" }, more)));

    function renderList() {
      listHost.replaceChildren(table([{ label: "State" }, { label: "Plan" }, { label: "Created" }, { label: "Duration" }, { label: "Requests", num: 1 },
        { label: "p95 ms", num: 1 }, { label: "Errors", num: 1 }, { label: "Baseline" }, { label: "By" }], rows, stateFilter.value ? "No runs in this state." : "No runs yet. Start one from a plan."));
    }

    function loadRuns(reset) {
      if (reset) { cursor = null; }
      return api("GET", "/runs", { query: { limit: 50, state: stateFilter.value, cursor: cursor || undefined } }).then(function (page) {
        var items = page.items.map(runRow);
        rows = reset ? items : rows.concat(items);
        cursor = page.next_cursor;
        more.hidden = !cursor;
        renderList();
      }).catch(failed);
    }

    function renderLive(st) {
      queueLine.textContent = (st.queued ? st.queued + " queued" : "Queue is empty") + ", " + st.workers.length + " pool worker" + (st.workers.length === 1 ? "" : "s") + " connected";
      if (!st.active_run) {
        if (liveId !== null || strip.classList.contains("empty")) {
          liveId = null;
          strip.className = "strip empty";
          strip.replaceChildren(h("p", { text: "No run in progress." + (st.queued ? " The next queued run starts shortly." : "") }));
        }
        return;
      }
      if (liveId === st.active_run) { return; }
      liveId = st.active_run;
      strip.className = "strip";
      trace = traceBlock();
      figuresHost = h("div", null, figures(null));
      var title = h("a", { href: "#/runs/" + liveId, text: "Run in progress" });
      strip.replaceChildren(h("div", { class: "strip-head" }, h("h2", null, title), stateMark("running")), figuresHost, trace.el);
      api("GET", "/runs/" + liveId).then(function (r) { title.textContent = r.plan_name; }).catch(function () {});
      var id = liveId;
      follow(id, {
        run: function () {},
        stats: function (snap) { if (liveId === id) { figuresHost.replaceChildren(figures(snap)); trace.set(snap.timeline); } },
        end: function () { liveId = null; refresh(); loadRuns(true); }
      });
    }

    function refresh() { return api("GET", "/status").then(renderLive).catch(failed); }

    stateFilter.addEventListener("change", function () { loadRuns(true); });
    more.addEventListener("click", function () { loadRuns(false); });
    refresh();
    loadRuns(true);
    every(5000, function () { refresh(); if (!cursor || rows.length <= 50) { loadRuns(true); } });
  }

  // ------------------------------------------------------------------ run detail

  function overridesText(o) {
    var parts = [];
    Object.keys(o || {}).forEach(function (k) {
      var v = o[k];
      parts.push(k.replace(/_/g, " ") + " " + (Array.isArray(v) ? v.join(", ") : v));
    });
    return parts.length ? parts.join("; ") : "none";
  }

  function viewRun(root, id) {
    if (!RUN_ID.test(id)) { root.append(h("p", { class: "notice fail", text: "That is not a valid run id." })); return; }
    var head = h("div", { class: "view-head" });
    var strip = h("section", { class: "strip" });
    var trace = traceBlock();
    var figuresHost = h("div", null, figures(null));
    var verdict = h("div");
    var details = h("div", { class: "section two" });
    var statsHost = h("div", { class: "section" });
    var errorsHost = h("div", { class: "section" });
    var compareHost = h("div");
    var run = null;
    root.append(head, strip, verdict, compareHost, statsHost, errorsHost, details);
    strip.append(figuresHost, trace.el);

    function renderHead(r) {
      run = r;
      var actions = h("div", { class: "row" });
      if (can("run") && (r.state === "queued" || r.state === "running")) {
        actions.append(h("button", { class: "btn danger", type: "button", text: r.state === "queued" ? "Cancel run" : "Stop run", on: { click: stop } }));
      }
      if (can("run") && TERMINAL[r.state]) {
        actions.append(h("button", { class: "btn", type: "button", text: "Run again", on: { click: rerun } }));
      }
      if (can("admin") && TERMINAL[r.state]) {
        actions.append(h("button", { class: "btn danger", type: "button", text: "Delete run", on: { click: remove } }));
      }
      head.replaceChildren(
        h("div", null, h("div", { class: "row" }, h("h1", { text: r.plan_name }), stateMark(r.state)),
          h("p", { class: "sub muted", text: "Started " + when(r.started_at || r.created_at) + " by " + r.created_by + (r.workers ? ", on " + r.workers + " workers" : "") })),
        actions);
      var notices = [];
      if (r.state === "queued") { notices.push(h("p", { class: "notice", text: "Waiting in the queue" + (typeof r.queue_position === "number" ? " at position " + (r.queue_position + 1) : "") + "." })); }
      if (r.error) { notices.push(h("p", { class: "notice fail", text: sentence(r.error) })); }
      if (r.stopped_by) { notices.push(h("p", { class: "small muted", text: "Stopped by " + r.stopped_by + "." })); }
      if (r.summary) {
        notices.push(h("div", { class: "section" }, h("h2", { text: "Thresholds" }), h("div", { class: "section" }, thresholdsTable(r.summary.thresholds))));
      }
      verdict.replaceChildren.apply(verdict, notices);
      compareHost.replaceChildren(TERMINAL[r.state] ? compareSection(r) : h("span"));
      var labels = Object.keys(r.labels || {});
      details.replaceChildren(
        h("div", null, h("h2", { text: "Details" }), h("dl", { class: "facts section" },
          h("dt", { text: "Run" }), h("dd", { text: r.id }),
          h("dt", { text: "Plan" }), h("dd", null, h("a", { href: "#/bundles/" + r.bundle, text: r.bundle })),
          h("dt", { text: "Created" }), h("dd", { text: r.created_at }),
          h("dt", { text: "Finished" }), h("dd", { text: r.ended_at || "-" }),
          h("dt", { text: "Overrides" }), h("dd", { text: overridesText(r.overrides) }),
          h("dt", { text: "Labels" }), h("dd", null, labels.length ? h("div", { class: "chips" }, labels.map(function (k) { return h("span", { class: "chip", text: k + "=" + r.labels[k] }); })) : "none"),
          h("dt", { text: "Regression gate" }), h("dd", { text: r.regression ? "fail above " + (r.regression.max_regression_pct === undefined ? 10 : r.regression.max_regression_pct) + "% against " + (r.regression.baseline || "auto") : "off" }))),
        h("div", null, h("h2", { text: "Reports" }), TERMINAL[r.state] && r.summary ? h("div", { class: "row section" }, ARTIFACTS.map(function (a) {
          var href = "/api/v1/runs/" + r.id + "/artifacts/" + a[0];
          return a[0] === "report.html" ? h("a", { class: "btn", href: href, text: a[1], title: "Opens in a new tab" }) : h("a", { class: "btn", href: href, download: r.id + "-" + a[0], text: a[1] });
        })) : h("p", { class: "muted section", text: "Reports are available when the run finishes." })));
      details.querySelectorAll("a[title]").forEach(function (a) { a.setAttribute("target", "_blank"); a.setAttribute("rel", "noopener noreferrer"); });
    }

    function renderStats(snap) {
      figuresHost.replaceChildren(figures(snap, run));
      trace.set(snap.timeline);
      statsHost.replaceChildren(h("h2", { text: "Requests" }), h("div", { class: "section" }, statsRows(snap)));
      errorsHost.replaceChildren(h("h2", { text: "Errors" }), h("div", { class: "section" }, errorsTable(snap)));
    }

    function stop() {
      if (!window.confirm(run.state === "queued" ? "Cancel this queued run?" : "Stop this run? Users finish their current step and the report is kept.")) { return; }
      api("POST", "/runs/" + id + "/stop").then(function (r) { renderHead(r); toast(r.state === "cancelled" ? "Run cancelled." : "Stopping run."); }).catch(failed);
    }

    function rerun() {
      api("POST", "/runs", { body: { bundle: run.bundle, overrides: run.overrides || {}, labels: run.labels || {}, regression: run.regression || undefined }, headers: { "Idempotency-Key": uuid() } })
        .then(function (r) { toast("Run queued."); location.hash = "#/runs/" + r.id; }).catch(failed);
    }

    function remove() {
      if (!window.confirm("Delete this run and its reports? This cannot be undone.")) { return; }
      api("DELETE", "/runs/" + id).then(function () { toast("Run deleted."); location.hash = "#/"; }).catch(failed);
    }

    function compareSection(r) {
      var host = h("div");
      var baseline = h("input", { id: "cmp-baseline", placeholder: "auto", maxlength: 25, spellcheck: "false" });
      var limit = h("input", { id: "cmp-limit", type: "number", min: 0, max: 10000, step: "any", placeholder: "10" });
      var boxes = METRICS.map(function (m) {
        var on = ["p95", "p99", "error_rate"].indexOf(m) >= 0;
        return h("label", { class: "row" }, h("input", { type: "checkbox", value: m, checked: on }), m.replace("_", " "));
      });
      var form = h("form", { class: "section", novalidate: true },
        h("fieldset", null, h("legend", { text: "Compare with another run" }),
          h("div", { class: "fields" },
            h("div", { class: "field" }, h("label", { for: "cmp-baseline", text: "Baseline run (blank for the last passed run)" }), baseline),
            h("div", { class: "field" }, h("label", { for: "cmp-limit", text: "Allowed slowdown %" }), limit)),
          h("div", { class: "row section" }, boxes),
          h("div", { class: "row section" }, h("button", { class: "btn", type: "submit", text: "Compare" }))));
      form.addEventListener("submit", function (ev) {
        ev.preventDefault();
        var b = baseline.value.trim();
        if (b && !RUN_ID.test(b)) { toast("Baseline must be a run id such as r0123456789abcdef01234567.", true); return; }
        var metrics = boxes.map(function (l) { return l.querySelector("input"); }).filter(function (i) { return i.checked; }).map(function (i) { return i.value; });
        if (!metrics.length) { toast("Pick at least one metric to compare.", true); return; }
        api("GET", "/runs/" + r.id + "/compare", { query: { baseline: b || "auto", max_regression_pct: limit.value, metrics: metrics.join(",") } })
          .then(function (c) { host.replaceChildren(comparisonBlock(c)); }).catch(failed);
      });
      host.append(comparisonBlock(r.comparison) || h("span"));
      return h("div", null, host, form);
    }

    api("GET", "/runs/" + id).then(function (r) {
      renderHead(r);
      if (TERMINAL[r.state]) {
        return api("GET", "/runs/" + id + "/stats").then(renderStats).catch(function () {
          figuresHost.replaceChildren(h("p", { class: "muted", text: "No statistics were recorded for this run." }));
        });
      }
      follow(id, {
        run: renderHead,
        stats: renderStats,
        end: function (fin) { renderHead(fin); api("GET", "/runs/" + id + "/stats").then(renderStats).catch(function () {}); }
      });
      return null;
    }).catch(function (e) {
      if (e.status === 404) { root.replaceChildren(h("p", { class: "notice fail", text: "This run does not exist or was deleted." }), h("p", { class: "section" }, h("a", { href: "#/", text: "Back to runs" }))); }
      else { failed(e); }
    });
  }

  // ------------------------------------------------------------------ plans

  function viewBundles(root) {
    var host = h("div");
    root.append(h("div", { class: "view-head" }, h("div", null, h("h1", { text: "Plans" }), h("p", { class: "sub muted", text: "Stored test plans. Open one to start a run." })),
      can("run") ? h("a", { class: "btn primary", href: "#/new", text: "Add a plan" }) : null), host);
    api("GET", "/bundles").then(function (page) {
      var items = page.items.slice().sort(function (a, b) { return a.created_at < b.created_at ? 1 : -1; });
      host.replaceChildren(table([{ label: "Plan" }, { label: "Scenarios" }, { label: "Files", num: 1 }, { label: "Size", num: 1 }, { label: "Added" }, { label: "By" }],
        items.map(function (b) {
          var tr = h("tr", { class: "link" }, h("td", null, h("a", { href: "#/bundles/" + b.id, text: b.plan_name })), td(b.scenarios.join(", ")),
            td(fmt(b.files.length, 0), "num"), td(bytes(b.size), "num"), td(when(b.created_at)), td(b.created_by));
          tr.addEventListener("click", function (ev) { if (ev.target.tagName !== "A") { location.hash = "#/bundles/" + b.id; } });
          return tr;
        }), "No plans stored yet. Add one, or upload from an SDK."));
    }).catch(failed);
  }

  function numberInput(id, attrs) { return h("input", Object.assign({ id: id, type: "number", inputmode: "decimal" }, attrs)); }

  function viewBundle(root, id) {
    if (!BUNDLE_ID.test(id)) { root.append(h("p", { class: "notice fail", text: "That is not a valid plan id." })); return; }
    api("GET", "/bundles/" + id).then(function (b) {
      var actions = h("div", { class: "row" });
      if (can("admin")) {
        actions.append(h("button", { class: "btn danger", type: "button", text: "Delete plan", on: { click: function () {
          if (!window.confirm("Delete this plan? Runs that used it keep their reports.")) { return; }
          api("DELETE", "/bundles/" + id).then(function () { toast("Plan deleted."); location.hash = "#/bundles"; }).catch(failed);
        } } }));
      }
      root.append(h("div", { class: "view-head" }, h("div", null, h("h1", { text: b.plan_name }), h("p", { class: "sub muted", text: "Added " + when(b.created_at) + " by " + b.created_by })), actions));
      if (can("run")) { root.append(launchForm(b)); }
      root.append(h("div", { class: "section two" },
        h("div", null, h("h2", { text: "Scenarios" }), h("div", { class: "chips section" }, b.scenarios.map(function (s) { return h("span", { class: "chip", text: s }); }))),
        h("div", null, h("h2", { text: "Files" }), h("div", { class: "section" }, table([{ label: "Path" }, { label: "Size", num: 1 }, { label: "SHA-256" }],
          b.files.map(function (f) { return h("tr", null, td(f.path), td(bytes(f.size), "num"), td(f.sha256.slice(0, 16), "muted")); }), "The plan carries no extra files.")))));
    }).catch(function (e) {
      if (e.status === 404) { root.append(h("p", { class: "notice fail", text: "This plan does not exist or was deleted." })); } else { failed(e); }
    });
  }

  function launchForm(b) {
    var f = {
      users: numberInput("o-users", { min: 1, step: "any", placeholder: "from plan" }),
      rate: numberInput("o-rate", { min: 0, step: "any", placeholder: "from plan" }),
      time: h("input", { id: "o-time", placeholder: "from plan, e.g. 5m", maxlength: 32 }),
      iterations: numberInput("o-iter", { min: 1, step: 1, placeholder: "unlimited" }),
      workers: numberInput("o-workers", { min: 0, max: 512, step: 1, placeholder: "all available" }),
      tags: h("input", { id: "o-tags", placeholder: "all scenarios", maxlength: 512 }),
      host: h("input", { id: "o-host", placeholder: "from plan", maxlength: 512, spellcheck: "false" }),
      labels: h("input", { id: "o-labels", placeholder: "key=value, key=value", maxlength: 2048 }),
      gate: h("input", { id: "o-gate", type: "checkbox" }),
      gatePct: numberInput("o-gate-pct", { min: 0, max: 10000, step: "any", placeholder: "10" })
    };
    function field(id, label, input) { return h("div", { class: "field" }, h("label", { for: id, text: label }), input); }
    var error = h("p", { class: "form-error", role: "alert", hidden: true });
    var submit = h("button", { class: "btn primary", type: "submit", text: "Start run" });
    var form = h("form", { class: "section", novalidate: true },
      h("fieldset", null, h("legend", { text: "Start a run" }),
        h("div", { class: "fields" },
          field("o-users", "Users or rate", f.users), field("o-rate", "Ramp per second", f.rate), field("o-time", "Run time", f.time),
          field("o-iter", "Iterations", f.iterations), field("o-workers", "Pool workers (0 for this server only)", f.workers),
          field("o-tags", "Only tags", f.tags), field("o-host", "Target URL", f.host), field("o-labels", "Labels", f.labels)),
        h("div", { class: "row section" }, f.gate, h("label", { for: "o-gate", text: "Fail the run if it is slower than the last passed run by more than" }), f.gatePct, h("span", { class: "muted", text: "%" })),
        h("div", { class: "row section" }, submit), error));

    function fail(msg, input) { error.textContent = msg; error.hidden = false; if (input) { input.focus(); } return null; }

    function collect() {
      error.hidden = true;
      var o = {};
      if (f.users.value !== "") { var u = Number(f.users.value); if (!(u > 0)) { return fail("Users must be a positive number.", f.users); } o.users = u; }
      if (f.rate.value !== "") { var r = Number(f.rate.value); if (!(r > 0)) { return fail("Ramp must be a positive number.", f.rate); } o.spawn_rate = r; }
      if (f.time.value.trim()) { if (!DURATION.test(f.time.value)) { return fail("Run time must be a duration such as 90s, 5m or 1h 30m.", f.time); } o.run_time = f.time.value.trim(); }
      if (f.iterations.value !== "") { var it = Number(f.iterations.value); if (!(Number.isInteger(it) && it >= 1)) { return fail("Iterations must be a whole number of at least 1.", f.iterations); } o.iterations = it; }
      if (f.workers.value !== "") { var w = Number(f.workers.value); if (!(Number.isInteger(w) && w >= 0 && w <= 512)) { return fail("Workers must be a whole number from 0 to 512.", f.workers); } o.workers = w; }
      if (f.tags.value.trim()) {
        var tags = f.tags.value.split(",").map(function (t) { return t.trim(); }).filter(Boolean);
        if (tags.length > 64 || !tags.every(function (t) { return NAME.test(t); })) { return fail("Tags are simple names separated by commas.", f.tags); }
        o.tags = tags;
      }
      if (f.host.value.trim()) { if (!/^https?:\/\/[^\s/@]+(\/\S*)?$/.test(f.host.value.trim())) { return fail("Target URL must start with http:// or https:// and have no credentials.", f.host); } o.host = f.host.value.trim(); }
      var labels = {};
      var pairs = f.labels.value.split(",").map(function (p) { return p.trim(); }).filter(Boolean);
      for (var i = 0; i < pairs.length; i++) {
        var eq = pairs[i].indexOf("=");
        var k = eq > 0 ? pairs[i].slice(0, eq).trim() : "";
        var v = eq > 0 ? pairs[i].slice(eq + 1).trim() : "";
        if (!NAME.test(k) || v.length > 256 || /[\u0000-\u001f\u007f]/.test(v)) { return fail("Labels are key=value pairs; keys use letters, digits, dot, dash or underscore.", f.labels); }
        labels[k] = v;
      }
      if (Object.keys(labels).length > 32) { return fail("At most 32 labels.", f.labels); }
      var body = { bundle: b.id, overrides: o, labels: labels };
      if (f.gate.checked) {
        var gate = { baseline: "auto" };
        if (f.gatePct.value !== "") { var p = Number(f.gatePct.value); if (!(p >= 0 && p <= 10000)) { return fail("Allowed slowdown must be between 0 and 10000%.", f.gatePct); } gate.max_regression_pct = p; }
        body.regression = gate;
      }
      return body;
    }

    var key = uuid();
    form.addEventListener("input", function () { key = uuid(); });
    form.addEventListener("submit", function (ev) {
      ev.preventDefault();
      var body = collect();
      if (!body) { return; }
      submit.disabled = true;
      api("POST", "/runs", { body: body, headers: { "Idempotency-Key": key } }).then(function (r) {
        toast("Run queued.");
        location.hash = "#/runs/" + r.id;
      }).catch(function (e) { submit.disabled = false; if (e.status === 422) { fail(e.message); } else { failed(e); } });
    });
    return form;
  }

  // ------------------------------------------------------------------ new plan

  var SAMPLE = JSON.stringify({
    name: "checkout smoke",
    target: { base_url: "https://shop.example.com/", timeout: "5s" },
    load: { mode: "users", stages: [{ duration: "30s", target: 10 }, { duration: "2m", target: 10 }] },
    scenario: [{ name: "browse", wait: { between: ["500ms", "1500ms"] }, step: [{ get: "/items", name: "list items", expect: 200 }] }],
    threshold: [{ metric: "p95", request: "*", max: 500 }, { metric: "error_rate", request: "*", max: 0.01 }]
  }, null, 2);

  function readBase64(file) {
    return new Promise(function (resolve, reject) {
      var r = new FileReader();
      r.onload = function () { var s = String(r.result); resolve(s.slice(s.indexOf(",") + 1)); };
      r.onerror = function () { reject(new Error("Could not read " + file.name + ".")); };
      r.readAsDataURL(file);
    });
  }

  function validPath(p) {
    if (!p || p.length > 200 || p.charAt(0) === "/") { return false; }
    var parts = p.split("/");
    return parts.length <= 8 && parts[0] !== "inline" && p !== "plan.toml" && p !== "bundle.json" && parts.every(function (s) { return SEGMENT.test(s); });
  }

  function viewNew(root) {
    if (!can("run")) { root.append(h("p", { class: "notice", text: "Adding plans needs a token with the run scope." })); return; }
    var text = h("textarea", { id: "plan-text", rows: 22, spellcheck: "false", "aria-describedby": "plan-help" });
    text.value = SAMPLE;
    var fileRows = h("div", { class: "section" });
    var result = h("div", { class: "section" });
    var files = [];
    var validate = h("button", { class: "btn", type: "button", text: "Check plan" });
    var save = h("button", { class: "btn primary", type: "button", text: "Save plan" });

    function planValue() {
      var raw = text.value;
      if (!raw.trim()) { throw new Error("The plan is empty."); }
      if (new Blob([raw]).size > 1048576) { throw new Error("The plan is larger than 1 MiB."); }
      if (raw.trim().charAt(0) === "{") {
        var doc;
        try { doc = JSON.parse(raw); } catch (e) { throw new Error("The plan looks like JSON but does not parse: " + e.message); }
        if (!doc || typeof doc !== "object" || Array.isArray(doc)) { throw new Error("A JSON plan must be an object."); }
        return doc;
      }
      return raw;
    }

    function addFileRow(file, needed) {
      if (file && !needed) {
        var waiting = files.filter(function (f) { return !f.file && f.path.value.split("/").pop() === file.name; })[0];
        if (waiting) { waiting.attach(file); return; }
      }
      var entry = { file: file, path: h("input", { value: needed || ("files/" + (file ? file.name.replace(/[^A-Za-z0-9._-]/g, "_") : "")), maxlength: 200, spellcheck: "false", "aria-label": "Path in plan" }) };
      var pick = h("input", { type: "file", "aria-label": "Choose file" });
      pick.addEventListener("change", function () { entry.file = pick.files[0] || null; if (entry.file && !needed && !entry.path.value.trim()) { entry.path.value = "files/" + entry.file.name; } });
      var removeBtn = h("button", { class: "btn ghost small", type: "button", text: "Remove" });
      var picked = h("span", { class: "muted", text: file ? file.name + ", " + bytes(file.size) : "" });
      var row = h("div", { class: "row" }, entry.path, file ? picked : pick, removeBtn);
      entry.attach = function (f) { entry.file = f; picked.textContent = f.name + ", " + bytes(f.size); pick.replaceWith(picked); };
      removeBtn.addEventListener("click", function () { files.splice(files.indexOf(entry), 1); row.remove(); });
      files.push(entry);
      fileRows.append(row);
    }

    function check() {
      var plan;
      try { plan = planValue(); } catch (e) { result.replaceChildren(h("p", { class: "notice fail", text: e.message })); return Promise.resolve(null); }
      return api("POST", "/plans/validate", { body: { plan: plan } }).then(function (c) {
        var have = files.map(function (f) { return f.path.value.trim(); });
        var missing = c.files.filter(function (p) { return have.indexOf(p) < 0; });
        missing.forEach(function (p) { addFileRow(null, p); });
        result.replaceChildren(h("p", { class: "notice pass", text: "Plan " + c.name + " is valid with scenarios " + c.scenarios.join(", ") + "." }),
          c.files.length ? h("p", { class: "small muted section", text: "It needs these files: " + c.files.join(", ") + ". Choose each one below." }) : null);
        return c;
      }).catch(function (e) {
        result.replaceChildren(h("p", { class: "notice fail", text: e.message }));
        return null;
      });
    }

    function upload() {
      var plan;
      try { plan = planValue(); } catch (e) { result.replaceChildren(h("p", { class: "notice fail", text: e.message })); return; }
      var total = 0;
      for (var i = 0; i < files.length; i++) {
        var p = files[i].path.value.trim();
        if (!validPath(p)) { result.replaceChildren(h("p", { class: "notice fail", text: "File path " + p + " is not allowed. Use relative paths of letters, digits, dot, dash and underscore." })); return; }
        if (!files[i].file) { result.replaceChildren(h("p", { class: "notice fail", text: "Choose a file for " + p + "." })); return; }
        if (files[i].file.size > MAX_FILE) { result.replaceChildren(h("p", { class: "notice fail", text: p + " is larger than 16 MiB." })); return; }
        total += files[i].file.size;
      }
      if (total > MAX_TOTAL) { result.replaceChildren(h("p", { class: "notice fail", text: "Files total more than 64 MiB." })); return; }
      save.disabled = true;
      Promise.all(files.map(function (f) { return readBase64(f.file).then(function (b64) { return [f.path.value.trim(), b64]; }); })).then(function (pairs) {
        var payload = {};
        pairs.forEach(function (p) { payload[p[0]] = p[1]; });
        return api("POST", "/bundles", { body: { plan: plan, files: payload, labels: {} } });
      }).then(function (b) {
        toast("Plan saved.");
        location.hash = "#/bundles/" + b.id;
      }).catch(function (e) {
        save.disabled = false;
        result.replaceChildren(h("p", { class: "notice fail", text: e.message }));
      });
    }

    var drop = h("input", { type: "file", multiple: true, id: "plan-files" });
    drop.addEventListener("change", function () { Array.prototype.forEach.call(drop.files, function (f) { addFileRow(f); }); drop.value = ""; });
    validate.addEventListener("click", check);
    save.addEventListener("click", upload);
    root.append(
      h("div", { class: "view-head" }, h("div", null, h("h1", { text: "New plan" }),
        h("p", { class: "sub muted", id: "plan-help", text: "Paste a JSON plan document or plan file text. Scripts and data can go inline in the plan, or be attached as files." }))),
      text,
      h("div", { class: "section" }, h("h2", { text: "Files" }), h("div", { class: "row section" }, h("label", { for: "plan-files", text: "Attach files" }), drop), fileRows),
      h("div", { class: "row section" }, validate, save),
      result);
  }

  // ------------------------------------------------------------------ workers

  function viewWorkers(root) {
    var host = h("div");
    var limits = h("dl", { class: "facts section" });
    root.append(h("div", { class: "view-head" }, h("div", null, h("h1", { text: "Workers" }), h("p", { class: "sub muted", text: "Pool workers spread a run's users across machines. A run uses every idle worker unless it asks for a number." }))),
      host, h("div", { class: "section" }, h("h2", { text: "Server limits" }), limits));
    function load() {
      api("GET", "/status").then(function (st) {
        $("server-version").textContent = "Server " + st.version;
        host.replaceChildren(table([{ label: "Name" }, { label: "Address" }, { label: "State" }, { label: "Cores", num: 1 }, { label: "Users", num: 1 },
          { label: "CPU", num: 1 }, { label: "Scheduler lag ms", num: 1 }, { label: "Last report" }],
          st.workers.map(function (w) {
            return h("tr", { class: w.saturated ? "regressed" : null }, td(w.name), td(w.address), h("td", null, stateMark(w.saturated ? "saturated" : w.state)),
              td(fmt(w.cores, 0), "num"), td(fmt(w.users, 0), "num"), td(w.cpu_pct === null || w.cpu_pct === undefined ? "-" : fmt(w.cpu_pct, 0) + "%", "num"),
              td(fmt(w.scheduler_lag_ms, 1), "num"), td(fmt(w.last_report_secs, 0) + " s ago"));
          }), "No workers connected. Runs execute on this server. Start workers with ballista worker --controller pointing at the worker port."));
        limits.replaceChildren(
          h("dt", { text: "Most users per run" }), h("dd", { text: fmt(st.limits.max_users, 0) }),
          h("dt", { text: "Longest run" }), h("dd", { text: duration(st.limits.max_run_time_secs) }),
          h("dt", { text: "Queue size" }), h("dd", { text: fmt(st.limits.max_queue, 0) }));
      }).catch(failed);
    }
    load();
    every(5000, load);
  }

  // ------------------------------------------------------------------ router

  var ROUTES = [
    [/^#?\/?$/, "overview", viewOverview],
    [/^#\/runs\/([^/]+)$/, "overview", viewRun],
    [/^#\/bundles$/, "bundles", viewBundles],
    [/^#\/bundles\/([^/]+)$/, "bundles", viewBundle],
    [/^#\/new$/, "new", viewNew],
    [/^#\/workers$/, "workers", viewWorkers]
  ];

  function route() {
    cleanup.forEach(function (fn) { try { fn(); } catch (e) { /* ignore */ } });
    cleanup = [];
    var hash = location.hash || "#/";
    var view = $("view");
    view.replaceChildren();
    for (var i = 0; i < ROUTES.length; i++) {
      var m = hash.match(ROUTES[i][0]);
      if (m) {
        document.querySelectorAll("[data-nav]").forEach(function (a) {
          if (a.dataset.nav === ROUTES[i][1]) { a.setAttribute("aria-current", "page"); } else { a.removeAttribute("aria-current"); }
        });
        ROUTES[i][2](view, m[1] ? decodeURIComponent(m[1]) : undefined);
        view.focus({ preventScroll: true });
        return;
      }
    }
    view.append(h("p", { class: "notice", text: "This page does not exist." }), h("p", { class: "section" }, h("a", { href: "#/", text: "Back to runs" })));
  }

  function start() {
    fetch("/console/api/session", { credentials: "same-origin", cache: "no-store" }).then(function (res) {
      if (!res.ok) { toLogin(); return null; }
      return res.json();
    }).then(function (s) {
      if (!s) { return; }
      session = s;
      $("who").textContent = s.name + ", " + s.scope + " access";
      document.querySelectorAll("[data-scope]").forEach(function (el) { el.hidden = !can(el.dataset.scope); });
      $("signout").addEventListener("click", function () {
        fetch("/console/api/logout", { method: "POST", credentials: "same-origin", headers: { "X-CSRF-Token": session.csrf } }).finally(toLogin);
      });
      api("GET", "/health").then(function (hl) { $("server-version").textContent = "Server " + hl.version; }).catch(function () {});
      window.addEventListener("hashchange", route);
      route();
    }).catch(toLogin);
  }

  start();
}());
