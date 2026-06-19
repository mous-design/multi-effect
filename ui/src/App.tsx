import { useEffect, useRef, useState } from 'react';
import { AppState, ChainDef, ControllerDef, NodeDef } from './types';
import { sendSet, sendChainSet, sendChains, savePreset, saveConfig, sendProgram, deletePreset, sendCompare, sendParamMeta } from './api';
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
import { TypeOverridesPopup } from './components/TypeOverridesPopup';
import { DevicesPage } from './components/DevicesPage';
import { splitN } from './api';
import type { ParamInfo } from './types';

/// Optimistic patch of one `ParamInfo` after a 3-segment meta override.
/// `active` / `visible` are top-level on every kind. `ParamMeta` writes
/// other aspects directly to `info[aspect]`. Event / BoundMeta / ActionsGroup
/// entries with non-{active,visible} aspects pass through unchanged. The
/// `as ParamInfo` cast bypasses TS's struggle to narrow the discriminated
/// union through a computed-property spread; at runtime the shape is valid
/// by construction (server only sends 3-segment PARAMs for aspects that
/// legitimately exist on the target entry).
function patchMetaAspect(
    info: ParamInfo, param: string, aspect: string, value: number | string | boolean,
): ParamInfo {
    if (info.name !== param) return info;
    // `active` and `visible` are top-level on every ParamInfo regardless of
    // kind — apply directly. Without this, Event / ActionsGroup entries
    // don't optimistically update on toggle (originator's broadcast is
    // filtered).
    if ((aspect === 'active' || aspect === 'visible') && typeof value === 'boolean')
        return { ...info, [aspect]: value } as ParamInfo;
    if (info.kind?.tag === 'ParamMeta') return { ...info, [aspect]: value } as ParamInfo;
    return info;
}

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
    // Per-effect bounds editor — hoisted out of SettingsPopup so closing
    // Settings doesn't unmount it (clicking "Effect bounds" closes Settings
    // and opens this).
    const [showTypeOverrides, setShowTypeOverrides] = useState(false);

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
    const { connected, audioConfig, setAudioConfig, devices, canonical } = useConnection((msg, params) => {
        switch(msg) {
            case 'PARAM': {
                const [path, valueStr] = splitN(params, ' ', 2);
                const segs = path.split('.');
                // Typed wire — match the server's Bool → Int → Float → Action
                // order. Bools arrive as `true`/`false` (not 0/1).
                let value: number | string | boolean;
                if      (valueStr === 'true')  value = true;
                else if (valueStr === 'false') value = false;
                else {
                    const num = Number(valueStr);
                    value = isFinite(num) ? num : valueStr;
                }
                setIsDirty(true);
                if (segs.length === 2) {
                    // 2-segment path = param value change.
                    const [nodeKey, param] = segs;
                    setState(prev => prev && {
                        ...prev, chains: prev.chains.map(chain => ({
                            ...chain,
                            nodes: chain.nodes.map(node =>
                                node.key === nodeKey ? {...node, [param]: value} : node
                            ),
                        }))
                    });
                } else if (segs.length === 3) {
                    // 3-segment path = meta override. Patch `params_info` so
                    // subsequent reads (override popup, knob render) see the
                    // new value. Shapes mirror `handleMetaSet`.
                    const [nodeKey, param, aspect] = segs;
                    setState(prev => prev && {
                        ...prev, chains: prev.chains.map(chain => ({
                            ...chain,
                            nodes: chain.nodes.map(node => node.key === nodeKey
                                ? { ...node, params_info: node.params_info?.map(info =>
                                    patchMetaAspect(info, param, aspect, value)) }
                                : node
                            ),
                        }))
                    });
                }
                break;
            }
            case 'LIVE': {
                // Effect-published live state (looper pos/duration/buffer_cnt,
                // future effects' meters/scopes). Same shape as PARAM but does
                // NOT mark the preset dirty — live state is ephemeral, not a
                // user mutation.
                const [path, valueStr] = splitN(params, ' ', 2);
                const segs = path.split('.');
                if (segs.length !== 2) break;
                const [nodeKey, param] = segs;
                let value: number | string | boolean;
                if      (valueStr === 'true')  value = true;
                else if (valueStr === 'false') value = false;
                else {
                    const num = Number(valueStr);
                    value = isFinite(num) ? num : valueStr;
                }
                setState(prev => prev && {
                    ...prev, chains: prev.chains.map(chain => ({
                        ...chain,
                        nodes: chain.nodes.map(node =>
                            node.key === nodeKey ? {...node, [param]: value} : node
                        ),
                    }))
                });
                break;
            }
            case 'PARAM_CHAIN': {
                // Chain-level user set echo (`mute_dry`). Marks dirty.
                // Format: `<chain_idx> <param> <value>`.
                const [idxStr, param, valueStr] = splitN(params, ' ', 3);
                const idx = Number(idxStr);
                if (!Number.isFinite(idx)) break;
                let value: number | string | boolean;
                if      (valueStr === 'true')  value = true;
                else if (valueStr === 'false') value = false;
                else {
                    const num = Number(valueStr);
                    value = isFinite(num) ? num : valueStr;
                }
                setIsDirty(true);
                setState(prev => prev && {
                    ...prev, chains: prev.chains.map((chain, i) =>
                        i === idx ? { ...chain, [param]: value } : chain
                    )
                });
                break;
            }
            case 'LIVE_CHAIN': {
                // Chain-level derived state (`dry_effective`). No dirty mark.
                // Format: `<chain_idx> <param> <value>`. Used today by the
                // hardware LED daemon (future) — the UI just keeps state in
                // sync so SNAPSHOT-shaped reads remain current.
                const [idxStr, param, valueStr] = splitN(params, ' ', 3);
                const idx = Number(idxStr);
                if (!Number.isFinite(idx)) break;
                let value: number | string | boolean;
                if      (valueStr === 'true')  value = true;
                else if (valueStr === 'false') value = false;
                else {
                    const num = Number(valueStr);
                    value = isFinite(num) ? num : valueStr;
                }
                setState(prev => prev && {
                    ...prev, chains: prev.chains.map((chain, i) =>
                        i === idx ? { ...chain, [param]: value } : chain
                    )
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
                if (event === 'state') {
                    // Abstract state tag broadcast (e.g. `looper-recording`).
                    // Stored on the node as `node.state_tag`; widgets match on it
                    // for state-driven styling.
                    const { tag } = JSON.parse(json);
                    setState(prev => prev && {
                        ...prev, chains: prev.chains.map(chain => ({
                            ...chain,
                            nodes: chain.nodes.map(node =>
                                node.key === key ? { ...node, state_tag: tag } : node
                            ),
                        }))
                    });
                } else if (event === 'loop_wrap') {
                    setState(prev => prev && {
                        ...prev, chains: prev.chains.map(chain => ({
                            ...chain,
                            nodes: chain.nodes.map(node =>
                                node.key === key ? { ...node, pos_secs: 0, _wrap_ts: Date.now() } : node
                            ),
                        }))
                    });
                }
                break;
        }
    });

    // --- Preset handlers ---

    const handleSwitchPreset = async (n: number) => {
        if (!presets.includes(n)) { addToast('error.preset_missing', n); return; }
        if (await sendProgram(n)) {
            setActivePreset(n);
            setIsDirty(false);
            setIsComparing(false);
        }
    };

    const handleCompare = async () => { await sendCompare(); };

    const handleConfirmSave = async () => {
        setShowSavePopup(false);
        // If we're comparing, the displayed preset is the saved version and the
        // user's edits live in `stash` on the server. Exit compare first so the
        // edits become active, then save them.
        if (isComparing) await sendCompare();
        if (await savePreset(savePresetNum)) {
            setActivePreset(savePresetNum);
            setIsDirty(false);
            setIsComparing(false);
            // Optimistically add to preset list (server filters out our INDICES broadcast).
            setPresetDefs(prev =>
                prev.includes(savePresetNum) ? prev : [...prev, savePresetNum].sort((a, b) => a - b),
            );
            setSavedFeedback(true);
            setTimeout(() => setSavedFeedback(false), 2000);
        }
    };

    const handleQuickSave = async () => {
        if (isComparing) await sendCompare();
        if (await savePreset(activePreset)) {
            setIsDirty(false);
            setIsComparing(false);
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
        // Optimistic param update for instant knob feedback. Dirty/Comparing flags
        // are set authoritatively by the server's `STATE` response (handled by
        // the SNAPSHOT-style dispatch in handleLine).
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

    // Chain-level set (`mute_dry`). Same optimistic-then-wire pattern as
    // `handleSet`. The originator's PARAM_CHAIN echo is dropped by master's
    // source filter, so this local patch is the *only* path that updates
    // this client's view until the next SNAPSHOT.
    const handleChainSet = (chainIdx: number, param: string, value: number | boolean) => {
        setState(prev => prev && {
            ...prev, chains: prev.chains.map((chain, i) =>
                i === chainIdx ? { ...chain, [param]: value } : chain
            )
        });
        sendChainSet(chainIdx, param, value);
    };

    // Meta-override (3-segment SET) — bound / visibility / active edit on a
    // single param. Optimistic update of `params_info[i]` on the addressed
    // node; the originator's source-filter drops master's echoed PARAM, so
    // the local update is the *only* way this client learns the new state
    // until next snapshot. Returns the wire result so callers that need to
    // surface `confirm_required` (bound-growth) can react.
    const handleMetaSet = async (
        nodeKey: string, param: string, aspect: string,
        value: number | boolean, confirmed?: boolean,
    ): Promise<{ ok: boolean; confirmRequired: boolean }> => {
        setState(prev => prev && {
            ...prev, chains: prev.chains.map(chain => ({
                ...chain,
                nodes: chain.nodes.map(node => node.key === nodeKey
                    ? { ...node, params_info: node.params_info?.map(info =>
                        patchMetaAspect(info, param, aspect, value)) }
                    : node
                ),
            }))
        });
        return sendParamMeta(nodeKey, param, aspect, value, confirmed);
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
        // Default chain-level state: `mute_dry` off (digital dry in output),
        // `dry_effective` true (matches default behaviour). Master will
        // recompute `dry_effective` after the chains land.
        const next = { ...state, chains: [...state.chains, { input, output, mute_dry: false, dry_effective: true, nodes: [] }] };
        setState(next);
        sendChains(next.chains);
    };

    const handleSaveConfig = async (cfg: typeof audioConfig) => {
        if (await saveConfig(cfg)) {
            setAudioConfig(cfg);
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
                    onOpenEffectBounds={() => { setShowSettings(false); setShowTypeOverrides(true); }}
                />
            )}
            {showTypeOverrides && (
                <TypeOverridesPopup onClose={() => setShowTypeOverrides(false)} />
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
                        effectTypes={Object.keys(canonical).sort()}
                        onSet={handleSet}
                        onChainSet={handleChainSet}
                        onMetaSet={handleMetaSet}
                        onDelete={handleDelete}
                        onReorder={handleReorder}
                        onAddNode={handleAddNode}
                        onDeleteChain={handleDeleteChain}
                        onRouting={setRoutingIdx}
                        onSaveControllers={(c) => { setControllers(c); setIsDirty(true); }}
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
