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
      // The window follows the system appearance; the page themes itself.
      setTheme: noop,
      // Updates come from Kimi Code itself, not from this shell.
      onUpdateStatus: () => noop,
      getUpdateStatus: () => Promise.resolve({ state: 'idle' }),
    };
  }

  // ---- Window dragging from the top strip (no -webkit-app-region here) ----
  if (config.dragStrip) {
    const INTERACTIVE =
      'a,button,input,textarea,select,label,summary,video,audio,iframe,[contenteditable]:not([contenteditable="false"]),' +
      '[role=button],[role=link],[role=tab],[role=menuitem],[role=option],[role=checkbox],[role=switch],[role=textbox],[role=slider],[draggable=true]';
    const isDragTarget = (event) => {
      if (event.button !== 0 || event.clientY > config.dragStrip) return false;
      if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return false;
      const target = event.target;
      if (!(target instanceof Element) || target.closest(INTERACTIVE)) return false;
      const cursor = getComputedStyle(target).cursor;
      return cursor === 'auto' || cursor === 'default';
    };
    window.addEventListener(
      'mousedown',
      (event) => {
        if (!isDragTarget(event)) return;
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
        .pill {
          pointer-events:auto; display:flex; align-items:center; gap:8px;
          font: 500 12px/1 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif;
          color: rgba(0,0,0,.82); background: rgba(250,250,250,.86);
          border: .5px solid rgba(0,0,0,.12); border-radius: 999px;
          padding: 6px 6px 6px 10px; box-shadow: 0 4px 16px rgba(0,0,0,.12);
          backdrop-filter: blur(20px) saturate(1.6); -webkit-backdrop-filter: blur(20px) saturate(1.6);
          transform: translateY(-8px); opacity: 0;
          transition: opacity .18s ease, transform .18s ease;
        }
        .pill.show { opacity:1; transform:none; }
        .pill.ok { padding-right: 10px; }
        .dot { width:8px; height:8px; border-radius:50%; background:#f5a524; flex:none; }
        .ok .dot { background:#22c55e; }
        .spin { animation: pulse 1.2s ease-in-out infinite; }
        @keyframes pulse { 50% { opacity:.35; } }
        .msg { white-space:nowrap; max-width:60vw; overflow:hidden; text-overflow:ellipsis; }
        .muted { opacity:.55; font-variant-numeric: tabular-nums; }
        button {
          font: inherit; color: inherit; cursor: pointer;
          background: rgba(0,0,0,.06); border: 0; border-radius: 999px; padding: 4px 10px;
        }
        button:hover { background: rgba(0,0,0,.1); }
        .ok button, .ok .muted, .idle button { display:none; }
        @media (prefers-color-scheme: dark) {
          .pill { color: rgba(255,255,255,.88); background: rgba(40,40,40,.82); border-color: rgba(255,255,255,.12); box-shadow: 0 4px 16px rgba(0,0,0,.4); }
          button { background: rgba(255,255,255,.1); }
          button:hover { background: rgba(255,255,255,.16); }
        }
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
    requestAnimationFrame(() => pill.classList.add('show'));
    if (ok) hideTimer = setTimeout(() => pill.classList.remove('show'), 1600);
  };

  let disconnected = false;
  window.__kd = {
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
      render({ phase: 'ready', name: (status && status.name) || document.title || 'host' });
      // Standard signal: lets the app re-open its socket right away instead
      // of waiting out its own backoff.
      window.dispatchEvent(new Event('online'));
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
