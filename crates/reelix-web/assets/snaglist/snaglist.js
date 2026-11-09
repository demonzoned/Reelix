/*
 * Snaglist 0.3.2: feedback for any website or app. People report bugs and UX problems, or suggest features and
 * improvements, tagged with the page and the part of it, with the technical detail needed to act on it and
 * without personal information. One file, no libraries, MIT licence.
 * https://github.com/TheMightyTRex/snaglist
 *
 * Add it:     <script src="snaglist.js" data-github="owner/repo" defer></script>
 *   or:       Snaglist.start({ app: "My app", send: { github: { repo: "owner/repo" } } })
 * Remove it:  Snaglist.destroy({ forget: true }), then delete the script tag. Nothing else is left behind.
 *
 * Everything lives in one element, <snag-list>, with its own shadow root, so its styles never touch the page
 * and the page's styles never break it. Help is in docs/README.md; settings in docs/topics/settings-reference.md.
 */
(() => {
  "use strict";
  if (window.Snaglist) return;

  const VERSION = "0.3.2";
  const KEY = "snaglist:";

  // What people can send. Each type is a problem or a suggestion, asks its own questions and records the
  // technical detail it needs. pick: start by choosing a part of the page. scale: the "how bad" or "how much
  // would it help" question. A field is { id, label, hint, type: "textarea" | "text" | "select", options, required }.
  const WHAT = { id: "what", label: "What is wrong", hint: "What did you expect, and what happened?", type: "textarea", required: true };
  const TYPES = {
    bug: { group: "problem", pick: true, label: "Something is broken", hint: "An error, a button that does nothing, wrong data",
      fields: [
        { id: "actual", label: "What happened", type: "textarea", required: true },
        { id: "expected", label: "What you expected", type: "textarea" },
        { id: "steps", label: "Steps to make it happen again", hint: "One step per line. Or go back and choose A series of steps to record them.", type: "textarea", unlessRecorded: true },
      ],
      collect: ["element", "text", "errors", "console", "network", "trail", "html"] },
    ux: { group: "problem", pick: true, label: "Hard to use", hint: "Confusing, too many steps, not where you expected",
      fields: [WHAT], collect: ["element", "text", "trail"] },
    visual: { group: "problem", pick: true, label: "Looks wrong", hint: "Overlaps, cut off, misaligned, wrong colour or size",
      fields: [WHAT], collect: ["element", "text", "styles", "layout", "html"] },
    accessibility: { group: "problem", pick: true, label: "Hard to use with my tools or needs", hint: "Keyboard, screen reader, zoom, contrast, motion",
      fields: [WHAT, { id: "using", label: "What were you using", type: "select", options: [["keyboard", "Keyboard only"],
        ["screen-reader", "A screen reader"], ["zoom", "Zoom or large text"], ["voice", "Voice control"], ["switch", "Switch access"],
        ["contrast", "High contrast or forced colours"], ["none", "Nothing special"], ["other", "Something else"]] }],
      collect: ["element", "text", "a11y", "styles", "layout"] },
    performance: { group: "problem", pick: false, label: "Slow", hint: "Takes too long to load, freezes or stutters",
      fields: [WHAT, { id: "when", label: "When is it slow", type: "select", options: [["load", "While the page loads"],
        ["action", "After I do something"], ["always", "All the time"]] }],
      collect: ["performance", "network", "trail"] },
    content: { group: "problem", pick: true, label: "Words are wrong", hint: "Spelling, unclear or out-of-date text",
      fields: [WHAT, { id: "suggestion", label: "Suggested wording", type: "textarea" }], collect: ["element", "text"] },
    feature: { group: "suggestion", pick: false, scale: "importance", label: "A new feature", hint: "Something the site cannot do yet",
      fields: [
        { id: "want", label: "What would you like to be able to do", type: "textarea", required: true },
        { id: "why", label: "What would it help you with", hint: "The task or goal behind it, so the team can find the best way.", type: "textarea" },
        { id: "today", label: "How do you manage today", hint: "A workaround, another site, or not at all.", type: "textarea" },
      ],
      collect: ["page"] },
    idea: { group: "suggestion", pick: true, scale: "importance", label: "Make something better", hint: "An improvement to something that already works",
      fields: [
        { id: "now", label: "What could be better", type: "textarea", required: true },
        { id: "idea", label: "Your suggestion", type: "textarea" },
      ],
      collect: ["element"] },
    other: { group: "suggestion", pick: false, scale: false, label: "Something else", hint: "A question, a thank you, anything",
      fields: [{ id: "message", label: "Your message", type: "textarea", required: true }], collect: [] },
    check: { group: "check", pick: false, scale: false, label: "Check this page for accessibility problems", hint: "An automatic check; you choose which problems to report",
      fields: [], collect: ["page", "environment", "display"] },
  };

  // Optional second question, "Kind of issue". A category is [value, label, hint].
  const CATEGORIES = {
    ux: [
      ["layout", "Layout", "Overlaps, alignment, spacing, things cut off"],
      ["words", "Words", "Unclear, wrong or inconsistent text"],
      ["behaviour", "Behaviour", "Does not do what you expected"],
      ["accessibility", "Accessibility", "Keyboard, screen reader, contrast, zoom"],
      ["looks", "Looks", "Colour, icons, type, not like the rest"],
    ],
    heuristics: [
      ["status", "Status not clear", "Nielsen 1: visibility of system status"],
      ["language", "Not the user's language", "Nielsen 2: match with the real world"],
      ["control", "No way out", "Nielsen 3: user control and freedom"],
      ["consistency", "Inconsistent", "Nielsen 4: consistency and standards"],
      ["errors", "Easy to make a mistake", "Nielsen 5: error prevention"],
      ["recall", "Too much to remember", "Nielsen 6: recognition rather than recall"],
      ["efficiency", "Slow to use", "Nielsen 7: flexibility and efficiency"],
      ["clutter", "Cluttered", "Nielsen 8: aesthetic and minimalist design"],
      ["recovery", "Unhelpful error", "Nielsen 9: help users recover from errors"],
      ["help", "Help missing", "Nielsen 10: help and documentation"],
    ],
    wcag: [
      ["perceivable", "Hard to see or hear", "WCAG: perceivable (contrast, text size, alternatives)"],
      ["operable", "Hard to use", "WCAG: operable (keyboard, focus, targets, time)"],
      ["understandable", "Hard to understand", "WCAG: understandable (words, labels, errors)"],
      ["robust", "Does not work with my tools", "WCAG: robust (screen readers, zoom, other browsers)"],
    ],
  };
  const SEVERITIES = {
    three: [
      ["minor", "Minor", "Cosmetic: I can carry on as normal"],
      ["in-the-way", "Gets in the way", "Slows me down or confuses me"],
      ["stops-me", "Stops me", "It stops me finishing what I came to do"],
    ],
    nielsen: [
      ["1", "1 Cosmetic", "Fix if there is time"],
      ["2", "2 Minor", "Low priority"],
      ["3", "3 Major", "High priority"],
      ["4", "4 Catastrophe", "Fix before release"],
    ],
    importance: [
      ["nice", "Nice to have", "I would use it now and then"],
      ["often", "I would use it often", "It would save me time regularly"],
      ["need", "I need it", "I need it to do what I came to do"],
    ],
  };
  const GROUPS = [["problem", "Report a problem"], ["suggestion", "Make a suggestion"], ["check", "Check the page"]];

  // What a report can record. true: always; false: never; "auto": when the report type asks for it.
  const PARTS = [
    ["page", "Page name and path"], ["element", "The part of the page"], ["text", "Text on screen"], ["steps", "Recorded steps"], ["styles", "Its font and colours"],
    ["layout", "Its size, position and what covers it"], ["a11y", "Its accessibility details"], ["html", "Its HTML, without text"],
    ["environment", "Screen, browser and device"], ["display", "Display settings"], ["performance", "Page speed"],
    ["errors", "Script errors"], ["console", "Console messages"], ["network", "Failed and slow requests"],
    ["trail", "What was selected before"], ["context", "App details"],
  ];

  // axe-core for check.engine "axe", pinned, with the hash of that exact file (the same on jsDelivr and npm).
  const AXE = { url: "https://cdn.jsdelivr.net/npm/axe-core@4.10.2/axe.min.js",
    integrity: "sha384-3NYxCdpLKVHfNs2FHPtg3qqaYuhq85m4mMnlHBlN0JzSpKYKct2PMGYfsKGaKIj4" };

  const DEFAULTS = {
    app: "",
    version: "",
    language: "auto",      // "auto" (the page's lang), "en-GB", "en-US"; other languages through text
    theme: "light",        // "light", "dark", or "auto" to follow the device
    enabled: true,
    userToggle: true,
    launcher: "bottom-right",
    shortcut: "",
    types: ["bug", "ux", "visual", "accessibility", "performance", "content", "feature", "idea", "other", "check"],
    check: { engine: "built-in", axe: AXE.url, integrity: "", contrastLimit: 300 },
    categories: null,
    severities: "three",
    importance: "importance",
    labels: ["feedback"],
    collect: {
      page: true, element: "auto", text: "auto", steps: true, styles: "auto", layout: "auto", a11y: "auto", html: "auto",
      environment: true, display: true, performance: "auto",
      errors: "auto", console: "auto", network: "auto", trail: "auto", context: true,
    },
    limits: { errors: 5, console: 10, network: 8, trail: 8, slowRequest: 2000 },
    page: { name: null, keepParams: [] },
    component: { attribute: "data-component", from: null },
    context: null,
    redact: [],
    send: { github: null, email: null, endpoint: null, copy: true, download: true, qr: true },
    beforeSend: null,
    text: {},
  };

  const TEXT = {
    launcher: "Send feedback",
    chooseTitle: "Send feedback",
    chooseNext: "Continue",
    show: "What do you want to show",
    showOne: "One thing on the page",
    showOneHint: "Choose it, and Snaglist notes what it is and the text on it",
    showSteps: "A series of steps",
    showStepsHint: "Record what you do, step by step, then describe it",
    showNone: "Describe it in words",
    showNoneHint: "Without choosing a part of the page",
    recRegion: "Recording steps",
    recReady: "Ready to record. Select 'Start recording', then use the site as normal.",
    recStart: "Start recording",
    recPause: "Pause",
    recResume: "Resume",
    recStop: "Stop and describe",
    recRestart: "Start again",
    recRecording: "Recording",
    recPaused: "Paused",
    recLast: "Last step",
    recSteps: "Recorded steps",
    recNone: "No steps yet",
    checkTitle: "Accessibility check",
    checking: "Checking this page…",
    checkNone: "No problems found by the automatic check. Automatic checks find only some problems: also try the page with a keyboard only, at 200% zoom and with a screen reader.",
    checkFound: "problems found",
    checkLimits: "Automatic checks find only some problems. Also try the page with a keyboard only, at 200% zoom and with a screen reader.",
    checkShow: "Show on the page",
    checkReport: "Report this",
    checkReportAll: "Report all",
    checkFix: "How to fix it",
    checkWhy: "Why it matters",
    checkWhere: "Where",
    checkFailed: "The check could not run",
    checkFailedNext: "Reload the page, then try again. Nothing was sent.",
    pickPrompt: "Choose the part this is about: select it, or Tab to it and press Enter. Escape stops.",
    wholePage: "Whole page",
    stop: "Stop",
    formTitle: "Send feedback",
    where: "Where",
    chooseAgain: "Choose again",
    choosePart: "Choose a part of the page",
    type: "What would you like to send",
    sending: "Sending",
    change: "Change",
    fieldError: "Fill this in",
    category: "Kind of issue",
    severity: "How bad is it",
    importance: "How much would it help you",
    review: "Review what is sent",
    cancel: "Cancel",
    reviewTitle: "What is sent",
    include: "Include",
    privacy: "This report keeps no account, cookies or anything entered in the page's fields, and anything in the form of an email, a long number or a key is replaced. The page title and the text on screen are copied as they are, so check they show nothing personal. Untick anything you would rather not send.",
    githubNote: "GitHub opens with the report filled in. You post it from your own GitHub account, so GitHub shows your user name on the issue.",
    emailNote: "Your mail app opens with the report filled in. The reply address is your own.",
    endpointNote: "The report is sent to the site's own server, without cookies.",
    sendGithub: "Open in GitHub",
    sendEmail: "Open in email",
    sendEndpoint: "Send",
    copy: "Copy as Markdown",
    download: "Download",
    back: "Back",
    close: "Close",
    copied: "Report copied",
    copiedForGithub: "The technical details were too long for GitHub's address, so the full report is on your clipboard: paste it into the issue.",
    sent: "Report sent. Thank you.",
    sendFailed: "Not sent",
    sendFailedSafe: "Your report is kept: select 'Back', then send it another way, such as 'Copy as Markdown'.",
    opened: "Opened. Thank you.",
    sendQr: "Show as a QR code",
    qrScan: "Scan it with your phone's camera to open the report there.",
    qrGithub: "Your phone opens a new GitHub issue with the report filled in. You post it from your own GitHub account.",
    qrEmail: "Your phone opens a new email with the report filled in. It is sent from your own address.",
    qrAltGithub: "QR code: opens this report as a new GitHub issue on {repo}",
    qrAltEmail: "QR code: opens this report as a new email to {to}",
    qrShort: "The technical details were left out, so the code is small enough to scan. The code holds the summary and your own words.",
    qrDownload: "To keep the full report, select 'Download'.",
    qrLink: "The link in the code",
    qrCopy: "Copy link",
    qrCopied: "Link copied",
    qrOffline: "This computer is not connected to the internet, so 'Show as a QR code' comes first. Scan the code with your phone to send the report from there.",
  };

  let config = null;
  let host = null;
  let root = null;
  let wrap = null;
  let launcher = null;
  let picking = null;
  let rec = null; // the step recorder: { status, steps, values, started, bar }
  let lastFocus = null;
  const buffers = { errors: [], console: [], network: [], trail: [], longTasks: [], resources: [] };
  const vitals = { lcp: null, cls: 0, inp: null };
  const undo = []; // functions that take back everything start() added to the page

  // Small helpers -----------------------------------------------------------------------------------------

  function h(tag, attrs = {}, ...children) {
    const node = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs)) {
      if (v == null || v === false) continue;
      if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v === true ? "" : v);
    }
    node.append(...children.flat().filter((c) => c != null && c !== false));
    return node;
  }

  const ICONS = {
    flag: ["M4 22V4a1 1 0 0 1 .4-.8A6 6 0 0 1 8 2c3 0 5 2 7.333 2q2 0 3.067-.8A1 1 0 0 1 20 4v10a1 1 0 0 1-.4.8A6 6 0 0 1 16 16c-3 0-5-2-8-2a6 6 0 0 0-4 1.528"],
    x: ["M18 6 6 18", "m6 6 12 12"],
  };
  function icon(name) {
    const ns = "http://www.w3.org/2000/svg";
    const s = document.createElementNS(ns, "svg");
    for (const [k, v] of Object.entries({ viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", "stroke-width": 2,
      "stroke-linecap": "round", "stroke-linejoin": "round", "aria-hidden": "true", focusable: "false" })) s.setAttribute(k, v);
    for (const d of ICONS[name]) { const p = document.createElementNS(ns, "path"); p.setAttribute("d", d); s.append(p); }
    return s;
  }

  const US = [[/colour/g, "color"], [/Colour/g, "Color"], [/behaviour/g, "behavior"], [/Behaviour/g, "Behavior"], [/organis/g, "organiz"],
    [/recognis/g, "recogniz"], [/centre/g, "center"], [/favourite/g, "favorite"], [/catalogue/g, "catalog"], [/licence/g, "license"],
    [/minimise/g, "minimize"], [/customis/g, "customiz"], [/optimis/g, "optimiz"], [/summaris/g, "summariz"]];
  let american = false;
  const spell = (s) => (american && typeof s === "string" ? US.reduce((out, [a, b]) => out.replace(a, b), s) : s);
  // tr("types.bug.label", "Something is broken"): the site's own text for a key, or the default.
  const tr = (key, fallback) => (config && config.text[key] != null ? config.text[key] : spell(fallback));
  const t = (key) => tr(key, TEXT[key]);
  const store = {
    get(name) { try { return localStorage.getItem(KEY + name); } catch { return null; } },
    set(name, value) { try { value == null ? localStorage.removeItem(KEY + name) : localStorage.setItem(KEY + name, value); } catch { /* private window */ } },
  };
  const isPlain = (v) => v && typeof v === "object" && !Array.isArray(v) && typeof v !== "function";
  const merge = (base, extra) => {
    const out = { ...base };
    for (const [k, v] of Object.entries(extra || {})) if (v !== undefined) out[k] = isPlain(v) && isPlain(base[k]) ? merge(base[k], v) : v;
    return out;
  };
  const ours = (event) => event.composedPath().includes(host);
  const list = (value, presets) => (Array.isArray(value) ? value : presets[value] || null);
  const push = (buffer, item, max = 30) => { buffer.push(item); if (buffer.length > max) buffer.shift(); };
  const ms = (n) => (n == null ? undefined : `${Math.round(n)} ms`);

  // Anonymity --------------------------------------------------------------------------------------------

  const PATTERNS = [
    [/[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]{1,253}\.[A-Za-z]{2,24}/g, "[email]"],
    [/\b(?:ghp|gho|github_pat|sk|pk|xox[bap])[-_][A-Za-z0-9_-]{8,}/g, "[key]"],
    [/\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]+/g, "[key]"],
    [/\bAKIA[0-9A-Z]{16}\b/g, "[key]"],
    [/\bbearer\s+[A-Za-z0-9._~+/=-]{6,}/gi, "[key]"],
    // eslint-disable-next-line security/detect-unsafe-regex -- fixed words, then one run of each kind, so no backtracking
    [/(?:\b|_)(?:bearer|token|api[_-]?key|password|passwd|pwd|secret|session(?:[_-]?id)?|auth(?:orization)?)\s*[:=]\s*\S+/gi, "[key]"],
    // eslint-disable-next-line security/detect-unsafe-regex -- bounded repeats, so it runs in linear time
    [/\b(?:\d{1,3}\.){3}\d{1,3}\b/g, "[address]"],
    [/\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi, "[id]"],
    [/\b[A-Za-z0-9_-]*\d[A-Za-z0-9_-]*[A-Za-z][A-Za-z0-9_-]{22,}\b/g, "[key]"],
    [/\+?\d[\d ()-]{5,}\d/g, "[number]"],
    [/\b\d{3,5}\.\d{3}\.\d{3,4}\b/g, "[number]"],
  ];
  function redact(text) {
    let out = String(text ?? "").slice(0, 20000);
    for (const [pattern, label] of [...PATTERNS, ...(config ? config.redact : [])]) out = out.replace(pattern, label);
    return out;
  }
  // Cut to n characters, never through the middle of an emoji (half of one cannot be put in a link).
  const clip = (text, n) => (text.length > n ? `${text.slice(0, n - 1).replace(/[\uD800-\uDBFF]$/, "")}…` : text);

  const idLike = (s) => /^\d+$/.test(s) || /^[0-9a-f-]{8,}$/i.test(s) || /@/.test(s) || (/\d/.test(s) && s.length >= 16);
  function cleanPath(pathname) {
    return pathname.split("/").map((s) => { let raw = s; try { raw = decodeURIComponent(s); } catch { /* keep */ } return idLike(raw) ? ":id" : raw.replace(/[\s\u0000-\u001f]+/g, " "); }).join("/");
  }
  function pathPattern() {
    const params = new URLSearchParams(location.search);
    const kept = config.page.keepParams.filter((k) => params.has(k)).map((k) => `${k}=${redact(params.get(k))}`);
    return cleanPath(location.pathname) + (kept.length ? `?${kept.join("&")}` : "");
  }
  // A request's address without its query, hash or ids; the host only when it is another site.
  function cleanUrl(url) {
    try {
      const u = new URL(url, location.href);
      if (!/^https?:$/.test(u.protocol)) return u.protocol; // tel:, mailto:, skype: and the like hold the person
      return (u.origin === location.origin ? "" : u.host) + cleanPath(u.pathname);
    } catch { return "[address]"; }
  }

  function browserAndSystem() {
    const ua = navigator.userAgent;
    const browser = [[/Edg\/(\d+)/, "Edge"], [/OPR\/(\d+)/, "Opera"], [/Firefox\/(\d+)/, "Firefox"], [/Chrome\/(\d+)/, "Chrome"],
      [/Version\/(\d+).*Safari/, "Safari"]].map(([re, name]) => { const m = ua.match(re); return m && `${name} ${m[1]}`; }).find(Boolean) || "Other";
    const system = [[/Android/, "Android"], [/iPhone|iPad|iPod/, "iOS"], [/Mac OS X|Macintosh/, "macOS"], [/Windows/, "Windows"],
      [/CrOS/, "ChromeOS"], [/Linux/, "Linux"]].find(([re]) => re.test(ua))?.[1] || "Other";
    return { browser, system };
  }

  // Recording in the background, only what reports can use, kept in memory on this page only ------------------

  function on(target, type, fn, opts = true) { target.addEventListener(type, fn, opts); undo.push(() => target.removeEventListener(type, fn, opts)); }
  const wanted = (part) => config.collect[part] !== false;

  // Messages from scripts can hold addresses: keep each address, but take out its query and fragment.
  const noQueries = (text) => String(text).split(/(\s+)/).map((w) => (/[/.]/.test(w) ? w.replace(/[?#][^?#].*$/, "") : w)).join("");
  const scrub = (text, n) => clip(redact(noQueries(String(text).slice(0, n * 4)).replace(/\s+/g, " ")), n);

  function stackLines(stack) {
    return String(stack || "").split("\n").slice(1, 4).map((l) => redact(l.trim().replace(/\?[^:)\s]*/g, "").replace(/https?:\/\/[^/]+/g, ""))).filter(Boolean);
  }

  function record() {
    const lim = config.limits;
    if (wanted("errors")) {
      on(window, "error", (e) => {
        const target = e.target;
        if (target && target !== window && target.tagName) { // a picture, script or style sheet that did not load
          push(buffers.resources, `${target.tagName.toLowerCase()} did not load: ${cleanUrl(target.src || target.href || "")}`, 10);
          return;
        }
        recNote(`Error on the page: ${scrub(e.message || "Error", 160)}`);
        push(buffers.errors, { message: scrub(e.message || "Error", 300),
          where: e.filename ? `${cleanUrl(e.filename)}:${e.lineno}:${e.colno}` : undefined, stack: stackLines(e.error?.stack) }, lim.errors * 2);
      });
      on(window, "unhandledrejection", (e) => push(buffers.errors, { message: scrub(`Unhandled promise: ${e.reason?.message || e.reason}`, 300),
        stack: stackLines(e.reason?.stack) }, lim.errors * 2));
      on(document, "securitypolicyviolation", (e) => push(buffers.errors, { message: `Content Security Policy blocked ${e.effectiveDirective}: ${cleanUrl(e.blockedURI || "")}` }, lim.errors * 2));
    }
    if (wanted("console")) {
      for (const level of ["error", "warn"]) {
        const original = console[level];
        const wrapper = console[level] = function (...args) {
          try {
            const text = args.map((a) => (a instanceof Error ? `${a.name}: ${a.message}` : a && typeof a === "object" ? (Array.isArray(a) ? `[a list of ${a.length}]` : `{${Object.keys(a).slice(0, 10).join(", ")}}`) : String(a))).join(" ");
            push(buffers.console, `${level}: ${scrub(text, 300)}`, lim.console * 2);
          } catch { /* never break the page's own logging */ }
          return original.apply(this, args);
        };
        undo.push(() => { if (console[level] === wrapper) console[level] = original; }); // never undo another tool's wrapper
      }
    }
    if (wanted("network")) {
      const note = (method, url, status, started) => {
        const duration = performance.now() - started;
        if (status >= 400 || status === 0) recNote(`Request failed: ${String(method || "GET").toUpperCase()} ${cleanUrl(url)} ${status || "(no answer)"}`);
        else if (duration > lim.slowRequest) recNote(`Slow request: ${String(method || "GET").toUpperCase()} ${cleanUrl(url)} took ${Math.round(duration)} ms`);
        if (status >= 400 || status === 0 || duration > lim.slowRequest) {
          push(buffers.network, { method: String(method || "GET").toUpperCase(), url: cleanUrl(url), status: status || "failed", duration: Math.round(duration) }, lim.network * 2);
        }
      };
      if (window.fetch) {
        const original = window.fetch;
        const wrapper = window.fetch = function (input, init) {
          const started = performance.now();
          const url = typeof input === "string" ? input : input?.url || String(input);
          const method = init?.method || input?.method || "GET";
          return original.apply(this, arguments).then(
            (res) => { note(method, url, res.status, started); return res; },
            (err) => { note(method, url, 0, started); throw err; });
        };
        undo.push(() => { if (window.fetch === wrapper) window.fetch = original; });
      }
      const open = XMLHttpRequest.prototype.open;
      const send = XMLHttpRequest.prototype.send;
      const xo = XMLHttpRequest.prototype.open = function (method, url) { this.__snag = { method, url }; return open.apply(this, arguments); };
      const xs = XMLHttpRequest.prototype.send = function () {
        const info = this.__snag;
        if (info) { const started = performance.now(); this.addEventListener("loadend", () => note(info.method, info.url, this.status, started), { once: true }); }
        return send.apply(this, arguments);
      };
      undo.push(() => {
        if (XMLHttpRequest.prototype.open === xo) XMLHttpRequest.prototype.open = open;
        if (XMLHttpRequest.prototype.send === xs) XMLHttpRequest.prototype.send = send;
      });
    }
    if (wanted("trail")) {
      on(window, "click", (e) => {
        if (picking || ours(e) || !(e.target instanceof Element)) return;
        const el = e.target.closest("a, button, [role], input, select, textarea, summary, label") || e.target;
        if (el.tagName === "LABEL" && el.control) return; // the control's own click follows
        const d = describe(el);
        if (isChoice(el)) { push(buffers.trail, choiceStep(el, d.component ? ` in ${d.component}` : ""), lim.trail * 2); return; }
        push(buffers.trail, `Selected ${[d.component, d.role, d.name && `"${d.name}"`].filter(Boolean).join(", ")}`, lim.trail * 2);
      });
      on(window, "change", (e) => {
        if (ours(e) || !(e.target instanceof Element) || isChoice(e.target)) return;
        const d = describe(e.target);
        push(buffers.trail, `Changed ${[d.component, d.role, d.name && `"${d.name}"`].filter(Boolean).join(", ")}`, lim.trail * 2);
      });
      on(window, "submit", (e) => { if (!ours(e)) push(buffers.trail, `Sent the form ${componentOf(e.target) || ""}`.trim(), lim.trail * 2); });
      let lastPath = location.pathname;
      const moved = () => { if (location.pathname !== lastPath) { lastPath = location.pathname; push(buffers.trail, `Went to ${cleanPath(lastPath)}`, lim.trail * 2); } };
      on(window, "popstate", moved);
      const timer = setInterval(moved, 1000); // single-page apps change the address without an event
      undo.push(() => clearInterval(timer));
    }
    if (wanted("performance") && "PerformanceObserver" in window) {
      const observe = (type, fn, extra = {}) => {
        try { const o = new PerformanceObserver((l) => l.getEntries().forEach(fn)); o.observe({ type, buffered: true, ...extra }); undo.push(() => o.disconnect()); } catch { /* not supported */ }
      };
      observe("largest-contentful-paint", (e) => { vitals.lcp = e.startTime; });
      observe("layout-shift", (e) => { if (!e.hadRecentInput) vitals.cls += e.value; });
      observe("event", (e) => { if (e.interactionId && (vitals.inp == null || e.duration > vitals.inp)) vitals.inp = e.duration; }, { durationThreshold: 40 });
      observe("longtask", (e) => push(buffers.longTasks, { at: e.startTime, duration: e.duration }, 50));
    }
  }

  // Naming and describing the chosen part --------------------------------------------------------------------

  const ROLES = { a: "link", button: "button", input: "field", select: "choice", textarea: "text box", img: "picture", svg: "icon",
    nav: "menu", header: "header", footer: "footer", main: "main content", aside: "side panel", dialog: "dialog", form: "form",
    table: "table", tr: "table row", td: "table cell", th: "table heading", ul: "list", ol: "list", li: "list item",
    label: "label", p: "text", section: "section", article: "card" };
  const NAMED = new Set(["a", "button", "label", "h1", "h2", "h3", "h4", "h5", "h6", "th", "legend", "summary", "option"]);

  function roleOf(el) {
    const tag = el.tagName.toLowerCase();
    if (tag === "input") return el.type && el.type !== "text" ? `${el.type} field` : "field";
    return el.getAttribute("role") || ROLES[tag] || (/^h[1-6]$/.test(tag) ? `heading ${tag[1]}` : "area");
  }

  // The accessible name, but only for things whose name is a label (buttons, links, headings, fields), never the
  // text of a whole area, which could hold someone's name or messages. Fields give their label, never their value.
  function nameOf(el) {
    if (el.closest("[data-snag-private]")) return "[private]";
    if (el.isContentEditable) return ""; // a heading or link typed into an editor is what someone wrote
    const tag = el.tagName.toLowerCase();
    let name = el.getAttribute("aria-label") || "";
    const labelledby = el.getAttribute("aria-labelledby");
    if (!name && labelledby) name = labelledby.split(/\s+/).map((id) => document.getElementById(id)?.textContent || "").join(" ");
    if (!name && ["input", "select", "textarea"].includes(tag)) name = (el.labels && el.labels[0]?.textContent) || el.getAttribute("placeholder") || "";
    if (!name && tag === "img") name = el.getAttribute("alt") || "";
    if (!name && (NAMED.has(tag) || /^(button|link|tab|menuitem|heading|checkbox|radio|switch|option)$/.test(el.getAttribute("role") || "")))
      name = el.textContent || "";
    if (!name) name = el.getAttribute("title") || "";
    return clip(redact(name.replace(/\s+/g, " ").trim()), 80);
  }

  function componentOf(el) {
    if (typeof config.component.from === "function") {
      try { const named = config.component.from(el); if (named) return String(named); } catch { /* fall through */ }
    }
    const attr = config.component.attribute;
    const marked = attr && el.closest(`[${attr}]`);
    if (marked) return marked.getAttribute(attr);
    for (let n = el, i = 0; n && n !== document.body && i < 5; n = n.parentElement, i++) {
      const block = [...n.classList].find((c) => !/__|--|^(is|has|js)-/.test(c) && c.length > 1 && !/\d{3,}/.test(c));
      if (block) return block;
    }
    return "";
  }

  function selectorOf(el) {
    const steps = [];
    for (let n = el; n && n !== document.documentElement && steps.length < 5; n = n.parentElement) {
      if (n.id && !/\d{3,}/.test(n.id)) { steps.unshift(`#${CSS.escape(n.id)}`); break; }
      let step = n.tagName.toLowerCase();
      const classes = [...n.classList].filter((c) => !/^(is|has)-/.test(c) && !/\d{3,}/.test(c)).slice(0, 2);
      if (classes.length) step += classes.map((c) => `.${CSS.escape(c)}`).join("");
      const same = n.parentElement ? [...n.parentElement.children].filter((c) => c.tagName === n.tagName) : [];
      if (same.length > 1) step += `:nth-of-type(${same.indexOf(n) + 1})`;
      steps.unshift(step);
    }
    return steps.join(" > ");
  }

  function parseColour(value) {
    // eslint-disable-next-line security/detect-unsafe-regex -- the repeated classes never overlap, so no backtracking
    const m = value.match(/rgba?\(([\d.]+)[ ,]+([\d.]+)[ ,]+([\d.]+)(?:[ ,/]+([\d.]+%?))?/);
    if (!m) return null;
    const a = m[4] == null ? 1 : m[4].endsWith("%") ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
    return { r: +m[1], g: +m[2], b: +m[3], a };
  }
  const hex = (c) => `#${[c.r, c.g, c.b].map((v) => Math.round(v).toString(16).padStart(2, "0")).join("")}`;
  function luminance(c) {
    const [r, g, b] = [c.r, c.g, c.b].map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  }
  function stylesOf(el) {
    const s = getComputedStyle(el);
    const fg = parseColour(s.color);
    let bg = null;
    for (let n = el; n; n = n.parentElement) {
      const c = parseColour(getComputedStyle(n).backgroundColor);
      if (c && c.a > 0.95) { bg = c; break; }
    }
    bg = bg || { r: 255, g: 255, b: 255, a: 1 };
    const out = { font: `${s.fontSize}, weight ${s.fontWeight}, line height ${s.lineHeight}`, text: fg ? hex(fg) : s.color, background: hex(bg) };
    if (fg) {
      const [l1, l2] = [luminance(fg), luminance(bg)].sort((a, b) => b - a);
      const ratio = (l1 + 0.05) / (l2 + 0.05);
      const large = parseFloat(s.fontSize) >= 24 || (parseFloat(s.fontSize) >= 18.66 && +s.fontWeight >= 700);
      out.contrast = `${ratio.toFixed(2)}:1 (${ratio >= (large ? 3 : 4.5) ? "passes" : "fails"} WCAG AA for ${large ? "large" : "normal"} text)`;
    }
    return out;
  }

  function layoutOf(el) {
    const r = el.getBoundingClientRect();
    const s = getComputedStyle(el);
    const out = { box: `${Math.round(r.width)}×${Math.round(r.height)} at ${Math.round(r.left)}, ${Math.round(r.top + scrollY)}`,
      display: s.display, position: s.position !== "static" ? s.position : undefined, zIndex: s.zIndex !== "auto" ? s.zIndex : undefined };
    // Content bigger than its box by more than a few pixels: cut off when overflow hides it, spilling out when not.
    if (el.scrollWidth > el.clientWidth + 4 || el.scrollHeight > el.clientHeight + 4) {
      const clipped = [s.overflowX, s.overflowY].some((o) => o !== "visible");
      out.overflow = `content is ${el.scrollWidth}×${el.scrollHeight} in a ${el.clientWidth}×${el.clientHeight} box, ${clipped ? `cut off or scrolling (overflow ${s.overflowX} ${s.overflowY})` : "spilling out"}`;
    }
    const x = r.left + r.width / 2;
    const y = r.top + r.height / 2;
    if (r.right < 0 || r.bottom < 0 || r.left > innerWidth || r.top > innerHeight) out.onScreen = "not in view when reported";
    else if (r.left < 0 || r.right > innerWidth) out.onScreen = "partly off the side of the page";
    if (x >= 0 && y >= 0 && x < innerWidth && y < innerHeight) {
      const top = document.elementFromPoint(x, y);
      if (top && top !== el && !el.contains(top) && top !== host) out.coveredBy = selectorOf(top);
    }
    if (s.visibility !== "visible" || +s.opacity < 0.1) out.hidden = `visibility ${s.visibility}, opacity ${s.opacity}`;
    if (document.documentElement.scrollWidth > innerWidth + 1) out.pageScrollsSideways = `${document.documentElement.scrollWidth} px wide in a ${innerWidth} px window`;
    return out;
  }

  const FOCUSABLE = "a[href], button, input, select, textarea, summary, [tabindex], [contenteditable]";
  function a11yOf(el) {
    const r = el.getBoundingClientRect();
    const states = ["aria-expanded", "aria-pressed", "aria-checked", "aria-selected", "aria-current", "aria-invalid", "aria-disabled", "aria-busy", "aria-live"]
      .filter((a) => el.hasAttribute(a)).map((a) => `${a.slice(5)}=${el.getAttribute(a)}`);
    if (el.disabled) states.push("disabled");
    if (el.required) states.push("required");
    if (el.checked) states.push("checked");
    const hiddenBy = el.closest("[aria-hidden='true'], [inert]");
    const landmark = el.closest("main, nav, header, footer, aside, form, dialog, [role=dialog], [role=region], [role=navigation], [role=main]");
    const out = {
      role: el.getAttribute("role") || roleOf(el), name: nameOf(el) || "none",
      states: states.join(", ") || undefined,
      focusable: el.matches(FOCUSABLE) && el.tabIndex >= 0 ? `yes${el.hasAttribute("tabindex") ? `, tabindex ${el.getAttribute("tabindex")}` : ""}` : el.matches(FOCUSABLE) ? "only by script (tabindex -1)" : "no",
      target: `${Math.round(r.width)}×${Math.round(r.height)} px${el.matches(FOCUSABLE) && (r.width < 24 || r.height < 24) ? " (under the 24×24 px minimum in WCAG 2.5.8)" : ""}`,
      hidden: hiddenBy ? `inside ${hiddenBy.hasAttribute("inert") ? "an inert" : "an aria-hidden"} element` : undefined,
      landmark: landmark ? `${landmark.getAttribute("role") || landmark.tagName.toLowerCase()}${landmark.getAttribute("aria-label") ? ` "${clip(redact(landmark.getAttribute("aria-label")), 40)}"` : ""}` : "none",
      language: document.documentElement.lang || "not set",
    };
    if (el.tagName === "IMG" && !el.hasAttribute("alt")) out.problem = "picture with no alt attribute";
    if (["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName) && !(el.labels && el.labels.length) && !el.getAttribute("aria-label") && !el.getAttribute("aria-labelledby")) out.problem = "field with no label";
    return out;
  }

  // The part's HTML with every bit of text and every value taken out, so the structure shows without the content.
  const SAFE_ATTRS = /^(id|class|role|type|name|for|tabindex|disabled|hidden|href|src|alt|aria-[a-z]+|data-component|data-testid)$/;
  function htmlOf(el) {
    if (el.closest("[data-snag-private]")) return "[private]";
    const copy = el.cloneNode(true);
    const walk = (node) => {
      for (const child of [...node.childNodes]) {
        if (child.nodeType === 3) { const n = child.textContent.trim().length; child.textContent = n ? `…${n > 1 ? `(${n})` : ""}` : child.textContent; }
        else if (child.nodeType === 1) {
          if (child.hasAttribute("data-snag-private") || /^(script|style|svg)$/i.test(child.tagName)) { child.replaceWith(document.createComment(child.tagName.toLowerCase())); continue; }
          clean(child); walk(child);
        } else child.remove();
      }
    };
    const clean = (node) => {
      for (const a of [...node.attributes]) {
        if (!SAFE_ATTRS.test(a.name)) node.removeAttribute(a.name);
        else if (a.name === "href" || a.name === "src") node.setAttribute(a.name, cleanUrl(a.value));
        else if (/^aria-(valuetext|valuenow|description|placeholder|roledescription)$/.test(a.name)) node.setAttribute(a.name, "…");
        else node.setAttribute(a.name, clip(redact(a.value), 60));
      }
      if (/^(INPUT|TEXTAREA)$/.test(node.tagName)) node.removeAttribute("value");
    };
    clean(copy); walk(copy);
    return clip(copy.outerHTML.replace(/\s+/g, " "), 1500);
  }

  // The visible text of the chosen part, cleaned; never a field's value, never what was typed into an editable
  // area, and never the text of a private part inside it.
  function textOf(el) {
    if (el.closest("[data-snag-private]")) return "[private]";
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) || el.isContentEditable) return "";
    let text = (el.innerText || "").replace(/\s+/g, " ");
    for (const inner of el.querySelectorAll("[contenteditable]:not([contenteditable=false]), [data-snag-private]")) {
      const hidden = (inner.innerText || "").replace(/\s+/g, " ").trim();
      if (hidden) text = text.split(hidden).join("…");
    }
    return clip(redact(text.trim()), 300);
  }

  // Messages the page is showing now: alerts, status messages and field errors.
  function messagesOnScreen() {
    const seen = new Set();
    const out = [];
    for (const el of document.querySelectorAll("[role=alert], [role=status], [aria-live=assertive], [aria-live=polite], .error, .alert, .notice, [aria-invalid=true]")) {
      if (el === host || host.contains(el) || el.closest("[data-snag-private]")) continue;
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height) continue;
      let text = el.getAttribute("aria-invalid") === "true"
        ? `${nameOf(el)}: ${(el.getAttribute("aria-describedby") || "").split(/\s+/).map((id) => document.getElementById(id)?.innerText || "").join(" ")}`
        : el.innerText || "";
      text = clip(redact(text.replace(/\s+/g, " ").trim()), 200);
      if (text && !seen.has(text)) { seen.add(text); out.push(text); }
      if (out.length >= 5) break;
    }
    return out;
  }

  function describe(el) {
    if (!el || el === document.body || el === document.documentElement) return { whole: true, label: t("wholePage") };
    const out = { component: componentOf(el), role: roleOf(el), name: nameOf(el), selector: selectorOf(el) };
    out.label = [out.component, out.role, out.name && `"${out.name}"`].filter(Boolean).join(", ");
    out.text = textOf(el);
    out.node = el;
    return out;
  }

  function performanceNow() {
    const nav = performance.getEntriesByType?.("navigation")?.[0];
    const recent = buffers.longTasks.filter((l) => l.at > performance.now() - 60000);
    const out = {
      load: nav ? `server answered in ${ms(nav.responseStart)}, page ready in ${ms(nav.domContentLoadedEventEnd)}, fully loaded in ${ms(nav.loadEventEnd || undefined) || "not yet"}` : undefined,
      lcp: vitals.lcp != null ? `${ms(vitals.lcp)} (${vitals.lcp <= 2500 ? "good" : vitals.lcp <= 4000 ? "needs improvement" : "poor"})` : undefined,
      cls: `${vitals.cls.toFixed(3)} (${vitals.cls <= 0.1 ? "good" : vitals.cls <= 0.25 ? "needs improvement" : "poor"})`,
      inp: vitals.inp != null ? `${ms(vitals.inp)} slowest interaction (${vitals.inp <= 200 ? "good" : vitals.inp <= 500 ? "needs improvement" : "poor"})` : undefined,
      longTasks: recent.length ? `${recent.length} in the last minute, longest ${ms(Math.max(...recent.map((l) => l.duration)))}` : "none in the last minute",
      timeOnPage: `${Math.round(performance.now() / 1000)} s`,
      memory: performance.memory ? `${Math.round(performance.memory.usedJSHeapSize / 1048576)} MB of script memory in use` : undefined,
    };
    if (buffers.resources.length) out.notLoaded = buffers.resources.slice(-5).join("; ");
    return out;
  }

  // The report -------------------------------------------------------------------------------------------

  function partsFor(type) {
    const asked = new Set(type.collect || []);
    return PARTS.map(([key]) => key).filter((key) => config.collect[key] === true || (config.collect[key] === "auto" && asked.has(key)));
  }

  function buildReport(part, values, typeKey) {
    const type = types().find(([k]) => k === typeKey)[1];
    const lim = config.limits;
    const report = { tool: `snaglist ${VERSION}`, app: config.app || location.host, version: config.version || undefined, type: typeKey,
      category: values.category, severity: values.severity || undefined, scale: values.severity ? scaleOf(type).name : undefined, answers: {} };
    for (const f of type.fields) if (values[f.id] && typeof values[f.id] === "string") report.answers[f.id] = f.type === "select" ? values[f.id] : clip(redact(values[f.id]), 3000);
    const el = part.node;
    for (const key of partsFor(type)) {
      if (key === "page") {
        let name = document.title;
        if (typeof config.page.name === "function") { try { name = config.page.name() || name; } catch { /* keep the title */ } }
        report.page = { name: clip(redact(name), 120), path: pathPattern() };
      }
      if (key === "element") report.element = part.whole ? { whole: true } : { component: part.component, role: part.role, name: part.name, selector: part.selector };
      if (key === "text") {
        const messages = messagesOnScreen();
        if ((part.text && !part.whole) || messages.length) report.text = { chosen: part.whole ? undefined : part.text || undefined, messages: messages.length ? messages : undefined };
      }
      if (key === "steps" && values.steps?.length) report.steps = values.steps;
      if (key === "page" && values.check) report.check = values.check;
      if (el && key === "styles") report.styles = stylesOf(el);
      if (el && key === "layout") report.layout = layoutOf(el);
      if (el && key === "a11y") report.a11y = a11yOf(el);
      if (el && key === "html") report.html = htmlOf(el);
      if (key === "environment") {
        const { browser, system } = browserAndSystem();
        const c = navigator.connection;
        report.environment = { viewport: `${innerWidth}×${innerHeight}`, screen: `${screen.width}×${screen.height}`, zoom: Math.round(devicePixelRatio * 100) / 100,
          browser, system, language: navigator.language, touch: matchMedia("(pointer: coarse)").matches,
          orientation: screen.orientation?.type?.split("-")[0], online: navigator.onLine,
          connection: c ? `${c.effectiveType || "unknown"}${c.saveData ? ", data saver on" : ""}` : undefined,
          device: navigator.deviceMemory ? `${navigator.deviceMemory} GB memory, ${navigator.hardwareConcurrency} cores` : undefined };
      }
      if (key === "display") {
        const rootEl = document.documentElement;
        const scheme = rootEl.dataset.theme || rootEl.getAttribute("data-color-scheme") || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark (device)" : "light (device)");
        report.display = { theme: scheme, textSize: getComputedStyle(rootEl).fontSize,
          reducedMotion: matchMedia("(prefers-reduced-motion: reduce)").matches,
          moreContrast: matchMedia("(prefers-contrast: more)").matches, forcedColours: matchMedia("(forced-colors: active)").matches };
      }
      if (key === "performance") report.performance = performanceNow();
      if (key === "errors" && buffers.errors.length) report.errors = buffers.errors.slice(-lim.errors);
      if (key === "console" && buffers.console.length) report.console = buffers.console.slice(-lim.console);
      if (key === "network" && buffers.network.length) report.network = buffers.network.slice(-lim.network);
      if (key === "trail" && buffers.trail.length) report.trail = buffers.trail.slice(-lim.trail);
      if (key === "context" && typeof config.context === "function") {
        try {
          const c = config.context();
          if (c && typeof c === "object") report.context = Object.fromEntries(Object.entries(c).map(([k, v]) => [k, clip(redact(typeof v === "string" ? v : JSON.stringify(v)), 200)]));
        } catch { /* the app's own function failed */ }
      }
    }
    report.created = new Date().toISOString().slice(0, 16) + "Z";
    if (typeof config.beforeSend === "function") {
      try { return config.beforeSend(structuredClone(report)) || report; } catch { return report; }
    }
    return report;
  }

  const label = (items, value) => (items?.find(([v]) => v === value) || [value, value])[1];

  function types() {
    return config.types.map((x) => (typeof x === "string" ? [x, TYPES[x]] : [x.id, { collect: [], group: "problem", pick: true, ...x }]))
      .filter(([, v]) => v).map(([k, v]) => [k, { ...v, label: tr(`types.${k}.label`, v.label), hint: tr(`types.${k}.hint`, v.hint),
        fields: (v.fields || []).map((f) => ({ ...f, label: tr(`fields.${f.id}.label`, f.label), hint: f.hint && tr(`fields.${f.id}.hint`, f.hint),
          options: f.options && f.options.map(([ov, ol]) => [ov, tr(`fields.${f.id}.options.${ov}`, ol)]) })) }]);
  }
  const scaleItems = (name, items) => items.map(([v, l, hint]) => [v, tr(`scales.${name}.${v}`, l), hint && tr(`scales.${name}.${v}.hint`, hint)]);
  const typeOf = (key) => types().find(([k]) => k === key)?.[1];

  // The rating question for a type: problems ask how bad, suggestions how much it would help; scale: false asks nothing.
  function scaleOf(type) {
    if (type.scale === false) return null;
    if (type.scale === "importance" || (type.group === "suggestion" && type.scale == null)) {
      return { name: "importance", items: scaleItems("importance", list(config.importance, SEVERITIES) || SEVERITIES.importance), question: t("importance"), row: "How much it would help" };
    }
    return { name: "severity", items: scaleItems("severity", list(type.scale || config.severities, SEVERITIES) || SEVERITIES.three), question: t("severity"), row: "How bad" };
  }

  function titleOf(report) {
    const type = types().find(([k]) => k === report.type)?.[1];
    if (report.check) return clip(report.check.length === 1 ? `Accessibility: ${report.check[0].name} (${report.check[0].count})` : `Accessibility check: ${report.check.length} problems on ${report.page?.name || "this page"}`, 90);
    const first = (Object.values(report.answers).find((v) => v && v.length > 8) || type?.label || "").split("\n")[0];
    const where = report.element && !report.element.whole && report.element.component ? `${report.element.component}: ` : "";
    return clip(`${where}${first}`, 90);
  }

  // Text for a table cell: a backslash or | from the page cannot add a column.
  const cell = (v) => String(v).replace(/\\/g, "\\\\").replace(/\|/g, "\\|");
  function markdown(report, short) {
    const type = types().find(([k]) => k === report.type)?.[1] || { fields: [] };
    const row = (k, v) => (v === undefined || v === "" || v === false ? null : `| ${k} | ${cell(v).replace(/\n/g, " ")} |`);
    const table = (obj, names = {}) => ["| | |", "|---|---|", ...Object.entries(obj).map(([k, v]) => row(names[k] || k.replace(/([A-Z])/g, " $1").toLowerCase().replace(/^./, (c) => c.toUpperCase()), v === true ? "yes" : v))].filter(Boolean);
    const out = [];
    for (const f of type.fields) {
      const v = report.answers[f.id];
      if (!v) continue;
      out.push(`**${f.label}**`, f.type === "select" ? label(f.options, v) : v, "");
    }
    out.push("| | |", "|---|---|", row("Report", type.label || report.type));
    if (report.category) out.push(row("Kind", label(list(config.categories, CATEGORIES), report.category)));
    const scale = report.severity ? scaleOf(type) : null;
    if (scale) out.push(row(scale.row, label(scale.items, report.severity)));
    out.push(row("App", [report.app, report.version].filter(Boolean).join(" ")));
    if (report.page) out.push(row("Page", report.page.name), row("Path", `\`${report.page.path}\``));
    const e = report.element;
    if (e) {
      if (e.whole) out.push(row("Part", "Whole page"));
      else out.push(row("Component", e.component && `\`${e.component}\``), row("Part", [e.role, e.name && `"${e.name}"`].filter(Boolean).join(", ")), row("Selector", e.selector && `\`${e.selector}\``));
    }
    const env = short === "qr" ? null : report.environment;
    if (env) out.push(row("Window", `${env.viewport} (screen ${env.screen}), zoom ${env.zoom}${env.touch ? ", touch" : ""}${env.orientation ? `, ${env.orientation}` : ""}`),
      row("Browser", `${env.browser} on ${env.system}, ${env.language}`), row("Device", [env.device, env.connection && `connection ${env.connection}`, env.online === false && "offline"].filter(Boolean).join(", ")));
    const d = short === "qr" ? null : report.display;
    if (d) out.push(row("Display", [d.theme, `text ${d.textSize}`, d.reducedMotion && "reduced motion", d.moreContrast && "more contrast", d.forcedColours && "forced colours"].filter(Boolean).join(", ")));
    out.push(row("Reported", report.created.replace("T", " ")));
    const lines = out.filter((l) => l != null);
    const section = (title, body) => { lines.push("", `**${title}**`, "", ...body); };
    if (short) {
      if (short !== "qr") lines.push("", "_The technical details did not fit in GitHub's address: paste the full report from your clipboard here._");
    } else {
      if (report.check) for (const c of report.check) {
        section(`Accessibility check: ${c.name}`, [
          `WCAG ${c.wcag}${c.level ? `, level ${c.level}` : ""}${c.impact ? `, impact ${c.impact}` : ""}. Found ${c.count} ${c.count === 1 ? "time" : "times"}.`, "",
          `${c.why}`, "", `**How to fix it:** ${c.fix}`,
          ...(c.example ? ["", "```html", "<!-- Before -->", c.example.before, "", "<!-- After -->", c.example.after, "```"] : []),
          "", "| Where | Part |", "|---|---|", ...c.where.map((w) => `| \`${w.selector}\` | ${cell(w.part)} |`),
          ...(c.count > c.where.length ? [`| and ${c.count - c.where.length} more | |`] : [])]);
      }
      if (report.steps) section("Recorded steps", report.steps.map((s, i) => `${i + 1}. ${s.text} (${s.at} s)`));
      if (report.text) section("Text on screen", [...(report.text.chosen ? [`> ${report.text.chosen}`] : []), ...(report.text.messages || []).map((m) => `- Message: ${m}`)]);
      if (report.styles) section("Styles", table(report.styles));
      if (report.layout) section("Layout", table(report.layout, { coveredBy: "Covered by", onScreen: "On the page", pageScrollsSideways: "Page scrolls sideways" }));
      if (report.a11y) section("Accessibility", table(report.a11y, { a11y: "Accessibility" }));
      if (report.html) section("HTML, with text and values taken out", ["```html", report.html, "```"]);
      if (report.performance) section("Page speed", table(report.performance, { lcp: "Largest paint (LCP)", cls: "Layout shift (CLS)", inp: "Interaction delay (INP)", notLoaded: "Did not load" }));
      if (report.errors) section("Script errors", ["```", ...report.errors.flatMap((x) => [x.message, ...(x.where ? [`  at ${x.where}`] : []), ...(x.stack || []).map((s) => `  ${s}`)]), "```"]);
      if (report.console) section("Console", ["```", ...report.console, "```"]);
      if (report.network) section("Failed and slow requests", ["| Request | Status | Time |", "|---|---|---|", ...report.network.map((n) => `| ${n.method} \`${n.url}\` | ${n.status} | ${n.duration} ms |`)]);
      if (report.trail) section("Before this", report.trail.map((s, i) => `${i + 1}. ${s}`));
      if (report.context) section("App details", table(report.context));
    }
    // Text from the page must never notify anyone on GitHub: an invisible space after @ stops a mention.
    if (short !== "qr") lines.push("", `<sub>Sent with ${report.tool}, which leaves out accounts, cookies and what was typed into the page.</sub>`);
    return lines.join("\n").replace(/@(?=[A-Za-z0-9-])/g, "@\u200b");
  }

  const plain = (md) => md.replace(/^\|---\|---\|(---\|)?$/gm, "").replace(/^\| \| \|$/gm, "").replace(/^\| (.+?) \| (.*) \|$/gm, "$1: $2")
    .replace(/[`*_]|<\/?sub>/g, "").replace(/\n{3,}/g, "\n\n");

  // Sending ----------------------------------------------------------------------------------------------

  async function copyText(text) {
    try { await navigator.clipboard.writeText(text); return true; } catch {
      const area = h("textarea", { style: "position:fixed;opacity:0" }); area.value = text; wrap.append(area); area.select();
      const ok = document.execCommand("copy"); area.remove(); return ok;
    }
  }

  // A new tab for GitHub, cut off from this page; the same tab when pop-ups are blocked. Mail links open the
  // mail app without leaving the page.
  function openUrl(url) {
    if (url.startsWith("mailto:")) { location.href = url; return; }
    const w = window.open(url, "_blank");
    if (w) w.opener = null;
    else location.href = url;
  }

  const enc = (text) => encodeURIComponent(text.toWellFormed ? text.toWellFormed() : text);
  // The new issue's address, without the body: the repository, labels and title.
  function githubBase(report) {
    const g = config.send.github;
    const group = typeOf(report.type)?.group;
    const labels = [...config.labels, ...(g.labels || []), group === "suggestion" ? "suggestion" : group === "problem" ? "problem" : null, report.check ? "accessibility" : null, `type: ${report.type}`,
      report.category && `kind: ${report.category}`, report.severity && `${report.scale}: ${report.severity}`].filter(Boolean).join(",");
    return `https://github.com/${g.repo}/issues/new?labels=${enc(labels)}&title=${enc(titleOf(report))}`;
  }

  async function sendGithub(report) {
    const base = githubBase(report);
    let url = `${base}&body=${enc(markdown(report))}`;
    if (url.length > 7500) {
      await copyText(markdown(report));
      url = `${base}&body=${enc(markdown(report, true))}`;
      if (url.length > 7500) url = `${base}&body=${enc("Paste the report here: it is on your clipboard.")}`;
      say(t("copiedForGithub"));
    }
    openUrl(url);
    return t("opened");
  }

  const mailtoUrl = (report, body) => { const m = config.send.email; return `mailto:${enc(m.to)}?subject=${enc(`${m.subject || "Feedback"}: ${titleOf(report)}`)}&body=${enc(body)}`; };
  async function sendEmail(report) {
    let body = plain(markdown(report));
    if (body.length > 1800) { await copyText(body); body = `${clip(plain(markdown(report, true)), 1600)}\n\n(The full report is on your clipboard.)`; }
    openUrl(mailtoUrl(report, body));
    return t("opened");
  }

  async function sendEndpoint(report) {
    const ep = config.send.endpoint;
    const res = await fetch(ep.url, { method: "POST", credentials: "omit", referrerPolicy: "no-referrer",
      headers: { "Content-Type": "application/json", ...(ep.headers || {}) },
      body: JSON.stringify({ title: titleOf(report), markdown: markdown(report), report }) });
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    return t("sent");
  }

  function download(report) {
    const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: "application/json" }));
    const a = h("a", { href: url, download: `feedback-${report.created.slice(0, 10)}.json` });
    wrap.append(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  // QR code ----------------------------------------------------------------------------------------------
  // A QR code of the GitHub or email address, for a computer with no internet: the phone opens the report.
  // Byte mode (UTF-8), versions 1 to 40, level M when it fits in the same size as L, Reed-Solomon, and the
  // mask with the lowest penalty score (ISO/IEC 18004).

  // Error correction codewords per block, and blocks, for versions 1 to 40, at levels L and M.
  const QR_EC = { L: [7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    M: [10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28] };
  const QR_BLOCKS = { L: [1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
    M: [1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49] };
  // The longest address put in a code, in bytes. 1,200 bytes is a version 25 code at level L, 117 modules
  // square, which a phone reads from a laptop screen. Longer reports are shortened first.
  const QR_LIMIT = 1200;
  const gfMul = (x, y) => { let z = 0; for (let i = 7; i >= 0; i--) { z = (z << 1) ^ ((z >>> 7) * 0x11d); z ^= ((y >>> i) & 1) * x; } return z; };
  const qrRaw = (v) => { let n = (16 * v + 128) * v + 64; if (v >= 2) { const a = Math.floor(v / 7) + 2; n -= (25 * a - 10) * a - 55; if (v >= 7) n -= 36; } return n; };
  const qrData = (v, l) => Math.floor(qrRaw(v) / 8) - QR_EC[l][v - 1] * QR_BLOCKS[l][v - 1];

  // qr(text) returns { size, dark: rows of true for a dark module, version, level, mask }.
  function qr(text, forceMask) {
    const bytes = [...new TextEncoder().encode(text)];
    const fits = (v, l) => 4 + (v < 10 ? 8 : 16) + 8 * bytes.length <= qrData(v, l) * 8;
    let v = 1;
    while (v <= 40 && !fits(v, "L")) v++;
    if (v > 40) throw new Error("Too long for a QR code");
    const level = fits(v, "M") ? "M" : "L";
    // The data: mode 0100 (bytes), the length, the bytes, a terminator, then the pad bytes.
    const bits = [];
    const put = (val, n) => { for (let i = n - 1; i >= 0; i--) bits.push((val >>> i) & 1); };
    put(4, 4); put(bytes.length, v < 10 ? 8 : 16); bytes.forEach((b) => put(b, 8));
    const capacity = qrData(v, level) * 8;
    put(0, Math.min(4, capacity - bits.length)); put(0, (8 - (bits.length % 8)) % 8);
    for (let pad = 0xec; bits.length < capacity; pad ^= 0xec ^ 0x11) put(pad, 8);
    const data = [];
    for (let i = 0; i < bits.length; i += 8) data.push(parseInt(bits.slice(i, i + 8).join(""), 2));
    // Reed-Solomon error correction for each block, then the blocks interleaved.
    const blocks = QR_BLOCKS[level][v - 1], ecLen = QR_EC[level][v - 1], raw = Math.floor(qrRaw(v) / 8);
    const short = blocks - (raw % blocks), shortLen = Math.floor(raw / blocks);
    const div = Array(ecLen).fill(0); div[ecLen - 1] = 1;
    for (let i = 0, root = 1; i < ecLen; i++, root = gfMul(root, 2)) for (let j = 0; j < ecLen; j++) div[j] = gfMul(div[j], root) ^ (div[j + 1] || 0);
    const all = [];
    for (let i = 0, k = 0; i < blocks; i++) {
      const block = data.slice(k, k += shortLen - ecLen + (i < short ? 0 : 1));
      const ec = Array(ecLen).fill(0);
      for (const b of block) { const f = b ^ ec.shift(); ec.push(0); div.forEach((c, j) => { ec[j] ^= gfMul(c, f); }); }
      if (i < short) block.push(0);
      all.push(block.concat(ec));
    }
    const words = [];
    for (let i = 0; i < all[0].length; i++) all.forEach((b, j) => { if (i !== shortLen - ecLen || j >= short) words.push(b[i]); });
    // The finder, timing and alignment patterns, and the format and version information.
    const size = v * 4 + 17;
    const dark = Array.from({ length: size }, () => Array(size).fill(false));
    const fixed = dark.map((r) => r.slice());
    const set = (x, y, on) => { if (x >= 0 && y >= 0 && x < size && y < size) { dark[y][x] = on; fixed[y][x] = true; } };
    for (let i = 0; i < size; i++) { set(6, i, i % 2 === 0); set(i, 6, i % 2 === 0); }
    for (const [cx, cy] of [[3, 3], [size - 4, 3], [3, size - 4]]) {
      for (let dy = -4; dy <= 4; dy++) for (let dx = -4; dx <= 4; dx++) { const d = Math.max(Math.abs(dx), Math.abs(dy)); set(cx + dx, cy + dy, d !== 2 && d !== 4); }
    }
    const pos = [];
    if (v > 1) {
      const n = Math.floor(v / 7) + 2, step = v === 32 ? 26 : Math.ceil((v * 4 + 4) / (n * 2 - 2)) * 2;
      pos.push(6);
      for (let p = size - 7; pos.length < n; p -= step) pos.splice(1, 0, p);
    }
    pos.forEach((x, i) => pos.forEach((y, j) => {
      if ((i === 0 && j === 0) || (i === 0 && j === pos.length - 1) || (i === pos.length - 1 && j === 0)) return;
      for (let dy = -2; dy <= 2; dy++) for (let dx = -2; dx <= 2; dx++) set(x + dx, y + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
    }));
    const format = (mask) => {
      const d = ((level === "L" ? 1 : 0) << 3) | mask;
      let r = d;
      for (let i = 0; i < 10; i++) r = (r << 1) ^ ((r >>> 9) * 0x537);
      const f = ((d << 10) | r) ^ 0x5412, bit = (i) => ((f >>> i) & 1) === 1;
      for (let i = 0; i < 6; i++) set(8, i, bit(i));
      set(8, 7, bit(6)); set(8, 8, bit(7)); set(7, 8, bit(8));
      for (let i = 9; i < 15; i++) set(14 - i, 8, bit(i));
      for (let i = 0; i < 8; i++) set(size - 1 - i, 8, bit(i));
      for (let i = 8; i < 15; i++) set(8, size - 15 + i, bit(i));
      set(8, size - 8, true);
    };
    format(0);
    if (v >= 7) {
      let r = v;
      for (let i = 0; i < 12; i++) r = (r << 1) ^ ((r >>> 11) * 0x1f25);
      const f = (v << 12) | r;
      for (let i = 0; i < 18; i++) { const on = ((f >>> i) & 1) === 1, a = size - 11 + (i % 3), b = Math.floor(i / 3); set(a, b, on); set(b, a, on); }
    }
    // The codewords, in a zigzag from the bottom right.
    let n = 0;
    for (let right = size - 1; right >= 1; right -= 2) {
      if (right === 6) right = 5;
      for (let vert = 0; vert < size; vert++) for (let j = 0; j < 2; j++) {
        const x = right - j, y = ((right + 1) & 2) === 0 ? size - 1 - vert : vert;
        if (!fixed[y][x] && n < words.length * 8) { dark[y][x] = ((words[n >>> 3] >>> (7 - (n & 7))) & 1) === 1; n++; }
      }
    }
    // Try each mask and keep the one with the lowest penalty score.
    const MASKS = [(x, y) => (x + y) % 2, (x, y) => y % 2, (x) => x % 3, (x, y) => (x + y) % 3, (x, y) => (Math.floor(x / 3) + Math.floor(y / 2)) % 2,
      (x, y) => (x * y) % 2 + (x * y) % 3, (x, y) => ((x * y) % 2 + (x * y) % 3) % 2, (x, y) => ((x + y) % 2 + (x * y) % 3) % 2];
    const flip = (m) => { for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) if (!fixed[y][x] && MASKS[m](x, y) === 0) dark[y][x] = !dark[y][x]; };
    const penalty = () => {
      let score = 0, count = 0;
      for (const line of [...dark, ...dark.map((_, x) => dark.map((r) => r[x]))]) {
        for (let i = 0, run = 1; i < size; i++, run++) if (i === size - 1 || line[i] !== line[i + 1]) { if (run >= 5) score += run - 2; run = 0; }
        const s = line.map(Number).join("");
        for (let i = s.indexOf("1011101"); i >= 0; i = s.indexOf("1011101", i + 1)) score += ((i >= 4 && s.slice(i - 4, i) === "0000") + (s.slice(i + 7, i + 11) === "0000")) * 40;
      }
      for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
        if (dark[y][x]) count++;
        if (x && y && dark[y][x] === dark[y - 1][x] && dark[y][x] === dark[y][x - 1] && dark[y][x] === dark[y - 1][x - 1]) score += 3;
      }
      return score + Math.floor(Math.abs((count * 100) / (size * size) - 50) / 5) * 10;
    };
    let best = forceMask ?? 0;
    if (forceMask == null) {
      let lowest = Infinity;
      for (let m = 0; m < 8; m++) { flip(m); format(m); const p = penalty(); if (p < lowest) { lowest = p; best = m; } flip(m); }
    }
    flip(best); format(best);
    return { size, dark, version: v, level, mask: best };
  }
  // End of the QR code encoder.

  // The code as an SVG picture: dark modules on white, with a quiet zone of 4 modules, in any theme.
  function qrSvg(code, label) {
    const ns = "http://www.w3.org/2000/svg", q = 4, w = code.size + q * 2;
    const svg = document.createElementNS(ns, "svg");
    for (const [k, v] of Object.entries({ viewBox: `0 0 ${w} ${w}`, role: "img", "aria-label": label, "shape-rendering": "crispEdges", class: "sl-qr__code" })) svg.setAttribute(k, v);
    let d = "";
    code.dark.forEach((row, y) => {
      for (let x = 0; x < code.size; x++) {
        if (!row[x]) continue;
        let end = x;
        while (end < code.size && row[end]) end++;
        d += `M${x + q} ${y + q}h${end - x}v1h${x - end}z`;
        x = end;
      }
    });
    const back = document.createElementNS(ns, "rect"), path = document.createElementNS(ns, "path");
    for (const [k, v] of Object.entries({ width: w, height: w, fill: "#fff" })) back.setAttribute(k, v);
    path.setAttribute("d", d); path.setAttribute("fill", "#000");
    svg.append(back, path);
    return svg;
  }

  // The same address the GitHub or email button opens, kept to QR_LIMIT bytes: the whole report when it fits;
  // if not, the summary and the person's own words without the technical details, cut to fit if need be.
  function qrAddress(report) {
    const make = config.send.github?.repo ? (md) => `${githubBase(report)}&body=${enc(md)}` : (md) => mailtoUrl(report, plain(md));
    const full = make(markdown(report));
    if (full.length <= QR_LIMIT) return { url: full, short: false };
    // The note is added after any cut, so it is always there.
    const note = "\n\n_The technical details were left out to fit in a QR code. The person reporting can download the full report._";
    let md = markdown(report, "qr");
    while (make(md + note).length > QR_LIMIT && md.length > 40) md = clip(md, Math.floor(md.length * 0.9));
    return { url: make(md + note), short: true };
  }

  // Interface --------------------------------------------------------------------------------------------

  function say(message) {
    const region = wrap.querySelector(".sl-status");
    region.textContent = "";
    requestAnimationFrame(() => { region.textContent = message; });
    region.hidden = false;
    clearTimeout(say.timer);
    say.timer = setTimeout(() => { region.hidden = true; }, 6000);
  }

  function dialog(title, body, foot) {
    const id = `sl-${Math.random().toString(36).slice(2)}`;
    const d = h("dialog", { class: "sl-dialog", "aria-labelledby": id },
      h("div", { class: "sl-head" }, h("h2", { id, text: title }),
        h("button", { type: "button", class: "sl-icon", "aria-label": `${t("close")}: ${title}`, onclick: () => d.close() }, icon("x"))),
      h("div", { class: "sl-body sl-scroll" }, body),
      h("div", { class: "sl-foot" }, foot));
    d.addEventListener("close", () => { d.remove(); if (!wrap.querySelector("dialog[open]")) restoreFocus(); });
    wrap.append(d);
    d.showModal();
    return d;
  }

  function restoreFocus() {
    const target = lastFocus && lastFocus.isConnected ? lastFocus : launcher;
    target?.focus?.();
  }

  function startPicking(values = {}) {
    if (picking || !config) return;
    if (!host.contains(document.activeElement) && document.activeElement !== host) lastFocus = document.activeElement;
    if (launcher) launcher.hidden = true;
    const outline = h("div", { class: "sl-outline", hidden: true });
    const whole = h("button", { type: "button", class: "sl-btn", text: t("wholePage"), onclick: () => finishPicking(document.body) });
    const stopBtn = h("button", { type: "button", class: "sl-btn", text: t("stop"), onclick: () => endPicking(true) });
    const banner = h("div", { class: "sl-banner", role: "region", "aria-label": t("formTitle") },
      h("p", { text: t("pickPrompt") }), h("div", { class: "sl-actions" }, whole, stopBtn));
    wrap.append(outline, banner);
    let current = null;
    const show = (target) => {
      if (!(target instanceof Element) || target === host) { outline.hidden = true; current = null; return; }
      current = target;
      const r = target.getBoundingClientRect();
      Object.assign(outline.style, { top: `${r.top}px`, left: `${r.left}px`, width: `${r.width}px`, height: `${r.height}px` });
      outline.hidden = false;
    };
    const block = (e) => { if (!ours(e)) { e.preventDefault(); e.stopImmediatePropagation(); } };
    const listeners = {
      pointermove: (e) => { if (!ours(e)) show(e.target); },
      pointerdown: block, mousedown: block, pointerup: block, mouseup: block, touchstart: block,
      click: (e) => { if (ours(e)) return; block(e); finishPicking(e.target); },
      focusin: (e) => { if (!ours(e)) show(e.target); },
      keydown: (e) => {
        if (e.key === "Escape") { e.preventDefault(); endPicking(true); return; }
        if ((e.key === "Enter" || e.key === " ") && !ours(e) && document.activeElement && document.activeElement !== document.body) {
          block(e); finishPicking(document.activeElement);
        }
      },
      scroll: () => current && show(current),
    };
    for (const [type, fn] of Object.entries(listeners)) window.addEventListener(type, fn, { capture: true, passive: false });
    picking = { outline, banner, listeners, values };
    whole.focus();
  }

  function endPicking(refocus) {
    if (!picking) return;
    for (const [type, fn] of Object.entries(picking.listeners)) window.removeEventListener(type, fn, { capture: true });
    picking.outline.remove();
    picking.banner.remove();
    picking = null;
    if (launcher) launcher.hidden = false;
    if (refocus) restoreFocus();
  }

  function finishPicking(target) {
    const values = picking?.values || {};
    endPicking(false);
    openForm(describe(target), values);
  }

  // First question: what would you like to send. Problems usually start by choosing a part of the page.
  function openChooser(part = null, previous = {}) {
    if (!config) return;
    if (document.activeElement && !host.contains(document.activeElement) && document.activeElement !== host) lastFocus = document.activeElement;
    const all = types();
    const go = (key, show) => {
      const values = { ...previous, type: key };
      if (key === "check") openCheck();
      else if (values.steps?.length) openForm(describe(document.body), values);
      else if (show === "steps") startRecorder(values);
      else if (show === "one" && !part) startPicking(values);
      else openForm(show === "none" ? describe(document.body) : part || describe(document.body), values);
    };
    const defaultShow = (key) => (part && !part.whole ? "one" : typeOf(key).pick ? "one" : "none");
    const first = previous.type && typeOf(previous.type) ? previous.type : all[0][0];
    const groups = GROUPS.map(([g, legend]) => [tr(`groups.${g}`, legend), all.filter(([, v]) => (v.group || "problem") === g)]).filter(([, items]) => items.length);
    const typeGroups = all.length > 1 ? groups.map(([legend, items]) => choiceGroup("type", items.map(([k, v]) => [k, v.label, v.hint]), first, legend, "cards")) : [];
    const showGroup = choiceGroup("show", [["one", t("showOne"), t("showOneHint")], ["steps", t("showSteps"), t("showStepsHint")], ["none", t("showNone"), t("showNoneHint")]],
      defaultShow(first), t("show"), "cards");
    const form = h("form", { id: "sl-choose", class: "sl-form", novalidate: true }, ...typeGroups, previous.steps?.length ? null : showGroup);
    for (const input of form.querySelectorAll("[name=type]")) input.checked = input.value === first;
    showGroup.hidden = first === "check";
    let touched = false;
    form.addEventListener("change", (e) => {
      if (e.target.name === "show") touched = true;
      if (e.target.name === "type" && !touched) for (const r of form.querySelectorAll("[name=show]")) r.checked = r.value === defaultShow(e.target.value);
      if (e.target.name === "type") showGroup.hidden = e.target.value === "check";
    });
    const next = h("button", { type: "submit", form: "sl-choose", class: "sl-btn sl-btn--primary", text: t("chooseNext") });
    const d = dialog(t("chooseTitle"), form, [next, h("button", { type: "button", class: "sl-btn", text: t("cancel"), onclick: () => d.close() })]);
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const key = form.querySelector("[name=type]:checked")?.value || all[0][0];
      const show = form.querySelector("[name=show]:checked")?.value || "none";
      d.close();
      go(key, show);
    });
    form.querySelector("input:checked").focus();
  }

  function choiceGroup(name, items, checked, legend, style) {
    return h("fieldset", { class: `sl-choices${style ? ` sl-choices--${style}` : ""}` }, h("legend", { text: legend }),
      items.map(([value, text, hint], i) => h("label", { class: "sl-choice" },
        h("input", { type: "radio", name, value, checked: value === checked || (!checked && i === 0) }),
        h("span", {}, h("span", { class: "sl-choice__name", text }), hint ? h("span", { class: "sl-muted", text: hint }) : null))));
  }

  function fieldFor(f, value) {
    const id = `sl-f-${f.id}`;
    const hintId = f.hint ? `${id}-hint` : null;
    const control = f.type === "select"
      ? h("select", { id, name: f.id }, f.options.map(([v, text]) => h("option", { value: v, text, selected: v === value })))
      : h(f.type === "text" ? "input" : "textarea", { id, name: f.id, rows: f.type === "text" ? null : 3, type: f.type === "text" ? "text" : null,
        required: !!f.required, "aria-describedby": hintId });
    if (f.type !== "select" && value) control.value = value;
    const error = f.required ? h("p", { class: "sl-error", id: `${id}-error`, hidden: true, text: t("fieldError") }) : null;
    return h("div", { class: "sl-field" }, h("label", { class: "sl-label", for: id, text: f.label }),
      f.hint ? h("p", { class: "sl-muted", id: hintId, text: f.hint }) : null, error, control);
  }

  function openForm(part, previous = {}) {
    const all = types();
    const typeKey = previous.type && typeOf(previous.type) ? previous.type : all[0][0];
    const type = typeOf(typeKey);
    const cats = type.group === "problem" ? list(config.categories, CATEGORIES) : null;
    const scale = scaleOf(type);
    const recorded = previous.steps?.length ? previous.steps : null;
    const fields = h("div", { class: "sl-form" }, type.fields.filter((f) => !(f.unlessRecorded && recorded)).map((f) => fieldFor(f, previous[f.id])));
    const kept = () => ({ ...previous, ...Object.fromEntries([...form.querySelectorAll("[name]")].filter((c) => c.type !== "radio" || c.checked).map((c) => [c.name, c.value])), type: typeKey });
    const category = cats ? h("select", { id: "sl-category", name: "category" },
      cats.map(([value, text, hint]) => h("option", { value, text: hint ? `${text}: ${hint}` : text, selected: value === previous.category }))) : null;
    const where = recorded ? t("showSteps") : part.label;
    const summary = h("div", { class: "sl-summary" },
      h("p", {}, h("strong", { text: type.label }), h("span", { text: ` · ${where}` })),
      part.text && !part.whole ? h("p", { class: "sl-muted", text: `"${clip(part.text, 120)}"` }) : null,
      h("div", { class: "sl-actions" },
        all.length > 1 ? h("button", { type: "button", class: "sl-link", text: t("change"), onclick: () => { const v = kept(); d.close(); openChooser(recorded ? null : part, v); } }) : null,
        recorded ? null : h("button", { type: "button", class: "sl-link", text: part.whole ? t("choosePart") : t("chooseAgain"), onclick: () => { const v = kept(); d.close(); startPicking(v); } })),
    );
    const stepsBox = recorded ? h("details", { class: "sl-disclosure" },
      h("summary", {}, h("span", { text: t("recSteps") }), h("span", { class: "sl-disclosure__meta", text: `${recorded.length} ${recorded.length === 1 ? "step" : "steps"}` })),
      h("ol", { class: "sl-disclosure__body sl-steps" }, recorded.map((s) => h("li", { text: s.text })))) : null;
    const form = h("form", { id: "sl-form", class: "sl-form", novalidate: true },
      summary, stepsBox, fields,
      category ? h("div", { class: "sl-field" }, h("label", { class: "sl-label", for: "sl-category", text: t("category") }), category) : null,
      scale ? choiceGroup("severity", scale.items.map(([v, l]) => [v, l]), previous.severity || scale.items[Math.min(1, scale.items.length - 1)][0], scale.question, "row") : null);
    const next = h("button", { type: "submit", form: "sl-form", class: "sl-btn sl-btn--primary", text: t("review") });
    const d = dialog(t("formTitle"), form, [next, h("button", { type: "button", class: "sl-btn", text: t("cancel"), onclick: () => d.close() })]);
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      let firstEmpty = null;
      for (const control of fields.querySelectorAll("[required]")) {
        const empty = !control.value.trim();
        const error = form.querySelector(`#${control.id}-error`);
        error.hidden = !empty;
        control.toggleAttribute("aria-invalid", empty);
        control.setAttribute("aria-describedby", [empty && error.id, control.id + "-hint"].filter((x) => x && form.querySelector(`#${x}`)).join(" "));
        if (empty && !firstEmpty) firstEmpty = control;
      }
      if (firstEmpty) { firstEmpty.focus(); return; }
      const values = Object.fromEntries([...fields.querySelectorAll("[name]")].map((c) => [c.name, c.value.trim()]));
      values.type = typeKey;
      if (recorded) values.steps = recorded;
      if (scale) values.severity = form.severity.value;
      if (category) values.category = category.value;
      d.close();
      openReview(part, values);
    });
    (fields.querySelector("textarea, input, select") || form.querySelector("input, button")).focus();
  }

  function openReview(part, values) {
    const full = buildReport(part, values, values.type);
    let report = full;
    const removed = new Set();
    const preview = h("pre", { class: "sl-preview sl-scroll", tabindex: "0", "aria-label": t("reviewTitle") });
    const draw = () => { preview.textContent = markdown(report); };
    const optional = PARTS.filter(([key]) => full[key] !== undefined).map(([key, text]) => [key, tr(`parts.${key}`, text)]);
    const toggles = optional.map(([key, text]) =>
      h("label", { class: "sl-check" }, h("input", { type: "checkbox", checked: true, onchange: (e) => {
        e.target.checked ? removed.delete(key) : removed.add(key);
        report = { ...full };
        for (const k of removed) delete report[k];
        draw();
      } }), h("span", { text })));
    draw();
    const s = config.send;
    const notes = [s.github && t("githubNote"), s.email && t("emailNote"), s.endpoint && t("endpointNote")].filter(Boolean);
    // A QR code needs an address a phone can open: GitHub or email, not the site's own server.
    const qrOn = s.qr && !!(s.github?.repo || s.email?.to);
    const offline = qrOn && navigator.onLine === false;
    const body = [
      offline ? h("p", { class: "sl-privacy", text: t("qrOffline") }) : null,
      h("p", { class: "sl-privacy", text: t("privacy") }),
      toggles.length ? h("fieldset", { class: "sl-checks" }, h("legend", { text: t("include") }), toggles) : null,
      preview,
      ...notes.map((n) => h("p", { class: "sl-muted", text: n })),
    ];
    const run = async (button, fn) => {
      button.disabled = true;
      try { const message = await fn(report); if (message) say(message); d.close(); }
      catch (err) { say(`${t("sendFailed")}: ${err.message || err}. ${t("sendFailedSafe")}`); button.disabled = false; }
    };
    // The first way of sending is the primary button; the rest are secondary.
    const buttons = [];
    const add = (text, fn) => {
      const b = h("button", { type: "button", class: buttons.length ? "sl-btn" : "sl-btn sl-btn--primary", text });
      b.addEventListener("click", () => run(b, fn));
      buttons.push(b);
    };
    const addQr = () => {
      const b = h("button", { type: "button", class: buttons.length ? "sl-btn" : "sl-btn sl-btn--primary", text: t("sendQr"), onclick: () => openQr(report, b) });
      buttons.push(b);
    };
    if (offline) addQr();
    if (s.github?.repo) add(t("sendGithub"), sendGithub);
    if (s.email?.to) add(t("sendEmail"), sendEmail);
    if (s.endpoint?.url) add(t("sendEndpoint"), sendEndpoint);
    if (qrOn && !offline) addQr();
    if (s.copy) add(t("copy"), async (r) => ((await copyText(markdown(r))) ? t("copied") : null));
    if (s.download) add(t("download"), async (r) => { download(r); return null; });
    buttons.push(h("button", { type: "button", class: "sl-btn", text: t("back"), onclick: () => { d.close(); openForm(part, values); } }));
    const d = dialog(t("reviewTitle"), body, buttons);
    buttons[0].focus();
  }

  // The report as a QR code, with the same link as text and a Copy button, so the code is never the only way.
  function openQr(report, opener) {
    const s = config.send;
    const { url, short } = qrAddress(report);
    const label = s.github?.repo ? t("qrAltGithub").replace("{repo}", s.github.repo) : t("qrAltEmail").replace("{to}", s.email.to);
    const link = h("textarea", { id: "sl-qr-link", class: "sl-qr__link", readonly: true, rows: 4, spellcheck: "false" });
    link.value = url;
    const copy = h("button", { type: "button", class: "sl-btn sl-btn--primary", text: t("qrCopy"),
      onclick: async () => { if (await copyText(url)) say(t("qrCopied")); } });
    const body = h("div", { class: "sl-qr" },
      h("div", { class: "sl-qr__frame" }, qrSvg(qr(url), label)),
      h("div", { class: "sl-form" },
        h("p", { class: "sl-qr__lead", text: t("qrScan") }),
        h("p", { class: "sl-muted", text: s.github?.repo ? t("qrGithub") : t("qrEmail") }),
        short ? h("p", { class: "sl-privacy", text: [t("qrShort"), s.download ? t("qrDownload") : ""].filter(Boolean).join(" ") }) : null,
        h("div", { class: "sl-field" }, h("label", { class: "sl-label", for: "sl-qr-link", text: t("qrLink") }), link)));
    const foot = [copy,
      s.download ? h("button", { type: "button", class: "sl-btn", text: t("download"), onclick: () => download(report) }) : null,
      h("button", { type: "button", class: "sl-btn", text: t("back"), onclick: () => d.close() })];
    const d = dialog(t("sendQr"), body, foot);
    d.addEventListener("close", () => { if (opener.isConnected) opener.focus(); });
    copy.focus();
  }

  // Automatic accessibility check ---------------------------------------------------------------------------
  // Built-in rules for the most common problems, each with its WCAG criterion, why it matters and how to fix it.
  // With check.engine "axe", Snaglist loads axe-core (from check.axe) and reports what it finds instead.

  const visible = (el) => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none"; };
  const outside = (el) => el === host || host.contains(el) || !!el.closest("snag-list, [aria-hidden='true'], [hidden], template");
  const named = (el) => !!(el.getAttribute("aria-label")?.trim() || el.getAttribute("aria-labelledby") || el.getAttribute("title")?.trim() ||
    el.textContent.trim() || el.querySelector("img[alt]:not([alt=''])") || (el.tagName === "INPUT" && el.value));
  const all = (sel) => [...document.querySelectorAll(sel)].filter((el) => !outside(el));

  const RULES = [
    { id: "image-alt", name: "Pictures with no text alternative", wcag: "1.1.1 Non-text content", level: "A", impact: "serious",
      why: "Screen reader users hear nothing, or the file name, instead of what the picture shows.",
      fix: "Add alt text that says what the picture shows. For a picture that is only decoration, use an empty alt.",
      before: '<img src="cover.jpg">', after: '<img src="cover.jpg" alt="Cover of The Glass Harbour">\n<img src="divider.svg" alt="">',
      find: () => all("img:not([alt]), [role=img]:not([aria-label]):not([aria-labelledby])").filter((el) => el.getAttribute("role") !== "presentation") },
    { id: "control-name", name: "Buttons and links with no name", wcag: "4.1.2 Name, role, value", level: "A", impact: "critical",
      why: "Screen readers announce only \"button\" or \"link\", so people cannot tell what it does.",
      fix: "Give every button and link a name: visible words, or an aria-label when it shows only an icon.",
      before: '<button><svg>…</svg></button>', after: '<button aria-label="Close the menu"><svg aria-hidden="true">…</svg></button>',
      find: () => all("button, a[href], [role=button], [role=link], [role=menuitem], [role=tab]").filter((el) => visible(el) && !named(el)) },
    { id: "field-label", name: "Fields with no label", wcag: "1.3.1 Info and relationships, 3.3.2 Labels or instructions", level: "A", impact: "critical",
      why: "People using a screen reader or voice control cannot tell what to type, and a placeholder disappears as soon as they type.",
      fix: "Give every field a visible label joined to it with for and id.",
      before: '<input type="email" placeholder="Email">', after: '<label for="email">Email</label>\n<input type="email" id="email" autocomplete="email">',
      find: () => all("input:not([type=hidden]):not([type=submit]):not([type=button]):not([type=reset]):not([type=image]), select, textarea")
        .filter((el) => visible(el) && !(el.labels && el.labels.length) && !el.getAttribute("aria-label")?.trim() && !el.getAttribute("aria-labelledby") && !el.getAttribute("title")) },
    { id: "contrast", name: "Text with too little contrast", wcag: "1.4.3 Contrast (minimum)", level: "AA", impact: "serious",
      why: "Faint text is hard or impossible to read for people with low vision, and for everyone in bright light.",
      fix: "Make text at least 4.5 to 1 against its background, or 3 to 1 for large text (24px, or 18.66px bold). Use a darker text colour or a stronger background.",
      before: '.summary { color: #9aa3b0; }  /* 2.4:1 on white */', after: '.summary { color: #424b5b; }  /* 8.9:1 on white */',
      find: () => {
        const out = [];
        const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT, { acceptNode: (n) => (n.textContent.trim().length > 1 ? NodeFilter.FILTER_ACCEPT : NodeFilter.FILTER_SKIP) });
        const seen = new Set();
        for (let n = walker.nextNode(), i = 0; n && i < config.check.contrastLimit; n = walker.nextNode()) {
          const el = n.parentElement;
          if (!el || seen.has(el) || outside(el) || !visible(el)) continue;
          seen.add(el); i++;
          const s = stylesOf(el);
          const ratio = parseFloat(s.contrast);
          if (s.contrast && /\bfails\b/.test(s.contrast) && ratio > 0) out.push(el);
        }
        return out;
      } },
    { id: "target-size", name: "Controls smaller than 24 by 24 pixels", wcag: "2.5.8 Target size (minimum)", level: "AA", impact: "moderate",
      why: "Small targets are hard to hit for people with tremors, on touch screens, and for anyone in a hurry.",
      fix: "Make buttons, links in menus and other controls at least 24 by 24 pixels (44 is better), or leave space around them.",
      before: '<button class="icon">⚙</button>  /* 18 × 18 */', after: '.icon { min-width: 44px; min-height: 44px; }',
      find: () => all("button, [role=button], input[type=checkbox], input[type=radio], select, a[href]")
        .filter((el) => { if (!visible(el)) return false;
          if (el.tagName === "A" && getComputedStyle(el).display === "inline" && el.closest("p, li, td, dd")) return false; // a link inside a sentence is exempt
          // A tick box or radio button inside, or joined to, a big enough label is selected through the label.
          const target = (/^(checkbox|radio)$/.test(el.type) && el.labels?.[0]) || el;
          const r = target.getBoundingClientRect();
          return r.width < 24 || r.height < 24; }) },
    { id: "headings", name: "Headings out of order", wcag: "1.3.1 Info and relationships, 2.4.6 Headings and labels", level: "AA", impact: "moderate",
      why: "Screen reader users move around a page by its headings. A missing main heading or a skipped level makes the page hard to follow.",
      fix: "Give each page one h1, then go down one level at a time: h2 for sections, h3 inside them. Style headings with CSS, not by choosing the level.",
      before: '<h1>My books</h1>\n<h4>Following</h4>', after: '<h1>My books</h1>\n<h2>Following</h2>',
      find: () => {
        const hs = all("h1, h2, h3, h4, h5, h6, [role=heading]").filter(visible);
        const out = [];
        if (!hs.some((x) => x.tagName === "H1" || x.getAttribute("aria-level") === "1")) out.push(document.body);
        let last = 0;
        for (const x of hs) { const lvl = +(x.getAttribute("aria-level") || x.tagName[1] || 2); if (last && lvl > last + 1) out.push(x); last = lvl; }
        return out;
      } },
    { id: "page-language", name: "Page language not set", wcag: "3.1.1 Language of page", level: "A", impact: "serious",
      why: "Screen readers choose their voice and pronunciation from the page's language. Without it, words can be read out wrongly.",
      fix: "Set the lang attribute on the html element to the page's main language.",
      before: '<html>', after: '<html lang="en-GB">',
      find: () => (document.documentElement.lang.trim() ? [] : [document.documentElement]) },
    { id: "page-title", name: "Page has no title", wcag: "2.4.2 Page titled", level: "A", impact: "serious",
      why: "The title is the first thing a screen reader says and the name of the browser tab.",
      fix: "Give every page a title that says what it is, then the site's name.",
      before: '<title></title>', after: '<title>My books · Harbour Library</title>',
      find: () => (document.title.trim() ? [] : [document.documentElement]) },
    { id: "zoom", name: "Zoom is blocked", wcag: "1.4.4 Resize text", level: "AA", impact: "critical",
      why: "People with low vision zoom in on phones. Blocking it can make the site impossible to read.",
      fix: "Remove user-scalable=no and maximum-scale from the viewport tag.",
      before: '<meta name="viewport" content="width=device-width, user-scalable=no">', after: '<meta name="viewport" content="width=device-width, initial-scale=1">',
      find: () => all("meta[name=viewport]").filter((m) => /user-scalable\s*=\s*(no|0)|maximum-scale\s*=\s*(1(\.0)?|0\.\d)\b/i.test(m.content)) },
    { id: "tab-order", name: "Tab order changed by tabindex", wcag: "2.4.3 Focus order", level: "A", impact: "serious",
      why: "A tabindex above 0 makes keyboard focus jump around the page in an order that does not match what people see.",
      fix: "Use tabindex=\"0\" or none, and put elements in the order they should be reached.",
      before: '<button tabindex="3">Save</button>', after: '<button>Save</button>',
      find: () => all("[tabindex]").filter((el) => +el.getAttribute("tabindex") > 0) },
    { id: "hidden-focus", name: "Focusable things hidden from screen readers", wcag: "4.1.2 Name, role, value", level: "A", impact: "serious",
      why: "Keyboard users can reach them, but screen readers say nothing, so it feels like focus has disappeared.",
      fix: "Do not put aria-hidden on anything that can take focus. Use the inert attribute, or hide it completely.",
      before: '<div aria-hidden="true"><a href="/help">Help</a></div>', after: '<div inert><a href="/help">Help</a></div>',
      find: () => [...document.querySelectorAll("[aria-hidden='true'] a[href], [aria-hidden='true'] button, [aria-hidden='true'] input, [aria-hidden='true'] select, [aria-hidden='true'] textarea, [aria-hidden='true'] [tabindex]")]
        .filter((el) => !host.contains(el) && el.tabIndex >= 0 && !el.disabled && !el.closest("[inert]")) },
    { id: "link-text", name: "Links that do not say where they go", wcag: "2.4.4 Link purpose (in context)", level: "A", impact: "moderate",
      why: "Screen reader users often list a page's links. \"Click here\" and \"Read more\" say nothing on their own.",
      fix: "Make the link's words name where it goes.",
      before: 'To change your plan, <a href="/plan">click here</a>.', after: '<a href="/plan">Change your plan</a>.',
      find: () => all("a[href]").filter((el) => /^(click here|here|read more|more|link|this|learn more|details)\.?$/i.test(el.textContent.trim()) && !el.getAttribute("aria-label")) },
    { id: "duplicate-id", name: "The same id used twice", wcag: "4.1.2 Name, role, value", level: "A", impact: "moderate",
      why: "Labels, descriptions and skip links point at ids. When an id is used twice, they can point at the wrong thing.",
      fix: "Make every id unique on the page.",
      before: '<input id="name"> … <input id="name">', after: '<input id="first-name"> … <input id="last-name">',
      find: () => { const seen = new Map(); for (const el of all("[id]")) { const k = el.id; seen.set(k, [...(seen.get(k) || []), el]); }
        return [...seen.values()].filter((v) => v.length > 1).map((v) => v[1]); } },
  ];

  async function runCheck() {
    if (config.check.engine === "axe") {
      if (!window.axe) {
        await new Promise((resolve, reject) => {
          // The default copy is checked against its hash, so a changed file on the CDN never runs.
          const integrity = config.check.integrity || (config.check.axe === AXE.url ? AXE.integrity : "");
          const s = Object.assign(document.createElement("script"), { src: config.check.axe, crossOrigin: "anonymous", onload: resolve,
            onerror: () => reject(new Error("axe-core did not load")) });
          if (integrity) s.integrity = integrity;
          undo.push(() => { delete window.axe; }); // destroy() takes away the axe-core it loaded
          document.head.append(s);
          undo.push(() => s.remove());
        });
      }
      const result = await window.axe.run(document, { exclude: [["snag-list"]] });
      return result.violations.map((v) => ({
        id: v.id, name: v.help, wcag: v.tags.filter((x) => /^wcag\d{3,}$/.test(x)).map((x) => x.replace(/^wcag(\d)(\d)(\d+)$/, "$1.$2.$3")).join(", ") || "Best practice",
        level: v.tags.includes("wcag2aa") || v.tags.includes("wcag21aa") || v.tags.includes("wcag22aa") ? "AA" : v.tags.includes("wcag2a") || v.tags.includes("wcag21a") ? "A" : "",
        impact: v.impact || "", why: v.description, fix: `Read how to fix it: ${v.helpUrl}`, helpUrl: v.helpUrl,
        elements: v.nodes.map((n) => { try { return document.querySelector(n.target[0]); } catch { return null; } }).filter(Boolean),
      }));
    }
    return RULES.map((r) => {
      let elements = [];
      try { elements = r.find(); } catch { /* one rule failing must not stop the others */ }
      return { ...r, name: tr(`checks.${r.id}.name`, r.name), fix: tr(`checks.${r.id}.fix`, r.fix), why: spell(r.why), elements };
    }).filter((r) => r.elements.length);
  }

  function flash(el) {
    if (!el || el === document.documentElement || el === document.body) return;
    el.scrollIntoView({ block: "center", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
    const outline = h("div", { class: "sl-outline sl-outline--found" });
    wrap.append(outline);
    const place = () => { const r = el.getBoundingClientRect(); Object.assign(outline.style, { top: `${r.top}px`, left: `${r.left}px`, width: `${r.width}px`, height: `${r.height}px` }); };
    place();
    const timer = setInterval(place, 50);
    setTimeout(() => { clearInterval(timer); outline.remove(); }, 4000);
  }

  // A found problem becomes an accessibility report, with where, why and how to fix it.
  function checkPart(results) {
    return results.map((r) => ({
      rule: r.id, name: r.name, wcag: r.wcag, level: r.level || undefined, impact: r.impact || undefined, why: r.why, fix: r.fix,
      example: r.before ? { before: r.before, after: r.after } : undefined, help: r.helpUrl,
      count: r.elements.length,
      where: r.elements.slice(0, 10).map((el) => { const d = describe(el); return { selector: d.whole ? "the whole page" : d.selector, part: d.whole ? "" : d.label }; }),
    }));
  }

  async function openCheck() {
    const body = h("div", { class: "sl-form" }, h("p", { role: "status", text: t("checking") }));
    const close = h("button", { type: "button", class: "sl-btn", text: t("close"), onclick: () => d.close() });
    const d = dialog(t("checkTitle"), body, [close]);
    let results;
    try { results = await runCheck(); }
    catch (err) { body.replaceChildren(h("p", { class: "sl-error", role: "alert", text: `${t("checkFailed")}: ${err.message || err}. ${t("checkFailedNext")}` })); return; }
    if (!d.open) return;
    if (!results.length) { body.replaceChildren(h("p", { role: "status", text: t("checkNone") })); return; }
    const total = results.reduce((n, r) => n + r.elements.length, 0);
    const report = (picked) => { const values = { type: "check", check: checkPart(picked) }; d.close(); openReview(describe(document.body), values); };
    body.replaceChildren(
      h("p", { role: "status" }, h("strong", { text: `${total} ${t("checkFound")}` }), ` · ${results.length} ${results.length === 1 ? "rule" : "rules"}`),
      h("p", { class: "sl-muted", text: t("checkLimits") }),
      h("ul", { class: "sl-results" }, results.map((r) => h("li", { class: "sl-result" },
        h("details", { class: "sl-disclosure" },
          h("summary", {}, h("span", { class: "sl-result__name", text: r.name }), h("span", { class: "sl-disclosure__meta", text: `${r.elements.length} · WCAG ${r.wcag}${r.level ? ` (${r.level})` : ""}` })),
          h("p", {}, h("strong", { text: `${t("checkWhy")}: ` }), r.why),
          h("p", {}, h("strong", { text: `${t("checkFix")}: ` }), r.fix),
          r.before ? h("pre", { class: "sl-preview sl-preview--code", text: `<!-- Before -->\n${r.before}\n\n<!-- After -->\n${r.after}` }) : null,
          h("p", {}, h("strong", { text: `${t("checkWhere")}: ` }), r.elements.slice(0, 3).map((el) => describe(el).label).join("; ") + (r.elements.length > 3 ? " …" : "")),
          h("div", { class: "sl-actions" },
            h("button", { type: "button", class: "sl-btn", text: t("checkShow"), onclick: () => { d.close(); flash(r.elements[0]); } }),
            h("button", { type: "button", class: "sl-btn sl-btn--primary", text: t("checkReport"), onclick: () => report([r]) })))))));
    d.querySelector(".sl-foot").prepend(h("button", { type: "button", class: "sl-btn sl-btn--primary", text: t("checkReportAll"), onclick: () => report(results) }));
    d.querySelector("summary")?.focus();
  }

  // Recording a series of steps ---------------------------------------------------------------------------
  // The recorder notes what the person selects, changes and sends, page changes, errors and failed requests,
  // by component and label only, never what they type. It is kept in this tab (sessionStorage), so it carries
  // on across page loads, and cleared when it stops.

  const REC_KEY = `${KEY}recording`;
  const MAX_STEPS = 60;
  // A choice's own label is the answer given (a condition, a title, an address), so it is never kept: only
  // that a choice was made, and the name of the group or field it was made in.
  const isChoice = (el) => (el.tagName === "INPUT" && /^(checkbox|radio)$/.test(el.type)) || el.tagName === "OPTION" ||
    /^(option|radio|checkbox|switch|menuitemradio|menuitemcheckbox)$/.test(el.getAttribute("role") || "");
  function groupOf(el) {
    const g = el.closest("fieldset, [role=radiogroup], [role=listbox], [role=group], [role=menu]");
    if (!g) return "";
    const name = (g.tagName === "FIELDSET" && g.querySelector("legend")?.textContent) || g.getAttribute("aria-label") || "";
    return clip(redact(name.replace(/\s+/g, " ").trim()), 60);
  }
  const choiceStep = (el, where) => { const g = groupOf(el); return `Made a choice${g ? ` in "${g}"` : ""}${where}`; };

  function saveRec() {
    try { rec ? sessionStorage.setItem(REC_KEY, JSON.stringify({ status: rec.status, steps: rec.steps, values: rec.values, started: rec.started, path: location.pathname })) : sessionStorage.removeItem(REC_KEY); } catch { /* private window */ }
  }
  function recNote(text) {
    if (!rec || rec.status !== "recording") return;
    const last = rec.steps[rec.steps.length - 1];
    if (last && last.text === text && Date.now() - rec.started - last.at * 1000 < 800) return; // the same step twice in a row
    rec.steps.push({ at: Math.round((Date.now() - rec.started) / 100) / 10, text });
    if (rec.steps.length > MAX_STEPS) rec.steps.shift();
    saveRec();
    drawRec();
  }
  function stepFor(e) {
    if (!(e.target instanceof Element) || ours(e)) return null;
    if (e.type === "submit") return `Sent the form ${componentOf(e.target) || nameOf(e.target) || ""}`.trim();
    const el = e.type === "click" ? e.target.closest("a, button, summary, label, [role=button], [role=link], [role=tab], [role=menuitem], [role=option], [role=switch], [role=checkbox], input, select, textarea") || e.target : e.target;
    const d = describe(el);
    const what = [d.role, d.name && `"${d.name}"`].filter(Boolean).join(" ");
    const where = d.component ? ` in ${d.component}` : "";
    if (e.type === "click") {
      if (/^(INPUT|SELECT|TEXTAREA)$/.test(el.tagName) && !/^(radio|button|submit|reset)$/.test(el.type)) return null; // noted on change
      if (el.tagName === "LABEL" && el.control) return null; // the control's own click follows
      if (isChoice(el)) return el.type === "checkbox" ? null : choiceStep(el, where); // tick boxes are noted on change
      return `Selected ${what || "the page"}${where}`;
    }
    if (el.tagName === "SELECT") return `Made a choice in ${what}${where}`;
    if (el.type === "checkbox") return choiceStep(el, where);
    if (el.type === "radio" || isChoice(el)) return null; // the click already noted it
    if (/^(INPUT|TEXTAREA)$/.test(el.tagName) || el.isContentEditable) return `Entered text in ${what}${where} (the text is not kept)`;
    return null;
  }
  function listenRec() {
    const handler = (e) => { const s = stepFor(e); if (s) recNote(s); };
    for (const type of ["click", "change", "submit"]) on(window, type, handler);
    let lastPath = location.pathname;
    const timer = setInterval(() => { if (location.pathname !== lastPath) { lastPath = location.pathname; recNote(`Went to ${cleanPath(lastPath)}`); } }, 500);
    undo.push(() => clearInterval(timer));
  }

  const REC_ICON = (state) => {
    const ns = "http://www.w3.org/2000/svg";
    const s = document.createElementNS(ns, "svg");
    for (const [k, v] of Object.entries({ viewBox: "0 0 24 24", "aria-hidden": "true", focusable: "false" })) s.setAttribute(k, v);
    const shape = state === "pause" ? [["rect", { x: 6, y: 5, width: 4, height: 14 }], ["rect", { x: 14, y: 5, width: 4, height: 14 }]]
      : state === "stop" ? [["rect", { x: 6, y: 6, width: 12, height: 12 }]]
      : state === "again" ? [["path", { d: "M3 12a9 9 0 1 0 3-6.7L3 8", fill: "none", stroke: "currentColor", "stroke-width": 2, "stroke-linecap": "round" }], ["path", { d: "M3 3v5h5", fill: "none", stroke: "currentColor", "stroke-width": 2, "stroke-linecap": "round" }]]
      : [["circle", { cx: 12, cy: 12, r: 7 }]];
    for (const [tag, attrs] of shape) { const n = document.createElementNS(ns, tag); for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v); if (!attrs.fill) n.setAttribute("fill", "currentColor"); s.append(n); }
    return s;
  };

  function startRecorder(values, restored) {
    if (!config) return;
    rec?.bar?.remove();
    rec = restored || { status: "ready", steps: [], values, started: Date.now() };
    if (launcher) launcher.hidden = true;
    rec.bar = h("div", { class: "sl-recorder", role: "region", "aria-label": t("recRegion") });
    wrap.append(rec.bar);
    drawRec(true);
    saveRec();
  }

  function drawRec(focus) {
    if (!rec || !rec.bar) return;
    const btn = (text, iconName, fn, primary) => h("button", { type: "button", class: primary ? "sl-btn sl-btn--primary" : "sl-btn", onclick: fn }, REC_ICON(iconName), text);
    const n = rec.steps.length;
    const count = n ? `${n} ${n === 1 ? "step" : "steps"}` : t("recNone");
    const status = rec.status === "ready" ? t("recReady") : `${rec.status === "recording" ? t("recRecording") : t("recPaused")}: ${count}`;
    const actions = rec.status === "ready"
      ? [btn(t("recStart"), "record", () => { rec.status = "recording"; rec.started = Date.now(); saveRec(); drawRec(true); }, true),
        h("button", { type: "button", class: "sl-btn", text: t("cancel"), onclick: () => stopRecorder(false) })]
      : [rec.status === "recording"
          ? btn(t("recPause"), "pause", () => { rec.status = "paused"; saveRec(); drawRec(true); })
          : btn(t("recResume"), "record", () => { rec.status = "recording"; saveRec(); drawRec(true); }),
        btn(t("recStop"), "stop", () => stopRecorder(true), true),
        btn(t("recRestart"), "again", () => { rec.steps = []; rec.status = "recording"; rec.started = Date.now(); saveRec(); drawRec(true); }),
        h("button", { type: "button", class: "sl-btn", text: t("cancel"), onclick: () => stopRecorder(false) })];
    const last = rec.steps[n - 1];
    const focused = rec.bar.contains(root.activeElement) ? [...rec.bar.querySelectorAll("button")].indexOf(root.activeElement) : -1;
    rec.bar.replaceChildren(...[
      h("div", { class: `sl-recorder__status is-${rec.status}` }, h("span", { class: "sl-recorder__dot", "aria-hidden": "true" }),
        h("p", { role: "status", text: status })),
      last ? h("p", { class: "sl-muted sl-recorder__last", text: `${t("recLast")}: ${last.text}` }) : null,
      h("div", { class: "sl-actions" }, actions)].filter(Boolean));
    const buttons = rec.bar.querySelectorAll("button");
    if (focus) buttons[0].focus();
    else if (focused >= 0) buttons[Math.min(focused, buttons.length - 1)]?.focus();
  }

  function stopRecorder(describeIt) {
    if (!rec) return;
    const { steps, values } = rec;
    rec.bar?.remove();
    rec = null;
    saveRec();
    if (launcher) launcher.hidden = false;
    if (describeIt) openForm(describe(document.body), { ...values, steps: steps.length ? steps : undefined });
    else restoreFocus();
  }

  function restoreRecorder() {
    let saved = null;
    try { saved = JSON.parse(sessionStorage.getItem(REC_KEY)); } catch { /* none */ }
    if (!saved) return;
    startRecorder(saved.values, { status: saved.status, steps: saved.steps || [], values: saved.values, started: saved.started });
    if (saved.path !== location.pathname) recNote(`Went to ${cleanPath(location.pathname)}`);
  }

  // Styles, inside the shadow root only. Colours come from the page's design tokens when it has them. ----------

  const CSS_TEXT = `
:host { all: initial; }
* { box-sizing: border-box; }
.sl { --a: var(--color-accent, #0b5f5a); --at: var(--color-accent-text, #fff); --bg: var(--color-surface-raised, #fff);
  --tx: var(--color-text, #141416); --mu: var(--color-text-muted, #424b5b); --bd: var(--color-border, #7d879a);
  --sk: var(--color-surface-sunken, #eef1f6); --fo: var(--color-focus, var(--a)); --er: var(--status-danger-fg, #a1262a); --rec: #c4161c;
  --sb: var(--color-scrollbar, #8a94a6); --sbh: var(--color-scrollbar-hover, #5d6778);
  font: 1rem/1.5 var(--font, system-ui, -apple-system, "Segoe UI", sans-serif); color: var(--tx); }
.sl--dark { --sb: #6f7b91; --sbh: #a4aec1; --a: #5ed3c6; --at: #101827; --bg: #232f42; --tx: #f3f4f6; --mu: #a4aec1; --bd: #6f7b91; --sk: #0c1220; --er: #ff9b9b; --rec: #ff6b6b; }
@media (prefers-color-scheme: dark) { .sl--auto { --a: var(--color-accent, #5ed3c6); --at: var(--color-accent-text, #101827);
  --bg: var(--color-surface-raised, #232f42); --tx: var(--color-text, #f3f4f6); --mu: var(--color-text-muted, #a4aec1);
  --bd: var(--color-border, #6f7b91); --sk: var(--color-surface-sunken, #0c1220); --er: var(--status-danger-fg, #ff9b9b); --rec: #ff6b6b; } }
button, input, select, textarea { font: inherit; color: inherit; }
/* Scroll bars, as in the design system: thin, square, in the scroll bar colour on a clear track, and visible
   whenever there is more to scroll. A shadow at the edge says there is more, for systems that hide scroll bars. */
.sl, .sl * { scrollbar-width: thin; scrollbar-color: var(--sb) transparent; }
@supports not (scrollbar-color: auto) {
  .sl ::-webkit-scrollbar { width: 10px; height: 10px; }
  .sl ::-webkit-scrollbar-track { background: transparent; }
  .sl ::-webkit-scrollbar-thumb { border: 2px solid transparent; border-radius: 0; background: var(--sb) padding-box; }
  .sl ::-webkit-scrollbar-thumb:hover { background: var(--sbh) padding-box; }
}
.sl-scroll { scrollbar-gutter: stable;
  background: linear-gradient(var(--sbg, var(--bg)) 30%, transparent) center top / 100% 24px no-repeat local,
    linear-gradient(transparent, var(--sbg, var(--bg)) 70%) center bottom / 100% 24px no-repeat local,
    linear-gradient(rgb(0 0 0 / .2), transparent) center top / 100% 10px no-repeat scroll,
    linear-gradient(transparent, rgb(0 0 0 / .2)) center bottom / 100% 10px no-repeat scroll,
    var(--sbg, var(--bg)); }
@media (forced-colors: active) { .sl-scroll { background: Canvas; } }
:focus-visible { outline: 3px solid var(--fo); outline-offset: 2px; }
[hidden] { display: none !important; }
.sl-btn { min-height: 44px; padding: 0 16px; border: 1px solid var(--bd); background: var(--bg); color: var(--tx); cursor: pointer;
  display: inline-flex; align-items: center; gap: 8px; border-radius: 0; font-weight: 600; }
.sl-btn--primary { background: var(--a); color: var(--at); border-color: var(--a); }
.sl-btn:disabled { opacity: .6; cursor: progress; }
.sl-btn svg, .sl-icon svg { width: 18px; height: 18px; }
.sl-launcher { position: fixed; bottom: 16px; z-index: 2147483000; box-shadow: 0 2px 8px rgb(0 0 0 / .25); }
.sl-launcher--bottom-right { right: 16px; } .sl-launcher--bottom-left { left: 16px; }
.sl-icon { width: 44px; height: 44px; display: grid; place-items: center; border: 0; background: none; cursor: pointer; color: var(--tx); }
.sl-link { min-height: 44px; border: 0; background: none; color: var(--a); text-decoration: underline; cursor: pointer; padding: 0; justify-self: start; }
.sl-banner { position: fixed; left: 50%; top: 16px; transform: translateX(-50%); z-index: 2147483001; width: min(640px, calc(100vw - 32px));
  background: var(--bg); border: 2px solid var(--a); padding: 12px 16px; display: flex; flex-wrap: wrap; gap: 12px; align-items: center;
  justify-content: space-between; box-shadow: 0 4px 16px rgb(0 0 0 / .25); }
.sl-banner p { margin: 0; flex: 1 1 260px; }
.sl-actions, .sl-foot { display: flex; flex-wrap: wrap; gap: 8px; }
.sl-outline { position: fixed; z-index: 2147482999; pointer-events: none; outline: 3px solid var(--a); outline-offset: 2px;
  background: color-mix(in srgb, var(--a) 12%, transparent); transition: all 80ms ease-out; }
@media (prefers-reduced-motion: reduce) { .sl-outline { transition: none; } }
.sl-dialog { width: min(680px, calc(100vw - 32px)); max-height: calc(100vh - 32px); padding: 0; border: 1px solid var(--bd);
  background: var(--bg); color: var(--tx); border-radius: 0; display: flex; flex-direction: column; }
.sl-dialog:not([open]) { display: none; }
.sl-dialog::backdrop { background: rgb(0 0 0 / .45); }
.sl-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; padding: 4px 4px 4px 20px; border-bottom: 1px solid var(--bd); }
.sl-head h2 { font-size: 1.125rem; margin: 0; }
.sl-body { padding: 16px 20px; overflow: auto; display: grid; gap: 16px; overscroll-behavior: contain; }
.sl-foot { padding: 12px 20px; border-top: 1px solid var(--bd); }
.sl-form { display: grid; gap: 16px; }
.sl-field { display: grid; gap: 4px; }
.sl-label, legend { font-weight: 600; padding: 0; }
.sl-where { margin: 0; padding: 8px 12px; background: var(--sk); overflow-wrap: anywhere; }
.sl-muted { color: var(--mu); margin: 0; font-size: .875rem; }
.sl-error { color: var(--er); font-weight: 600; margin: 0; }
input[type=text], textarea, select { width: 100%; min-height: 44px; padding: 8px 12px; border: 1px solid var(--bd); background: var(--bg); border-radius: 0; }
[aria-invalid] { border: 2px solid var(--er) !important; }
fieldset { border: 0; margin: 0; padding: 0; display: grid; gap: 4px; min-inline-size: 0; }
.sl-choices--cards { grid-template-columns: repeat(auto-fill, minmax(min(100%, 260px), 1fr)); gap: 8px; }
.sl-choices--cards legend { grid-column: 1 / -1; margin-bottom: 4px; }
.sl-choices--cards .sl-choice { border: 1px solid var(--bd); padding: 8px 12px; min-height: 44px; gap: 10px; }
.sl-choices--cards .sl-choice:has(input:checked) { border: 2px solid var(--a); padding: 7px 11px; }
.sl-choices--cards .sl-muted { font-size: .8125rem; }
.sl-choice, .sl-check { display: flex; gap: 12px; align-items: flex-start; min-height: 44px; padding: 8px 0; cursor: pointer; }
.sl-choice input, .sl-check input { width: 22px; height: 22px; margin: 1px 0 0; accent-color: var(--a); flex: none; }
.sl-choice > span { display: grid; }
.sl-choice__name { font-weight: 600; }
.sl-checks { grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); column-gap: 16px; }
.sl-checks legend { grid-column: 1 / -1; }
.sl-check { padding: 4px 0; }
.sl-privacy { margin: 0; padding: 12px 16px; border-left: 4px solid var(--a); background: var(--sk); }
.sl-preview { margin: 0; padding: 12px; background: var(--sk); border: 1px solid var(--bd); white-space: pre-wrap; overflow-wrap: anywhere;
  font: .8125rem/1.5 ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace; min-height: 12rem; max-height: 40vh; overflow: auto;
  --sbg: var(--sk); overscroll-behavior: contain; }
.sl-summary { display: grid; gap: 4px; padding: 12px 16px; background: var(--sk); }
.sl-summary p { margin: 0; overflow-wrap: anywhere; }
.sl-summary .sl-actions { gap: 16px; }
.sl-disclosure { border: 1px solid var(--bd); background: var(--bg); }
.sl-disclosure > summary { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 12px; min-height: 44px; padding: 8px 40px 8px 16px;
  font-weight: 600; cursor: pointer; list-style: none; position: relative; }
.sl-disclosure > summary::-webkit-details-marker { display: none; }
.sl-disclosure > summary::after { content: ""; position: absolute; right: 18px; top: 50%; width: 8px; height: 8px; margin-top: -6px;
  border-right: 2px solid currentColor; border-bottom: 2px solid currentColor; transform: rotate(45deg); transition: transform 120ms; }
.sl-disclosure[open] > summary::after { transform: rotate(225deg); margin-top: -2px; }
.sl-disclosure > summary:hover { background: var(--sk); }
.sl-disclosure__meta { color: var(--mu); font-weight: 400; font-size: .875rem; }
.sl-disclosure__body { padding: 0 16px 16px; }
.sl-steps { margin: 0; padding-left: 40px; font-size: .875rem; display: grid; gap: 4px; }
[dir=rtl] .sl-disclosure > summary { padding: 8px 16px 8px 40px; }
[dir=rtl] .sl-disclosure > summary::after { right: auto; left: 18px; }
@media (prefers-reduced-motion: reduce) { .sl-disclosure > summary::after { transition: none; } }
.sl-choices--row { grid-template-columns: 1fr; }
.sl-choices--row legend { margin-bottom: 4px; }
.sl-choices--row .sl-choice { padding: 4px 0; min-height: 44px; align-items: center; }
@media (min-width: 560px) { .sl-choices--row { display: flex; flex-wrap: wrap; gap: 0 24px; } .sl-choices--row legend { width: 100%; } }
.sl-recorder { position: fixed; left: 50%; bottom: 16px; transform: translateX(-50%); z-index: 2147483001; width: min(720px, calc(100vw - 32px));
  background: var(--bg); border: 2px solid var(--a); padding: 12px 16px; display: grid; gap: 8px; box-shadow: 0 4px 16px rgb(0 0 0 / .3); }
.sl-recorder p { margin: 0; }
.sl-recorder__status { display: flex; gap: 12px; align-items: center; font-weight: 600; }
.sl-recorder__dot { width: 14px; height: 14px; border-radius: 50%; background: var(--mu); flex: none; }
.sl-recorder__status.is-recording .sl-recorder__dot { background: var(--rec); animation: sl-pulse 1.2s ease-in-out infinite; }
.sl-recorder__status.is-paused .sl-recorder__dot { background: var(--rec); opacity: .45; }
.sl-recorder__last { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.sl-recorder .sl-btn svg { width: 16px; height: 16px; }
.sl-recorder .sl-btn:first-child svg circle { fill: var(--rec); }
.sl-recorder .sl-btn--primary svg circle { fill: currentColor; }
@keyframes sl-pulse { 50% { opacity: .35; } }
@media (prefers-reduced-motion: reduce) { .sl-recorder__dot { animation: none !important; } }
.sl-results { list-style: none; margin: 0; padding: 0; display: grid; gap: 0; }
.sl-results .sl-result + .sl-result .sl-disclosure { border-top: 0; }
.sl-result details[open] { padding-bottom: 16px; display: grid; gap: 8px; }
.sl-result details[open] > :not(summary) { margin-left: 16px; margin-right: 16px; }
.sl-result__name { font-weight: 600; }
.sl-result p { margin: 0; }
.sl-preview--code { min-height: 0; max-height: none; }
.sl-outline--found { outline-color: var(--rec); outline-width: 4px; background: color-mix(in srgb, var(--rec) 12%, transparent); }
.sl-status { position: fixed; bottom: 72px; right: 16px; z-index: 2147483002; max-width: min(420px, calc(100vw - 32px)); padding: 12px 16px;
  background: var(--tx); color: var(--bg); box-shadow: 0 2px 8px rgb(0 0 0 / .3); }
.sl-qr { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 280px), 1fr)); gap: 16px 20px; align-items: start; }
.sl-qr__frame { background: #fff; border: 1px solid var(--bd); justify-self: center; width: 100%; max-width: 360px; line-height: 0; }
.sl-qr__code { width: 100%; height: auto; display: block; forced-color-adjust: none; }
.sl-qr__lead { margin: 0; font-weight: 600; }
.sl-qr__link { font: .8125rem/1.5 ui-monospace, "SFMono-Regular", Menlo, Consolas, monospace; resize: vertical; overflow-wrap: anywhere; }
@media (forced-colors: active) { .sl-outline { outline-color: Highlight; background: none; } .sl-btn--primary { border: 2px solid ButtonText; }
  .sl-qr__frame { border: 2px solid CanvasText; } }
`;

  // Start, stop and the switches ----------------------------------------------------------------------------

  function userEnabled() { return store.get("off") !== "1"; }

  function start(options = {}) {
    if (host) destroy();
    config = merge(DEFAULTS, options);
    if (!config.enabled || (config.userToggle && !userEnabled())) { config = null; return false; }
    if (!types().length) config.types = DEFAULTS.types;
    host = document.createElement("snag-list");
    host.setAttribute("data-snag-private", "");
    root = host.attachShadow({ mode: "open" });
    const lang = config.language === "auto" ? document.documentElement.lang || navigator.language || "en-GB" : config.language;
    american = /^en-(US|PH|PR)$/i.test(lang) || /^en$/i.test(lang) && /^en-US/i.test(navigator.language);
    const rtl = /^(ar|he|fa|ur|ps|yi)\b/i.test(lang);
    wrap = h("div", { class: `sl sl--${["dark", "auto"].includes(config.theme) ? config.theme : "light"}`, lang, dir: rtl ? "rtl" : null },
      h("div", { class: "sl-status", role: "status", hidden: true }));
    root.append(h("style", { text: CSS_TEXT }), wrap);
    if (config.launcher) {
      launcher = h("button", { type: "button", class: `sl-btn sl-btn--primary sl-launcher sl-launcher--${config.launcher}`, onclick: () => openChooser() },
        icon("flag"), t("launcher"));
      wrap.append(launcher);
    }
    (document.body || document.documentElement).append(host);
    record();
    listenRec();
    restoreRecorder();
    if (config.shortcut) {
      const parts = config.shortcut.toLowerCase().split("+");
      on(window, "keydown", (e) => {
        if (e.key.toLowerCase() === parts[parts.length - 1] && e.altKey === parts.includes("alt") && e.shiftKey === parts.includes("shift") &&
          e.ctrlKey === parts.includes("ctrl") && e.metaKey === parts.includes("meta")) { e.preventDefault(); openChooser(); }
      });
    }
    return true;
  }

  function destroy({ forget = false } = {}) {
    endPicking(false);
    if (rec) { rec.bar?.remove(); rec = null; try { sessionStorage.removeItem(REC_KEY); } catch { /* none */ } }
    while (undo.length) { try { undo.pop()(); } catch { /* already gone */ } }
    host?.remove();
    host = root = wrap = launcher = config = null;
    for (const b of Object.values(buffers)) b.length = 0;
    Object.assign(vitals, { lcp: null, cls: 0, inp: null });
    if (forget) { try { Object.keys(localStorage).filter((k) => k.startsWith(KEY)).forEach((k) => localStorage.removeItem(k)); } catch { /* none */ } }
  }

  let lastOptions = null;
  function setUserEnabled(enabled) {
    store.set("off", enabled ? null : "1");
    if (!enabled) destroy();
    else if (lastOptions && !host) start(lastOptions);
    return enabled;
  }

  window.Snaglist = {
    version: VERSION,
    start: (options) => { lastOptions = options || {}; return start(lastOptions); },
    destroy,
    open: (type) => openChooser(null, type ? { type } : {}),
    report: (element, type) => { if (config) { const part = describe(element || document.body); type && typeOf(type) ? openForm(part, { type }) : openChooser(part); } },
    isUserEnabled: userEnabled,
    setUserEnabled,
    presets: { types: TYPES, categories: CATEGORIES, severities: SEVERITIES, groups: GROUPS },
    redact: (text) => redact(text),
    // Every piece of interface text, by key, for translating: Snaglist.start({ text: { key: "translation" } }).
    strings: () => {
      const out = { ...TEXT };
      for (const [k, v] of Object.entries(TYPES)) {
        out[`types.${k}.label`] = v.label; out[`types.${k}.hint`] = v.hint;
        for (const f of v.fields) {
          out[`fields.${f.id}.label`] = f.label; if (f.hint) out[`fields.${f.id}.hint`] = f.hint;
          for (const [ov, ol] of f.options || []) out[`fields.${f.id}.options.${ov}`] = ol;
        }
      }
      for (const [g, l] of GROUPS) out[`groups.${g}`] = l;
      for (const [name, items] of [["severity", SEVERITIES.three], ["importance", SEVERITIES.importance]]) for (const [v, l, hint] of items) { out[`scales.${name}.${v}`] = l; if (hint) out[`scales.${name}.${v}.hint`] = hint; }
      for (const [k, l] of PARTS) out[`parts.${k}`] = l;
      for (const r of RULES) { out[`checks.${r.id}.name`] = r.name; out[`checks.${r.id}.fix`] = r.fix; }
      return out;
    },
  };

  // Start from the script tag's data attributes, when it has any.
  const tag = document.currentScript;
  if (tag && (tag.dataset.github || tag.dataset.email || tag.dataset.endpoint || tag.dataset.start != null)) {
    const go = () => window.Snaglist.start({
      app: tag.dataset.app || "", version: tag.dataset.version || "",
      types: tag.dataset.types ? tag.dataset.types.split(/[\s,]+/).filter(Boolean) : undefined,
      send: { github: tag.dataset.github ? { repo: tag.dataset.github } : null, email: tag.dataset.email ? { to: tag.dataset.email } : null,
        endpoint: tag.dataset.endpoint ? { url: tag.dataset.endpoint } : null },
    });
    document.readyState === "loading" ? document.addEventListener("DOMContentLoaded", go, { once: true }) : go();
  }
})();
