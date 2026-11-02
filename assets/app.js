// File: ballista/assets/app.js
(function () {
  "use strict";

  var csrf = "";
  var mode = "users";
  var source = null;
  var reconnectDelay = 1000;
  var last = null;
  var iterationLimit = null;
  var logSeq = 0;
  var logTimer = 0;
  var MAX_LOG_LINES = 1000;

  function $(id) { return document.getElementById(id); }

  function fmt(n, digits) {
    if (typeof n !== "number" || !isFinite(n)) { return "-"; }
    return n.toLocaleString(undefined, { maximumFractionDigits: digits, minimumFractionDigits: 0 });
  }

  function fmtDuration(secs) {
    secs = Math.max(0, Math.floor(secs || 0));
    var h = Math.floor(secs / 3600);
    var m = Math.floor((secs % 3600) / 60);
    var s = secs % 60;
    if (h > 0) { return h + "h " + m + "m"; }
    if (m > 0) { return m + "m " + s + "s"; }
    return s + "s";
  }

  function notice(msg, isError) {
    var el = $("notice");
    el.textContent = msg;
    el.classList.toggle("error", !!isError);
    el.hidden = !msg;
    if (msg) {
      window.clearTimeout(notice.timer);
      notice.timer = window.setTimeout(function () { el.hidden = true; }, 6000);
    }
  }

  function toLogin() { window.location.replace("/login"); }

  function api(method, path, body) {
    var opts = {
      method: method,
      credentials: "same-origin",
      cache: "no-store",
      headers: { "X-CSRF-Token": csrf }
    };
    if (body !== undefined) {
      opts.headers["Content-Type"] = "application/json";
      opts.body = JSON.stringify(body);
    }
    return fetch(path, opts).then(function (res) {
      if (res.status === 401) { toLogin(); throw new Error("unauthorized"); }
      return res.json().catch(function () { return {}; }).then(function (data) {
        if (!res.ok) {
          var msg = (data && data.error) ? data.error : ("request failed (" + res.status + ")");
          if (data && data.detail) { msg += ": " + data.detail; }
          throw new Error(msg);
        }
        return data;
      });
    });
  }

  function cell(tr, text, cls) {
    var td = document.createElement("td");
    td.textContent = text;
    if (cls) { td.className = cls; }
    tr.appendChild(td);
    return td;
  }

  function replaceRows(tbody, rows) {
    var frag = document.createDocumentFragment();
    rows.forEach(function (r) { frag.appendChild(r); });
    tbody.replaceChildren(frag);
  }

  var extraLabels = [];

  function pctLabel(p) { return "p" + String(p).replace(/\.0+$/, ""); }

  function syncExtraHeaders(labels) {
    var key = JSON.stringify(labels || []);
    if (key === JSON.stringify(extraLabels)) { return; }
    extraLabels = labels || [];
    var headRow = document.querySelector("#t-requests").parentNode.querySelector("thead tr");
    Array.prototype.forEach.call(headRow.querySelectorAll("th.pct"), function (th) { th.remove(); });
    extraLabels.forEach(function (p) {
      var th = document.createElement("th");
      th.className = "pct";
      th.textContent = pctLabel(p);
      headRow.appendChild(th);
    });
  }

  function requestRow(r, isTotal) {
    var tr = document.createElement("tr");
    if (r.failures > 0 && !isTotal) { tr.className = "bad"; }
    cell(tr, isTotal ? "" : r.scenario, "l");
    cell(tr, isTotal ? "" : r.method, "l");
    cell(tr, r.name, "l");
    cell(tr, fmt(r.count, 0));
    cell(tr, fmt(r.failures, 0));
    cell(tr, fmt(r.rps, 1));
    cell(tr, fmt(r.avg_ms, 1));
    cell(tr, fmt(r.min_ms, 1));
    cell(tr, fmt(r.p50_ms, 1));
    cell(tr, fmt(r.p90_ms, 1));
    cell(tr, fmt(r.p95_ms, 1));
    cell(tr, fmt(r.p99_ms, 1));
    cell(tr, fmt(r.max_ms, 1));
    cell(tr, fmt(r.avg_bytes, 0));
    (r.percentiles || []).forEach(function (v) { cell(tr, fmt(v, 1)); });
    return tr;
  }

  function transactionRow(r) {
    var tr = document.createElement("tr");
    if (r.failures > 0) { tr.className = "bad"; }
    cell(tr, r.scenario, "l");
    cell(tr, r.name, "l");
    cell(tr, fmt(r.count, 0));
    cell(tr, fmt(r.failures, 0));
    cell(tr, fmt(r.rps, 2));
    cell(tr, fmt(r.avg_ms, 1));
    cell(tr, fmt(r.p50_ms, 1));
    cell(tr, fmt(r.p95_ms, 1));
    cell(tr, fmt(r.p99_ms, 1));
    cell(tr, fmt(r.max_ms, 1));
    return tr;
  }

  function pct(v) { return fmt((v || 0) * 100, 1) + "%"; }

  function ratioScenarioRow(r) {
    var tr = document.createElement("tr");
    cell(tr, r.name, "l");
    cell(tr, pct(r.weight_share));
    cell(tr, fmt(r.users, 0));
    cell(tr, pct(r.user_share));
    cell(tr, fmt(r.iterations, 0));
    cell(tr, pct(r.iteration_share));
    return tr;
  }

  function ratioTaskRow(t) {
    var tr = document.createElement("tr");
    cell(tr, t.scenario, "l");
    cell(tr, t.task, "l");
    cell(tr, fmt(t.count, 0));
    cell(tr, pct(t.share));
    return tr;
  }

  function iterationRow(r) {
    var tr = document.createElement("tr");
    if (r.failures > 0) { tr.className = "bad"; }
    cell(tr, r.scenario, "l");
    cell(tr, fmt(r.count, 0));
    cell(tr, fmt(r.failures, 0));
    cell(tr, fmt(r.rps, 1));
    cell(tr, fmt(r.avg_ms, 1));
    cell(tr, fmt(r.p50_ms, 1));
    cell(tr, fmt(r.p95_ms, 1));
    cell(tr, fmt(r.p99_ms, 1));
    cell(tr, fmt(r.max_ms, 1));
    return tr;
  }

  function errorRow(e) {
    var tr = document.createElement("tr");
    cell(tr, fmt(e.count, 0));
    cell(tr, e.scenario, "l");
    cell(tr, e.method + " " + e.name, "l");
    cell(tr, e.message, "l msg");
    return tr;
  }

  function thresholdRow(t) {
    var tr = document.createElement("tr");
    var opt = function (v) { return (v === null || v === undefined) ? "-" : fmt(v, 3); };
    cell(tr, t.metric, "l");
    cell(tr, t.request, "l");
    cell(tr, opt(t.min));
    cell(tr, opt(t.max));
    cell(tr, opt(t.actual));
    var td = cell(tr, "");
    var pill = document.createElement("span");
    pill.className = "pill " + (t.passed ? "pass" : "fail");
    pill.textContent = t.passed ? "pass" : "fail";
    td.appendChild(pill);
    return tr;
  }

  function workerRow(w) {
    var tr = document.createElement("tr");
    if (w.saturated) { tr.className = "bad"; }
    cell(tr, String(w.id));
    cell(tr, w.name, "l");
    cell(tr, w.address, "l");
    cell(tr, w.state + (w.saturated ? " (saturated)" : ""), "l");
    cell(tr, fmt(w.cores, 0));
    cell(tr, fmt(w.users, 0));
    cell(tr, fmt(w.target, 1));
    cell(tr, fmt(w.iterations, 0));
    cell(tr, (typeof w.cpu_pct === "number") ? fmt(w.cpu_pct, 0) + "%" : "n/a");
    cell(tr, fmt(w.scheduler_lag_ms, 1));
    cell(tr, fmt(w.last_report_secs, 0) + "s ago");
    return tr;
  }

  function cssVar(name) {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  function drawChart(canvas, points, series) {
    var dpr = window.devicePixelRatio || 1;
    var w = canvas.clientWidth;
    var h = canvas.clientHeight || 200;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    var ctx = canvas.getContext("2d");
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);
    var padL = 48, padR = 10, padT = 22, padB = 22;
    var muted = cssVar("--muted");
    var grid = cssVar("--grid");
    ctx.font = "11px system-ui, sans-serif";

    var tMin = points.length ? points[0].t : 0;
    var tMax = points.length ? points[points.length - 1].t : 1;
    if (tMax - tMin < 1) { tMax = tMin + 1; }
    var vMax = 0;
    points.forEach(function (p) {
      series.forEach(function (s) { var v = s.get(p); if (v > vMax) { vMax = v; } });
    });
    vMax = vMax <= 0 ? 1 : vMax * 1.1;
    var x = function (t) { return padL + (t - tMin) / (tMax - tMin) * (w - padL - padR); };
    var y = function (v) { return h - padB - v / vMax * (h - padT - padB); };

    ctx.strokeStyle = grid;
    ctx.fillStyle = muted;
    ctx.lineWidth = 1;
    for (var i = 0; i <= 4; i++) {
      var v = vMax * i / 4;
      var yy = Math.round(y(v)) + 0.5;
      ctx.beginPath(); ctx.moveTo(padL, yy); ctx.lineTo(w - padR, yy); ctx.stroke();
      ctx.textAlign = "right";
      ctx.fillText(fmt(v, v < 10 ? 1 : 0), padL - 6, yy + 4);
    }
    ctx.textAlign = "left";
    ctx.fillText(fmtDuration(tMin), padL, h - 6);
    ctx.textAlign = "right";
    ctx.fillText(fmtDuration(tMax), w - padR, h - 6);

    var lx = padL;
    series.forEach(function (s) {
      var color = cssVar(s.color);
      ctx.strokeStyle = color;
      ctx.lineWidth = 1.6;
      ctx.beginPath();
      points.forEach(function (p, idx) {
        var px = x(p.t), py = y(s.get(p));
        if (idx === 0) { ctx.moveTo(px, py); } else { ctx.lineTo(px, py); }
      });
      ctx.stroke();
      ctx.fillStyle = color;
      ctx.fillRect(lx, 6, 10, 10);
      ctx.fillStyle = muted;
      ctx.textAlign = "left";
      ctx.fillText(s.label, lx + 14, 15);
      lx += ctx.measureText(s.label).width + 30;
    });
  }

  function drawCharts(snap) {
    var pts = snap.timeline || [];
    var userLabel = mode === "arrival_rate" ? "busy users" : "users";
    var targetLabel = mode === "arrival_rate" ? "target iter/s" : "target";
    drawChart($("c-rps"), pts, [
      { label: "req/s", color: "--s1", get: function (p) { return p.rps; } },
      { label: "fail/s", color: "--s2", get: function (p) { return p.fail_rps; } }
    ]);
    drawChart($("c-lat"), pts, [
      { label: "p50", color: "--s3", get: function (p) { return p.p50_ms; } },
      { label: "p95", color: "--s4", get: function (p) { return p.p95_ms; } },
      { label: "p99", color: "--s5", get: function (p) { return p.p99_ms; } }
    ]);
    drawChart($("c-users"), pts, [
      { label: userLabel, color: "--s6", get: function (p) { return p.users; } },
      { label: targetLabel, color: "--s7", get: function (p) { return p.target; } }
    ]);
  }

  function render(snap) {
    last = snap;
    var stateEl = $("run-state");
    stateEl.textContent = snap.state + (snap.manual_target ? " (override)" : "") + (snap.aborted ? " (aborted)" : "");
    stateEl.setAttribute("data-state", snap.state);
    var running = snap.state === "running";
    $("btn-start").disabled = running || snap.state === "stopping";
    $("btn-stop").disabled = !running;
    $("btn-resume").disabled = !running || !snap.manual_target;

    var t = snap.total;
    $("m-users").textContent = fmt(snap.users, 0);
    $("m-target").textContent = "target " + fmt(snap.target, 1);
    $("m-rps").textContent = fmt(t.rps, 1);
    $("m-count").textContent = fmt(t.count, 0) + " total";
    $("m-fail").textContent = fmt(t.fail_ratio * 100, 2) + "%";
    $("m-failrps").textContent = fmt(t.fail_rps, 1) + "/s";
    $("m-p50").textContent = fmt(t.p50_ms, 1);
    $("m-p95").textContent = fmt(t.p95_ms, 1);
    $("m-p99").textContent = fmt(t.p99_ms, 1);
    $("m-dropped").textContent = fmt(snap.dropped_iterations, 0);
    $("m-elapsed").textContent = fmtDuration(snap.elapsed_secs);
    var g = snap.generator || {};
    $("m-cpu").textContent = (typeof g.cpu_pct === "number") ? fmt(g.cpu_pct, 0) + "%" : "n/a";
    $("m-lag").textContent = "lag " + fmt(g.scheduler_lag_ms || 0, 1) + " ms" +
      (g.saturated_secs ? ", saturated " + g.saturated_secs + "s" : "");
    $("gen-card").classList.toggle("alert", !!g.saturated);
    $("m-iter").textContent = fmt(snap.iterations_started, 0);
    $("m-iter-limit").textContent = iterationLimit ? ("limit " + fmt(iterationLimit, 0)) : "no limit";

    syncExtraHeaders(snap.percentiles);
    replaceRows($("t-requests"), snap.rows.map(function (r) { return requestRow(r, false); }));
    replaceRows($("t-transactions"), (snap.transactions || []).map(transactionRow));
    $("tx-count").textContent = String((snap.transactions || []).length);
    var ratio = snap.ratio || { scenarios: [], tasks: [] };
    replaceRows($("t-ratio-scenarios"), ratio.scenarios.map(ratioScenarioRow));
    replaceRows($("t-ratio-tasks"), ratio.tasks.map(ratioTaskRow));
    replaceRows($("t-total"), [requestRow(t, true)]);
    replaceRows($("t-iterations"), snap.iterations.map(iterationRow));
    replaceRows($("t-errors"), snap.errors.map(errorRow));
    replaceRows($("t-thresholds"), snap.thresholds.map(thresholdRow));
    $("err-count").textContent = String(snap.errors.length);
    var workers = snap.workers || [];
    if (workers.length > 0) { $("workers-tab").hidden = false; }
    $("wk-count").textContent = String(workers.length);
    replaceRows($("t-workers"), workers.map(workerRow));
    var failed = snap.thresholds.filter(function (x) { return !x.passed; }).length;
    $("th-count").textContent = failed > 0 ? (failed + " fail") : String(snap.thresholds.length);
    drawCharts(snap);
  }

  function pollLogs() {
    window.clearTimeout(logTimer);
    fetch("/api/logs?after=" + encodeURIComponent(String(logSeq)), { credentials: "same-origin", cache: "no-store" })
      .then(function (res) {
        if (res.status === 401) { toLogin(); throw new Error("unauthorized"); }
        return res.json();
      })
      .then(function (entries) {
        var view = $("log-view");
        var stick = view.scrollTop + view.clientHeight >= view.scrollHeight - 8;
        entries.forEach(function (e) {
          logSeq = Math.max(logSeq, e.seq);
          var line = document.createElement("div");
          line.className = "log-line " + e.level;
          var ts = document.createElement("span");
          ts.className = "ts";
          ts.textContent = new Date(e.ts_ms).toLocaleTimeString();
          var lvl = document.createElement("span");
          lvl.className = "lvl";
          lvl.textContent = e.level;
          var msg = document.createElement("span");
          msg.textContent = e.message;
          line.appendChild(ts);
          line.appendChild(lvl);
          line.appendChild(msg);
          view.appendChild(line);
        });
        while (view.childElementCount > MAX_LOG_LINES) { view.removeChild(view.firstChild); }
        if (stick) { view.scrollTop = view.scrollHeight; }
      })
      .catch(function () { return null; })
      .finally(function () { logTimer = window.setTimeout(pollLogs, 3000); });
  }

  function renderPicker(names) {
    var list = $("scenario-list");
    var frag = document.createDocumentFragment();
    names.forEach(function (n) {
      var label = document.createElement("label");
      var box = document.createElement("input");
      box.type = "checkbox";
      box.value = n;
      box.checked = true;
      label.appendChild(box);
      label.appendChild(document.createTextNode(n));
      frag.appendChild(label);
    });
    list.replaceChildren(frag);
  }

  function connect() {
    if (source) { source.close(); }
    source = new EventSource("/api/stream");
    source.addEventListener("snapshot", function (ev) {
      reconnectDelay = 1000;
      try { render(JSON.parse(ev.data)); } catch (e) { notice("Invalid update received.", true); }
    });
    source.onerror = function () {
      source.close();
      source = null;
      fetch("/api/session", { credentials: "same-origin", cache: "no-store" }).then(function (res) {
        if (res.status === 401) { toLogin(); return; }
        window.setTimeout(connect, reconnectDelay);
        reconnectDelay = Math.min(reconnectDelay * 2, 15000);
      }).catch(function () {
        window.setTimeout(connect, reconnectDelay);
        reconnectDelay = Math.min(reconnectDelay * 2, 15000);
      });
    };
  }

  function action(btn, method, path, body, okMsg) {
    btn.disabled = true;
    api(method, path, body)
      .then(function () { notice(okMsg, false); })
      .catch(function (e) { if (e.message !== "unauthorized") { notice(e.message, true); } })
      .finally(function () { if (last) { render(last); } else { btn.disabled = false; } });
  }

  function bind() {
    $("start-form").addEventListener("submit", function (ev) {
      ev.preventDefault();
      var body = {};
      var fields = [["start-users", "users"], ["start-rate", "spawn_rate"], ["start-time", "run_time_secs"]];
      for (var i = 0; i < fields.length; i++) {
        var raw = $(fields[i][0]).value.trim();
        if (raw === "") { continue; }
        var v = Number(raw);
        if (!isFinite(v) || v < 0) { notice("Start values must be non-negative numbers.", true); return; }
        body[fields[i][1]] = v;
      }
      var picked = Array.prototype.filter.call(
        document.querySelectorAll("#scenario-list input[type=checkbox]"), function (c) { return c.checked; }
      ).map(function (c) { return c.value; });
      var all = document.querySelectorAll("#scenario-list input[type=checkbox]").length;
      if (picked.length === 0) { notice("Select at least one scenario.", true); return; }
      if (picked.length < all) { body.scenarios = picked; }
      var hostSel = $("start-host");
      if (hostSel.selectedIndex > 0) { body.host = hostSel.value; }
      if (body.spawn_rate !== undefined && body.users === undefined) {
        notice("Ramp requires a users value.", true);
        return;
      }
      action($("btn-start"), "POST", "/api/run/start", body, "Run started.");
    });
    $("btn-stop").addEventListener("click", function () {
      action(this, "POST", "/api/run/stop", undefined, "Stopping run.");
    });
    $("btn-reset").addEventListener("click", function () {
      action(this, "POST", "/api/stats/reset", undefined, "Statistics reset.");
    });
    $("btn-resume").addEventListener("click", function () {
      action(this, "POST", "/api/run/resume", undefined, "Profile resumed.");
    });
    $("target-form").addEventListener("submit", function (ev) {
      ev.preventDefault();
      var target = Number($("target-input").value);
      var ramp = Number($("ramp-input").value || 0);
      if (!isFinite(target) || target < 0 || !isFinite(ramp) || ramp < 0) {
        notice("Target and ramp must be non-negative numbers.", true);
        return;
      }
      action(this.querySelector("button[type=submit]"), "POST", "/api/run/target",
        { target: target, ramp: ramp }, "Override applied.");
    });
    $("btn-logout").addEventListener("click", function () {
      api("POST", "/api/logout").catch(function () { return null; }).then(toLogin);
    });
    Array.prototype.forEach.call(document.querySelectorAll(".tab"), function (tab) {
      tab.addEventListener("click", function () {
        var name = tab.getAttribute("data-tab");
        Array.prototype.forEach.call(document.querySelectorAll(".tab"), function (t) {
          t.classList.toggle("active", t === tab);
        });
        Array.prototype.forEach.call(document.querySelectorAll(".tab-body"), function (b) {
          b.hidden = b.getAttribute("data-body") !== name;
        });
        if (name === "logs") { pollLogs(); } else { window.clearTimeout(logTimer); }
      });
    });
    var resizeTimer = 0;
    window.addEventListener("resize", function () {
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(function () { if (last) { drawCharts(last); } }, 150);
    });
  }

  function init() {
    fetch("/api/session", { credentials: "same-origin", cache: "no-store" })
      .then(function (res) {
        if (res.status === 401) { toLogin(); throw new Error("unauthorized"); }
        if (!res.ok) { throw new Error("session request failed"); }
        return res.json();
      })
      .then(function (info) {
        csrf = info.csrf;
        mode = info.mode;
        $("plan-name").textContent = info.plan;
        $("plan-target").textContent = info.target;
        var isRate = mode === "arrival_rate";
        $("target-label").textContent = isRate ? "Iterations/s" : "Users";
        $("users-label").textContent = isRate ? "Busy users" : "Users";
        $("m-mode").textContent = isRate ? "open model" : "closed model";
        iterationLimit = info.iteration_limit;
        renderPicker(info.scenarios);
        var hostSel = $("start-host");
        var hostFrag = document.createDocumentFragment();
        (info.hosts || []).forEach(function (h, i) {
          var o = document.createElement("option");
          o.value = h;
          o.textContent = i === 0 ? h + " (plan)" : h;
          hostFrag.appendChild(o);
        });
        hostSel.replaceChildren(hostFrag);
        if (info.controller) { $("workers-tab").hidden = false; }
        $("start-users-label").textContent = isRate ? "Iter/s" : "Users";
        var profile = (info.profile_secs === null || info.profile_secs === undefined)
          ? "scripted profile"
          : fmtDuration(info.profile_secs) + ", peak " + fmt(info.profile_peak, 1) + (isRate ? " iterations/s" : " users");
        var tags = info.tags.length ? (" Tags: " + info.tags.join(", ") + ".") : "";
        if (info.exclude_tags.length) { tags += " Excluded: " + info.exclude_tags.join(", ") + "."; }
        $("profile-info").textContent = "Profile: " + profile + ". Scenarios: " + info.scenarios.join(", ") + "." + tags +
          " Leave start fields empty to run the plan profile.";
        bind();
        connect();
      })
      .catch(function (e) { if (e.message !== "unauthorized") { notice(e.message, true); } });
  }

  init();
})();
