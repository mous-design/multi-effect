import { useEffect, useState } from 'react';
import type { ParamInfo } from '../types';
import { t, actionLabel } from '../i18n';

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
export function ParamRow({ info, scope, effective, bound, onChange }: {
    info: ParamInfo;
    /// `'type'` — Type-overrides popup; `'visible'` toggle is shown so the
    /// user can set the default "eye-state" for new instances.
    /// `'instance'` — per-tile settings popup; the `visible` toggle is
    /// suppressed because the tile's own eye-button writes the same Instance
    /// override directly. Two surfaces for one bit of state would just be
    /// noise.
    scope: 'type' | 'instance';
    effective: (aspect: string, fallback: number | boolean) => number | boolean;
    bound?: (aspect: string) => [number, number] | undefined;
    onChange: (aspect: string, value: number | boolean) => void;
}) {
    const unit = (info.type === 'ContinuousFloat' || info.type === 'ContinuousInt') ? info.unit : undefined;

    // `active` controls "declared at all" — toggle to expose an inactive
    // entry, or remove a declared one from a specific instance. No tile-level
    // affordance, so it lives in both popups.
    const activeField = (
        <BoolField label="active" value={effective('active', info.active ?? true) as boolean}
            onChange={v => onChange('active', v)} />
    );
    // `visible` controls "default eye-state on the tile". Only meaningful in
    // the Type-override (sets the default for new instances). For an existing
    // instance, the tile's eye-button already toggles the same Instance
    // override directly.
    const visibleField = scope === 'type' ? (
        <BoolField label="visible" value={effective('visible', info.visible ?? true) as boolean}
            onChange={v => onChange('visible', v)} />
    ) : null;

    // Event entry: handled by `ActionsTable` (compact table-style render with
    // shared column headers). This branch shouldn't be hit when the caller
    // routes Events through `ActionsTable`; included for safety.
    if (info.kind?.tag === 'Event') return null;

    // ActionsGroup: UI grouping anchor (e.g. looper transport). Carries no
    // value of its own — only `active` / `visible` toggles apply. Both
    // overridable via the standard pipeline.
    if (info.kind?.tag === 'ActionsGroup') {
        return (
            <div className="param-settings-row">
                <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                {activeField}
                {visibleField}
            </div>
        );
    }

    // ParamMeta: standard min / max / default / log aspect editor.
    // No BoundMeta declared → envelope is the param's own bounds, matching
    // server-side `apply_override` (`bound_meta_float(...).unwrap_or((cmin, cmax))`).
    // For Type overrides those are canonical; for Instance overrides they're
    // Type-resolved. Either way client and server agree.
    //
    // Read-only entries (effect-driven values like looper duration / buffer_cnt)
    // suppress the `default` field — the effect owns the live value, the user
    // edits `max` to set the cap.
    const readOnly = info.kind?.tag === 'ParamMeta' && info.kind.read_only;
    const ownRange = (info.type === 'ContinuousFloat' || info.type === 'ContinuousInt')
        ? [info.min, info.max] as [number, number]
        : undefined;
    const minBound = bound?.('min') ?? ownRange;
    const maxBound = bound?.('max') ?? ownRange;
    switch (info.type) {
        case 'ContinuousFloat':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} range={minBound} unit={unit} />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} range={maxBound} unit={unit} />
                    {!readOnly && <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} unit={unit} />}
                    <BoolField   label="log"     value={effective('log',     !!info.log)   as boolean} onChange={v => onChange('log', v)} />
                    {activeField}
                    {visibleField}
                </div>
            );
        case 'ContinuousInt':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <NumberField label="min"     value={effective('min',     info.min)     as number} onCommit={v => onChange('min', v)} range={minBound} unit={unit} integer />
                    <NumberField label="max"     value={effective('max',     info.max)     as number} onCommit={v => onChange('max', v)} range={maxBound} unit={unit} integer />
                    {!readOnly && <NumberField label="default" value={effective('default', info.default) as number} onCommit={v => onChange('default', v)} unit={unit} integer />}
                    {activeField}
                    {visibleField}
                </div>
            );
        case 'DiscreteBool':
            return (
                <div className="param-settings-row">
                    <span className="param-settings-name">{t(`param.${info.name}`)}</span>
                    <BoolField label="default" value={effective('default', info.default) as boolean} onChange={v => onChange('default', v)} />
                    {activeField}
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

/// Compact multi-column table for Event entries. Splits the list into
/// `columns` side-by-side mini-tables; each mini-table carries its own
/// `active | visible` subheader. The outer `Actions` section header (in
/// the caller) labels the whole block once.
export function ActionsTable({ events, effective, onChange, columns = 4 }: {
    events: ParamInfo[];
    effective: (param: string, aspect: string, fallback: number | boolean) => number | boolean;
    onChange:  (param: string, aspect: string, value:    number | boolean) => void;
    columns?:  number;
}) {
    // Split into `columns` chunks of (near-)equal length. Earlier chunks get
    // the extra row when the count doesn't divide evenly.
    const total = events.length;
    const cols  = Math.min(columns, Math.max(1, total));
    const sz    = Math.ceil(total / cols);
    const chunks: ParamInfo[][] = Array.from({ length: cols }, (_, i) =>
        events.slice(i * sz, (i + 1) * sz));

    const renderRow = (info: ParamInfo) => {
        if (info.kind?.tag !== 'Event') return null;
        const action = info.kind.action;
        return (
            <div key={info.name} className="param-settings-actions-row">
                <span className="param-settings-actions-icon">{actionLabel(action)}</span>
                <input type="checkbox"
                    checked={effective(info.name, 'active', info.active ?? true) as boolean}
                    onChange={e => onChange(info.name, 'active', e.target.checked)} />
            </div>
        );
    };

    return (
        <div className="param-settings-actions-cols"
             style={{ gridTemplateColumns: `repeat(${cols}, 1fr)` }}>
            {chunks.map((chunk, i) => (
                <div key={i} className="param-settings-actions">
                    <div className="param-settings-actions-row param-settings-actions-header">
                        <span></span>
                        <span>active</span>
                    </div>
                    {chunk.map(renderRow)}
                </div>
            ))}
        </div>
    );
}
