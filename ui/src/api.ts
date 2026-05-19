import type { ControllerDef, AudioConfig, DeviceMap, ParamInfo, TypeOverrides } from './types';
export function splitN(s: string, sep: string, n: number): string[] {
    const out: string[] = [];
    let remaining = s;
    for (let i = 0; i < n - 1; i++) {
        const idx = remaining.indexOf(sep);
        if (idx === -1) break;
        out.push(remaining.slice(0, idx));
        remaining = remaining.slice(idx + sep.length);
    }
    out.push(remaining);
    return out;
}

//---------- Errors ----------//
// Error messages flow as i18n keys + optional placeholder args. The handler
// (toast layer) is responsible for translating; api.ts just emits. Variadic
// args feed `t()`'s `{}` placeholder substitution.
let onError: (key: string, ...args: (string | number)[]) => void = console.error;
export function setApiErrorHandler(fn: (key: string, ...args: (string | number)[]) => void) { onError = fn; }

//---------- Websocket ----------//
let ws: WebSocket|null = null;
type Pending = {expect: string; resolve: (value:string|null) => void; reject: (e: Error) => void};
const pending: Pending[] = [];

// Send a line. Drops silently if not connected — UI will resync on reconnect
// via the SNAPSHOT line the server sends on handshake.
//
// Return tuple: `[ok, payload]`. On success `ok=true`, `payload` is the
// server's response body (or null for plain `OK`). On failure `ok=false` and
// `payload` is the server's ERR message — callers that want to react to
// specific errors (e.g. `confirm_required:` for reload acknowledgement) parse
// it. The default `onError` toast still fires for visibility.
function sendWs(line: string, expect: string = 'OK'): Promise<[boolean, string|null]> {
    return new Promise(resolve => {
        if (!ws || ws.readyState !== WebSocket.OPEN) {
            onError('error.not_connected');
            resolve([false, 'not connected']);
            return;
        }
        pending.push({
            expect,
            resolve: (value: string|null) => {
                resolve([true, value]);
            }, reject: e => {
                // `confirm_required:` is a soft error the caller handles
                // (shows a reload-acknowledgement popup) — don't toast it.
                if (!e.message.startsWith('confirm_required:')) {
                    onError(e.message);
                }
                resolve([false, e.message]);
            }
        });
        ws.send(line);
    });
}

async function fetchWs<T>(command: string, expect: string): Promise<T | null> {
    const [ok, value] = await sendWs(command, expect);
    if (!ok || value === null) return null;
    try { return JSON.parse(value) as T; } catch { onError('error.bad_payload', expect); return null; }
}

function handleLine(line: string, onMessage: (msg: string, param: string) => void) {
    let [msg, param] = splitN(line, ' ', 2);
    if (msg === 'ERR') {
        pending.shift()?.reject(new Error(param));
        return;
    }
    // Resolve a pending request whose expected response type matches.
    if (pending.length && pending[0].expect === msg) {
        pending.shift()?.resolve(param);
    }
    // Always dispatch event-shaped messages through onMessage — including ones
    // that just resolved a pending. This way SNAPSHOT/STATE/PRESET responses
    // flow through the same handler as their unsolicited-broadcast counterparts.
    // Pure data responses (CONFIG, DEVICES) and acks (OK) hit the App's switch
    // default and no-op.
    onMessage(msg, param);
}

// Constructor
export function createWs(
    onMessage: (msg: string, param: string) => void,
    onConnect: () => void,
    onDisconnect: () => void,
): () => void {
    let timer: ReturnType<typeof setTimeout> | null = null;
    let stopped = false;
    let retryMs = 500;                 // snappy first retry (reload ~200-500ms)
    const RETRY_CAP = 8000;            // polite upper bound when server is truly down

    function connect() {
        const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(`${proto}//${window.location.host}/ws`);
        ws.onopen = () => { retryMs = 500; onConnect(); };          // reset backoff on success
        ws.onmessage = (e) => { handleLine(e.data, onMessage); };
        ws.onerror = () => { /* swallow — onclose triggers reconnect */ };
        ws.onclose = () => {
            for (const p of pending) p.reject(new Error('disconnected'));
            pending.length = 0;
            ws = null;
            onDisconnect();
            if (!stopped) {
                timer = setTimeout(connect, retryMs);
                retryMs = Math.min(retryMs * 2, RETRY_CAP);         // exponential backoff, capped
            }
        };
    }
    connect();
    return () => { stopped = true; ws?.close(); if (timer) clearTimeout(timer); };
}

//---------- Handlers ----------//
export async function sendChains(chains: object[]): Promise<boolean> {
    const chainsStr = JSON.stringify(chains);
    return (await sendWs(`CHAINS ${chainsStr}`))[0];
}

// Typed wire: send `true`/`false` for bool params, numbers for numeric ones —
// matches the server's strict ParamValue parsing (Bool → Int → Float → Action).
// UI knows the declared type from each node's `params_info`, so it sends the
// right variant; the server doesn't coerce.
//
// Server replies with `STATE <state>` (not OK) — it knows authoritatively
// whether the snapshot transitioned to Dirty, so the UI lets the response
// drive isDirty/isComparing rather than guessing optimistically.
export async function sendSet(path: string, value: number | boolean): Promise<boolean> {
    return (await sendWs(`SET ${path} ${value}`, 'STATE'))[0];
}

export async function sendAction(target: string, action: string): Promise<boolean> {
    return (await sendWs(`SET ${target} ${action}`))[0];
}

// Meta override — same `SET` verb as values; three-segment path discriminates
// from value (2-segment) / action (2-segment, non-parseable value).
// e.g. sendParamMeta('04-chorus', 'depth_ms', 'visible', false)
//      → wire: `SET 04-chorus.depth_ms.visible false`
//
// `confirmed` opts into a reload the master would otherwise refuse (when the
// edit widens a non-growable max). On `false`, server may reply with
// `ERR confirm_required:...` — caller surfaces a popup and re-sends with
// `confirmed=true`. Result: `{ok, confirmRequired}` — `confirmRequired` is
// set only when the server refused pending acknowledgement.
export async function sendParamMeta(
    nodeKey: string, param: string, aspect: string, value: number | boolean,
    confirmed: boolean = false,
): Promise<{ ok: boolean; confirmRequired: boolean }> {
    const flag = confirmed ? ' --confirmed' : '';
    const [ok, msg] = await sendWs(`SET ${nodeKey}.${param}.${aspect} ${value}${flag}`, 'STATE');
    return { ok, confirmRequired: !ok && (msg?.startsWith('confirm_required:') ?? false) };
}

export async function savePreset(n: number):Promise<boolean> {
    return (await sendWs(`SAVE_PRESET ${n}`))[0];
}

// Switch to preset `n`. Server replies with SNAPSHOT (originator is filtered
// out of the broadcast), which `handleLine` also dispatches to `onMessage`,
// so the SNAPSHOT case in App.tsx applies the new state automatically.
export async function sendProgram(n: number): Promise<boolean> {
    return (await sendWs(`PRESET ${n}`, 'SNAPSHOT'))[0];
}

// Toggle compare-mode. Same response shape as sendProgram.
export async function sendCompare(): Promise<boolean> {
    return (await sendWs('COMPARE', 'SNAPSHOT'))[0];
}

export async function fetchConfig(): Promise<AudioConfig|null> {
    return fetchWs<AudioConfig>('FETCH_CONFIG', 'CONFIG');
}

export async function saveConfig(cfg: AudioConfig): Promise<boolean> {
    const value = JSON.stringify(cfg);
    return (await sendWs(`SAVE_CONFIG ${value}`))[0];
}

/// Canonical (firmware-declared) `ParamInfo` per effect type. Absolute envelope
/// for the Type-overrides editor — overrides clamp against these.
export async function fetchCanonical(): Promise<Record<string, ParamInfo[]> | null> {
    return fetchWs<Record<string, ParamInfo[]>>('FETCH_CANONICAL', 'CANONICAL');
}

/// Fetch current Type overrides from the full config patch.
export async function fetchTypeOverrides(): Promise<TypeOverrides | null> {
    const cfg = await fetchWs<{ type_overrides?: TypeOverrides }>('FETCH_CONFIG', 'CONFIG');
    return cfg?.type_overrides ?? {};
}

/// Replace the full Type-overrides map. Master refreshes `params_info` on
/// every existing instance and broadcasts the new resolved preset.
///
/// `confirmed` opts into a reload the master would otherwise refuse (when
/// the change widens a non-growable max on an existing instance). On
/// `false`, server may reply with `ERR confirm_required:...` — caller
/// surfaces a popup and re-sends with `confirmed=true`.
export async function saveTypeOverrides(
    overrides: TypeOverrides, confirmed: boolean = false,
): Promise<{ ok: boolean; confirmRequired: boolean }> {
    const flag = confirmed ? '--confirmed ' : '';
    const value = JSON.stringify({ type_overrides: overrides });
    const [ok, msg] = await sendWs(`SAVE_CONFIG ${flag}${value}`);
    return { ok, confirmRequired: !ok && (msg?.startsWith('confirm_required:') ?? false) };
}

export async function deletePreset(n: number): Promise<boolean> {
    return (await sendWs(`DELETE_PRESET ${n}`))[0];
}

export async function fetchDevices(): Promise<DeviceMap | null> {
    return fetchWs<DeviceMap>('FETCH_DEVICES', 'DEVICES');
}

export async function putDevice(alias: string, def: object): Promise<boolean> { // @todo check if alias contains \W
    const value = JSON.stringify(def);
    return (await sendWs(`PUT_DEVICE ${alias} ${value}`))[0];
}

export async function renameDevice(oldAlias: string, newAlias: string): Promise<boolean> { // @todo check if alias contains \W
    return (await sendWs(`SET_DEVICE_NAME ${oldAlias} ${newAlias}`))[0];
}

export async function deleteDevice(alias: string): Promise<boolean> {
    return (await sendWs(`DELETE_DEVICE ${alias}`))[0];
}

export async function sendReload(): Promise<boolean> {
    return (await sendWs('RELOAD'))[0];
}

export async function putControllers(controllers: ControllerDef[]): Promise<boolean> {
    const value = JSON.stringify(controllers);
    return (await sendWs(`PUT_CONTROLLERS ${value}`))[0];
}
