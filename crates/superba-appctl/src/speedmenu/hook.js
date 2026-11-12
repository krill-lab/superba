// Superba speed menu: runs at the start of every document of the desktop
// app's window (Page.addScriptToEvaluateOnNewDocument), before the renderer
// bundle. Superba fills in the two placeholders for one app build.
//
// The renderer's Rspack runtime copies each chunk's module factories into its
// module table with a plain assignment (`table[id] = factory`). A
// non-enumerable accessor on Object.prototype for each patched id sees that
// first assignment; when the factory is one this build was planned from
// (same source length and FNV-1a hash; a module shipped in several chunks is
// minified differently in each, so there may be several), the table gets the
// matching patched copy instead, as an own accessor that keeps a patched copy
// when another chunk registers the same module again. Anything else is stored
// unchanged, as a plain assignment would. The patched copies differ from the
// originals only in calling `allow` below at the four speed-tier checks.
(() => {
  "use strict";
  const modules = __SUPERBA_MODULES__;
  const factories = __SUPERBA_FACTORIES__;
  if (Object.prototype.hasOwnProperty.call(globalThis, "__superbaSpeedMenu")) return;
  let href = "";
  try { href = String(globalThis.location?.href ?? ""); } catch { return; }
  if (!href.startsWith("app://-/")) return;

  const state = { version: 1, patched: {}, mismatched: {}, allowed: 0 };
  const api = Object.freeze({
    state,
    // The local host signed in with an API key gets the speed tier like a
    // ChatGPT sign-in; everything else keeps the app's own answer.
    allow(host, method) {
      const ok = host === "local" && (method === "apikey" || method === "apiKey");
      if (ok) state.allowed += 1;
      return ok;
    },
  });
  Object.defineProperty(globalThis, "__superbaSpeedMenu", { value: api });

  const toText = Function.prototype.toString;
  const fnv = (text) => {
    let hash = 0x811c9dc5;
    for (let i = 0; i < text.length; i += 1) {
      hash ^= text.charCodeAt(i);
      hash = Math.imul(hash, 0x01000193) >>> 0;
    }
    return hash >>> 0;
  };
  const proto = Object.prototype;

  // id -> [{len, fnv, patched}]
  const variants = new Map();
  modules.forEach((m, i) => {
    const patched = factories[i]?.[m.id];
    if (typeof patched !== "function") return;
    if (!variants.has(m.id)) variants.set(m.id, []);
    variants.get(m.id).push({ len: m.len, fnv: m.fnv, patched });
  });

  for (const [id, known] of variants) {
    // The patched copy for `value`, or null when it is not a planned factory.
    const planned = (value) => {
      if (typeof value !== "function") return null;
      let text;
      try { text = toText.call(value); } catch { return null; }
      let hash = null;
      for (const v of known) {
        if (v.len !== text.length) continue;
        hash ??= fnv(text);
        if (v.fnv === hash) return v.patched;
      }
      state.mismatched[id] = text.length;
      return null;
    };
    const store = (target, value) => {
      try {
        Object.defineProperty(target, id, { value, writable: true, enumerable: true, configurable: true });
      } catch { /* a frozen object rejects it, as the assignment would */ }
    };
    Object.defineProperty(proto, id, {
      configurable: true,
      enumerable: false,
      get() { return undefined; },
      set(value) {
        const first = this === proto ? null : planned(value);
        if (!first) {
          store(this, value);
          return;
        }
        let current = first;
        state.patched[id] = (state.patched[id] ?? 0) + 1;
        try {
          Object.defineProperty(this, id, {
            configurable: true,
            enumerable: true,
            get() { return current; },
            set(next) {
              const copy = planned(next);
              if (copy) {
                state.patched[id] += 1;
                // The first patched copy stays: the module may have run already.
              } else {
                current = next;
              }
            },
          });
        } catch {
          store(this, value);
          return;
        }
        delete proto[id];
      },
    });
  }
})();
