import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import type { NodeDef, ParamInfo } from '../types';
import { Popup } from './Popup';
import { t } from '../i18n';

interface Props {
    node: NodeDef;
    onMetaSet: (nodeKey: string, param: string, aspect: string, value: number | boolean) => void;
    onClose: () => void;
}

type Pending = Record<string, Record<string, number | boolean>>;

/// Per-tile Instance overrides editor.
///
/// Stages edits locally and commits all of them on Save (one `SET` per
/// aspect). Cancel discards. The popup is portalled to `document.body` so
/// parent-tile styles (e.g. the `inactive` opacity) don't bleed onto it.
///
/// Lists each live param (`ParamKind::ParamMeta`) with its editable aspects.
/// `DiscreteFloat` / `Event` params are skipped — no useful Instance bounds.
export function TileSettingsPopup({ node, onMetaSet, onClose }: Props) {
    const params = (node.params_info ?? []).filter(i => i.kind?.tag === 'ParamMeta');
    const [pending, setPending] = useState<Pending>({});

    function setAspect(param: string, aspect: string, value: number | boolean) {
        setPending(prev => ({
            ...prev,
            [param]: { ...(prev[param] ?? {}), [aspect]: value },
        }));
    }

    function effective(param: string, aspect: string, fallback: number | boolean) {
        const v = pending[param]?.[aspect];
        return v !== undefined ? v : fallback;
    }

    function save() {
        for (const [param, aspects] of Object.entries(pending)) {
            for (const [aspect, value] of Object.entries(aspects)) {
                onMetaSet(node.key, param, aspect, value);
            }
        }
        onClose();
    }

    return createPortal(
        <Popup title={`${t(`type.${node.type}`)} — ${t('ui.settings')}`}
            onClose={onClose}
            onConfirm={save}
            confirmLabel={t('ui.save_quick')}>
            <div className="tile-settings">
                {params.map(info => (
                    <ParamRow key={info.name} info={info}
                        effective={(aspect, fallback) => effective(info.name, aspect, fallback)}
                        onChange={(aspect, v) => setAspect(info.name, aspect, v)} />
                ))}
            </div>
        </Popup>,
        document.body,
    );
}

function ParamRow({ info, effective, onChange }: {
    info: ParamInfo;
    effective: (aspect: string, fallback: number | boolean) => number | boolean;
    onChange: (aspect: string, value: number | boolean) => void;
}) {
    switch (info.type) {
        case 'ContinuousFloat':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} />
                    <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} />
                    <BoolField   label="log"     value={effective('log',     !!info.log)   as boolean} onChange={v => onChange('log', v)} />
                </div>
            );
        case 'ContinuousInt':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} integer />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} integer />
                    <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} integer />
                </div>
            );
        case 'DiscreteBool':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <BoolField label="default" value={effective('default', info.default) as boolean} onChange={v => onChange('default', v)} />
                </div>
            );
        // DiscreteFloat / Event: no aspect editor today.
        default:
            return null;
    }
}

/// Commit-on-blur/Enter number input. Reverts to current value on invalid
/// entry or when unchanged on blur.
function NumberField({ label, value, onCommit, integer }: {
    label: string;
    value: number;
    onCommit: (v: number) => void;
    integer?: boolean;
}) {
    const [text, setText] = useState(String(value));
    useEffect(() => { setText(String(value)); }, [value]);
    const commit = () => {
        const v = integer ? parseInt(text, 10) : parseFloat(text);
        if (Number.isFinite(v) && v !== value) onCommit(v);
        else setText(String(value));
    };
    return (
        <label className="param-settings-field">
            <span>{label}</span>
            <input type="number" value={text}
                step={integer ? 1 : 'any'}
                onChange={e => setText(e.target.value)}
                onBlur={commit}
                onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />
        </label>
    );
}

function BoolField({ label, value, onChange }: {
    label: string;
    value: boolean;
    onChange: (v: boolean) => void;
}) {
    return (
        <label className="param-settings-field">
            <span>{label}</span>
            <input type="checkbox" checked={value}
                onChange={e => onChange(e.target.checked)} />
        </label>
    );
}
