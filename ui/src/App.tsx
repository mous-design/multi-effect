import { useEffect, useRef, useState } from 'react';
import { AppState, ChainDef, ControllerDef, NodeDef } from './types';
import { sendSet, sendChains, savePreset, saveConfig, sendProgram, deletePreset, sendCompare } from './api';
import { t } from './i18n';
import { useToasts } from './hooks/useToasts';
import { useTheme } from './hooks/useTheme';
import { useConnection } from './hooks/useConnection';
import { AppHeader } from './components/AppHeader';
import { SavePresetPopup } from './components/SavePresetPopup';
import { BottomBar } from './components/BottomBar';
import { ChainView } from './components/ChainView';
import { ChainRoutingPopup } from './components/ChainRoutingPopup';
import { SettingsPopup } from './components/SettingsPopup';
import { DevicesPage } from './components/DevicesPage';
import { splitN } from './api';

export default function App() {

    // --- Hooks ---
    const { toasts, addToast, dismissToast } = useToasts();
    const { theme, toggleTheme } = useTheme();

    // --- Page navigation (hash-based) ---
    const pageFromHash = (): 'home' | 'devices' =>
        window.location.hash === '#devices' ? 'devices' : 'home';
    const [page, setPage] = useState<'home' | 'devices'>(pageFromHash);
    useEffect(() => {
        const handler = () => setPage(pageFromHash());
        window.addEventListener('hashchange', handler);
        return () => window.removeEventListener('hashchange', handler);
    }, []);
    const navigateTo = (p: 'home' | 'devices') => {
        window.location.hash = p === 'devices' ? 'devices' : '';
    };

    // --- Preset state ---
    const [presets, setPresetDefs] = useState<number[]>([]);
    const [activePreset, setActivePreset] = useState(1);
    const [isDirty, setIsDirty] = useState(false);
    const [isComparing, setIsComparing] = useState(false);
    const [savedFeedback, setSavedFeedback] = useState(false);
    const [showSavePopup, setShowSavePopup] = useState(false);
    const [savePresetNum, setSavePresetNum] = useState(1);

    // --- Controller state (from preset) ---
    const [controllers, setControllers] = useState<ControllerDef[]>([]);

    // --- Chain state ---
    const [state, setState] = useState<AppState | null>(null);
    const stateRef = useRef<AppState | null>(null);
    stateRef.current = state;
    const [routingIdx, setRoutingIdx] = useState<number | null>(null);
    const [showSettings, setShowSettings] = useState(false);

    // Apply a SNAPSHOT message body (full state replacement).
    // Used both for initial WS handshake and for PROGRAM/COMPARE responses.
    const applySnapshot = (snap: any) => {
        const preset = snap.preset ?? {};
        setState({ chains: preset.chains ?? [] });
        setControllers(preset.controllers ?? []);
        if (typeof preset.index === 'number') setActivePreset(preset.index);
        if (Array.isArray(snap.preset_indices)) setPresetDefs(snap.preset_indices);
        setIsDirty(snap.state === 'Dirty');
        setIsComparing(snap.state === 'Comparing');
    };

    // --- Connection (WS + config + devices) ---
    const { connected, audioConfig, setAudioConfig, devices } = useConnection((msg, params) => {
        switch(msg) {
            case 'SET': {
                const [path, valueStr] = splitN(params, ' ', 2);
                const [nodeKey, param] = splitN(path, '.', 2);
                if (!nodeKey || !param) return;
                // Parse numeric values; keep non-numeric (action strings) as-is.
                const num = Number(valueStr);
                const value: number | string = isFinite(num) ? num : valueStr;
                setIsDirty(true);
                setState(prev => {
                    if (!prev) return prev;
                    return {
                        ...prev, chains: prev.chains.map(chain => ({
                            ...chain,
                            nodes: chain.nodes.map(node =>
                                node.key === nodeKey ? {...node, [param]: value} : node
                            ),
                        }))
                    };
                });
                break;
            }
            case 'SNAPSHOT':
                applySnapshot(JSON.parse(params));
                break;
            case 'PRESET': {
                const preset = JSON.parse(params);
                setState({ chains: preset.chains ?? [] });
                setControllers(preset.controllers ?? []);
                if (typeof preset.index === 'number') setActivePreset(preset.index);
                break;
            }
            case 'STATE': {
                const s = params.trim();
                setIsDirty(s === 'Dirty');
                setIsComparing(s === 'Comparing');
                break;
            }
            case 'INDICES': {
                const indices = JSON.parse(params);
                if (Array.isArray(indices)) setPresetDefs(indices);
                break;
            }
            case 'EVENT':
                const [key, event, json] = splitN(params, ' ', 3);
                if (event === 'looper_state') {
                    const data = JSON.parse(json); 
                    const { state: ls, loop_ms, pos_ms, overdub_count } = data;
                    setState(prev => {
                        if (!prev) return prev;
                        return {
                            ...prev, chains: prev.chains.map(chain => ({
                                ...chain,
                                nodes: chain.nodes.map(node =>
                                    node.key === key
                                        ? { ...node, state: ls, loop_secs: loop_ms / 1000, pos_secs: pos_ms / 1000, overdub_count }
                                        : node
                                ),
                            }))
                        };
                    });
                } else if (event === 'loop_wrap') {
                    setState(prev => {
                        if (!prev) return prev;
                        return {
                            ...prev, chains: prev.chains.map(chain => ({
                                ...chain,
                                nodes: chain.nodes.map(node =>
                                    node.key === key ? { ...node, pos_secs: 0, _wrap_ts: Date.now() } : node
                                ),
                            }))
                        };
                    });
                }
                break;
        }
    });

    // --- Preset handlers ---

    const handleSwitchPreset = async (n: number) => {
        if (!presets.includes(n)) { addToast(t('error.preset_missing', n)); return; }
        if (await sendProgram(n)) {
            setActivePreset(n);
            setIsDirty(false);
            setIsComparing(false);
        }
    };

    const handleCompare = async () => { await sendCompare(); };

    const handleConfirmSave = async () => {
        setShowSavePopup(false);
        if (await savePreset(savePresetNum)) {
            setActivePreset(savePresetNum);
            setIsDirty(false);
            // Optimistically add to preset list (server filters out our INDICES broadcast).
            setPresetDefs(prev =>
                prev.includes(savePresetNum) ? prev : [...prev, savePresetNum].sort((a, b) => a - b),
            );
            setSavedFeedback(true);
            setTimeout(() => setSavedFeedback(false), 2000);
        }
    };

    const handleQuickSave = async () => {
        if (await savePreset(activePreset)) {
            setIsDirty(false);
            setPresetDefs(prev =>
                prev.includes(activePreset) ? prev : [...prev, activePreset].sort((a, b) => a - b),
            );
            setSavedFeedback(true);
            setTimeout(() => setSavedFeedback(false), 2000);
        }
    };

    const handleDeletePreset = async () => {
        if (await deletePreset(activePreset)) {
            const remaining = presets.filter(n => n !== activePreset);
            setPresetDefs(remaining);
            if (remaining.length > 0) {
                const next = remaining[0];
                setActivePreset(next);
                sendProgram(next);
            } else {
                setState({ chains: [] });
            }
        }
    };

    // --- Chain handlers ---

    const handleSet = (path: string, value: number | boolean) => {
        const [nodeKey, param] = splitN(path, '.', 2);
        if (!nodeKey || !param) return;
        setIsDirty(true);
        setState(prev => {
            if (!prev) return prev;
            return {
                ...prev, chains: prev.chains.map(chain => ({
                    ...chain,
                    nodes: chain.nodes.map(node =>
                        node.key === nodeKey ? { ...node, [param]: value } : node
                    ),
                }))
            };
        });
        // @todo Can we rollback the view?
        sendSet(path, value);
    };

    const handleDelete = (nodeKey: string) => {
        const prev = stateRef.current;
        if (!prev) return;
        const next = {
            ...prev, chains: prev.chains.map(chain => ({
                ...chain, nodes: chain.nodes.filter(n => n.key !== nodeKey),
            }))
        };
        setState(next);
        setIsDirty(true);
        sendChains(next.chains).then(ok => { if (!ok) setState(prev); });
    };

    const handleReorder = (chainIdx: number, newNodes: NodeDef[]) => {
        const prev = stateRef.current;
        if (!prev) return;
        const next = { ...prev, chains: prev.chains.map((chain, i) => i === chainIdx ? { ...chain, nodes: newNodes } : chain) };
        setState(next);
        setIsDirty(true);
        sendChains(next.chains).then(ok => { if (!ok) setState(prev); });
    };

    const handleAddNode = (chainIdx: number, node: NodeDef) => {
        const prev = stateRef.current;
        if (!prev) return;
        const next = {
            ...prev, chains: prev.chains.map((chain, i) =>
                i === chainIdx ? { ...chain, nodes: [...chain.nodes, node] } : chain
            )
        };
        setState(next);
        setIsDirty(true);
        sendChains(next.chains).then(ok => { if (!ok) setState(prev); });
    };

    const handleRoutingApply = (chainIdx: number, updated: ChainDef) => {
        const prev = stateRef.current;
        if (!prev) return;
        const next = { ...prev, chains: prev.chains.map((c, i) => i === chainIdx ? updated : c) };
        setState(next);
        setRoutingIdx(null);
        sendChains(next.chains).then(ok => { if (!ok) setState(prev); });
    };

    const handleDeleteChain = (chainIdx: number) => {
        const prev = stateRef.current;
        if (!prev) return;
        const next = { ...prev, chains: prev.chains.filter((_, i) => i !== chainIdx) };
        setState(next);
        setIsDirty(true);
        sendChains(next.chains).then(ok => { if (!ok) setState(prev); });
    };

    const handleNewChain = (input: [number, number], output: [number, number]) => {
        if (!state) return;
        const next = { ...state, chains: [...state.chains, { input, output, nodes: [] }] };
        setState(next);
        sendChains(next.chains);
    };

    const handleSaveConfig = async (cfg: typeof audioConfig) => {
        if (await saveConfig(cfg)) {
            setAudioConfig(cfg);
            if (state && cfg.delay_max_seconds < audioConfig.delay_max_seconds) {
                state.chains.forEach(chain => chain.nodes.forEach(node => {
                    if (node.type === 'delay' && typeof node.time === 'number' && node.time > cfg.delay_max_seconds) {
                        handleSet(`${node.key}.time`, cfg.delay_max_seconds);
                    }
                }));
            }
            return true;
        }
        return false;
    };

    // --- Derived ---
    const routingChain = routingIdx !== null && state ? state.chains[routingIdx] : null;

    // ===================================================================
    // JSX
    // ===================================================================

    if (page === 'devices') return <DevicesPage onHome={() => navigateTo('home')} />;

    return (
        <div className="app">
            <AppHeader
                connected={connected}
                toasts={toasts} onDismissToast={dismissToast}
                presets={presets} activePreset={activePreset}
                isDirty={isDirty} isComparing={isComparing} savedFeedback={savedFeedback}
                theme={theme}
                onSwitchPreset={handleSwitchPreset}
                onCompare={handleCompare}
                onQuickSave={handleQuickSave}
                onOpenSave={() => { setSavePresetNum(activePreset || 1); setShowSavePopup(true); }}
                onOpenSettings={() => setShowSettings(true)}
                onNavigateDevices={() => navigateTo('devices')}
                onNavigateHome={() => navigateTo('home')}
                onToggleTheme={toggleTheme}
            />

            {showSavePopup && (
                <SavePresetPopup
                    presetNum={savePresetNum}
                    onChangeNum={setSavePresetNum}
                    onConfirm={handleConfirmSave}
                    onClose={() => setShowSavePopup(false)}
                />
            )}
            {showSettings && (
                <SettingsPopup
                    config={audioConfig}
                    onSave={handleSaveConfig}
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
                        controllers={controllers}
                        devices={devices}
                        allNodes={state.chains.flatMap(c => c.nodes)}
                        delayMaxSeconds={audioConfig.delay_max_seconds}
                        onSet={handleSet}
                        onDelete={handleDelete}
                        onReorder={handleReorder}
                        onAddNode={handleAddNode}
                        onDeleteChain={handleDeleteChain}
                        onRouting={setRoutingIdx}
                        onSaveControllers={setControllers}
                    />
                ))}
                <BottomBar
                    hasChains={(state?.chains.length ?? 0) > 0}
                    inChannels={audioConfig.in_channels}
                    outChannels={audioConfig.out_channels}
                    onNewChain={handleNewChain}
                    onDeletePreset={handleDeletePreset}
                />
            </main>
        </div>
    );
}
