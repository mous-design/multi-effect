import { useRef, useState } from 'react';
import { ChainDef, DeviceMap, NodeDef } from '../types';
import { EffectTile } from './EffectTile';
import { MappingsPanel } from './MappingsPanel';
import { t } from '../i18n';

const EQ_TYPES = new Set(['eq_param', 'eq_low', 'eq_high']);

type NodeItem = NodeDef | NodeDef[];

function groupNodes(nodes: NodeDef[]): NodeItem[] {
  const result: NodeItem[] = [];
  let i = 0;
  while (i < nodes.length) {
    if (EQ_TYPES.has(nodes[i].type)) {
      const group = [nodes[i]];
      while (i + 1 < nodes.length && EQ_TYPES.has(nodes[i + 1].type)) {
        i++;
        group.push(nodes[i]);
      }
      result.push(group.length >= 2 ? group : group[0]);
    } else {
      result.push(nodes[i]);
    }
    i++;
  }
  return result;
}

function flattenItem(item: NodeItem): NodeDef[] {
  return Array.isArray(item) ? item : [item];
}

function itemKey(item: NodeItem): string {
  return Array.isArray(item) ? item.map(n => n.key).join('|') : item.key;
}

const EFFECT_TYPES = ['delay', 'reverb', 'chorus', 'looper', 'mix', 'eq_param', 'eq_low', 'eq_high'];

const DEFAULT_PARAMS: Record<string, object> = {
  delay:      { time: 0.3, feedback: 0.4, wet: 0.5, active: true },
  reverb:     { room_size: 0.5, damping: 0.5, wet: 0.3, active: true },
  chorus:     { rate_hz: 1.0, depth_ms: 8, wet: 0.3, active: true },
  harmonizer: { root: 57, wet: 0.5, vel_sense: 0.0, active: true },
  looper:     { wet: 1.0, decay: 1.0, active: true },
  mix:        { dry: 0.0, wet: 1.0, gain: 1.0, pan: 0.0, active: true },
  eq_param:   { freq: 1000, gain_db: 0, q: 1.0, active: true },
  eq_low:     { freq: 200,  gain_db: 0, active: true },
  eq_high:    { freq: 8000, gain_db: 0, active: true },
};

interface Props {
  chainIdx: number;
  chain: ChainDef;
  presetName: string;
  devices: DeviceMap;
  allNodes: NodeDef[];
  delayMaxSeconds: number;
  onSet: (path: string, value: number | boolean) => void;
  onDelete: (key: string) => void;
  onReorder: (chainIdx: number, newNodes: NodeDef[]) => void;
  onAddNode: (chainIdx: number, node: NodeDef) => void;
  onDeleteChain: (chainIdx: number) => void;
  onRouting: (chainIdx: number) => void;
}

export function ChainView({ chainIdx, chain, presetName, devices, allNodes, delayMaxSeconds, onSet, onDelete, onReorder, onAddNode, onDeleteChain, onRouting }: Props) {
  const items = groupNodes(chain.nodes);
  const presetNum = parseInt(presetName, 10);

  const [mappingsOpen, setMappingsOpen] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  // Drag state
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const [dragOverIndex, setDragOverIndex] = useState<number | null>(null);
  const dragItem = useRef<number | null>(null);
  const dragFromHeader = useRef(false);

  function handleMouseDown(e: React.MouseEvent, el: HTMLElement) {
    const headers = Array.from(el.querySelectorAll('.tile-header'));
    dragFromHeader.current = headers.some(h => h.contains(e.target as Node));
  }

  function handleDragStart(e: React.DragEvent, idx: number) {
    if (!dragFromHeader.current) {
      e.preventDefault();
      return;
    }
    setDragIndex(idx);
    dragItem.current = idx;
  }

  function handleDragOver(e: React.DragEvent, idx: number) {
    e.preventDefault();
    setDragOverIndex(idx);
  }

  function handleDrop(e: React.DragEvent, idx: number) {
    e.preventDefault();
    const from = dragItem.current;
    if (from === null || from === idx) {
      setDragIndex(null);
      setDragOverIndex(null);
      return;
    }
    const newItems = [...items];
    const [moved] = newItems.splice(from, 1);
    newItems.splice(idx, 0, moved);
    const newNodes = newItems.flatMap(flattenItem);
    onReorder(chainIdx, newNodes);
    setDragIndex(null);
    setDragOverIndex(null);
    dragItem.current = null;
  }

  function handleDragEnd() {
    setDragIndex(null);
    setDragOverIndex(null);
    dragItem.current = null;
  }

  // Add node form state
  const [showAddForm, setShowAddForm] = useState(false);
  const [addType, setAddType] = useState('delay');
  const [addKey, setAddKey] = useState('');

  function suggestKey(type: string, nodeCount: number): string {
    return `${String(nodeCount + 1).padStart(2, '0')}-${type}`;
  }

  function handleAddTypeChange(t: string) {
    setAddType(t);
    setAddKey(suggestKey(t, chain.nodes.length));
  }

  function handleOpenAddForm() {
    setAddType('delay');
    setAddKey(suggestKey('delay', chain.nodes.length));
    setShowAddForm(true);
  }

  function handleAddNode() {
    const key = addKey.trim();
    if (!key) return;
    const node: NodeDef = { key, type: addType, ...DEFAULT_PARAMS[addType] };
    onAddNode(chainIdx, node);
    setShowAddForm(false);
  }

  return (
    <div className="chain">
      <div className="chain-header">
        <button
          className={`chain-caret${mappingsOpen ? ' chain-caret-active' : ''}`}
          onClick={() => { setMappingsOpen(o => !o); setConfirmDelete(false); }}
          title={t('ui.ctrl_mappings')}
        >
          <svg width="14" height="12" viewBox="0 0 14 12" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <line x1="0" y1="2"  x2="14" y2="2"  />
            <line x1="0" y1="6"  x2="14" y2="6"  />
            <line x1="0" y1="10" x2="14" y2="10" />
            <circle cx="9" cy="2"  r="2" fill="currentColor" stroke="none" />
            <circle cx="4" cy="6"  r="2" fill="currentColor" stroke="none" />
            <circle cx="10" cy="10" r="2" fill="currentColor" stroke="none" />
          </svg>
        </button>
        <button className="chain-routing-btn" onClick={() => onRouting(chainIdx)} title="Edit routing">
          in [{chain.input.join(',')}] → out [{chain.output.join(',')}]
        </button>
        {confirmDelete ? (
          <div className="chain-confirm-group">
            <span className="chain-confirm-text">{t('ui.confirm_delete_chain')}</span>
            <button className="chain-confirm-yes" onClick={() => { setConfirmDelete(false); onDeleteChain(chainIdx); }}>✓</button>
            <button className="chain-confirm-no"  onClick={() => setConfirmDelete(false)}>✗</button>
          </div>
        ) : (
          <button
            className="tile-delete chain-delete"
            onClick={() => chain.nodes.length === 0 ? onDeleteChain(chainIdx) : setConfirmDelete(true)}
            title="Delete chain"
          >×</button>
        )}
      </div>
      {mappingsOpen && (
        <MappingsPanel
          presetNum={presetNum}
          devices={devices}
          allNodes={allNodes}
          onClose={() => setMappingsOpen(false)}
        />
      )}
      <div className="chain-nodes">
        {items.map((item, idx) => {
          const key = itemKey(item);
          const isDragging = dragIndex === idx;
          const isDragOver = dragOverIndex === idx && dragIndex !== idx;
          const cls = [
            'tile-wrapper',
            isDragging ? 'dragging' : '',
            isDragOver ? 'drag-over' : '',
          ].filter(Boolean).join(' ');

          return (
            <div
              key={key}
              className={cls}
              draggable
              onMouseDown={(e) => handleMouseDown(e, e.currentTarget)}
              onDragStart={(e) => handleDragStart(e, idx)}
              onDragOver={(e) => handleDragOver(e, idx)}
              onDrop={(e) => handleDrop(e, idx)}
              onDragEnd={handleDragEnd}
            >
              {Array.isArray(item) ? (
                <div className="eq-group-wrapper">
                  {item.map((node) => (
                    <div key={node.key} className="eq-band-wrapper">
                      <EffectTile node={node} presetName={presetName} delayMaxSeconds={delayMaxSeconds} onSet={onSet} onDelete={onDelete} />
                    </div>
                  ))}
                </div>
              ) : (
                <EffectTile node={item} presetName={presetName} delayMaxSeconds={delayMaxSeconds} onSet={onSet} onDelete={onDelete} />
              )}
            </div>
          );
        })}

        {/* Add node button / form */}
        {!showAddForm ? (
          <button className="add-node-btn" onClick={handleOpenAddForm}>＋ new effect</button>
        ) : (
          <div className="add-node-form">
            <select
              value={addType}
              onChange={e => handleAddTypeChange(e.target.value)}
            >
              {EFFECT_TYPES.map(type => <option key={type} value={type}>{t(`type.${type}`)}</option>)}
            </select>
            <input
              type="text"
              value={addKey}
              onChange={e => setAddKey(e.target.value)}
              placeholder={t('ui.node_key')}
            />
            <button onClick={handleAddNode}>{t('ui.add')}</button>
            <button onClick={() => setShowAddForm(false)}>{t('ui.cancel')}</button>
          </div>
        )}
      </div>
    </div>
  );
}
