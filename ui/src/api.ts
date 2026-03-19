export async function fetchState() {
  const res = await fetch('/api/state');
  return res.json();
}

export async function fetchConfig(): Promise<{
  in_channels: number; out_channels: number;
  sample_rate: number; buffer_size: number; device: string;
  delay_max_seconds: number; looper_max_seconds: number;
}> {
  const res = await fetch('/api/config');
  return res.json();
}

export async function fetchPresets(): Promise<{ presets: number[]; active: number }> {
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
  device: string;
  in_channels: number;
  out_channels: number;
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
