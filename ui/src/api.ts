import type { ControllerDef, AudioConfig, DeviceMap } from './types';
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
// Set default error handler to consoele. Overload with setApiErrorHandler()
let onError: (msg: string) => void = console.error;
// Global error handler — set by App on mount.
export function setApiErrorHandler(fn: (msg: string) => void) { onError = fn; }

//---------- Websocket ----------//
let ws: WebSocket|null = null;
type Pending = {expect: string; resolve: (value:string|null) => void; reject: (e: Error) => void};
const pending: Pending[] = [];

// Send a line. Drops silently if not connected — UI will resync on reconnect
// via the SNAPSHOT line the server sends on handshake.
function sendWs(line: string, expect: string = 'OK'): Promise<[boolean, string|null]> {
    return new Promise(resolve => {
        if (!ws || ws.readyState !== WebSocket.OPEN) {
            onError('not connected');
            resolve([false, null]);
            return;
        }
        pending.push({
            expect,
            resolve: (value: string|null) => {
                resolve([true, value]);
            }, reject: e => {
                onError(e.message);
                resolve([false, null]);
            }
        });
        ws.send(line);
    });
}

async function fetchWs<T>(command: string, expect: string): Promise<T | null> {
    const [ok, value] = await sendWs(command, expect);
    if (!ok || value === null) return null;
    try { return JSON.parse(value) as T; } catch { onError(`bad ${expect} payload`); return null; }
}

function handleLine(line: string, onMessage: (msg: string, param: string) => void) {
    let [msg, param] = splitN(line, ' ', 2);
    if (msg === 'ERR') {
        pending.shift()?.reject(new Error(param));
        return;
    }
    // Resolve a pending request whose expected response type matches.
    // SNAPSHOT is special: it's also the canonical "apply current state" event,
    // so it ALSO goes through onMessage even when received as a response.
    // Other typed responses (CONFIG, DEVICES) skip onMessage — only the caller
    // wants the data.
    if (pending.length && pending[0].expect === msg) {
        pending.shift()?.resolve(param);
        if (msg === 'SNAPSHOT') onMessage(msg, param);
        return;
    }
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

export async function sendSet(path: string, value: number | boolean): Promise<boolean> {
    return (await sendWs(`SET ${path} ${value}`))[0];
}

export async function sendAction(target: string, action: string): Promise<boolean> {
    return (await sendWs(`SET ${target} ${action}`))[0];
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
