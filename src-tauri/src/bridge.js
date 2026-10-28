// Injected into every page of a host window before the page's own scripts.
// It only activates on a Kimi Code server origin (http://127.0.0.1:<port>)
// and relies on web standards, plus Kimi's optional desktop hooks when
// `window.__KD_CONFIG__.hooks` is on.
(() => {
  if (location.protocol !== 'http:' || location.hostname !== '127.0.0.1') return;
  if (window.__kd) return;

  const config = window.__KD_CONFIG__ || {};
  const invoke = (cmd, args) => {
    try {
      return window.__TAURI_INTERNALS__.invoke(cmd, args);
    } catch (error) {
      return Promise.reject(error);
    }
  };

  // ---- Notifications: WKWebView has no Web Notification support. ----------
  class KdNotification extends EventTarget {
    constructor(title, options = {}) {
      super();
      this.title = String(title ?? '');
      this.body = String(options.body ?? '');
      this.tag = options.tag ?? '';
      this.data = options.data;
      this.onclick = this.onshow = this.onclose = this.onerror = null;
      invoke('kd_notify', { title: this.title, body: this.body }).then(
        () => this.#fire('show'),
        () => this.#fire('error'),
      );
    }
    #fire(type) {
      const event = new Event(type);
      this.dispatchEvent(event);
      this['on' + type]?.(event);
    }
    close() {}
    static get permission() {
      return 'granted';
    }
    static requestPermission(callback) {
      callback?.('granted');
      return Promise.resolve('granted');
    }
  }
  Object.defineProperty(window, 'Notification', { value: KdNotification, configurable: true, writable: true });

  // ---- Frosted sidebar ------------------------------------------------------
  // The window carries the native sidebar material. Kimi's own desktop UI
  // opts in with `?kimi_vibrancy=1` (html.vibrancy); for builds that do not
  // know it yet, this shim mirrors its rules: the page background goes
  // transparent and the sidebar drops its fill, while the main pane keeps
  // its own opaque background. Unknown markup simply stays opaque.
  let vibrancy = !!config.vibrancy;
  const VIBRANCY_CSS = `
    html.kl-vibrancy, html.kl-vibrancy body, html.kl-vibrancy #app, html.kl-vibrancy .app {
      background: transparent !important;
    }
    html.kl-vibrancy { --color-sidebar-bg: transparent; }`;
  // Runs `fn` once Kimi's layout is on the page (now, or when it appears).
  const layoutReady = () => !!document.querySelector('#app .app');
  function whenLayout(fn) {
    if (layoutReady()) return fn();
    const observer = new MutationObserver(() => {
      if (!layoutReady()) return;
      observer.disconnect();
      fn();
    });
    observer.observe(document.documentElement || document, { childList: true, subtree: true });
  }

  // Glass only once Kimi's layout is there: until then the transparent page
  // would show nothing but glass.
  const applyVibrancy = () => {
    const root = document.documentElement;
    if (!root) return;
    if (!document.getElementById('kl-vibrancy-style')) {
      const style = document.createElement('style');
      style.id = 'kl-vibrancy-style';
      style.textContent = VIBRANCY_CSS;
      (document.head || root).appendChild(style);
    }
    root.classList.toggle('kl-vibrancy', vibrancy);
  };
  if (config.platform === 'darwin') whenLayout(applyVibrancy);

  // ---- Host identity: sidebar tint and host chip ----------------------------
  // Both hang off Kimi's sidebar (`.side`); without it they simply stay away.
  let identity = config.host || null;
  const TINT_CSS = `
    html.kl-tint #app .side { background-image: linear-gradient(var(--kl-tint), var(--kl-tint)); }
    html.kl-tint ::selection { background-color: color-mix(in srgb, var(--color-accent) 26%, transparent); }`;
  const ACCENT_PROPS = [
    '--kl-tint', '--color-accent', '--color-accent-hover', '--color-accent-soft', '--color-accent-bd',
    '--color-code-selection', '--p-selection', 'accent-color',
  ];
  const sidebar = () => document.querySelector('#app .side');
  const applyTint = () => {
    const root = document.documentElement;
    if (!root) return;
    if (!document.getElementById('kl-tint-style')) {
      const style = document.createElement('style');
      style.id = 'kl-tint-style';
      style.textContent = TINT_CSS;
      (document.head || root).appendChild(style);
    }
    const color = identity && identity.color;
    root.classList.toggle('kl-tint', !!color);
    // Kimi's accent (buttons, links, focus rings, toggles), text selection and
    // native form controls follow the host color too. Kimi declares these on
    // <html> per color scheme, so inline values win in light and dark alike;
    // hover leans toward the text color, i.e. darker in light, lighter in dark.
    const accent = color && {
      '--kl-tint': `color-mix(in srgb, ${color} 9%, transparent)`,
      '--color-accent': color,
      '--color-accent-hover': `color-mix(in oklch, ${color} 88%, var(--color-text, #000))`,
      '--color-accent-soft': `color-mix(in srgb, ${color} 12%, transparent)`,
      '--color-accent-bd': `color-mix(in srgb, ${color} 27%, transparent)`,
      '--color-code-selection': `color-mix(in srgb, ${color} 30%, transparent)`,
      '--p-selection': `color-mix(in srgb, ${color} 20%, transparent)`,
      'accent-color': color,
    };
    for (const name of ACCENT_PROPS) {
      if (accent) root.style.setProperty(name, accent[name]);
      else root.style.removeProperty(name);
    }
  };

  let chip = null;
  let chipRoot = null;
  const ensureChip = () => {
    if (chip) return;
    chip = document.createElement('kimi-lite-chip');
    chip.style.cssText = 'position:fixed;top:12px;left:0;z-index:2147483646;display:none;';
    chipRoot = chip.attachShadow({ mode: 'closed' });
    chipRoot.innerHTML = `
      <style>
        :host { all: initial; }
        button {
          display: flex; align-items: center; gap: 6px; height: 24px; max-width: 100%;
          padding: 0 6px 0 8px; border: 0; border-radius: 6px; background: transparent; cursor: default;
          font: 500 12.5px/1 var(--font-ui, "Schibsted Grotesk Variable", -apple-system, system-ui, sans-serif);
          color: var(--color-text-muted, rgba(0,0,0,.6));
        }
        button:hover { background: var(--color-hover, rgba(0,0,0,.04)); color: var(--color-text, rgba(0,0,0,.9)); }
        .dot { width: 8px; height: 8px; border-radius: 50%; flex: none; background: var(--chip-color, #9ca3af); }
        /* Room for descenders: the ellipsis needs overflow hidden. */
        .name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; line-height: 1.4; }
        svg { width: 10px; height: 10px; flex: none; opacity: .6; }
      </style>
      <button type="button" title="Switch host (⇧⌘O)"><span class="dot"></span><span class="name"></span>
        <svg viewBox="0 0 10 10" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 4l2.5 2.5L7.5 4"/></svg>
      </button>`;
    chipRoot.querySelector('button').addEventListener('click', () => {
      const rect = chip.getBoundingClientRect();
      invoke('kd_host_menu', { x: rect.left, y: rect.bottom + 4 }).catch(() => {});
    });
    document.documentElement.appendChild(chip);
  };
  const placeChip = () => {
    if (!identity || !config.dragStrip) return;
    ensureChip();
    if (!chip.isConnected) document.documentElement.appendChild(chip);
    chipRoot.querySelector('.name').textContent = identity.name;
    chipRoot.querySelector('button').style.setProperty('--chip-color', identity.color || '#9ca3af');
    const side = sidebar();
    const rect = side && side.getBoundingClientRect();
    chip.style.display = 'block';
    if (rect && rect.width >= 180 && rect.right >= 180) {
      // Right-aligned in the sidebar header, clear of the traffic lights and
      // Kimi's collapse button (which end around x = 110).
      const room = rect.right - 10 - 116;
      chip.style.maxWidth = `${room}px`;
      chip.style.left = `${rect.right - 10 - Math.min(chip.offsetWidth, room)}px`;
      return;
    }
    // Sidebar collapsed: right-aligned in the main header, just left of the
    // icon buttons Kimi keeps at its right end (more appear with the right
    // panel open), where the header is otherwise empty.
    if (window.innerWidth < 720) {
      chip.style.display = 'none';
      return;
    }
    chip.style.maxWidth = '200px';
    chip.style.left = `${headerButtonsLeft() - 8 - Math.min(chip.offsetWidth, 200)}px`;
  };
  // Left edge of the run of buttons at the right end of the header, found by
  // hit-testing leftwards from the window edge (no class names involved).
  const headerButtonsLeft = () => {
    const y = config.dragStrip / 2;
    let edge = window.innerWidth - 8;
    chip.style.pointerEvents = 'none';
    for (let i = 0; i < 8; i++) {
      let hit = null;
      for (const dx of [4, 10, 16]) {
        const el = document.elementFromPoint(edge - dx, y);
        hit = el && el.closest('button,a,[role=button],[role=tab]');
        if (hit) break;
      }
      if (!hit) break;
      edge = Math.min(edge, hit.getBoundingClientRect().left);
    }
    chip.style.pointerEvents = '';
    return edge;
  };

  let observedSide = null;
  const resizeObserver = typeof ResizeObserver === 'function' ? new ResizeObserver(placeChip) : null;
  const refreshIdentity = () => {
    applyTint();
    const side = sidebar();
    if (side !== observedSide) {
      if (observedSide) resizeObserver?.unobserve(observedSide);
      if (side) resizeObserver?.observe(side);
      observedSide = side;
    }
    placeChip();
  };
  let watchingIdentity = false;
  const watchIdentity = () => {
    if (watchingIdentity) return;
    watchingIdentity = true;
    // Kimi re-renders its layout as it boots and when the sidebar toggles.
    whenLayout(refreshIdentity);
    // Also catches a sidebar that collapses by moving rather than resizing.
    setInterval(() => (sidebar() !== observedSide ? refreshIdentity() : placeChip()), 1000);
    // Panels open and close on clicks: follow them without waiting a second.
    window.addEventListener('click', () => [60, 320].forEach((ms) => setTimeout(placeChip, ms)), true);
    window.addEventListener('resize', placeChip);
  };
  if (identity) watchIdentity();

  // ---- Optional Kimi desktop hooks ----------------------------------------
  const menuListeners = new Set();
  if (config.hooks) {
    const noop = () => {};
    window.kimiDesktop = {
      platform: config.platform,
      onMenuAction(listener) {
        menuListeners.add(listener);
        return () => menuListeners.delete(listener);
      },
      showWindow() {
        invoke('kd_show_window').catch(noop);
      },
      // 'light' | 'dark' pin the window appearance (title bar, glass);
      // 'system' follows macOS again.
      setTheme(theme) {
        invoke('kd_set_theme', { theme: String(theme) }).catch(noop);
      },
      getVibrancy: () => Promise.resolve(vibrancy),
      setVibrancy(enabled) {
        invoke('kd_set_vibrancy', { enabled: !!enabled }).catch(noop);
      },
      // "Fetch model list" for a custom provider, run with curl on the host:
      // no CORS, and the host's own network. Kimi falls back to fetch().
      fetchProviderModels(request) {
        return invoke('kd_fetch_provider_models', { request }).catch(() => null);
      },
      // Remembered app-wide, so other hosts skip the first-run screens.
      setOnboarded() {
        invoke('kd_set_onboarded').catch(noop);
      },
      // Updates come from Kimi Code itself, not from this shell.
      onUpdateStatus: () => noop,
      getUpdateStatus: () => Promise.resolve({ state: 'idle' }),
    };
  }

  // ---- Arrow cursor, like a native app ---------------------------------------
  // Kimi's stylesheets (same-origin) are rewritten once: `cursor: pointer`
  // becomes `default`. The rewritten selectors are remembered, since they
  // also tell the drag strip what is clickable. Links in prose keep the hand.
  let pointerSelector = '';
  const pointerSelectors = new Set();
  const patchedSheets = new WeakSet();
  const patchRules = (rules) => {
    for (const rule of rules) {
      if (rule.cssRules && rule.cssRules.length) patchRules(rule.cssRules);
      if (!rule.style || rule.style.getPropertyValue('cursor') !== 'pointer') continue;
      rule.style.setProperty('cursor', 'default', rule.style.getPropertyPriority('cursor'));
      if (rule.selectorText) pointerSelectors.add(rule.selectorText.replace(/::?(before|after|marker|placeholder)\b/g, ''));
    }
  };
  const patchSheets = () => {
    for (const sheet of document.styleSheets) {
      if (patchedSheets.has(sheet) || sheet.ownerNode?.id?.startsWith('kl-')) continue;
      try {
        patchRules(sheet.cssRules);
        patchedSheets.add(sheet);
      } catch {}
    }
    pointerSelector = [...pointerSelectors]
      .filter((selector) => {
        try {
          document.documentElement.matches(selector);
          return true;
        } catch {
          return false;
        }
      })
      .join(',');
  };
  const PROSE_LINKS = `
    :is(p, li, td, blockquote) a[href] { cursor: pointer; }`;
  const startCursorPatch = () => {
    const style = document.createElement('style');
    style.id = 'kl-cursor-style';
    style.textContent = PROSE_LINKS;
    (document.head || document.documentElement).appendChild(style);
    patchSheets();
    // Vite loads CSS chunks on demand: patch each new stylesheet once loaded.
    new MutationObserver((records) => {
      for (const record of records) {
        for (const node of record.addedNodes) {
          if (node.nodeName === 'LINK') node.addEventListener('load', patchSheets, { once: true });
          else if (node.nodeName === 'STYLE') patchSheets();
        }
      }
    }).observe(document.head || document.documentElement, { childList: true });
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', startCursorPatch, { once: true });
  else startCursorPatch();
  window.addEventListener('load', patchSheets, { once: true });

  // ---- Window dragging from the top strip (no -webkit-app-region here) ----
  if (config.dragStrip) {
    const INTERACTIVE =
      'a,button,input,textarea,select,label,summary,video,audio,iframe,[contenteditable]:not([contenteditable="false"]),' +
      '[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=checkbox],[role=switch],[role=textbox],[role=slider],[draggable=true],' +
      'kimi-lite-chip,kimi-lite-banner';
    const isDragTarget = (event) => {
      if (event.button !== 0 || event.clientY > config.dragStrip) return false;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return false;
      const target = event.target;
      if (!(target instanceof Element) || target.closest(INTERACTIVE)) return false;
      if (pointerSelector && target.closest(pointerSelector)) return false;
      const cursor = getComputedStyle(target).cursor;
      return cursor === 'auto' || cursor === 'default';
    };
    window.addEventListener(
      'mousedown',
      (event) => {
        if (!isDragTarget(event)) return;
        // Otherwise WebKit also starts a text selection, which sweeps over
        // the page while the window moves under the pointer.
        event.preventDefault();
        if (event.detail === 2) {
          invoke('plugin:window|internal_toggle_maximize').catch(() => {});
        } else {
          invoke('plugin:window|start_dragging').catch(() => {});
        }
      },
      true,
    );
  }

  // ---- Connection banner ---------------------------------------------------
  let host = null;
  let root = null;
  let hideTimer = 0;
  let countdownTimer = 0;
  let showFrame = 0;
  const ensureBanner = () => {
    if (root) return root;
    host = document.createElement('kimi-lite-banner');
    host.style.cssText = 'position:fixed;inset:0 0 auto 0;z-index:2147483647;pointer-events:none;';
    root = host.attachShadow({ mode: 'closed' });
    root.innerHTML = `
      <style>
        :host { all: initial; }
        /* Below the 48px header strip, so the page title stays readable. */
        .wrap { display:flex; justify-content:center; padding-top:58px; }
        /* Kimi's own tokens when the page defines them, matching fallbacks otherwise. */
        .pill {
          pointer-events:auto; display:flex; align-items:center; gap:8px;
          font: 500 13px/1 var(--font-ui, "Schibsted Grotesk Variable", -apple-system, system-ui, sans-serif);
          color: var(--color-text, rgba(0,0,0,.9));
          background: var(--color-surface-raised, #fff);
          border: .5px solid var(--color-line, rgba(0,0,0,.13)); border-radius: 999px;
          padding: 5px 5px 5px 12px;
          box-shadow: 0 4px 12px rgba(16,24,40,.07), 0 2px 4px rgba(16,24,40,.05);
          transform: translateY(-6px); opacity: 0;
          transition: opacity .16s cubic-bezier(.16,1,.3,1), transform .16s cubic-bezier(.16,1,.3,1);
        }
        .pill.show { opacity:1; transform:none; }
        .pill.ok, .pill.idle { padding-right: 12px; }
        .dot { width:7px; height:7px; border-radius:50%; background:#f5a524; flex:none; }
        .ok .dot { background:#22c55e; }
        .spin { animation: pulse 1.2s ease-in-out infinite; }
        @keyframes pulse { 50% { opacity:.35; } }
        .msg { white-space:nowrap; max-width:60vw; overflow:hidden; text-overflow:ellipsis; line-height:1.4; }
        .muted { color: var(--color-text-faint, rgba(0,0,0,.45)); font-variant-numeric: tabular-nums; }
        button {
          font: inherit; color: inherit; cursor: default; height: 24px;
          background: var(--color-surface-sunken, #f5f5f5); border: .5px solid var(--color-line-strong, rgba(0,0,0,.15));
          border-radius: 999px; padding: 0 10px;
        }
        button:hover { background: var(--color-selected, rgba(0,0,0,.05)); }
        .ok button, .ok .muted, .idle button { display:none; }
      </style>
      <div class="wrap"><div class="pill" role="status" aria-live="polite">
        <span class="dot"></span><span class="msg"></span><span class="muted"></span>
        <button type="button">Retry now</button>
      </div></div>`;
    root.querySelector('button').addEventListener('click', () => invoke('kd_retry').catch(() => {}));
    document.documentElement.appendChild(host);
    return root;
  };

  const render = (status) => {
    const r = ensureBanner();
    if (!host.isConnected) document.documentElement.appendChild(host);
    const pill = r.querySelector('.pill');
    clearTimeout(hideTimer);
    clearInterval(countdownTimer);
    cancelAnimationFrame(showFrame);
    const ok = status.phase === 'ready';
    pill.classList.toggle('ok', ok);
    // "Retry now" only makes sense while waiting for a scheduled retry.
    pill.classList.toggle('idle', !ok && !status.retryIn);
    r.querySelector('.dot').classList.toggle('spin', !ok);
    r.querySelector('.msg').textContent = ok ? `Reconnected to ${status.name}` : `${status.message}`;
    const muted = r.querySelector('.muted');
    muted.textContent = '';
    if (!ok && status.retryIn) {
      let left = status.retryIn;
      const tick = () => {
        muted.textContent = left > 0 ? `retry in ${left}s` : '';
        left -= 1;
      };
      tick();
      countdownTimer = setInterval(tick, 1000);
    }
    // Frames are paused while the window is hidden or covered, so the
    // "Reconnected" pill starts its hide timer only once it is on screen;
    // otherwise the timer runs out first and the pill stays up for good.
    showFrame = requestAnimationFrame(() => {
      pill.classList.add('show');
      if (ok) hideTimer = setTimeout(() => pill.classList.remove('show'), 1600);
    });
  };

  // ---- Realtime sockets ----------------------------------------------------
  // Kimi shows a toast for every WebSocket `error` and retries with a growing
  // backoff (up to 30s), so a short outage stacks up toasts and the page can
  // stay offline long after the host is back. While the banner says the host
  // is unreachable, a socket to Kimi's own server waits instead and connects
  // as soon as the host is back. Otherwise only the first error of a run of
  // failures reaches the page, and only if the banner doesn't take over
  // within a moment. `close` always follows an error and is what Kimi
  // reconnects on, so dropping errors changes nothing else.
  let disconnected = false;
  let failing = false;
  const NativeWebSocket = window.WebSocket;
  const ERROR_GRACE = 2000;
  const held = new Set();
  const replayed = new WeakSet();
  const ownServer = (url) => {
    try {
      return new URL(url, location.href).host === location.host;
    } catch {
      return false;
    }
  };
  // Registered before the page's own handlers, so it can stop them.
  const quiet = (ws) => {
    ws.addEventListener('open', () => (failing = false));
    ws.addEventListener('error', (event) => {
      if (replayed.has(event)) return;
      event.stopImmediatePropagation();
      if (failing || disconnected) return;
      failing = true;
      setTimeout(() => {
        if (disconnected) return;
        const again = new Event('error');
        replayed.add(again);
        ws.dispatchEvent(again);
      }, ERROR_GRACE);
    });
    return ws;
  };
  const STATES = ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'];
  class HeldSocket extends EventTarget {
    #ws = null;
    #closed = false;
    #binaryType = 'blob';
    constructor(url, protocols) {
      super();
      this.url = new URL(url, location.href).href;
      this.protocols = protocols;
      held.add(this);
    }
    get readyState() {
      return this.#ws ? this.#ws.readyState : this.#closed ? 3 : 0;
    }
    get protocol() {
      return this.#ws ? this.#ws.protocol : '';
    }
    get extensions() {
      return this.#ws ? this.#ws.extensions : '';
    }
    get bufferedAmount() {
      return this.#ws ? this.#ws.bufferedAmount : 0;
    }
    get binaryType() {
      return this.#binaryType;
    }
    set binaryType(value) {
      this.#binaryType = value;
      if (this.#ws) this.#ws.binaryType = value;
    }
    send(data) {
      if (!this.#ws) throw new DOMException('Still in CONNECTING state.', 'InvalidStateError');
      this.#ws.send(data);
    }
    close(code, reason) {
      if (this.#ws) return this.#ws.close(code, reason);
      if (this.#closed) return;
      this.#closed = true;
      held.delete(this);
      setTimeout(() => this.dispatchEvent(new CloseEvent('close', { code: 1006, wasClean: false })));
    }
    connect() {
      held.delete(this);
      if (this.#closed || this.#ws) return;
      const ws = (this.#ws = quiet(new NativeWebSocket(this.url, this.protocols)));
      ws.binaryType = this.#binaryType;
      ws.addEventListener('open', () => this.dispatchEvent(new Event('open')));
      ws.addEventListener('message', (e) =>
        this.dispatchEvent(new MessageEvent('message', { data: e.data, origin: e.origin, lastEventId: e.lastEventId })),
      );
      ws.addEventListener('error', () => this.dispatchEvent(new Event('error')));
      ws.addEventListener('close', (e) =>
        this.dispatchEvent(new CloseEvent('close', { code: e.code, reason: e.reason, wasClean: e.wasClean })),
      );
    }
  }
  for (const type of ['open', 'message', 'error', 'close']) {
    const handlers = new WeakMap();
    Object.defineProperty(HeldSocket.prototype, 'on' + type, {
      get() {
        return handlers.get(this) || null;
      },
      set(handler) {
        const old = handlers.get(this);
        if (old) this.removeEventListener(type, old);
        handlers.delete(this);
        if (typeof handler === 'function') {
          handlers.set(this, handler);
          this.addEventListener(type, handler);
        }
      },
    });
  }
  function LiteWebSocket(url, protocols) {
    if (!new.target) throw new TypeError("Failed to construct 'WebSocket': Please use the 'new' operator.");
    if (!ownServer(url)) return new NativeWebSocket(url, protocols);
    return disconnected ? new HeldSocket(url, protocols) : quiet(new NativeWebSocket(url, protocols));
  }
  LiteWebSocket.prototype = NativeWebSocket.prototype;
  STATES.forEach((name, value) => {
    LiteWebSocket[name] = value;
    HeldSocket.prototype[name] = value;
  });
  window.WebSocket = LiteWebSocket;

  // ---- In-page navigation for kimi-lite:// links ----------------------------
  // A page that routes on `popstate` can switch to a session without a full
  // reload (seconds of loading screens). Whether it does is seen from its own
  // listener; without one, links load the page as before.
  let routesOnPopState = false;
  const addWindowListener = window.addEventListener;
  window.addEventListener = function (type, ...rest) {
    if (type === 'popstate') routesOnPopState = true;
    return addWindowListener.call(this, type, ...rest);
  };
  const openPath = (path) => {
    if (location.pathname === path) return;
    const url = path + location.search;
    if (!routesOnPopState && !window.onpopstate) return location.assign(url);
    history.pushState(null, '', url);
    window.dispatchEvent(new PopStateEvent('popstate', { state: null }));
  };

  window.__kd = {
    openPath,
    status(status) {
      if (status.phase === 'ready') {
        if (disconnected) window.__kd.reconnected(status);
        return;
      }
      disconnected = true;
      render(status);
    },
    reconnected(status) {
      if (!disconnected) return;
      disconnected = false;
      failing = false;
      for (const socket of [...held]) socket.connect();
      render({ phase: 'ready', name: (status && status.name) || document.title || 'host' });
      // Standard signal: lets the app re-open its socket right away instead
      // of waiting out its own backoff.
      window.dispatchEvent(new Event('online'));
    },
    setVibrancy(enabled) {
      vibrancy = !!enabled;
      applyVibrancy();
    },
    // Host edits (name, color) apply to open windows without a reopen.
    setHost(next) {
      identity = next || null;
      if (identity) watchIdentity();
      refreshIdentity();
    },
    menu(action) {
      for (const listener of menuListeners) {
        try {
          listener(action);
        } catch {}
      }
    },
  };
})();
