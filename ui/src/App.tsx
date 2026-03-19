import { useEffect, useRef, useState } from 'react';
import { AppState, ChainDef, NodeDef } from './types';
import { ChainView } from './components/ChainView';
import { Toasts, Toast } from './components/Toasts';
import { ChainRoutingPopup } from './components/ChainRoutingPopup';
import { SettingsPopup } from './components/SettingsPopup';
import { fetchState, fetchPresets, fetchConfig, setParam, patchChains, savePreset, saveConfig, switchPreset, createWs } from './api';
import { t } from './i18n';

function getInitialTheme() {
  return localStorage.getItem('theme') === 'light' ? 'light' : 'dark';
}

export default function App() {
  const [state, setState] = useState<AppState | null>(null);
  const [connected, setConnected] = useState(false);
  const [theme, setTheme] = useState(getInitialTheme);

  // Audio config (channel counts etc.)
  const [audioConfig, setAudioConfig] = useState({ in_channels: 2, out_channels: 2, sample_rate: 48000, buffer_size: 256, device: 'default' });

  // Chain routing popup: index into state.chains
  const [routingIdx, setRoutingIdx] = useState<number | null>(null);

  // Available presets + active preset
  const [presets, setPresets] = useState<number[]>([]);
  const [activePreset, setActivePreset] = useState(1);

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

  useEffect(() => {
    fetchState().then(setState);
    fetchConfig().then(cfg => setAudioConfig({ in_channels: cfg.in_channels, out_channels: cfg.out_channels, sample_rate: cfg.sample_rate, buffer_size: cfg.buffer_size, device: cfg.device }));
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
        } else if (msg.type === 'preset') {
          if (msg.n) setActivePreset(Number(msg.n));
          fetchState().then(setState);
        } else if (msg.type === 'reset') {
          fetchState().then(setState);
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

  const handleSwitchPreset = (n: number) => {
    if (!presets.includes(n)) {
      addToast(t('error.preset_missing', n));
      return;
    }
    setActivePreset(n);
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
    setSavedFeedback(true);
    setTimeout(() => setSavedFeedback(false), 2000);
    fetchPresets().then(({ presets }) => setPresets(presets));
  };

  const routingChain = routingIdx !== null && state ? state.chains[routingIdx] : null;

  return (
    <div className="app">
      <header className="app-header">
        <h1>multi-effect</h1>
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
              <option key={n} value={n}>{n}</option>
            ))}
          </select>
          <button className="preset-save-btn" onClick={handleOpenSavePopup}>
            {savedFeedback ? t('ui.saved') : t('ui.save')}
          </button>
          <button className="settings-btn" onClick={() => setShowSettings(true)} title={t('ui.settings')}>⚙</button>
        </div>
        <button className="theme-btn" onClick={() => setTheme(t => t === 'dark' ? 'light' : 'dark')}>
          {theme === 'dark' ? '🌙' : '☀️'}
        </button>
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
            if (ok) setAudioConfig(cfg);
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
            onSet={handleSet}
            onDelete={handleDelete}
            onReorder={handleReorder}
            onAddNode={handleAddNode}
            onDeleteChain={handleDeleteChain}
            onRouting={setRoutingIdx}
          />
        ))}

        {/* New chain */}
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
      </main>
    </div>
  );
}
