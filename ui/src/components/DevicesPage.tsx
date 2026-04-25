import { useEffect, useRef, useState } from 'react';
import { fetchDevices, putDevice, renameDevice, deleteDevice } from '../api';
import { t } from '../i18n';
import { DeviceField } from './DeviceField';

type AnyDevice = { type: string; active: boolean;[key: string]: any };

const DEVICE_TYPES = ['serial', 'net', 'midi-in', 'midi-out'] as const;
type DeviceType = typeof DEVICE_TYPES[number];

function defaultDef(type: DeviceType): AnyDevice {
    switch (type) {
        case 'serial': return { type: 'serial', dev: '/dev/ttyUSB0', baud: 115200, fallback: true, active: true };
        case 'net': return { type: 'net', host: '0.0.0.0', port: 9000, fallback: true, active: true };
        case 'midi-in': return { type: 'midi-in', channel: '*', active: true };
        case 'midi-out': return { type: 'midi-out', channel: 1, active: true };
    }
}

function sanitizeAlias(s: string): string {
    return s.replace(/\//g, '-');
}

interface TileProps {
    alias: string;
    def: AnyDevice;
    isNew?: boolean;
    onSave: (oldAlias: string, newAlias: string, def: AnyDevice) => Promise<boolean>;
    onDelete: (alias: string) => void;
}

function DeviceTile({ alias: initialAlias, def: initialDef, isNew, onSave, onDelete }: TileProps) {
    const [alias, setAlias] = useState(initialAlias);
    const [def, setDef] = useState<AnyDevice>(initialDef);
    const [dirty, setDirty] = useState(!!isNew);
    const [saving, setSaving] = useState(false);
    const [aliasWarning, setAliasWarning] = useState('');
    const aliasRef = useRef<HTMLInputElement>(null);

    useEffect(() => { if (isNew) aliasRef.current?.focus(); }, [isNew]);

    function set(patch: Partial<AnyDevice>) {
        setDef(prev => ({ ...prev, ...patch }));
        setDirty(true);
    }

    function changeType(type: string) {
        setDef(type ? defaultDef(type as DeviceType) : { type: '', active: def.active });
        setDirty(true);
    }

    async function handleSave() {
        const sanitized = sanitizeAlias(alias);
        if (sanitized !== alias) {
            setAlias(sanitized);
            setAliasWarning(t('device.alias_sanitized', alias, sanitized));
        } else {
            setAliasWarning('');
        }
        setSaving(true);
        const ok = await onSave(initialAlias, sanitized, def);
        setSaving(false);
        if (ok) setDirty(false);
    }

    return (
        <div className={`device-tile${def.active ? '' : ' inactive'}`}>
            <div className="device-tile-header">
                <input
                    type="checkbox"
                    className="device-active"
                    checked={def.active}
                    title={t('ui.active')}
                    onChange={e => set({ active: e.target.checked })}
                />
                <input
                    ref={aliasRef}
                    className="device-alias"
                    value={alias}
                    placeholder={t('device.alias')}
                    onChange={e => { setAlias(e.target.value); setDirty(true); setAliasWarning(''); }}
                />
                <button className="tile-delete" onClick={() => onDelete(initialAlias)}>×</button>
            </div>
            {aliasWarning && <div className="device-alias-warning">{aliasWarning}</div>}

            <div className="device-tile-fields">
                <DeviceField label={t('device.type')}>
                    <select value={def.type} onChange={e => changeType(e.target.value as DeviceType)}>
                        <option value=""> </option>
                        {DEVICE_TYPES.map(dt => (
                            <option key={dt} value={dt}>{t(`device.type.${dt}`)}</option>
                        ))}
                    </select>
                </DeviceField>

                {def.type === 'serial' && <>
                    <DeviceField label={t('device.dev')}>
                        <input value={def.dev ?? ''} onChange={e => set({ dev: e.target.value })} />
                    </DeviceField>
                    <DeviceField label={t('device.baud')}>
                        <select value={def.baud ?? 115200} onChange={e => set({ baud: Number(e.target.value) })}>
                            {[9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600].map(b => (
                                <option key={b} value={b}>{b}</option>
                            ))}
                        </select>
                    </DeviceField>
                    <DeviceField label={t('device.fallback')} wide>
                        <input type="checkbox" checked={def.fallback ?? true} onChange={e => set({ fallback: e.target.checked })} />
                    </DeviceField>
                </>}

                {def.type === 'net' && <>
                    <DeviceField label={t('device.host')}>
                        <input value={def.host ?? '0.0.0.0'} onChange={e => set({ host: e.target.value })} />
                    </DeviceField>
                    <DeviceField label={t('device.port')}>
                        <input type="number" value={def.port ?? 9000} onChange={e => set({ port: Number(e.target.value) })} />
                    </DeviceField>
                    <DeviceField label={t('device.fallback')} wide>
                        <input type="checkbox" checked={def.fallback ?? true} onChange={e => set({ fallback: e.target.checked })} />
                    </DeviceField>
                </>}

                {(def.type === 'midi-in' || def.type === 'midi-out') && <>
                    <DeviceField label={t('device.dev')}>
                        <input
                            value={def.dev ?? ''}
                            placeholder={t('device.dev_any')}
                            onChange={e => set({ dev: e.target.value || undefined })}
                        />
                    </DeviceField>
                    <DeviceField label={t('device.channel')}>
                        {def.type === 'midi-in'
                            ? <select value={def.channel ?? '*'} onChange={e => {
                                const v = e.target.value;
                                set({ channel: v === '*' ? '*' : Number(v) });
                            }}>
                                <option value="*">*</option>
                                {Array.from({ length: 16 }, (_, i) => i + 1).map(ch => (
                                    <option key={ch} value={ch}>{ch}</option>
                                ))}
                            </select>
                            : <select value={def.channel ?? 1} onChange={e => set({ channel: Number(e.target.value) })}>
                                {Array.from({ length: 16 }, (_, i) => i + 1).map(ch => (
                                    <option key={ch} value={ch}>{ch}</option>
                                ))}
                            </select>
                        }
                    </DeviceField>
                </>}
            </div>

            {dirty && (
                <div className="device-tile-footer">
                    <button className="device-save-btn" onClick={handleSave} disabled={saving}>
                        {saving ? '…' : t('ui.apply')}
                    </button>
                </div>
            )}
        </div>
    );
}

interface Props {
    onHome: () => void;
}

let nextTempId = 0;

export function DevicesPage({ onHome }: Props) {
    const [devices, setDevices] = useState<Record<string, AnyDevice>>({});
    const [pending, setPending] = useState<Array<{ id: number; def: AnyDevice }>>([]);

    useEffect(() => {
        fetchDevices().then(d => setDevices(d ?? {}));
    }, []);

    async function handleSave(oldAlias: string, newAlias: string, def: AnyDevice): Promise<boolean> {
        if (!newAlias.trim()) return false;
        if (oldAlias && oldAlias !== newAlias) {
            // Rename: atomically moves device + updates all preset controller refs
            const ok = await renameDevice(oldAlias, newAlias);
            if (!ok) return false;
            setDevices(prev =>
                Object.fromEntries(Object.entries(prev).map(([k, v]) =>
                    k === oldAlias ? [newAlias, def] : [k, v]
                ))
            );
        } else {
            const ok = await putDevice(newAlias, def);
            if (!ok) return false;
            setDevices(prev => ({ ...prev, [newAlias]: def }));
        }
        return true;
    }

    async function handleDelete(alias: string): Promise<void> {
        if (await deleteDevice(alias)) {
            setDevices(prev => { const next = { ...prev }; delete next[alias]; return next; });
        }
    }

    async function handleSaveNew(tempId: number, _oldAlias: string, newAlias: string, def: AnyDevice): Promise<boolean> {
        if (!newAlias.trim()) return false;
        if (await putDevice(newAlias, def)) {
            setDevices(prev => ({ ...prev, [newAlias]: def }));
            setPending(prev => prev.filter(p => p.id !== tempId));
            return true;
        }
        return false;
    }

    function handleCancelNew(tempId: number) {
        setPending(prev => prev.filter(p => p.id !== tempId));
    }

    return (
        <div className="app">
            <header className="app-header">
                <button className="back-btn" onClick={onHome} title={t('ui.home')}>←</button>
                <h1 className="app-title-link" onClick={onHome}>multi-effect</h1>
                <span className="devices-page-title">{t('ui.devices')}</span>
            </header>

            <main className="devices-main">
                <div className="device-tiles">
                    {Object.entries(devices).map(([alias, def]) => (
                        <DeviceTile
                            key={alias}
                            alias={alias}
                            def={def}
                            onSave={handleSave}
                            onDelete={handleDelete}
                        />
                    ))}
                    {pending.map(({ id, def }) => (
                        <DeviceTile
                            key={`new-${id}`}
                            alias=""
                            def={def}
                            isNew
                            onSave={(old, newAlias, d) => handleSaveNew(id, old, newAlias, d)}
                            onDelete={() => handleCancelNew(id)}
                        />
                    ))}
                </div>

                <button className="new-chain-btn" onClick={() => {
                    setPending(prev => [...prev, { id: nextTempId++, def: { type: '', active: true } as AnyDevice }]);
                }}>
                    {t('device.add')}
                </button>
            </main>
        </div>
    );
}
