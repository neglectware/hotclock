/* Property inspector of every Hotclock action. One page; the fields shown come from the action's entry in SPECS.
 * It talks to Stream Deck over the plugin's standard socket. It cannot reach the box itself (the box answers a browser
 * only on its own origins), so lists (stations, playlists, items, blocks) are fetched by the plugin and sent back. */
(function () {
  "use strict";

  var HOW = [["segue", "At the next segue"], ["now", "Right now"], ["end", "When this song ends"]];

  // Field kinds: select, number, text, check, playlist (picker; saves id and name), item (search; saves id, title and duration), block (picker; needs a playlist).
  var SPECS = {
    "elapsed-remaining": [{ k: "show", t: "select", label: "Show", opts: [["remaining", "Time remaining"], ["elapsed", "Time elapsed"]], def: "remaining", hint: "A press switches between the two." }],
    "fade-next": [{ k: "fadeMs", t: "number", label: "Fade (ms)", ph: "station default" }],
    stop: [{ k: "fadeMs", t: "number", label: "Fade (ms)", ph: "station default" }, { t: "note", text: "Hold the key for half a second to stop." }],
    cut: [{ t: "note", text: "Hold the key for half a second to cut." }],
    "switch-playlist": [{ k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" }, { k: "how", t: "select", label: "When", opts: HOW, def: "segue" }],
    "schedule-change": [
      { k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" },
      { k: "when", t: "select", label: "Schedule", opts: [["minute", "At a minute of the hour"], ["offset", "After a delay"]], def: "minute" },
      { k: "minute", t: "number", label: "Minute (0-59)", min: 0, max: 59, ph: "0", showIf: function (s) { return (s.when || "minute") === "minute"; } },
      { k: "offsetMin", t: "number", label: "Minutes from now", min: 1, ph: "5", showIf: function (s) { return s.when === "offset"; } },
      { k: "how", t: "select", label: "Change", opts: HOW, def: "segue" }
    ],
    "fire-item": [{ k: "item", t: "item", label: "Item", idKey: "itemId", nameKey: "itemTitle", durKey: "durationMs" }],
    "program-start": [{ k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" }, { t: "note", text: "Hold the key to cancel tonight's fallback automatic start." }],
    "program-arm": [{ k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" }],
    "next-block": [{ k: "fireOnLongPress", t: "check", label: "Hold to fire", text: "Fire the block now when the key is held" }],
    "fire-block": [{ k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" }, { k: "block", t: "block", label: "Block", idKey: "blockId", nameKey: "blockName" }],
    "abort-program": [{ k: "stopAudio", t: "check", label: "Audio", text: "Stop the audio too" }, { t: "note", text: "Hold the key for half a second to abort." }],
    "end-at": [
      { k: "playlist", t: "playlist", label: "Playlist", idKey: "playlistId", nameKey: "playlistName" },
      { k: "endMode", t: "select", label: "End", opts: [["at", "At a time of day"], ["in", "After a delay"]], def: "at" },
      { k: "endTime", t: "text", label: "Time (HH:MM)", ph: "18:55", showIf: function (s) { return (s.endMode || "at") === "at"; }, hint: "The box's local time." },
      { k: "endIn", t: "number", label: "Minutes from now", min: 1, ph: "30", showIf: function (s) { return s.endMode === "in"; } }
    ],
    "display-view": [
      { k: "view", t: "select", label: "A press", opts: [["cycle", "Steps through the views"], ["single", "Shows one station"], ["split", "Shows both stations"], ["rotate", "Rotates between stations"], ["processing", "Shows the processing view"]], def: "cycle" },
      { k: "displayStation", t: "station", label: "Station", showIf: function (s) { return s.view === "single"; } },
      { k: "includeProcessing", t: "check", label: "Steps", text: "Include the processing view", showIf: function (s) { return (s.view || "cycle") === "cycle"; } }
    ],
    loudness: [{ t: "note", text: "Hold the key to reset the integrated reading." }],
    "station-status": [{ t: "note", text: "A press opens the box's web page." }]
  };
  var NO_STATION = { "display-view": true };

  var ws = null, uuid = "", actionName = "", settings = {}, globals = {};
  var stationsCache = null, playlistsCache = null;
  var saveTimer = null, globalTimer = null;
  var pending = {};

  function $(id) { return document.getElementById(id); }
  function el(tag, attrs, kids) {
    var e = document.createElement(tag);
    Object.keys(attrs || {}).forEach(function (k) { if (k === "text") e.textContent = attrs[k]; else if (attrs[k] !== undefined) e.setAttribute(k, attrs[k]); });
    (kids || []).forEach(function (c) { e.appendChild(c); });
    return e;
  }
  function send(msg) { if (ws && ws.readyState === 1) ws.send(JSON.stringify(msg)); }

  // ---- saving -------------------------------------------------------------------------------------------------

  function saveSettings() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(function () { send({ event: "setSettings", context: uuid, payload: settings }); }, 250);
  }
  function saveGlobals() {
    clearTimeout(globalTimer);
    globalTimer = setTimeout(function () { send({ event: "setGlobalSettings", context: uuid, payload: globals }); }, 250);
  }
  function setKey(k, v) {
    if (v === "" || v === null || v === undefined) delete settings[k]; else settings[k] = v;
    saveSettings();
  }

  // ---- asking the plugin --------------------------------------------------------------------------------------

  var tagN = 0;
  function ask(req, cb) {
    req.tag = String(++tagN);
    req.host = (settings.host || globals.host || "").trim();
    pending[req.tag] = cb;
    send({ event: "sendToPlugin", action: actionName, context: uuid, payload: req });
  }
  function onPluginMessage(p) {
    var cb = p && p.tag && pending[p.tag];
    if (cb) { delete pending[p.tag]; cb(p); }
  }

  function loadStations(force, cb) {
    if (stationsCache && !force) return cb(stationsCache);
    ask({ cmd: "stations" }, function (r) {
      $("station-error").hidden = r.ok;
      $("station-error").textContent = r.ok ? "" : (r.error || "No answer from the box.");
      stationsCache = r.ok ? r.stations : [];
      cb(stationsCache);
    });
  }
  // read again each time the panel is shown (and on Refresh): a playlist made since must appear
  function loadPlaylists(cb) {
    if (playlistsCache) return cb(forThisStation(playlistsCache));
    ask({ cmd: "playlists" }, function (r) {
      showActionError(r.ok ? "" : (r.error || "No answer from the box."));
      playlistsCache = r.ok ? r.playlists : [];
      cb(forThisStation(playlistsCache));
    });
  }
  // a program key lists the programs of its station (and those of no station): another station's show never shows up
  var PROGRAM_KEYS = { "program-start": true, "program-arm": true, "fire-block": true };
  function forThisStation(list) {
    if (!PROGRAM_KEYS[actionName]) return list;
    return list.filter(function (p) { return !p.station || p.station === settings.station; });
  }
  function showActionError(t) { $("action-error").hidden = !t; $("action-error").textContent = t; }

  // ---- the box and station section --------------------------------------------------------------------------

  function fillBox() {
    $("g-host").value = globals.host || "";
    $("g-libport").value = globals.libraryPort || "";
    $("k-host").value = settings.host || "";
  }
  function bindBox() {
    $("g-host").addEventListener("input", function () { globals.host = this.value.trim(); if (!globals.host) delete globals.host; saveGlobals(); invalidate(); });
    $("g-libport").addEventListener("input", function () { var n = parseInt(this.value, 10); if (n > 0) globals.libraryPort = n; else delete globals.libraryPort; saveGlobals(); invalidate(); });
    $("k-host").addEventListener("input", function () { setKey("host", this.value.trim()); invalidate(); });
    $("btn-test").addEventListener("click", function () {
      var s = $("test-status");
      s.className = "status"; s.textContent = "Testing...";
      ask({ cmd: "test" }, function (r) { s.className = "status " + (r.ok ? "ok" : "bad"); s.textContent = r.ok ? (r.message || "Connected") : (r.error || "No answer"); });
    });
    $("k-station").addEventListener("change", function () {
      var opt = this.options[this.selectedIndex];
      setKey("station", this.value);
      if (opt && opt.dataset.port) { settings.stationPort = parseInt(opt.dataset.port, 10); $("k-port").value = settings.stationPort; saveSettings(); }
      renderFields();   // the program pickers follow the station
    });
    $("k-port").addEventListener("input", function () { var n = parseInt(this.value, 10); setKey("stationPort", n > 0 ? n : ""); });
    $("btn-refresh").addEventListener("click", function () { invalidate(); });
  }
  function invalidate() {
    stationsCache = null; playlistsCache = null;
    clearTimeout(invalidate.t);
    invalidate.t = setTimeout(function () { fillStations(); renderFields(); }, 500);
  }
  function fillStations() {
    if (NO_STATION[actionName]) { $("station-section").hidden = true; return; }
    loadStations(true, function (list) {
      var sel = $("k-station");
      sel.innerHTML = "";
      sel.appendChild(el("option", { value: "", text: list.length ? "Choose a station" : "(no stations found)" }));
      list.forEach(function (s) {
        var o = el("option", { value: s.id, text: s.name + (s.name !== s.id ? " (" + s.id + ")" : "") });
        if (s.port) o.dataset.port = String(s.port);
        sel.appendChild(o);
      });
      if (settings.station && !list.some(function (s) { return s.id === settings.station; })) sel.appendChild(el("option", { value: settings.station, text: settings.station }));
      sel.value = settings.station || "";
      $("k-port").value = settings.stationPort || "";
    });
  }

  // ---- the action's own fields ------------------------------------------------------------------------------

  function renderFields() {
    var box = $("fields");
    box.innerHTML = "";
    (SPECS[actionName] || []).forEach(function (f) {
      if (f.showIf && !f.showIf(settings)) return;
      if (f.t === "note") { box.appendChild(el("p", { class: "hint", text: f.text })); return; }
      var row = el("div", { class: "row", "data-field": f.k });
      row.appendChild(el("label", { text: f.label || "" }));
      var input;
      if (f.t === "select") {
        input = el("select", {}, f.opts.map(function (o) { return el("option", { value: o[0], text: o[1] }); }));
        input.value = settings[f.k] || f.def || f.opts[0][0];
        input.addEventListener("change", function () { setKey(f.k, this.value); renderFields(); });
      } else if (f.t === "number") {
        input = el("input", { type: "number", min: f.min, max: f.max, placeholder: f.ph });
        input.value = settings[f.k] === undefined ? "" : settings[f.k];
        input.addEventListener("input", function () { var n = parseFloat(this.value); setKey(f.k, isNaN(n) ? "" : n); });
      } else if (f.t === "text") {
        input = el("input", { type: "text", placeholder: f.ph, spellcheck: "false" });
        input.value = settings[f.k] || "";
        input.addEventListener("input", function () { setKey(f.k, this.value.trim()); });
      } else if (f.t === "check") {
        var cb = el("input", { type: "checkbox", id: "c-" + f.k });
        cb.checked = settings[f.k] === true;
        cb.addEventListener("change", function () { setKey(f.k, this.checked ? true : ""); });
        input = el("div", { class: "inline" }, [cb, el("span", { text: f.text || "" })]);
      } else if (f.t === "playlist") {
        input = el("select", {}, [el("option", { value: "", text: "Loading..." })]);
        loadPlaylists(function (list) {
          input.innerHTML = "";
          input.appendChild(el("option", { value: "", text: "Choose a playlist" }));
          list.forEach(function (p) { input.appendChild(el("option", { value: String(p.id), text: p.name })); });
          if (settings[f.idKey] !== undefined && !list.some(function (p) { return p.id === settings[f.idKey]; })) input.appendChild(el("option", { value: String(settings[f.idKey]), text: settings[f.nameKey] || String(settings[f.idKey]) }));
          input.value = settings[f.idKey] === undefined ? "" : String(settings[f.idKey]);
        });
        input.addEventListener("change", function () {
          var opt = this.options[this.selectedIndex];
          var id = parseInt(this.value, 10);
          if (isNaN(id)) { delete settings[f.idKey]; delete settings[f.nameKey]; } else { settings[f.idKey] = id; settings[f.nameKey] = opt.textContent; }
          saveSettings(); renderFields();
        });
      } else if (f.t === "block") {
        input = el("select", {}, [el("option", { value: "", text: "Loading..." })]);
        var pid = settings.playlistId;
        if (pid === undefined) { input.innerHTML = ""; input.appendChild(el("option", { value: "", text: "Pick a playlist first" })); }
        else ask({ cmd: "blocks", playlistId: pid }, function (r) {
          input.innerHTML = "";
          input.appendChild(el("option", { value: "", text: r.ok && r.blocks.length ? "Choose a block" : "(no blocks in this playlist)" }));
          (r.ok ? r.blocks : []).forEach(function (b) { input.appendChild(el("option", { value: String(b.id), text: b.name || ("Block " + b.id) })); });
          if (settings[f.idKey] !== undefined && !(r.blocks || []).some(function (b) { return b.id === settings[f.idKey]; })) input.appendChild(el("option", { value: String(settings[f.idKey]), text: settings[f.nameKey] || String(settings[f.idKey]) }));
          input.value = settings[f.idKey] === undefined ? "" : String(settings[f.idKey]);
        });
        input.addEventListener("change", function () {
          var opt = this.options[this.selectedIndex];
          var id = parseInt(this.value, 10);
          if (isNaN(id)) { delete settings[f.idKey]; delete settings[f.nameKey]; } else { settings[f.idKey] = id; settings[f.nameKey] = opt.textContent; }
          saveSettings();
        });
      } else if (f.t === "station") {
        input = el("select", {}, [el("option", { value: "", text: "Loading..." })]);
        loadStations(false, function (list) {
          input.innerHTML = "";
          input.appendChild(el("option", { value: "", text: "Choose a station" }));
          list.forEach(function (s) { input.appendChild(el("option", { value: s.id, text: s.name })); });
          input.value = settings[f.k] || "";
        });
        input.addEventListener("change", function () { setKey(f.k, this.value); });
      } else if (f.t === "item") {
        input = el("input", { type: "text", placeholder: "Search the library", spellcheck: "false" });
        input.value = settings[f.nameKey] || "";
        var results = el("div", { class: "results", hidden: "" });
        var t;
        input.addEventListener("input", function () {
          clearTimeout(t);
          var q = this.value.trim();
          t = setTimeout(function () {
            ask({ cmd: "items", q: q }, function (r) {
              results.innerHTML = "";
              results.hidden = !r.ok || !r.items.length;
              showActionError(r.ok ? "" : (r.error || ""));
              (r.ok ? r.items : []).forEach(function (it) {
                var d = el("div", {}, [el("span", { text: it.title }), el("small", { text: it.artist ? "  " + it.artist : "" })]);
                d.addEventListener("click", function () {
                  settings[f.idKey] = it.id; settings[f.nameKey] = it.title; settings[f.durKey] = it.durationMs;
                  input.value = it.title; results.hidden = true; saveSettings();
                });
                results.appendChild(d);
              });
            });
          }, 250);
        });
        row.appendChild(input);
        $("fields").appendChild(row);
        if (f.hint) $("fields").appendChild(el("p", { class: "hint", text: f.hint }));
        $("fields").appendChild(results);
        return;
      }
      row.appendChild(input);
      box.appendChild(row);
      if (f.hint) box.appendChild(el("p", { class: "hint", text: f.hint }));
    });
  }

  // ---- the key's own face -----------------------------------------------------------------------------------

  // A picture made 144 px square (the key's size) as a PNG data URI, so the settings stay small.
  function pictureOf(file, cb) {
    var rd = new FileReader();
    rd.onload = function () {
      var img = new Image();
      img.onload = function () {
        var c = document.createElement("canvas"); c.width = 144; c.height = 144;
        var g = c.getContext("2d"), s = Math.max(144 / img.width, 144 / img.height);
        var w = img.width * s, h = img.height * s;
        g.drawImage(img, (144 - w) / 2, (144 - h) / 2, w, h);
        cb(c.toDataURL("image/png"));
      };
      img.onerror = function () { showActionError("That file is not a picture this panel can read."); };
      img.src = rd.result;
    };
    rd.readAsDataURL(file);
  }
  function fillFace() {
    $("f-label").value = settings.label || "";
    $("f-color").value = settings.labelColor || "#f4f5f7";
    $("f-tint").checked = settings.tintActive === true;
    $("f-img-clear").disabled = !settings.imageIdle;
    $("f-img2-clear").disabled = !settings.imageActive;
  }
  function bindFace() {
    $("f-label").addEventListener("input", function () { setKey("label", this.value); });
    $("f-color").addEventListener("input", function () { setKey("labelColor", this.value); });
    $("f-color-clear").addEventListener("click", function () { setKey("labelColor", ""); fillFace(); });
    $("f-tint").addEventListener("change", function () { setKey("tintActive", this.checked ? true : ""); });
    [["f-img", "imageIdle"], ["f-img2", "imageActive"]].forEach(function (x) {
      $(x[0]).addEventListener("change", function () { var f = this.files && this.files[0]; if (f) pictureOf(f, function (d) { setKey(x[1], d); fillFace(); }); });
      $(x[0] + "-clear").addEventListener("click", function () { setKey(x[1], ""); $(x[0]).value = ""; fillFace(); });
    });
  }

  // ---- the Stream Deck socket -------------------------------------------------------------------------------

  window.connectElgatoStreamDeckSocket = function (port, inUUID, registerEvent, info, actionInfo) {
    uuid = inUUID;
    var ai = typeof actionInfo === "string" ? JSON.parse(actionInfo) : actionInfo;
    actionName = String(ai.action || "").replace(/^com\.neglectware\.hotclock\./, "");
    settings = (ai.payload && ai.payload.settings) || {};
    $("action-title").textContent = "Key";
    bindBox();
    bindFace();
    fillBox();
    fillFace();
    $("station-section").hidden = !!NO_STATION[actionName];
    renderFields();
    // the panel shown again (another key, then this one): the lists are read again
    document.addEventListener("visibilitychange", function () { if (document.visibilityState === "visible") invalidate(); });
    ws = new WebSocket("ws://127.0.0.1:" + port);
    ws.onopen = function () {
      send({ event: registerEvent, uuid: uuid });
      send({ event: "getGlobalSettings", context: uuid });
    };
    ws.onmessage = function (ev) {
      var m = JSON.parse(ev.data);
      if (m.event === "didReceiveGlobalSettings") { globals = (m.payload && m.payload.settings) || {}; playlistsCache = null; fillBox(); fillStations(); renderFields(); }
      else if (m.event === "didReceiveSettings") { settings = (m.payload && m.payload.settings) || {}; fillBox(); fillFace(); renderFields(); }
      else if (m.event === "sendToPropertyInspector") onPluginMessage(m.payload);
    };
  };

  window.__pi = { SPECS: SPECS, state: function () { return { settings: settings, globals: globals, actionName: actionName }; } };
})();
