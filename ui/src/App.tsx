import { useEffect, useRef, useState } from 'react';
import { AppState, ChainDef, DeviceMap, NodeDef } from './types';
import { ChainView } from './components/ChainView';
import { Toasts, Toast } from './components/Toasts';
import { ChainRoutingPopup } from './components/ChainRoutingPopup';
import { SettingsPopup } from './components/SettingsPopup';
import { DevicesPage } from './components/DevicesPage';
import { fetchState, fetchPresets, fetchConfig, fetchDevices, setParam, patchChains, savePreset, saveConfig, switchPreset, deletePreset, createWs } from './api';
import { t } from './i18n';

function DevicesIcon() {
  return (
    <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <rect x="1" y="4" width="14" height="8" rx="1.5" />
      <line x1="4" y1="4" x2="4" y2="12" />
      <circle cx="8" cy="8" r="1.5" fill="currentColor" stroke="none" />
    </svg>
  );
}

function getInitialTheme() {
  return localStorage.getItem('theme') === 'light' ? 'light' : 'dark';
}

export default function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [connected, setConnected] = useState(false);
  const [theme, setTheme] = useState(getInitialTheme);

  // Audio config (channel counts etc.)
  const [audioConfig, setAudioConfig] = useState({ in_channels: 2, out_channels: 2, sample_rate: 48000, buffer_size: 256, device: 'default', delay_max_seconds: 2.0 });

  // Chain routing popup: index into state.chains
  const [routingIdx, setRoutingIdx] = useState<number | null>(null);

  // Available presets + active preset
  const [presets, setPresets] = useState<number[]>([]);
  const [activePreset, setActivePreset] = useState(1);

  // Devices map (for controller mappings)
  const [devices, setDevices] = useState<DeviceMap>({});

  // Dirty flag: unsaved param changes
  const [isDirty, setIsDirty] = useState(false);

  // Page routing — hash-based: #devices / '' = home
  function pageFromHash(): 'home' | 'devices' {
    return window.location.hash === '#devices' ? 'devices' : 'home';
  }
  const [page, setPage] = useState<'home' | 'devices'>(pageFromHash);

  useEffect(() => {
    const handler = () => setPage(pageFromHash());
    window.addEventListener('hashchange', handler);
    return () => window.removeEventListener('hashchange', handler);
  }, []);

  function navigateTo(p: 'home' | 'devices') {
    window.location.hash = p === 'devices' ? 'devices' : '';
  }

  // Settings popup
  const [showSettings, setShowSettings] = useState(false);

  // Save preset popup
  const [showSavePopup, setShowSavePopup] = useState(false);
  const [savePresetNum, setSavePresetNum] = useState(1);
  const [savedFeedback, setSavedFeedback] = useState(false);

  // Toasts
  const [toasts, setToasts] = useState<Toast[]>([]);
  const toastId = useRef(0);

  function addToast(msg: string) {
    const id = ++toastId.current;
    setToasts(prev => [...prev, { id, msg, fading: false }]);
    setTimeout(() => setToasts(prev => prev.map(t => t.id === id ? { ...t, fading: true } : t)), 9500);
    setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 10000);
  }

  function dismissToast(id: number) {
    setToasts(prev => prev.filter(t => t.id !== id));
  }

  // Delete preset confirm state
  const [confirmDeletePreset, setConfirmDeletePreset] = useState(false);

  // New chain form state
  const [showNewChain, setShowNewChain] = useState(false);
  const [newChainInput, setNewChainInput] = useState('1,1');
  const [newChainOutput, setNewChainOutput] = useState('1,2');

  useEffect(() => {
    document.body.classList.toggle('light', theme === 'light');
    localStorage.setItem('theme', theme);
  }, [theme]);

  // Keep a ref to state for ws callback (avoids stale closure)
  const stateRef = useRef<AppState | null>(null);
  stateRef.current = state;

  // Apply fetched state — also picks up the server-side dirty flag so it survives reloads.
  const applyFetchedState = (data: any) => {
    setState({ chains: data.chains ?? [] });
    if (typeof data.is_dirty === 'boolean') setIsDirty(data.is_dirty);
  };

  useEffect(() => {
    fetchState().then(applyFetchedState);
    fetchDevices().then(setDevices);
    fetchConfig().then(cfg => setAudioConfig({ in_channels: cfg.in_channels, out_channels: cfg.out_channels, sample_rate: cfg.sample_rate, buffer_size: cfg.buffer_size, device: cfg.device, delay_max_seconds: cfg.delay_max_seconds }));
    fetchPresets().then(({ presets, active }) => {
      setPresets(presets);
      if (active !== 0) setActivePreset(active);
      else if (presets.length > 0) setActivePreset(presets[0]);
    });

    const cleanup = createWs(
      (msg) => {
        if (msg.type === 'set' && stateRef.current) {
          const [nodeKey, param] = (msg.path as string).split('.');
          if (!nodeKey || !param) return;
          setIsDirty(true);
          setState(prev => {
            if (!prev) return prev;
            return {
              ...prev,
              chains: prev.chains.map(chain => ({
                ...chain,
                nodes: chain.nodes.map(node =>
                  node.key === nodeKey ? { ...node, [param]: msg.value } : node
                ),
              })),
            };
          });
        } else if (msg.type === 'node_event') {
          const { key, event, data } = msg as { key: string; event: string; data: Record<string, unknown> };
          if (event === 'looper_state') {
            const { state: looperState, loop_ms, pos_ms, overdub_count } = data as { state: string; loop_ms: number; pos_ms: number; overdub_count: number };
            setState(prev => {
              if (!prev) return prev;
              return {
                ...prev,
                chains: prev.chains.map(chain => ({
                  ...chain,
                  nodes: chain.nodes.map(node =>
                    node.key === key
                      ? { ...node, state: looperState, loop_secs: loop_ms / 1000, pos_secs: pos_ms / 1000, overdub_count }
                      : node
                  ),
                })),
              };
            });
          } else if (event === 'loop_wrap') {
            // Sync JS timer to 0 at loop boundary.
            // _wrap_ts always changes (even when pos_secs is already 0) so the useEffect re-runs.
            setState(prev => {
              if (!prev) return prev;
              return {
                ...prev,
                chains: prev.chains.map(chain => ({
                  ...chain,
                  nodes: chain.nodes.map(node =>
                    node.key === key ? { ...node, pos_secs: 0, _wrap_ts: Date.now() } : node
                  ),
                })),
              };
            });
          }
        } else if (msg.type === 'preset') {
          if (msg.n) setActivePreset(Number(msg.n));
          applyFetchedState(msg);
        } else if (msg.type === 'reset') {
          fetchState().then(applyFetchedState);
        }
      },
      () => setConnected(true),
      () => setConnected(false),
    );
    return cleanup;
  }, []);

  const handleSet = (path: string, value: number | boolean) => {
    const [nodeKey, param] = path.split('.');
    if (!nodeKey || !param) return;
    setIsDirty(true);
    setState(prev => {
      if (!prev) return prev;
      return {
        ...prev,
        chains: prev.chains.map(chain => ({
          ...chain,
          nodes: chain.nodes.map(node =>
            node.key === nodeKey ? { ...node, [param]: value } : node
          ),
        })),
      };
    });
    setParam(path, value);
  };

  const handleDelete = (nodeKey: string) => {
    const prev = stateRef.current;
    if (!prev) return;
    const next = {
      ...prev,
      chains: prev.chains.map(chain => ({
        ...chain,
        nodes: chain.nodes.filter(n => n.key !== nodeKey),
      })),
    };
    setState(next);
    setIsDirty(true);
    patchChains(next.chains).then(ok => {
      if (!ok) { setState(prev); addToast(t('error.delete_node')); }
    });
  };

  const handleReorder = (chainIdx: number, newNodes: NodeDef[]) => {
    const prev = stateRef.current;
    if (!prev) return;
    const next = {
      ...prev,
      chains: prev.chains.map((chain, i) => i === chainIdx ? { ...chain, nodes: newNodes } : chain),
    };
    setState(next);
    setIsDirty(true);
    patchChains(next.chains).then(ok => {
      if (!ok) { setState(prev); addToast(t('error.reorder')); }
    });
  };

  const handleAddNode = (chainIdx: number, node: NodeDef) => {
    const prev = stateRef.current;
    if (!prev) return;
    const next = {
      ...prev,
      chains: prev.chains.map((chain, i) =>
        i === chainIdx ? { ...chain, nodes: [...chain.nodes, node] } : chain
      ),
    };
    setState(next);
    setIsDirty(true);
    patchChains(next.chains).then(ok => {
      if (!ok) { setState(prev); addToast(t('error.add_node')); }
    });
  };

  const handleRoutingApply = (chainIdx: number, updated: ChainDef) => {
    const prev = stateRef.current;
    if (!prev) return;
    const next = { ...prev, chains: prev.chains.map((c, i) => i === chainIdx ? updated : c) };
    setState(next);
    setRoutingIdx(null);
    patchChains(next.chains).then(ok => {
      if (!ok) { setState(prev); addToast(t('error.routing')); }
    });
  };

  const handleDeleteChain = (chainIdx: number) => {
    const prev = stateRef.current;
    if (!prev) return;
    const next = { ...prev, chains: prev.chains.filter((_, i) => i !== chainIdx) };
    setState(next);
    setIsDirty(true);
    patchChains(next.chains).then(ok => {
      if (!ok) { setState(prev); addToast(t('error.delete_chain')); }
    });
  };

  function parseChannels(s: string): [number, number] {
    const parts = s.split(',').map(p => parseInt(p.trim(), 10)).filter(n => !isNaN(n));
    if (parts.length === 1) return [parts[0], parts[0]];
    if (parts.length >= 2) return [parts[0], parts[1]];
    return [1, 1];
  }

  const handleNewChain = () => {
    if (!state) return;
    const newChain: ChainDef = {
      input: parseChannels(newChainInput),
      output: parseChannels(newChainOutput),
      nodes: [],
    };
    const next = { ...state, chains: [...state.chains, newChain] };
    setState(next);
    patchChains(next.chains);
    setShowNewChain(false);
  };

  function handleOpenNewChain() {
    setNewChainInput('1,1');
    setNewChainOutput('1,2');
    setShowNewChain(true);
  }

  const handleDeletePreset = async () => {
    const ok = await deletePreset(activePreset);
    if (!ok) return;
    setConfirmDeletePreset(false);
    const remaining = presets.filter(n => n !== activePreset);
    setPresets(remaining);
    if (remaining.length > 0) {
      const next = remaining[0];
      setActivePreset(next);
      switchPreset(next);
      fetchState().then(applyFetchedState);
    } else {
      setState({ chains: [] });
    }
  };

  const handleSwitchPreset = (n: number) => {
    if (!presets.includes(n)) {
      addToast(t('error.preset_missing', n));
      return;
    }
    setActivePreset(n);
    setIsDirty(false);
    switchPreset(n);
  };

  const handleOpenSavePopup = () => {
    setSavePresetNum(activePreset);
    setShowSavePopup(true);
  };

  const handleConfirmSave = async () => {
    setShowSavePopup(false);
    await savePreset(savePresetNum);
    setActivePreset(savePresetNum);
    setIsDirty(false);
    setSavedFeedback(true);
    setTimeout(() => setSavedFeedback(false), 2000);
    fetchPresets().then(({ presets }) => setPresets(presets));
  };

  const handleQuickSave = async () => {
    await savePreset(activePreset);
    setIsDirty(false);
    setSavedFeedback(true);
    setTimeout(() => setSavedFeedback(false), 2000);
  };

  const routingChain = routingIdx !== null && state ? state.chains[routingIdx] : null;

  if (page === 'devices') {
    return <DevicesPage onHome={() => navigateTo('home')} />;
  }

  return (
    <div className="app">
      <header className="app-header">
        <h1 className="app-title-link" onClick={() => navigateTo('home')}>multi-effect</h1>
        <div className={`status ${connected ? 'connected' : 'disconnected'}`}>
          <span className="status-dot" />
          {connected ? t('ui.live') : t('ui.reconnecting')}
        </div>
        <Toasts toasts={toasts} onDismiss={dismissToast} />
        <div className="header-preset">
          <label className="preset-label">{t('ui.preset')}</label>
          <select
            value={activePreset}
            onChange={e => handleSwitchPreset(Number(e.target.value))}
            className="preset-select"
          >
            {presets.map(n => (
              <option key={n} value={n}>{n === activePreset && isDirty ? `${n}*` : n}</option>
            ))}
          </select>
          <button className="preset-save-btn" onClick={handleQuickSave} title={t('ui.save_quick')}>
            {savedFeedback ? t('ui.saved') : t('ui.save_quick')}
          </button>
          <button className="preset-save-btn" onClick={handleOpenSavePopup} title={t('ui.save')}>
            {t('ui.save')}
          </button>
          <button className="devices-btn" onClick={() => navigateTo('devices')} title={t('ui.devices')}>
            <DevicesIcon />
          </button>
          <button className="settings-btn" onClick={() => setShowSettings(true)} title={t('ui.settings')}>⚙</button>
          <button className="theme-btn" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}>
            {theme === 'dark' ? '🌙' : '☀️'}
          </button>
        </div>
      </header>

      {showSavePopup && (
        <div className="popup-overlay" onClick={() => setShowSavePopup(false)}>
          <div className="popup" onClick={e => e.stopPropagation()}>
            <p className="popup-title">{t('ui.save_preset_title')}</p>
            <div className="popup-row">
              <label>{t('ui.preset_number')}</label>
              <input
                type="number"
                min={1}
                max={127}
                value={savePresetNum}
                onChange={e => setSavePresetNum(Number(e.target.value))}
                className="preset-input"
                autoFocus
              />
            </div>
            <div className="popup-actions">
              <button className="popup-confirm" onClick={handleConfirmSave}>{t('ui.save')}</button>
              <button className="popup-cancel" onClick={() => setShowSavePopup(false)}>{t('ui.cancel')}</button>
            </div>
          </div>
        </div>
      )}
      {showSettings && (
        <SettingsPopup
          config={audioConfig}
          onSave={async (cfg) => {
            const ok = await saveConfig(cfg);
            if (ok) {
              setAudioConfig(cfg);
              if (state && cfg.delay_max_seconds < audioConfig.delay_max_seconds) {
                state.chains.forEach(chain => chain.nodes.forEach(node => {
                  if (node.type === 'delay' && typeof node.time === 'number' && node.time > cfg.delay_max_seconds) {
                    handleSet(`${node.key}.time`, cfg.delay_max_seconds);
                  }
                }));
              }
            }
            return ok;
          }}
          onClose={() => setShowSettings(false)}
        />
      )}
      {routingChain && routingIdx !== null && (
        <ChainRoutingPopup
          chain={routingChain}
          inChannels={audioConfig.in_channels}
          outChannels={audioConfig.out_channels}
          onApply={(updated) => handleRoutingApply(routingIdx, updated)}
          onClose={() => setRoutingIdx(null)}
        />
      )}

      <main>
        {!state && <div className="loading">{t('ui.loading')}</div>}
        {state?.chains.map((chain, chainIdx) => (
          <ChainView
            key={chainIdx}
            chainIdx={chainIdx}
            chain={chain}
            presetName={String(activePreset)}
            devices={devices}
            allNodes={state.chains.flatMap(c => c.nodes)}
            delayMaxSeconds={audioConfig.delay_max_seconds}
            onSet={handleSet}
            onDelete={handleDelete}
            onReorder={handleReorder}
            onAddNode={handleAddNode}
            onDeleteChain={handleDeleteChain}
            onRouting={setRoutingIdx}
          />
        ))}

        {/* Bottom bar: new chain (left) + delete preset (right) */}
        <div className="preset-bottom-bar">
          {!showNewChain ? (
            <button className="new-chain-btn" onClick={handleOpenNewChain}>{t('ui.new_chain')}</button>
          ) : (
            <div className="new-chain-form">
              <label>{t('ui.input_ch')}</label>
              <input
                type="text"
                value={newChainInput}
                onChange={e => setNewChainInput(e.target.value)}
                placeholder="1,1"
              />
              <label>{t('ui.output_ch')}</label>
              <input
                type="text"
                value={newChainOutput}
                onChange={e => setNewChainOutput(e.target.value)}
                placeholder="1,2"
              />
              <button onClick={handleNewChain}>{t('ui.create')}</button>
              <button onClick={() => setShowNewChain(false)}>{t('ui.cancel')}</button>
            </div>
          )}
          {confirmDeletePreset ? (
            <div className="chain-confirm-group">
              <span className="chain-confirm-text">{t('ui.confirm_delete_preset')}</span>
              <button className="chain-confirm-yes" onClick={handleDeletePreset}>✓</button>
              <button className="chain-confirm-no" onClick={() => setConfirmDeletePreset(false)}>✗</button>
            </div>
          ) : (
            <button
              className="new-chain-btn"
              onClick={() => state?.chains.length === 0 ? handleDeletePreset() : setConfirmDeletePreset(true)}
            >{t('ui.delete_preset')}</button>
          )}
        </div>
      </main>
    </div>
  );
}
