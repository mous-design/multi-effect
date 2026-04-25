import { useEffect, useState } from 'react';
import { ControllerDef, DeviceMap, NodeDef } from '../types';
import { PARAM_META } from './EffectTile';
import { putControllers } from '../api';
import { t } from '../i18n';

interface MappingRow {
    id: string;
    device: string;
    ctrlKey: string;
    targetNode: string;
    targetParam: string;
    ctrlMin: number;
    ctrlMax: number;
    paramMin: number;
    paramMax: number;
}

function fromApi(defs: ControllerDef[]): MappingRow[] {
    const rows: MappingRow[] = [];
    for (const def of defs) {
        for (const [key, cd] of Object.entries(def.mappings)) {
            const dot = cd.target.indexOf('.');
            rows.push({
                id: Math.random().toString(36).slice(2),
                device: def.device,
                ctrlKey: key,
                targetNode: dot >= 0 ? cd.target.slice(0, dot) : cd.target,
                targetParam: dot >= 0 ? cd.target.slice(dot + 1) : '',
                ctrlMin: cd.ctrl[0],
                ctrlMax: cd.ctrl[1],
                paramMin: cd.param[0],
                paramMax: cd.param[1],
            });
        }
    }
    return rows.sort((a, b) =>
        a.device !== b.device ? a.device.localeCompare(b.device) : a.ctrlKey.localeCompare(b.ctrlKey)
    );
}

function toApi(rows: MappingRow[]): ControllerDef[] {
    const map = new Map<string, ControllerDef>();
    for (const row of rows) {
        if (!row.device || !row.ctrlKey || !row.targetNode || !row.targetParam) continue;
        if (!map.has(row.device)) map.set(row.device, { device: row.device, mappings: {} });
        map.get(row.device)!.mappings[row.ctrlKey] = {
            target: `${row.targetNode}.${row.targetParam}`,
            ctrl: [row.ctrlMin, row.ctrlMax],
            param: [row.paramMin, row.paramMax],
        };
    }
    return Array.from(map.values());
}

const SKIP_PARAMS = new Set(['key', 'type', 'active']);

// --- Sub-components ---

function MappingGridRow({ row, nodeKeys, paramsFor, onUpdate, onDelete }: {
    row: MappingRow;
    nodeKeys: string[];
    paramsFor: (nodeKey: string) => string[];
    onUpdate: (id: string, patch: Partial<MappingRow>) => void;
    onDelete: (id: string) => void;
}) {
    return <>
        <input type="text" value={row.ctrlKey}
            onChange={e => onUpdate(row.id, { ctrlKey: e.target.value })}
            placeholder="70" />
        <select value={row.targetNode} onChange={e => {
            const node = e.target.value;
            onUpdate(row.id, { targetNode: node, targetParam: paramsFor(node)[0] ?? '' });
        }}>
            {nodeKeys.map(k => <option key={k} value={k}>{k}</option>)}
        </select>
        <select value={row.targetParam} onChange={e => {
            const p = e.target.value;
            const meta = PARAM_META[p];
            onUpdate(row.id, { targetParam: p, paramMin: meta?.min ?? 0, paramMax: meta?.max ?? 1 });
        }}>
            {paramsFor(row.targetNode).map(p => <option key={p} value={p}>{p}</option>)}
        </select>
        <div className="range-pair">
            <input type="number" value={row.ctrlMin}
                onChange={e => onUpdate(row.id, { ctrlMin: parseFloat(e.target.value) || 0 })} />
            <input type="number" value={row.ctrlMax}
                onChange={e => onUpdate(row.id, { ctrlMax: parseFloat(e.target.value) || 127 })} />
        </div>
        <div className="range-pair">
            <input type="number" value={row.paramMin} step="0.01"
                onChange={e => onUpdate(row.id, { paramMin: parseFloat(e.target.value) || 0 })} />
            <input type="number" value={row.paramMax} step="0.01"
                onChange={e => onUpdate(row.id, { paramMax: parseFloat(e.target.value) || 1 })} />
        </div>
        <button className="tile-delete" onClick={() => onDelete(row.id)}>×</button>
    </>;
}

function DeviceGroup({ alias, rows, nodeKeys, paramsFor, onUpdate, onDelete, onAddRow }: {
    alias: string;
    rows: MappingRow[];
    nodeKeys: string[];
    paramsFor: (nodeKey: string) => string[];
    onUpdate: (id: string, patch: Partial<MappingRow>) => void;
    onDelete: (id: string) => void;
    onAddRow: (device: string) => void;
}) {
    return (
        <div className="device-group">
            <div className="dg-header">
                <span className="dg-title">{alias}</span>
                <button className="dg-add-row-btn" onClick={() => onAddRow(alias)}>＋ {t('ui.add_mapping')}</button>
            </div>
            <div className="dg-grid">
                <div className="dg-col-hdr">{t('ui.mapping_ctrl')}</div>
                <div className="dg-col-hdr">{t('ui.mapping_node')}</div>
                <div className="dg-col-hdr">{t('ui.mapping_param')}</div>
                <div className="dg-col-hdr">{t('ui.mapping_ctrl_range')}</div>
                <div className="dg-col-hdr">{t('ui.mapping_param_range')}</div>
                <div />
                {rows.map(row => (
                    <MappingGridRow key={row.id} row={row} nodeKeys={nodeKeys}
                        paramsFor={paramsFor} onUpdate={onUpdate} onDelete={onDelete} />
                ))}
            </div>
        </div>
    );
}

// --- Main component ---

interface Props {
    controllers: ControllerDef[];
    devices: DeviceMap;
    allNodes: NodeDef[];
    onSave: (controllers: ControllerDef[]) => void;
    onClose: () => void;
}

export function MappingsPanel({ controllers, devices, allNodes, onSave, onClose }: Props) {
    const [rows, setRows] = useState<MappingRow[]>([]);
    const [saving, setSaving] = useState(false);

    useEffect(() => {
        setRows(fromApi(controllers));
    }, [controllers]);

    const deviceAliases = Object.keys(devices);
    const nodeKeys = [...new Set(allNodes.map(n => n.key))];

    function paramsFor(nodeKey: string): string[] {
        return allNodes
            .filter(n => n.key === nodeKey)
            .flatMap(n => Object.keys(n).filter(p => !SKIP_PARAMS.has(p)));
    }

    function defaultCtrlMax(alias: string): number {
        const def = devices[alias];
        return (!def || def.type === 'midi-in' || def.type === 'midi-out') ? 127 : 1023;
    }

    function defaultCtrlKey(alias: string): string {
        const def = devices[alias];
        return (!def || def.type === 'midi-in' || def.type === 'midi-out') ? '70' : 'ctrl_1';
    }

    const [showAddDevice, setShowAddDevice] = useState(false);

    function addRowForDevice(device: string) {
        const targetNode = nodeKeys[0] ?? '';
        const targetParam = paramsFor(targetNode)[0] ?? '';
        const meta = PARAM_META[targetParam];
        setRows(prev => [...prev, {
            id: Math.random().toString(36).slice(2),
            device,
            ctrlKey: defaultCtrlKey(device),
            targetNode,
            targetParam,
            ctrlMin: 0,
            ctrlMax: defaultCtrlMax(device),
            paramMin: meta?.min ?? 0,
            paramMax: meta?.max ?? 1,
        }]);
    }

    function update(id: string, patch: Partial<MappingRow>) {
        setRows(prev => prev.map(r => r.id === id ? { ...r, ...patch } : r));
    }

    function deleteRow(id: string) {
        setRows(prev => prev.filter(r => r.id !== id));
    }

    async function handleApply() {
        setSaving(true);
        const updated = toApi(rows);
        if (await putControllers(updated)) {
            setSaving(false);
            onSave(updated);
            onClose();
        }
    }

    const shownDevices = deviceAliases.filter(alias => rows.some(r => r.device === alias));
    const hiddenDevices = deviceAliases.filter(alias => !rows.some(r => r.device === alias));

    return (
        <div className="mappings-panel">
            <div className="mappings-header">
                <span className="mappings-title">{t('ui.ctrl_mappings')}</span>
            </div>

            <div className="device-groups">
                {shownDevices.map(alias => (
                    <DeviceGroup key={alias} alias={alias}
                        rows={rows.filter(r => r.device === alias)}
                        nodeKeys={nodeKeys} paramsFor={paramsFor}
                        onUpdate={update} onDelete={deleteRow} onAddRow={addRowForDevice} />
                ))}
            </div>

            {hiddenDevices.length > 0 && (
                <div className="dg-add-area">
                    {showAddDevice ? (
                        <div className="dg-device-list">
                            {hiddenDevices.map(alias => (
                                <button key={alias} className="dg-device-opt"
                                    onClick={() => { addRowForDevice(alias); setShowAddDevice(false); }}>
                                    {alias}
                                </button>
                            ))}
                            <button className="dg-device-cancel" onClick={() => setShowAddDevice(false)}>{t('ui.cancel')}</button>
                        </div>
                    ) : (
                        <button className="mappings-add-btn" onClick={() => setShowAddDevice(true)}>{t('device.add')}</button>
                    )}
                </div>
            )}

            <div className="mappings-footer">
                <button className="popup-confirm" onClick={handleApply} disabled={saving}>
                    {saving ? '…' : t('ui.apply')}
                </button>
                <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
            </div>
        </div>
    );
}
