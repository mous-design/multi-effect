export async function fetchState() {
  const res = await fetch('/api/state');
  return res.json();
}

export async function postAction(target: string, action: string): Promise<boolean> {
  const res = await fetch('/api/action', {
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
  const res = await fetch('/api/config');
  return res.json();
}

export async function fetchPresetDefs(): Promise<{ presets: number[]; active: number }> {
  const res = await fetch('/api/presets');
  const data = await res.json();
  return { presets: data.presets ?? [], active: data.active ?? 0 };
}

export async function patchChains(chains: object[]): Promise<boolean> {
  const res = await fetch('/api/patch', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ chains }),
  });
  return res.ok;
}

export function setParam(path: string, value: number | boolean) {
  fetch('/api/set', {
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
  const res = await fetch('/api/config', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(cfg),
  });
  return res.ok;
}

export async function reloadConfig(): Promise<boolean> {
  const res = await fetch('/api/reload', { method: 'POST' });
  return res.ok;
}

export function savePreset(n: number) {
  return fetch(`/api/preset/${n}/save`, { method: 'POST' });
}

export function switchPreset(n: number) {
  fetch(`/api/preset/${n}`, { method: 'POST' });
}

export async function deletePreset(n: number): Promise<boolean> {
  const res = await fetch(`/api/preset/${n}`, { method: 'DELETE' });
  return res.ok;
}

export async function fetchDevices(): Promise<Record<string, any>> {
  const res = await fetch('/api/devices');
  return res.json();
}

export async function putDevice(alias: string, def: object): Promise<boolean> {
  const res = await fetch(`/api/devices/${encodeURIComponent(alias)}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(def),
  });
  return res.ok;
}

export async function fetchControllers(n: number): Promise<import('./types').ControllerDef[]> {
  const res = await fetch(`/api/preset/${n}/controllers`);
  if (!res.ok) return [];
  return res.json();
}

export async function putControllers(n: number, controllers: import('./types').ControllerDef[]): Promise<boolean> {
  const res = await fetch(`/api/preset/${n}/controllers`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(controllers),
  });
  return res.ok;
}

export async function renameDevice(oldAlias: string, newAlias: string, def: object): Promise<boolean> {
  const res = await fetch(`/api/devices/${encodeURIComponent(oldAlias)}/rename`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ new_alias: newAlias, ...def }),
  });
  return res.ok;
}

export async function deleteDevice(alias: string): Promise<boolean> {
  const res = await fetch(`/api/devices/${encodeURIComponent(alias)}`, { method: 'DELETE' });
  return res.ok;
}

export async function postCompare(): Promise<boolean> {
  const res = await fetch('/api/compare', { method: 'POST' });
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

  function connect() {
    const proto = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    ws = new WebSocket(`${proto}//${window.location.host}/ws`);
    ws.onopen = onConnect;
    ws.onmessage = (e) => { try { onMessage(JSON.parse(e.data)); } catch {} };
    ws.onclose = () => {
      onDisconnect();
      if (!stopped) timer = setTimeout(connect, 3000);
    };
  }
  connect();
  return () => { stopped = true; ws?.close(); if (timer) clearTimeout(timer); };
}
