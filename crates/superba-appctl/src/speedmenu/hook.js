// Superba page patches (the speed menu with API keys, forced composer
// predictions): runs at the start of every document of the desktop app's
// window (Page.addScriptToEvaluateOnNewDocument), before the renderer bundle.
// Superba fills in the placeholders for one app build and the patch sets the
// launch wants.
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
// originals only in calling `allow` below at the four speed-tier checks, and
// `gate`, `services` and `client` for composer predictions.
(() => {
  "use strict";
  const prediction = __SUPERBA_PREDICTION__;
  const modules = __SUPERBA_MODULES__;
  const factories = __SUPERBA_FACTORIES__;
  if (Object.prototype.hasOwnProperty.call(globalThis, "__superbaSpeedMenu")) return;
  let href = "";
  try { href = String(globalThis.location?.href ?? ""); } catch { return; }
  if (!href.startsWith("app://-/")) return;

  const state = {
    version: 1,
    patched: {},
    mismatched: {},
    allowed: 0,
    forced: 0,
    // Superba's own composer predictions (see `predictionService`).
    predictions: { clients: 0, original: 0, requested: 0, suggested: 0, abstained: 0, failed: 0, last: null },
  };
  const clients = new Map();
  const api = Object.freeze({
    state,
    // The local host signed in with an API key gets the speed tier like a
    // ChatGPT sign-in; everything else keeps the app's own answer.
    allow(host, method) {
      const ok = host === "local" && (method === "apikey" || method === "apiKey");
      if (ok) state.allowed += 1;
      return ok;
    },
    // The Statsig gate of composer predictions reads as on. Only modules of
    // that patch set call it, and they are in the script only when the
    // setting is on.
    gate(id) {
      const ok = id === "4290031608";
      if (ok) state.forced += 1;
      return ok;
    },
    // The renderer's app-server request client of a host (one per host; the
    // latest one that can send wins).
    client(c) {
      try {
        if (c && typeof c.hostId === "string" && c.dispatchMessage != null && typeof c.sendRequest === "function") {
          clients.set(c.hostId, c);
          state.predictions.clients = clients.size;
        }
      } catch { /* never break the app */ }
    },
    // The renderer's `appServices`: the same object, with Superba's
    // prediction service in front of the app's `cumberbatch`.
    services(s) {
      if (s == null || (typeof s !== "object" && typeof s !== "function")) return s;
      let base;
      let wrapped;
      try {
        return new Proxy(s, {
          get(target, key, receiver) {
            const value = Reflect.get(target, key, receiver);
            if (key !== "cumberbatch" || value == null) return value;
            const own = Reflect.getOwnPropertyDescriptor(target, key);
            if (own && !own.configurable && !own.writable) return value;
            if (value !== base) {
              base = value;
              wrapped = predictionService(value);
            }
            return wrapped;
          },
        });
      } catch {
        return s;
      }
    },
  });
  Object.defineProperty(globalThis, "__superbaSpeedMenu", { value: api });

  // Composer predictions without the app's main process. The app asks its
  // main process for each prediction, which needs a ChatGPT sign-in, asks
  // chatgpt.com whether predictions are on for the account and then runs
  // `thread/sample` on the app-server. Superba's service asks the main
  // process first and uses its answer when there is one; otherwise, for the
  // composer's own request (`catch_up`, which only runs while the composer
  // shows predictions and the app's setting is on), it sends `thread/sample`
  // itself through the window's app-server client, with the prompt chatgpt.com
  // serves (`prediction`) and its effort, as the main process sends them, and
  // returns what the main process would have. Local host only (remote hosts
  // keep the app's own handling). One request per turn; a newer turn of the
  // same thread, a turn that starts in it or the deadline cancels it.
  const SCHEMA = '{"type":"object","properties":{"suggestion":{"anyOf":[{"type":"string"},{"type":"null"}]}},"required":["suggestion"],"additionalProperties":false}';
  const PROMPT = `${prediction.prompt}\n\nReturn only JSON matching this schema: ${SCHEMA}.`;
  const SAMPLE_TIMEOUT_MS = 45000;
  // The prompt asks for one line of at most 240 characters.
  const MAX_SUGGESTION = 240;
  const sampled = new Map(); // [host, thread, turn] -> {promise, cancel}
  const latestOfThread = new Map(); // [host, thread] -> key of its newest turn

  const fail = (why) => {
    state.predictions.failed += 1;
    state.predictions.last = String(why).slice(0, 200);
    return null;
  };
  // The suggestion of a sample's reply text (`{"suggestion": ...}`, possibly
  // in a code fence), `undefined` when it is not one.
  const suggestionOf = (text) => {
    if (typeof text !== "string") return undefined;
    const body = text.trim().replace(/^```(?:json)?\s*/i, "").replace(/\s*```$/, "");
    for (const candidate of [body, body.slice(body.indexOf("{"), body.lastIndexOf("}") + 1)]) {
      try {
        const v = JSON.parse(candidate);
        if (v && typeof v === "object" && "suggestion" in v) {
          const line = typeof v.suggestion === "string" ? v.suggestion.replace(/\s+/g, " ").trim() : "";
          return line && line.length <= MAX_SUGGESTION ? line : null;
        }
      } catch { /* try the next form */ }
    }
    return undefined;
  };
  const requestIds = (c) => {
    try { return new Set(c.requestPromises?.keys?.() ?? []); } catch { return new Set(); }
  };

  // A turn that starts in a thread makes its pending sample useless.
  try {
    globalThis.addEventListener?.("message", (event) => {
      const m = event?.data;
      if (m?.type !== "mcp-notification" || m.method !== "turn/started") return;
      const thread = m.params?.threadId ?? m.params?.turn?.threadId;
      if (typeof thread !== "string") return;
      const key = latestOfThread.get(JSON.stringify([m.hostId ?? "local", thread]));
      if (key) sampled.get(key)?.cancel();
    });
  } catch { /* the deadline still ends it */ }

  const samplePrediction = (target) => {
    const key = JSON.stringify([target.hostId, target.threadId, target.latestTurnId]);
    const known = sampled.get(key);
    if (known) return known.promise;
    const c = clients.get(target.hostId);
    if (!c) return Promise.resolve(fail("no app-server client for host " + target.hostId));
    const thread = JSON.stringify([target.hostId, target.threadId]);
    const older = latestOfThread.get(thread);
    if (older && older !== key) sampled.get(older)?.cancel();
    latestOfThread.set(thread, key);
    const entry = { cancel() {} };
    entry.promise = (async () => {
      const requestedAt = new Date().toISOString();
      const started = performance.now();
      state.predictions.requested += 1;
      const before = requestIds(c);
      let id = null;
      try {
        const pending = c.sendRequest("thread/sample", {
          threadId: target.threadId,
          expectedTurnId: target.latestTurnId,
          prompt: PROMPT,
          effort: prediction.effort,
          threadSource: "composer_predictions",
          turnTrigger: "composer_predictions",
          responsesapiClientMetadata: {
            prediction_source_thread_id: target.threadId,
            prediction_source_turn_id: target.latestTurnId,
            prediction_trigger: target.generationTrigger,
          },
          disableMcp: true,
        }, { priority: "background", timeoutMs: SAMPLE_TIMEOUT_MS + 5000 });
        pending.catch(() => {});
        // The request is queued synchronously: its id is the new one.
        try {
          for (const [k, v] of c.requestPromises ?? []) if (!before.has(k) && v?.method === "thread/sample") id = k;
        } catch { /* no cancel then */ }
        let stop;
        const stopped = new Promise((resolve) => { stop = resolve; });
        entry.cancel = () => {
          if (id != null) c.sendRequest("thread/sample/cancel", { requestId: id }, { priority: "background" }).catch(() => {});
          id = null;
          stop("cancelled");
        };
        const timer = setTimeout(() => entry.cancel(), SAMPLE_TIMEOUT_MS);
        let response;
        try {
          response = await Promise.race([pending, stopped.then((why) => { throw new Error(why); })]);
        } finally {
          clearTimeout(timer);
        }
        id = null;
        const result = response?.result ?? response;
        if (result?.sourceTurnId != null && result.sourceTurnId !== target.latestTurnId) return fail("sampled another turn");
        const text = suggestionOf(result?.text);
        if (text === undefined) return fail("no suggestion in the reply");
        if (text == null) state.predictions.abstained += 1;
        else state.predictions.suggested += 1;
        state.predictions.last = text;
        const number = (v) => (v == null ? undefined : Number(v));
        // What the app's main process returns (the composer, its accept
        // handling and the app's telemetry read these).
        return {
          requestedAt,
          hostKind: "local",
          text,
          responseId: result?.responseId ?? `superba-${key}`,
          generationTrigger: target.generationTrigger,
          sourceTurnId: target.latestTurnId,
          model: null,
          reasoningEffort: prediction.effort,
          serviceTier: null,
          parentReasoningEffort: null,
          configuredReasoningEffort: prediction.effort,
          promptVersion: prediction.promptVersion,
          totalDurationMs: Math.round(performance.now() - started),
          inputTokens: number(result?.inputTokens),
          cachedInputTokens: number(result?.cachedInputTokens),
        };
      } catch (e) {
        sampled.delete(key);
        return fail(e?.message ?? e);
      }
    })();
    sampled.set(key, entry);
    for (const k of sampled.keys()) {
      if (sampled.size <= 32) break;
      sampled.delete(k);
    }
    return entry.promise;
  };

  const predictionService = (original) => {
    const own = {
      async isEnabledByRemoteConfig() {
        return true;
      },
      async requestUserMessagePrediction(target, onIneligible) {
        try {
          const answer = await original.requestUserMessagePrediction(target, onIneligible);
          if (answer != null) {
            state.predictions.original += 1;
            return answer;
          }
        } catch { /* no ChatGPT sign-in, or the main process failed */ }
        if (target?.generationTrigger !== "catch_up" || target.hostId !== "local" || target.threadId == null || target.latestTurnId == null) {
          return null;
        }
        return samplePrediction(target);
      },
    };
    return new Proxy(original, {
      get(t, key, receiver) {
        return Object.prototype.hasOwnProperty.call(own, key) ? own[key] : Reflect.get(t, key, receiver);
      },
    });
  };

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
