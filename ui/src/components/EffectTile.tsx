import { useEffect, useRef, useState } from 'react';
import type { NodeDef, ParamInfo } from '../types';
import { sendAction } from '../api';
import { Knob } from './Knob';
import { Toggle } from './Toggle';
import { TileSettingsPopup } from './TileSettingsPopup';
import { t, actionLabel } from '../i18n';

// Looper `transport` is a composite widget (rec/play/stop + timer + seek).
// Its visibility/active state lives in `params_info` as an `ActionsGroup`
// entry — toggled via the standard override pipeline (`SET <key>.transport.
// visible <bool>`) so Type and Instance overrides work uniformly. The UI
// looks up the entry by name and treats it as just another tile element.
const TRANSPORT_NAME = 'transport';

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
    case 'None':         return null;
  }
}

/// Pair each live-param `ParamInfo` with its effective value.
/// Order follows the canonical declaration order (effect-author intent).
/// Only writable `ParamMeta` entries render as knobs/toggles. `BoundMeta`
/// describes the override envelope; read-only `ParamMeta` (current loop
/// length, current overdub count) is driven by the effect itself and
/// rendered by effect-specific widgets (e.g. looper transport).
function getRenderableParams(node: NodeDef): { info: ParamInfo; value: unknown }[] {
  const infos = node.params_info;
  if (!infos) return [];
  // Skip read-only entries: their values are effect-driven and rendered by
  // effect-specific widgets in the tile (timer for looper.duration, counter
  // for looper.buffer_cnt). Generic knob-render would mislead.
  return infos
    .filter(info => info.kind?.tag === 'ParamMeta'
        && info.active !== false
        && info.kind.read_only !== true)
    .map(info => ({ info, value: effectiveValue(node, info) }));
}

interface Props {
  node: NodeDef;
  presetName: string;
  onSet: (path: string, value: number | boolean) => void;
  /// Meta-override edit: applies the optimistic local patch + fires the wire
  /// `SET <key>.<param>.<aspect> <value>`. Returns the wire result so the
  /// settings popup can react to `confirm_required` (bound-growth path).
  onMetaSet: (
    nodeKey: string, param: string, aspect: string,
    value: number | boolean, confirmed?: boolean,
  ) => Promise<{ ok: boolean; confirmRequired: boolean }>;
  onDelete: (key: string) => void;
}

const LOOPING = new Set(['looper-playing', 'looper-overdub']);

function useLooperTimer(node: NodeDef): string {
  const looperState = String(node['state_tag'] ?? 'looper-idle');
  const loopSecs    = Number(node['duration'] ?? 0);
  const posSecs     = Number(node['pos_secs']  ?? 0);
  const wrapTs      = Number(node['_wrap_ts']  ?? 0);
  const isRunning   = looperState === 'looper-recording' || looperState === 'looper-playing' || looperState === 'looper-overdub';

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
      setDisplaySecs(looperState === 'looper-idle' ? 0 : startPos);
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

  if (looperState === 'looper-idle' || (looperState === 'looper-stop' && loopSecs === 0)) return '-.--';
  const sInt   = Math.floor(displaySecs);
  const sTenth = Math.floor((displaySecs * 10) % 10);
  return `${sInt}.${sTenth}`;
}

export function EffectTile({ node, presetName, onSet, onMetaSet, onDelete }: Props) {
  const [expanded, setExpanded] = useState(false);
  const [showSettings, setShowSettings] = useState(false);
  const looperTime = useLooperTimer(node);

  useEffect(() => {
    setExpanded(false);
  }, [presetName, node.key]);

  const allParams = getRenderableParams(node);
  // `active` is exclusively rendered as the header toggle — strip it from the
  // body so it doesn't render twice.
  const bodyParams = allParams.filter(({ info }) => info.name !== 'active');
  const activeEntry = allParams.find(({ info }) => info.name === 'active' && info.type === 'DiscreteBool');
  const active = activeEntry ? !!activeEntry.value : true;

  const isLooper = node.type === 'looper';

  // Transport visibility lives in params_info as an ActionsGroup entry.
  // `active === false` removes the group entirely (declared-but-off);
  // `visible === false` hides it from the default tile view but the
  // expand-arrow reveals it. Mirrors how every other ParamInfo entry behaves.
  const transportInfo    = node.params_info?.find(
    i => i.name === TRANSPORT_NAME && i.kind?.tag === 'ActionsGroup');
  const transportActive  = !!transportInfo && transportInfo.active !== false;
  const transportVisible = transportInfo?.visible !== false;
  const transportHidden  = transportActive && !transportVisible;

  const hiddenCount = bodyParams.filter(({ info }) => !info.visible).length
                    + (transportHidden ? 1 : 0);

  // Eye toggle — round-trips via `onMetaSet` (optimistic `info.visible`
  // update in App state, wire `SET <key>.<param>.visible`). Transport is
  // now just another `params_info` entry, so no special-case path.
  function setVisible(param: string, visible: boolean) {
    onMetaSet(node.key, param, 'visible', visible);
  }

  function renderControl(info: ParamInfo, val: unknown): React.ReactNode {
    switch (info.type) {
      case 'ContinuousFloat':
      case 'ContinuousInt': {
        if (typeof val !== 'number') return null;
        const log = info.type === 'ContinuousFloat' ? !!info.log : false;
        return <Knob
            value={val} min={info.min} max={info.max}
            label={t(`param.${info.name}`)} unit={info.unit} log={log}
            onSet={v => onSet(`${node.key}.${info.name}`, v)} />;
      }
      case 'DiscreteBool': {
        if (typeof val !== 'boolean') return null;
        return <Toggle value={active} 
            label={t(`param.${info.name}`)} 
            onSet={v => onSet(`${node.key}.${info.name}`, v)} />
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
        <TileSettingsPopup node={node} onMetaSet={onMetaSet}
          onClose={() => setShowSettings(false)} />
      )}
      <div className="tile-header">
        {activeEntry
          ? <Toggle value={active} label="" onSet={v => onSet(`${node.key}.active`, v)} />
          : <div className="tile-header-spacer" />}
        <span className="tile-type">{t(`type.${node.type}`)}</span>
        <button className="tile-settings-btn" onClick={() => setShowSettings(true)} title={t('ui.settings')}><CogIcon /></button>
        <button className="tile-delete" onClick={() => onDelete(node.key)} title={t('ui.delete')}>×</button>
      </div>
      <div className="tile-body">
        <div className="tile-params">
          {isLooper && transportActive && (expanded || transportVisible) && (() => {
            const nd          = node as Record<string, unknown>;
            const looperState = String(nd['state_tag'] ?? 'looper-idle');
            const bufferCnt   = Number(nd['buffer_cnt'] ?? 0);
            // The buffer-cap is the read-only ParamMeta's `max` (live cap
            // resolved from canonical + Type/Instance overrides).
            const bufInfo     = node.params_info?.find(
                i => i.name === 'buffer_cnt' && i.kind?.tag === 'ParamMeta');
            const maxBufs     = bufInfo && bufInfo.type === 'ContinuousInt' ? bufInfo.max : 0;
            const isIdle      = looperState === 'looper-idle';
            const isOverdub   = looperState === 'looper-overdub';
            const displayCnt  = isOverdub ? bufferCnt + 1 : bufferCnt;
            const atMerge     = maxBufs > 0 && displayCnt >= maxBufs;

            // Looper-specific styling — match on (action, state_tag). Other
            // effects' Event clusters would have their own table.
            const buttonClass = (action: string) => {
              if (action === 'rec' && (looperState === 'looper-recording' || isOverdub))
                return 'looper-btn looper-btn-rec';
              if (action === 'play' && looperState === 'looper-playing')
                return 'looper-btn looper-btn-play';
              return 'looper-btn';
            };
            const posSecs = Number(nd['pos_secs'] ?? 0);

            // Resolve a combined verb against current state → primitive that
            // would fire on press. Mirrors looper's `dispatch_action` server-
            // side, but only enough to drive the button's icon (the actual
            // dispatch still happens server-side; this is purely cosmetic).
            // Returns `null` when the verb would no-op in this state.
            const resolve = (action: string): string | null => {
              const primitives = ['rec', 'play', 'pause', 'stop', 'reset', 'undo'];
              if (primitives.includes(action)) return action;

              // PauseStopReset has a pos-dependent step in the backend: from
              // Stop with pos > 0, it fires Stop (resets pos to 0) before
              // becoming eligible for Reset on the next press. Mirror that
              // so the icon reflects the 3-step Pause → Stop → Reset sequence.
              if (action === 'pause-stop-reset' && looperState === 'looper-stop') {
                return posSecs > 0 ? 'stop' : 'reset';
              }
              const table: Record<string, Record<string, string | null>> = {
                'rec-play': {
                  'looper-idle':      'rec',
                  'looper-recording': 'play',
                  'looper-overdub':   'play',
                  'looper-playing':   'rec',
                  'looper-stop':      'rec',
                },
                'rec-play-stop': {
                  'looper-idle':      'rec',
                  'looper-recording': 'play',
                  'looper-overdub':   'play',
                  'looper-playing':   'stop',
                  'looper-stop':      'rec',
                },
                'rec-play-stop-play': {
                  'looper-idle':      'rec',
                  'looper-recording': 'play',
                  'looper-overdub':   'play',
                  'looper-playing':   'stop',
                  'looper-stop':      'play',
                },
                'play-stop': {
                  'looper-playing': 'stop',
                  'looper-stop':    'play',
                },
                'play-pause': {
                  'looper-playing': 'pause',
                  'looper-stop':    'play',
                },
                'rec-pause': {
                  'looper-idle':      'rec',
                  'looper-recording': 'pause',
                  'looper-overdub':   'pause',
                  'looper-playing':   'rec',
                  'looper-stop':      'rec',
                },
                'stop-reset': {
                  'looper-idle':      'reset',
                  'looper-recording': 'stop',
                  'looper-overdub':   'stop',
                  'looper-playing':   'stop',
                  'looper-stop':      'reset',
                },
                'pause-stop': {
                  'looper-recording': 'pause',
                  'looper-overdub':   'pause',
                  'looper-playing':   'pause',
                  'looper-stop':      'stop',
                },
                'pause-stop-reset': {
                  'looper-recording': 'pause',
                  'looper-overdub':   'pause',
                  'looper-playing':   'pause',
                  'looper-stop':      'reset',
                },
              };
              return table[action]?.[looperState] ?? null;
            };

            // When a combined verb has no resolution in the current state,
            // the button is disabled — but we still want to *show* something
            // informative, not the multi-icon composed fallback. Each verb
            // declares the icon to show when idle/no-op — typically the
            // primitive most associated with its active-state behaviour.
            const verbHintIcon: Record<string, string> = {
              'play-stop':        'stop',
              'play-pause':       'pause',
              'pause-stop':       'pause',
              'pause-stop-reset': 'pause',
            };

            const buttonIcon = (action: string) => {
              const resolved = resolve(action);
              if (resolved) return t(`action.${resolved}`);
              const hint = verbHintIcon[action];
              if (hint) return t(`action.${hint}`);
              return actionLabel(action);
            };

            const isDisabled = (action: string) => {
              // Primitives: hand-tuned per the looper's state machine.
              // `rec` is always enabled — from Playing starts overdub at
              // current pos; from Idle begins the base recording.
              if (action === 'play')  return isIdle;
              if (action === 'pause') return isIdle || looperState === 'looper-stop';
              if (action === 'stop')  return isIdle;
              if (action === 'undo')  return !(looperState === 'looper-recording' || isOverdub || bufferCnt > 1);
              if (action === 'reset') return isIdle;
              if (action === 'rec')   return false;
              // Combined verbs: disabled iff resolve returns null. The button
              // still renders a hint icon (see `verbHintIcon`).
              return resolve(action) === null;
            };

            // Collect transport cells in canonical order: Event entries
            // (buttons) + the `duration` read-only ParamMeta (timer display).
            // Other read-only ParamMetas (buffer_cnt) are consumed by the
            // Undo button's badge — they don't get their own cell.
            const cells = (node.params_info ?? [])
                .filter(i => i.active !== false && i.visible !== false)
                .filter(i =>
                    i.kind?.tag === 'Event' ||
                    (i.kind?.tag === 'ParamMeta' && i.kind.read_only && i.name === 'duration'));

            const renderCell = (info: ParamInfo) => {
              if (info.kind?.tag === 'Event') {
                const action = info.kind.action;
                let cls = buttonClass(action);
                if (action === 'undo') cls += ' looper-undo-btn';
                if (action === 'undo' && atMerge) cls += ' looper-btn-at-merge';
                return (
                  <button key={info.name}
                    className={cls}
                    disabled={isDisabled(action)}
                    title={action}
                    onMouseDown={e => e.stopPropagation()}
                    onClick={() => sendAction(`${node.key}.${info.name}`, action)}>
                    {buttonIcon(action)}
                    {action === 'undo' && bufferCnt > 0 &&
                      <span className="looper-undo-count">{displayCnt}</span>}
                  </button>
                );
              }
              // Timer display cell (read-only `duration`). Free-running ticker
              // driven by useLooperTimer; bound by node.duration / pos_secs.
              return (
                <div key={info.name} className="looper-time">{looperTime}</div>
              );
            };

            // Split active+visible cells half-and-half; top row gets the
            // extra one when odd. Canonical order = render order.
            const half  = Math.ceil(cells.length / 2);
            const top   = cells.slice(0, half);
            const bot   = cells.slice(half);

            return (
              <div className={`param-cell looper-transport${transportHidden ? ' param-hidden' : ''}`}>
                <div className="looper-transport-inner">
                  <div className="looper-row">{top.map(renderCell)}</div>
                  <div className="looper-row">{bot.map(renderCell)}</div>
                </div>
                <button className="param-vis-btn"
                  title={transportHidden ? t('ui.show_param') : t('ui.hide_param')}
                  onMouseDown={e => e.stopPropagation()}
                  onClick={() => setVisible(TRANSPORT_NAME, transportHidden)}>
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
