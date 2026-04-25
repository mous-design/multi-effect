import { useEffect, useRef, useState } from 'react';
import { NodeDef } from '../types';
import { sendAction } from '../api';
import { Knob } from './Knob';
import { Toggle } from './Toggle';
import { t } from '../i18n';

export const PARAM_META: Record<string, { min: number; max: number; unit?: string; log?: boolean }> = {
  wet:       { min: 0,    max: 1              },
  dry:       { min: 0,    max: 1              },
  gain:      { min: 0,    max: 4              },
  feedback:  { min: 0,    max: 1              },
  time:      { min: 0,    max: 2,   unit: 's'  },
  room_size: { min: 0,    max: 1              },
  damping:   { min: 0,    max: 1              },
  pan:       { min: -1,   max: 1              },
  rate_hz:   { min: 0.1,  max: 10,  unit: 'Hz' },
  depth_ms:  { min: 0,    max: 30,  unit: 'ms' },
  root:      { min: 0,    max: 127            },
  vel_sense: { min: 0,    max: 1              },
  freq:      { min: 50,   max: 10000, unit: 'Hz', log: true },
  gain_db:   { min: -15,  max: 15,  unit: 'dB' },
  q:         { min: 0.1,  max: 5              },
  loop_gain: { min: 0,    max: 4              },
};

const SKIP_PARAMS = new Set(['key', 'type', 'active', 'state', 'overdub_count', 'max_buffers', 'loop_secs', 'pos_secs', '_wrap_ts']);

const PARAM_ORDER: Record<string, string[]> = {
  mix:      ['gain', 'pan', 'dry', 'wet'],
  eq_param: ['freq', 'gain_db', 'q'],
  eq_low:   ['gain_db', 'freq'],
  eq_high:  ['gain_db', 'freq'],
};

const DEFAULT_HIDDEN: Record<string, string[]> = {
  mix:      ['dry', 'wet'],
  eq_param: ['q'],
  eq_low:   ['freq'],
  eq_high:  ['freq'],
  looper:   ['transport'],
};

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

function scalarOf(val: unknown): number | null {
  if (typeof val === 'number') return val;
  if (Array.isArray(val) && val.length > 0 && val.every(v => typeof v === 'number')) {
    return (val as number[]).reduce((a, b) => a + b, 0) / val.length;
  }
  return null;
}

function getOrderedParams(node: NodeDef): [string, unknown][] {
  const order = PARAM_ORDER[node.type];
  if (order) {
    return order
      .map(k => [k, node[k]] as [string, unknown])
      .filter(([, v]) => v !== undefined);
  }
  return Object.entries(node).filter(([k]) => !SKIP_PARAMS.has(k));
}

function lsKey(preset: string, nodeKey: string) {
  return `hidden:${preset}:${nodeKey}`;
}

function loadHidden(preset: string, nodeKey: string, nodeType: string): Set<string> {
  const stored = localStorage.getItem(lsKey(preset, nodeKey));
  if (stored !== null) return new Set(JSON.parse(stored) as string[]);
  return new Set(DEFAULT_HIDDEN[nodeType] ?? []);
}

function saveHidden(preset: string, nodeKey: string, hidden: Set<string>) {
  localStorage.setItem(lsKey(preset, nodeKey), JSON.stringify([...hidden]));
}

interface Props {
  node: NodeDef;
  presetName: string;
  delayMaxSeconds?: number;
  onSet: (path: string, value: number | boolean) => void;
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

export function EffectTile({ node, presetName, delayMaxSeconds, onSet, onDelete }: Props) {
  const [hidden, setHidden] = useState(() => loadHidden(presetName, node.key, node.type));
  const [expanded, setExpanded] = useState(false);
  const looperTime = useLooperTimer(node);

  useEffect(() => {
    setHidden(loadHidden(presetName, node.key, node.type));
    setExpanded(false);
  }, [presetName, node.key]);

  const hasActive = node.active !== undefined;
  const active = hasActive ? !!node.active : true;

  const allParams = getOrderedParams(node);
  const isLooper = node.type === 'looper';

  // Seek scrubber: only active when looper is in Stop state
  const looperStateVal = isLooper ? String((node as Record<string, unknown>)['state'] ?? 'Idle') : '';
  const [seekEditing, setSeekEditing] = useState(false);
  const [seekInput, setSeekInput]     = useState('');
  const seekDragRef = useRef<{ startY: number; startPos: number; moved: boolean } | null>(null);

  useEffect(() => {
    if (isLooper && looperStateVal !== 'Stop') setSeekEditing(false);
  }, [isLooper, looperStateVal]);
  const transportHidden = hidden.has('transport');
  const hiddenCount = allParams.filter(([k]) => hidden.has(k)).length + (isLooper && transportHidden ? 1 : 0);

  function hide(param: string) {
    const next = new Set(hidden);
    next.add(param);
    setHidden(next);
    saveHidden(presetName, node.key, next);
  }

  function show(param: string) {
    const next = new Set(hidden);
    next.delete(param);
    setHidden(next);
    saveHidden(presetName, node.key, next);
  }

  function renderControl(param: string, val: unknown): React.ReactNode {
    if (typeof val === 'boolean') {
      return <Toggle nodeKey={node.key} param={param}
        value={val} label={t(`param.${param}`)} onSet={(p, v) => onSet(p, v)} />;
    }
    const scalar = scalarOf(val);
    if (scalar !== null) {
      const meta = { ...(PARAM_META[param] ?? { min: 0, max: 1 }) };
      if (param === 'time' && node.type === 'delay' && delayMaxSeconds !== undefined) {
        meta.max = delayMaxSeconds;
      }
      return <Knob nodeKey={node.key} param={param}
        value={scalar} min={meta.min} max={meta.max}
        label={t(`param.${param}`)} unit={meta.unit} log={meta.log}
        onSet={(p, v) => onSet(p, v)} />;
    }
    return null;
  }

  const visibleParams = allParams.filter(([k]) => !hidden.has(k));

  return (
    <div className={`tile${active ? '' : ' inactive'}${expanded ? ' expanded' : ''}`}>
      <div className="tile-header">
        {hasActive
          ? <Toggle nodeKey={node.key} param="active" value={active} label="" onSet={(p, v) => onSet(p, v)} />
          : <div className="tile-header-spacer" />}
        <span className="tile-type">{t(`type.${node.type}`)}</span>
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
                  onClick={() => transportHidden ? show('transport') : hide('transport')}>
                  {transportHidden ? <EyeIcon /> : <EyeOffIcon />}
                </button>
              </div>
            );
          })()}
          {(expanded ? allParams : visibleParams).map(([param, val]) => {
            const isHidden = hidden.has(param);
            const ctrl = renderControl(param, val);
            if (!ctrl) return null;
            return (
              <div key={param} className={`param-cell${isHidden ? ' param-hidden' : ''}`}>
                {ctrl}
                <button
                  className="param-vis-btn"
                  title={isHidden ? t('ui.show_param') : t('ui.hide_param')}
                  onMouseDown={e => e.stopPropagation()}
                  onClick={() => isHidden ? show(param) : hide(param)}
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
