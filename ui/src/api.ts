// Global error handler — set by App on mount.
let onError: (msg: string) => void = console.error;
export function setApiErrorHandler(fn: (msg: string) => void) { onError = fn; }

async function api(url: string, init?: RequestInit): Promise<Response> {
    const res = await fetch(url, init).catch(e => { onError(e.message); throw e; });
    if (!res.ok) {
        const body = await res.json().catch(() => null);
        onError(body?.error ?? `${init?.method ?? 'GET'} ${url}: ${res.status}`);
    }
    return res;
}

export async function postAction(target: string, action: string): Promise<boolean> {
    const res = await api('/api/action', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ target, action }),
    });
    return res.ok;
}

export async function fetchConfig(): Promise<{
    in_channels: number; out_channels: number;
    sample_rate: number; buffer_size: number; audio_device: string;
    delay_max_seconds: number; looper_max_seconds: number;
}> {
    const res = await api('/api/config');
    return res.json();
}

export async function setChains(chains: object[]): Promise<boolean> {
    const res = await api('/api/chains', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ chains }),
    });
    return res.ok;
}

export function setParam(path: string, value: number | boolean) {
    api('/api/set', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ path, value }),
    });
}

export async function saveConfig(cfg: {
    sample_rate: number;
    buffer_size: number;
    audio_device: string;
    in_channels: number;
    out_channels: number;
    delay_max_seconds: number;
}): Promise<boolean> {
    const res = await api('/api/config', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(cfg),
    });
    return res.ok;
}

export async function reloadConfig(): Promise<boolean> {
    const res = await api('/api/reload', { method: 'POST' });
    return res.ok;
}

export function savePreset(n: number) {
    return api(`/api/preset/${n}/save`, { method: 'POST' });
}

export async function switchPreset(n: number): Promise<boolean> {
    const res = await api(`/api/preset/${n}`, { method: 'POST' });
    return res.ok;
}

export async function deletePreset(n: number): Promise<boolean> {
    const res = await api(`/api/preset/${n}`, { method: 'DELETE' });
    return res.ok;
}

export async function fetchDevices(): Promise<Record<string, any>> {
    const res = await api('/api/devices');
    return res.json();
}

export async function putDevice(alias: string, def: object): Promise<boolean> {
    const res = await api(`/api/devices/${encodeURIComponent(alias)}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(def),
    });
    return res.ok;
}

export async function putControllers(controllers: import('./types').ControllerDef[]): Promise<boolean> {
    const res = await api(`/api/controllers`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(controllers),
    });
    return res.ok;
}

export async function renameDevice(oldAlias: string, newAlias: string, def: object): Promise<boolean> {
    const res = await api(`/api/devices/${encodeURIComponent(oldAlias)}/rename`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ new_alias: newAlias, ...def }),
    });
    return res.ok;
}

export async function deleteDevice(alias: string): Promise<boolean> {
    const res = await api(`/api/devices/${encodeURIComponent(alias)}`, { method: 'DELETE' });
    return res.ok;
}

export async function postCompare(): Promise<boolean> {
    const res = await api('/api/compare', { method: 'POST' });
    return res.ok;
}

export function createWs(
    onMessage: (data: any) => void,
    onConnect: () => void,
    onDisconnect: () => void,
): () => void {
    let ws: WebSocket | null = null;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let stopped = false;
    let retryMs = 500;                 // snappy first retry (reload ~200-500ms)
    const RETRY_CAP = 8000;            // polite upper bound when server is truly down

    function connect() {
        const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        ws = new WebSocket(`${proto}//${window.location.host}/ws`);
        ws.onopen = () => { retryMs = 500; onConnect(); };          // reset backoff on success
        ws.onmessage = (e) => { try { onMessage(JSON.parse(e.data)); } catch { } };
        ws.onerror = () => { /* swallow — onclose triggers reconnect */ };
        ws.onclose = () => {
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
