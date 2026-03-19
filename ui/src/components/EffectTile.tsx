import { useEffect, useState } from 'react';
import { NodeDef } from '../types';
import { Knob } from './Knob';
import { Toggle } from './Toggle';
import { t } from '../i18n';

const PARAM_META: Record<string, { min: number; max: number; unit?: string }> = {
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
  freq:      { min: 50,   max: 10000, unit: 'Hz' },
  gain_db:   { min: -15,  max: 15,  unit: 'dB' },
  q:         { min: 0.1,  max: 5              },
  loop_gain: { min: 0,    max: 4              },
};

const SKIP_PARAMS = new Set(['key', 'type', 'active']);

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
};

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
  onSet: (path: string, value: number | boolean) => void;
  onDelete: (key: string) => void;
}

export function EffectTile({ node, presetName, onSet, onDelete }: Props) {
  const [hidden, setHidden] = useState(() => loadHidden(presetName, node.key, node.type));
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    setHidden(loadHidden(presetName, node.key, node.type));
    setExpanded(false);
  }, [presetName, node.key]);

  const hasActive = node.active !== undefined;
  const active = hasActive ? !!node.active : true;

  const allParams = getOrderedParams(node);
  const hiddenCount = allParams.filter(([k]) => hidden.has(k)).length;

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
      const meta = PARAM_META[param] ?? { min: 0, max: 1 };
      return <Knob nodeKey={node.key} param={param}
        value={scalar} min={meta.min} max={meta.max}
        label={t(`param.${param}`)} unit={meta.unit}
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
      <div className="tile-params">
        {(expanded ? allParams : visibleParams).map(([param, val]) => {
          const isHidden = hidden.has(param);
          const ctrl = renderControl(param, val);
          if (!ctrl) return null;
          return (
            <div key={param} className={`param-cell${isHidden ? ' param-hidden' : ''}`}>
              {ctrl}
              {isHidden
                ? <button className="param-show-btn" onClick={() => show(param)} title={t('ui.show_param')}>+</button>
                : <button className="param-hide-btn" onClick={() => hide(param)} title={t('ui.hide_param')}>−</button>
              }
            </div>
          );
        })}
      </div>
      <div className="tile-footer">
        {hiddenCount > 0 && (
          <button className="tile-expand" onClick={() => setExpanded(e => !e)}>
            {expanded ? '▴' : `▾ ${hiddenCount}`}
          </button>
        )}
      </div>
    </div>
  );
}
