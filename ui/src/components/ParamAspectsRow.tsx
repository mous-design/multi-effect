import { useEffect, useState } from 'react';
import type { ParamInfo } from '../types';
import { t } from '../i18n';

/// `BoundMeta` envelope lookup for `(param, aspect)` in a `ParamInfo[]` —
/// `[min, max]` an override of that aspect is allowed to take, or
/// `undefined` if no BoundMeta is declared (callers fall back to "any").
export function boundFor(
    params_info: ParamInfo[] | undefined,
    param: string,
    aspect: string,
): [number, number] | undefined {
    if (!params_info) return undefined;
    for (const i of params_info) {
        if (i.name !== param) continue;
        if (i.kind?.tag !== 'BoundMeta' || i.kind.aspect !== aspect) continue;
        if (i.type === 'ContinuousFloat' || i.type === 'ContinuousInt') {
            return [i.min, i.max];
        }
    }
    return undefined;
}

/// One editable row per `ParamMeta` param — renders min / max / default / log
/// fields per `ParamType` variant. The caller owns "current" and "edited"
/// state and decides whether `onChange` stages locally or fires immediately;
/// the row just reads via `effective` and writes via `onChange`.
///
/// `bound` (optional) returns the `BoundMeta` envelope for an aspect — used
/// as the `min`/`max` input range on min/max number fields. Falls through to
/// `undefined` (no constraint) when no BoundMeta is declared for that aspect.
///
/// `DiscreteFloat` / `Event` are skipped — no useful aspect editor today.
export function ParamRow({ info, effective, bound, onChange }: {
    info: ParamInfo;
    effective: (aspect: string, fallback: number | boolean) => number | boolean;
    bound?: (aspect: string) => [number, number] | undefined;
    onChange: (aspect: string, value: number | boolean) => void;
}) {
    const unit = (info.type === 'ContinuousFloat' || info.type === 'ContinuousInt') ? info.unit : undefined;

    // Setting: standalone configurable. Single editable field. The label is
    // just the param's name — context (Per-effect settings + a single value
    // field) makes the role obvious. The aspect lives on the entry purely for
    // wire routing (`<key>.<name>.<aspect>`) and structural disambiguation;
    // doesn't need to be inlined in the human label. The configured value
    // lives in `default`; the entry's own `[min, max]` is the editable envelope.
    if (info.kind?.tag === 'Setting') {
        if (info.type !== 'ContinuousFloat' && info.type !== 'ContinuousInt') return null;
        const aspect = info.kind.aspect;
        return (
            <div className="param-settings-row">
                <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                <NumberField label={aspect}
                    value={effective(aspect, info.default) as number}
                    onCommit={v => onChange(aspect, v)}
                    range={[info.min, info.max]}
                    unit={unit}
                    integer={info.type === 'ContinuousInt'} />
            </div>
        );
    }

    // ParamMeta: standard min / max / default / log / visible aspect editor.
    // No BoundMeta declared → envelope is the param's own bounds, matching
    // server-side `apply_override` (`bound_meta_float(...).unwrap_or((cmin, cmax))`).
    // For Type overrides those are canonical; for Instance overrides they're
    // Type-resolved. Either way client and server agree.
    const ownRange = (info.type === 'ContinuousFloat' || info.type === 'ContinuousInt')
        ? [info.min, info.max] as [number, number]
        : undefined;
    const minBound = bound?.('min') ?? ownRange;
    const maxBound = bound?.('max') ?? ownRange;
    // `visible` is universal across variants.
    const visibleField = (
        <BoolField label="visible" value={effective('visible', info.visible ?? true) as boolean}
            onChange={v => onChange('visible', v)} />
    );
    switch (info.type) {
        case 'ContinuousFloat':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} range={minBound} unit={unit} />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} range={maxBound} unit={unit} />
                    <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} unit={unit} />
                    <BoolField   label="log"     value={effective('log',     !!info.log)   as boolean} onChange={v => onChange('log', v)} />
                    {visibleField}
                </div>
            );
        case 'ContinuousInt':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} range={minBound} unit={unit} integer />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} range={maxBound} unit={unit} integer />
                    <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} unit={unit} integer />
                    {visibleField}
                </div>
            );
        case 'DiscreteBool':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <BoolField label="default" value={effective('default', info.default) as boolean} onChange={v => onChange('default', v)} />
                    {visibleField}
                </div>
            );
        default:
            return null;
    }
}

/// Commit-on-blur/Enter number input. Reverts to current value on invalid
/// entry or when unchanged on blur. `range` (optional) hints the browser
/// via HTML5 min/max and clamps on commit — used by override editors to
/// honour BoundMeta envelopes. `unit` renders next to the input.
export function NumberField({ label, value, onCommit, integer, range, unit }: {
    label: string;
    value: number;
    onCommit: (v: number) => void;
    integer?: boolean;
    range?: [number, number];
    unit?: string;
}) {
    const [text, setText] = useState(String(value));
    useEffect(() => { setText(String(value)); }, [value]);
    const commit = () => {
        let v = integer ? parseInt(text, 10) : parseFloat(text);
        if (!Number.isFinite(v)) { setText(String(value)); return; }
        if (range) v = Math.max(range[0], Math.min(range[1], v));
        if (v !== value) onCommit(v);
        else setText(String(value));
    };
    return (
        <label className="param-settings-field">
            <span>{label}</span>
            <span className="param-settings-input-wrap">
                <input type="number" value={text}
                    step={integer ? 1 : 'any'}
                    min={range?.[0]} max={range?.[1]}
                    onChange={e => setText(e.target.value)}
                    onBlur={commit}
                    onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />
                {unit && <span className="param-settings-unit">{unit}</span>}
            </span>
        </label>
    );
}

export function BoolField({ label, value, onChange }: {
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
