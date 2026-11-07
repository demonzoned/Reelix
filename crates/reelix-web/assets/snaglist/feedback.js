// Send feedback in Reelix's set-up wizard. Loads Snaglist (snaglist.js, MIT, see LICENSE-snaglist-MIT.txt)
// the first time Send feedback is pressed, so the wizard costs nothing extra until then. Each report names the
// wizard step it came from and goes to Reelix's GitHub issues; Show as a QR code sends it from a phone when this
// server is not online yet. The wizard calls window.reelixFeedback.open(stepKey, stepTitle, error).
(() => {
  "use strict";
  if (window.reelixFeedback) return;
  const here = document.currentScript ? new URL(".", document.currentScript.src).href : "";
  let current = { step: "", title: "", error: "" };
  let loading = null;

  // Reelix's look inside Snaglist's shadow root: Reelix's colours (they follow the light and dark themes),
  // rounded corners and the Reelix font. Snaglist keeps its 44px targets and its focus outline.
  const LOOK = `
.sl.sl { --a: var(--accent); --at: var(--accent-contrast); --bg: var(--surface); --tx: var(--text);
  --mu: var(--text-muted); --bd: var(--border-input); --sk: var(--surface-2); --fo: var(--accent-strong);
  --er: var(--danger); --sb: var(--border-strong); --sbh: var(--text-muted); font-family: var(--font-sans); }
.sl-dialog { border-radius: var(--radius-lg); border: 1px solid var(--border); box-shadow: var(--shadow-lg); overflow: hidden; }
.sl-btn, input[type=text], textarea, select, .sl-choices--cards .sl-choice, .sl-disclosure, .sl-banner, .sl-qr__frame,
.sl-preview { border-radius: var(--radius); }
.sl-btn--primary { border-color: var(--accent); }
.sl-head { border-bottom: 1px solid var(--border); }
.sl-qr__frame { padding: 8px; }`;

  function restyle() {
    const root = document.querySelector("snag-list")?.shadowRoot;
    if (root && !root.querySelector("style[data-reelix]")) {
      const style = document.createElement("style");
      style.dataset.reelix = "";
      style.textContent = LOOK;
      root.append(style);
    }
  }

  function load() {
    if (window.Snaglist) return Promise.resolve();
    loading ||= new Promise((resolve, reject) => {
      const script = document.createElement("script");
      script.src = `${here}snaglist.js`;
      script.onload = resolve;
      script.onerror = () => { loading = null; reject(new Error("Send feedback could not load")); };
      document.head.append(script);
    });
    return loading;
  }

  function start() {
    const version = document.documentElement.dataset.reelixVersion || "";
    window.Snaglist.start({
      app: "Reelix",
      version,
      language: "en-GB",
      theme: document.documentElement.dataset.theme === "light" ? "light" : "dark",
      launcher: false,
      // The accessibility check loads a script from another site, which Reelix's security policy blocks.
      types: ["bug", "ux", "content", "accessibility", "feature", "idea", "other"],
      labels: ["feedback", "set-up wizard"],
      page: { name: () => (current.title ? `Set-up wizard: ${current.title} (${current.step})` : "Set-up wizard") },
      // Never what was typed into the wizard: only the step and, after a failure, Reelix's own error.
      context: () => Object.fromEntries(Object.entries({ "wizard step": current.step, "last error": current.error }).filter(([, v]) => v)),
      send: { github: { repo: "demonzoned/Reelix" }, copy: true, download: true, qr: true },
    });
    restyle();
  }

  window.reelixFeedback = {
    open(step, title, error) {
      current = { step: String(step || ""), title: String(title || ""), error: String(error || "") };
      return load().then(() => {
        if (!document.querySelector("snag-list")) start();
        window.Snaglist.open(error ? "bug" : undefined);
        restyle();
      });
    },
  };
})();
