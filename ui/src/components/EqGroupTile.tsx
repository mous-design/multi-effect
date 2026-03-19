import { NodeDef } from '../types';
import { Knob } from './Knob';
import { Toggle } from './Toggle';

const EQ_PARAMS: Record<string, { min: number; max: number; label: string; unit?: string }> = {
  freq:    { min: 20,  max: 20000, label: 'Freq', unit: 'Hz' },
  gain_db: { min: -24, max: 24,    label: 'Gain', unit: 'dB' },
  q:       { min: 0.1, max: 10,    label: 'Q' },
};

const TYPE_SHORT: Record<string, string> = {
  eq_param: 'Peak',
  eq_low:   'Low',
  eq_high:  'High',
};

interface Props {
  nodes: NodeDef[];
  onSet: (path: string, value: number | boolean) => void;
  onDelete: (key: string) => void;
}

export function EqGroupTile({ nodes, onSet, onDelete }: Props) {
  return (
    <div className="tile tile-eq-group">
      <div className="tile-header">
        <span className="tile-type">EQ</span>
      </div>
      <div className="eq-bands">
        {nodes.map((node) => (
          <div key={node.key} className="eq-band">
            <div className="eq-band-label">
              {TYPE_SHORT[node.type] ?? node.type}
              <button className="tile-delete" onClick={() => onDelete(node.key)} title="Delete">×</button>
            </div>
            {typeof node.active === 'boolean' &&
              <Toggle nodeKey={node.key} param="active" value={node.active}
                label="On" onSet={(p, v) => onSet(p, v)} />}
            {Object.entries(node).filter(([k]) => EQ_PARAMS[k]).map(([param, val]) => {
              const meta = EQ_PARAMS[param];
              return <Knob key={param} nodeKey={node.key} param={param}
                value={val as number} min={meta.min} max={meta.max}
                label={meta.label} unit={meta.unit}
                onSet={(p, v) => onSet(p, v)} />;
            })}
          </div>
        ))}
      </div>
    </div>
  );
}
