import { useRef, useState } from 'react';
import { ChainDef, ControllerDef, DeviceMap, NodeDef } from '../types';
import { EffectTile } from './EffectTile';
import { MappingsPanel } from './MappingsPanel';
import { ConfirmDelete } from './ConfirmDelete';
import { t } from '../i18n';

const EQ_TYPES = new Set(['eq_mid', 'eq_low', 'eq_high']);

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

const EFFECT_TYPES = ['delay', 'reverb', 'chorus', 'looper', 'mix', 'eq_mid', 'eq_low', 'eq_high'];

interface Props {
  chainIdx: number;
  chain: ChainDef;
  presetName: string;
  controllers: ControllerDef[];
  devices: DeviceMap;
  allNodes: NodeDef[];
  onSet: (path: string, value: number | boolean) => void;
  onMetaSet: (nodeKey: string, param: string, aspect: string, value: number | boolean) => void;
  onDelete: (key: string) => void;
  onReorder: (chainIdx: number, newNodes: NodeDef[]) => void;
  onAddNode: (chainIdx: number, node: NodeDef) => void;
  onDeleteChain: (chainIdx: number) => void;
  onRouting: (chainIdx: number) => void;
  onSaveControllers: (controllers: ControllerDef[]) => void;
}

export function ChainView({ chainIdx, chain, presetName, controllers, devices, allNodes, onSet, onMetaSet, onDelete, onReorder, onAddNode, onDeleteChain, onRouting, onSaveControllers }: Props) {
  const items = groupNodes(chain.nodes);

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
    // No initial params — let backend's canonical defaults apply (sparse storage).
    // PRESET broadcast back will populate params_info; values stay at-default
    // unless the user explicitly edits them.
    const node: NodeDef = { key, type: addType };
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
          <ConfirmDelete
            message={t('ui.confirm_delete_chain')}
            onConfirm={() => { setConfirmDelete(false); onDeleteChain(chainIdx); }}
            onCancel={() => setConfirmDelete(false)}
          />
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
          controllers={controllers}
          devices={devices}
          allNodes={allNodes}
          onSave={onSaveControllers}
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
                      <EffectTile node={node} presetName={presetName} onSet={onSet} onMetaSet={onMetaSet} onDelete={onDelete} />
                    </div>
                  ))}
                </div>
              ) : (
                <EffectTile node={item} presetName={presetName} onSet={onSet} onMetaSet={onMetaSet} onDelete={onDelete} />
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
