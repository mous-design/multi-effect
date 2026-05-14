import { useEffect, useRef, useState } from 'react';
import type { NodeDef, ParamInfo } from '../types';
import { sendAction } from '../api';
import { Knob } from './Knob';
import { Toggle } from './Toggle';
import { TileSettingsPopup } from './TileSettingsPopup';
import { t } from '../i18n';

// Looper `transport` is a UI-only composite widget (rec/play/stop + timer +
// seek), not a real backend param — its visibility lives only in localStorage.
// All real params get their visibility from `info.visible` (canonical
// `with_hidden()` or runtime `SET <key>.<param>.visible <bool>` overrides).
const TRANSPORT_KEY = 'transport';
const LOOPER_TRANSPORT_HIDDEN_DEFAULT = true;

function EyeIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <ellipse cx="6" cy="6" rx="5" ry="3.5" />
      <circle cx="6" cy="6" r="1.5" fill="currentColor" stroke="none" />
    </svg>
  );
}

function EyeOffIcon() {
  return (
    <svg width="11" height="11" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
      <ellipse cx="6" cy="6" rx="5" ry="3.5" />
      <circle cx="6" cy="6" r="1.5" fill="currentColor" stroke="none" />
      <line x1="2" y1="2" x2="10" y2="10" />
    </svg>
  );
}

function PauseIcon() {
  return (
    <svg width="10" height="13" viewBox="0 0 10 13" fill="currentColor">
      <rect x="0" y="0" width="3.5" height="13" rx="1"/>
      <rect x="6.5" y="0" width="3.5" height="13" rx="1"/>
    </svg>
  );
}

function CogIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/// Resolve a live param's effective value. Master only mirrors values into
/// `node.params` once they're mutated past the canonical default, so a
/// freshly-loaded effect has its defaults missing from the wire. Falling back
/// to `info.default` is correct — that's the actual live value.
function effectiveValue(node: NodeDef, info: ParamInfo): unknown {
  const v = node[info.name];
  if (v !== undefined) return v;
  switch (info.type) {
    case 'ContinuousFloat':
    case 'ContinuousInt':
    case 'DiscreteFloat':
    case 'DiscreteBool': return info.default;
    case 'Event':        return null;
  }
}

/// Pair each live-param `ParamInfo` with its effective value.
/// Order follows the canonical declaration order (effect-author intent).
/// Only `ParamMeta` entries render as knobs/toggles — `TypeMeta` /
/// `InstanceMeta` entries are override-form descriptors handled elsewhere.
function getRenderableParams(node: NodeDef): { info: ParamInfo; value: unknown }[] {
  const infos = node.params_info;
  if (!infos) return [];
  return infos
    .filter(info => info.kind?.tag === 'ParamMeta')
    .map(info => ({ info, value: effectiveValue(node, info) }));
}

// Transport-only localStorage. Real-param visibility comes from `info.visible`
// on the wire; only the looper's UI-only `transport` widget caches its hidden
// state here.
function transportLsKey(preset: string, nodeKey: string) {
  return `transport-hidden:${preset}:${nodeKey}`;
}
function loadTransportHidden(preset: string, nodeKey: string): boolean {
  const stored = localStorage.getItem(transportLsKey(preset, nodeKey));
  return stored === null ? LOOPER_TRANSPORT_HIDDEN_DEFAULT : stored === 'true';
}
function saveTransportHidden(preset: string, nodeKey: string, hidden: boolean) {
  localStorage.setItem(transportLsKey(preset, nodeKey), hidden ? 'true' : 'false');
}

interface Props {
  node: NodeDef;
  presetName: string;
  onSet: (path: string, value: number | boolean) => void;
  onMetaSet: (nodeKey: string, param: string, aspect: string, value: number | boolean) => void;
  onDelete: (key: string) => void;
}

const LOOPING = new Set(['Playing', 'Overdub']);

function useLooperTimer(node: NodeDef): string {
  const looperState = String(node['state'] ?? 'Idle');
  const loopSecs    = Number(node['loop_secs'] ?? 0);
  const posSecs     = Number(node['pos_secs']  ?? 0);
  const wrapTs      = Number(node['_wrap_ts']  ?? 0);
  const isRunning   = looperState === 'Recording' || looperState === 'Playing' || looperState === 'Overdub';

  const [displaySecs, setDisplaySecs] = useState(0);
  const syncRef      = useRef<{ time: number; pos: number }>({ time: Date.now(), pos: 0 });
  const displayRef   = useRef(0);
  const prevStateRef = useRef(looperState);

  useEffect(() => {
    const prevState = prevStateRef.current;
    prevStateRef.current = looperState;

    // Playing ↔ Overdub: timer is continuous, don't snap to stale posSecs.
    // Use the last displayed position as sync anchor instead.
    const loopingTransition = LOOPING.has(prevState) && LOOPING.has(looperState) && looperState !== prevState;
    const startPos = loopingTransition ? displayRef.current : posSecs;

    syncRef.current = { time: Date.now(), pos: startPos };
    if (!isRunning) {
      setDisplaySecs(looperState === 'Idle' ? 0 : startPos);
      return;
    }
    const id = setInterval(() => {
      const elapsed = syncRef.current.pos + (Date.now() - syncRef.current.time) / 1000;
      displayRef.current = elapsed;
      setDisplaySecs(elapsed);
    }, 100);
    return () => clearInterval(id);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [looperState, posSecs, wrapTs]);

  if (looperState === 'Idle' || (looperState === 'Stop' && loopSecs === 0)) return '-.--';
  const sInt   = Math.floor(displaySecs);
  const sTenth = Math.floor((displaySecs * 10) % 10);
  return `${sInt}.${sTenth}`;
}

export function EffectTile({ node, presetName, onSet, onMetaSet, onDelete }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const [transportHidden, setTransportHidden] = useState(() => loadTransportHidden(presetName, node.key));
  const looperTime = useLooperTimer(node);

  useEffect(() => {
    setTransportHidden(loadTransportHidden(presetName, node.key));
    setExpanded(false);
  }, [presetName, node.key]);

  const allParams = getRenderableParams(node);
  // `active` is exclusively rendered as the header toggle — strip it from the
  // body so it doesn't render twice.
  const bodyParams = allParams.filter(({ info }) => info.name !== 'active');
  const activeEntry = allParams.find(({ info }) => info.name === 'active' && info.type === 'DiscreteBool');
  const active = activeEntry ? !!activeEntry.value : true;

  const isLooper = node.type === 'looper';

  // Seek scrubber: only active when looper is in Stop state
  const looperStateVal = isLooper ? String((node as Record<string, unknown>)['state'] ?? 'Idle') : '';
  const [seekEditing, setSeekEditing] = useState(false);
  const [seekInput, setSeekInput]     = useState('');
  const seekDragRef = useRef<{ startY: number; startPos: number; moved: boolean } | null>(null);

  useEffect(() => {
    if (isLooper && looperStateVal !== 'Stop') setSeekEditing(false);
  }, [isLooper, looperStateVal]);

  const hiddenCount = bodyParams.filter(({ info }) => !info.visible).length
                    + (isLooper && transportHidden ? 1 : 0);

  // Eye toggle. Real params: round-trip via `onMetaSet` (optimistic
  // `info.visible` update in App state, wire `SET <key>.<param>.visible`).
  // Transport: UI-only widget, localStorage cache.
  function setVisible(param: string, visible: boolean) {
    if (param === TRANSPORT_KEY) {
      setTransportHidden(!visible);
      saveTransportHidden(presetName, node.key, !visible);
    } else {
      onMetaSet(node.key, param, 'visible', visible);
    }
  }

  function renderControl(info: ParamInfo, val: unknown): React.ReactNode {
    switch (info.type) {
      case 'ContinuousFloat':
      case 'ContinuousInt': {
        if (typeof val !== 'number') return null;
        const log = info.type === 'ContinuousFloat' ? !!info.log : false;
        return <Knob nodeKey={node.key} param={info.name}
          value={val} min={info.min} max={info.max}
          label={t(`param.${info.name}`)} unit={info.unit} log={log}
          onSet={(p, v) => onSet(p, v)} />;
      }
      case 'DiscreteBool': {
        if (typeof val !== 'boolean') return null;
        return <Toggle nodeKey={node.key} param={info.name}
          value={val} label={t(`param.${info.name}`)} onSet={(p, v) => onSet(p, v)} />;
      }
      // DiscreteFloat (dropdown) and Event (action buttons) not wired yet.
      default:
        return null;
    }
  }

  const visibleParams = bodyParams.filter(({ info }) => info.visible);

  return (
    <div className={`tile${active ? '' : ' inactive'}${expanded ? ' expanded' : ''}`}>
      {showSettings && (
        <TileSettingsPopup node={node} onMetaSet={onMetaSet} onClose={() => setShowSettings(false)} />
      )}
      <div className="tile-header">
        {activeEntry
          ? <Toggle nodeKey={node.key} param="active" value={active} label="" onSet={(p, v) => onSet(p, v)} />
          : <div className="tile-header-spacer" />}
        <span className="tile-type">{t(`type.${node.type}`)}</span>
        <button className="tile-settings-btn" onClick={() => setShowSettings(true)} title={t('ui.settings')}><CogIcon /></button>
        <button className="tile-delete" onClick={() => onDelete(node.key)} title="Delete">×</button>
      </div>
      <div className="tile-body">
        <div className="tile-params">
          {isLooper && (expanded || !transportHidden) && (() => {
            const nd          = node as Record<string, unknown>;
            const looperState = String(nd['state'] ?? 'Idle');
            const overdubs    = Number(nd['overdub_count'] ?? 0);
            const maxBufs     = Number(nd['max_buffers']  ?? 0);
            const isIdle      = looperState === 'Idle';
            const isRecording = looperState === 'Recording';
            const isPlaying   = looperState === 'Playing';
            const isOverdub   = looperState === 'Overdub';
            // During Overdub, a layer is being recorded but not yet counted — show +1
            const displayOverdubs = isOverdub ? overdubs + 1 : overdubs;
            const atMerge     = maxBufs > 0 && displayOverdubs >= maxBufs;
            const canUndo     = isRecording || isOverdub || overdubs > 0;
            const isStop      = looperState === 'Stop';
            const loopSecs    = Number(nd['loop_secs'] ?? 0);
            const posSecs     = Number(nd['pos_secs']  ?? 0);

            const recActive   = isRecording || isOverdub;
            const recClass    = `looper-btn${isRecording ? ' looper-btn-rec' : isOverdub ? ' looper-btn-overdub' : ''}`;
            const recTitle    = isRecording ? 'Pause recording' : isOverdub ? 'Pause overdub' : isIdle ? 'Start recording' : 'Record overdub';

            const playClass   = `looper-btn${isPlaying ? ' looper-btn-play' : ''}`;
            const playTitle   = isPlaying ? 'Pause playback' : 'Play';

            return (
              <div className={`param-cell looper-transport${transportHidden ? ' param-hidden' : ''}`}>
                <div className="looper-transport-inner">
                  <div className="looper-buttons">
                    <button className={recClass}
                      disabled={isPlaying}
                      title={recTitle}
                      onMouseDown={e => e.stopPropagation()}
                      onClick={() => sendAction(`${node.key}.action`, recActive ? 'pause' : 'rec')}>
                      {recActive ? <PauseIcon /> : t('looper.rec')}
                    </button>
                    <button className={playClass}
                      disabled={isIdle}
                      title={playTitle}
                      onMouseDown={e => e.stopPropagation()}
                      onClick={() => sendAction(`${node.key}.action`, isPlaying ? 'pause' : 'play')}>
                      {isPlaying ? <PauseIcon /> : '▶'}
                    </button>
                    <button className="looper-btn"
                      disabled={isIdle}
                      title="Stop (go to start)"
                      onMouseDown={e => e.stopPropagation()}
                      onClick={() => sendAction(`${node.key}.action`, 'stop')}>
                      ■
                    </button>
                  </div>
                  <div className="looper-bottom-row">
                    {seekEditing
                      ? <input
                          className="looper-time looper-time-edit"
                          type="number"
                          step="0.1"
                          min={0}
                          max={loopSecs}
                          value={seekInput}
                          onChange={e => setSeekInput(e.target.value)}
                          onBlur={() => {
                            const v = Math.max(0, Math.min(loopSecs, parseFloat(seekInput) || 0));
                            onSet(`${node.key}.pos_secs`, v);
                            setSeekEditing(false);
                          }}
                          onKeyDown={e => {
                            if (e.key === 'Enter') (e.target as HTMLInputElement).blur();
                            if (e.key === 'Escape') setSeekEditing(false);
                          }}
                          onMouseDown={e => e.stopPropagation()}
                          // eslint-disable-next-line jsx-a11y/no-autofocus
                          autoFocus
                        />
                      : <div
                          className={`looper-time${isStop ? ' looper-time-seekable' : ''}`}
                          title={isStop ? 'Drag or click to seek' : undefined}
                          onMouseDown={!isStop ? undefined : e => {
                            e.stopPropagation();
                            const startPos = posSecs;
                            const sensitivity = loopSecs > 0 ? loopSecs / 200 : 0.1;
                            seekDragRef.current = { startY: e.clientY, startPos, moved: false };
                            const onMove = (me: MouseEvent) => {
                              if (!seekDragRef.current) return;
                              const dy = seekDragRef.current.startY - me.clientY;
                              if (Math.abs(dy) > 3) seekDragRef.current.moved = true;
                              const newPos = Math.max(0, Math.min(loopSecs, startPos + dy * sensitivity));
                              onSet(`${node.key}.pos_secs`, newPos);
                            };
                            const onUp = () => {
                              if (seekDragRef.current && !seekDragRef.current.moved) {
                                setSeekInput(startPos.toFixed(1));
                                setSeekEditing(true);
                              }
                              seekDragRef.current = null;
                              document.removeEventListener('mousemove', onMove);
                              document.removeEventListener('mouseup', onUp);
                            };
                            document.addEventListener('mousemove', onMove);
                            document.addEventListener('mouseup', onUp);
                          }}
                        >
                          {looperTime}
                        </div>
                    }
                    <button className="looper-btn looper-undo-btn"
                      disabled={!canUndo}
                      title={`Undo overdub (${displayOverdubs} layer${displayOverdubs !== 1 ? 's' : ''})`}
                      onMouseDown={e => e.stopPropagation()}
                      onClick={() => sendAction(`${node.key}.action`, 'undo')}>
                      ↩<span className="looper-undo-count" style={atMerge ? { color: 'var(--red, #e05)' } : undefined}>{displayOverdubs}</span>
                    </button>
                    <button className="looper-btn"
                      disabled={isIdle}
                      title="Reset (clear loop)"
                      onMouseDown={e => e.stopPropagation()}
                      onClick={() => sendAction(`${node.key}.action`, 'reset')}>
                      {t('looper.reset')}
                    </button>
                  </div>
                </div>
                <button className="param-vis-btn"
                  title={transportHidden ? t('ui.show_param') : t('ui.hide_param')}
                  onMouseDown={e => e.stopPropagation()}
                  onClick={() => setVisible(TRANSPORT_KEY, transportHidden)}>
                  {transportHidden ? <EyeIcon /> : <EyeOffIcon />}
                </button>
              </div>
            );
          })()}
          {(expanded ? bodyParams : visibleParams).map(({ info, value }) => {
            const isHidden = !info.visible;
            const ctrl = renderControl(info, value);
            if (!ctrl) return null;
            return (
              <div key={info.name} className={`param-cell${isHidden ? ' param-hidden' : ''}`}>
                {ctrl}
                <button
                  className="param-vis-btn"
                  title={isHidden ? t('ui.show_param') : t('ui.hide_param')}
                  onMouseDown={e => e.stopPropagation()}
                  onClick={() => setVisible(info.name, isHidden)}
                >
                  {isHidden ? <EyeIcon /> : <EyeOffIcon />}
                </button>
              </div>
            );
          })}
        </div>
        {hiddenCount > 0 && (
          <div className="tile-sidebar">
            <button className="tile-expand" onClick={() => setExpanded(e => !e)}>
              {expanded
                ? <span className="tile-expand-arrow">◂</span>
                : <><span className="tile-expand-arrow">▸</span><span className="tile-expand-count">{hiddenCount}</span></>
              }
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
