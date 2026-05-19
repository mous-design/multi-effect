import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import type { ParamInfo, TypeOverrides } from '../types';
import { fetchCanonical, fetchTypeOverrides, saveTypeOverrides } from '../api';
import { Popup } from './Popup';
import { ParamRow, boundFor } from './ParamAspectsRow';
import { t } from '../i18n';

interface Props {
    onClose: () => void;
}

/// Global Type-overrides editor.
///
/// Pick an effect type → list its canonical params → edit min / max / default
/// / log per param. Edits are staged locally and committed on Save (single
/// `SAVE_CONFIG` carrying the full merged `type_overrides` map). Backend
/// refreshes `params_info` on every existing instance and broadcasts PRESET.
///
/// Reads the canonical (firmware-declared, pre-override) `ParamInfo` for each
/// effect type via `FETCH_CANONICAL` — that's the absolute envelope. The
/// per-param effective value falls back through pending → saved override →
/// canonical.
export function TypeOverridesPopup({ onClose }: Props) {
    const [canonical, setCanonical] = useState<Record<string, ParamInfo[]> | null>(null);
    const [saved, setSaved] = useState<TypeOverrides>({});
    const [pending, setPending] = useState<TypeOverrides>({});
    const [selectedType, setSelectedType] = useState<string>('');
    // Holds the merged map waiting on user confirmation. `null` = no confirm
    // dialog open; non-null = nested popup is showing this payload.
    const [confirm, setConfirm] = useState<TypeOverrides | null>(null);

    useEffect(() => {
        Promise.all([fetchCanonical(), fetchTypeOverrides()]).then(([can, over]) => {
            setCanonical(can ?? {});
            setSaved(over ?? {});
            // Explicit reset — guards against HMR-preserved state retaining a
            // selection from a previous popup instance. User picks explicitly;
            // auto-picking a first entry was unstable (HashMap iteration order
            // on the server isn't deterministic).
            setSelectedType('');
        });
    }, []);

    function setAspect(type: string, param: string, aspect: string, value: number | boolean) {
        const key = `${param}.${aspect}`;
        setPending(prev => ({
            ...prev,
            [type]: { ...(prev[type] ?? {}), [key]: value },
        }));
    }

    function effective(type: string, param: string, aspect: string, fallback: number | boolean) {
        const key = `${param}.${aspect}`;
        const p = pending[type]?.[key];
        if (p !== undefined) return p;
        const s = saved[type]?.[key];
        if (s !== undefined) return s;
        return fallback;
    }

    async function save() {
        // Deep-merge pending into saved. Pending wins per-key. Try the save
        // without the reload-acknowledgement flag — if the server refuses
        // with `confirm_required:`, the change would widen a non-growable
        // bound. Stage the payload and surface the reload popup; user can
        // resume or cancel.
        const merged: TypeOverrides = {};
        for (const type of new Set([...Object.keys(saved), ...Object.keys(pending)])) {
            merged[type] = { ...(saved[type] ?? {}), ...(pending[type] ?? {}) };
        }
        const { ok, confirmRequired } = await saveTypeOverrides(merged);
        if (ok) { onClose(); return; }
        if (confirmRequired) setConfirm(merged);
        // else: hard error already toasted via the global error handler.
    }

    async function confirmSave() {
        if (!confirm) return;
        const { ok } = await saveTypeOverrides(confirm, true);
        if (ok) onClose();
        else    setConfirm(null);
    }

    if (!canonical) return null;

    const types = Object.keys(canonical).sort();
    const params = (canonical[selectedType] ?? []).filter(i => i.kind?.tag === 'ParamMeta');

    return createPortal(
        <>
            <Popup title={t('ui.type_overrides')}
                onClose={onClose}
                onConfirm={save}
                confirmLabel={t('ui.save_quick')}>
                <div className="type-overrides">
                    <div className="type-overrides-picker">
                        <label>
                            <span>{t('ui.effect_type')}</span>
                            <select value={selectedType} onChange={e => setSelectedType(e.target.value)}>
                                <option value="">{t('ui.select_effect')}</option>
                                {types.map(type => (
                                    <option key={type} value={type}>{t(`type.${type}`)}</option>
                                ))}
                            </select>
                        </label>
                    </div>
                    {selectedType && (
                        <div className="tile-settings">
                            {params.map(info => (
                                <ParamRow key={info.name} info={info}
                                    effective={(aspect, fallback) => effective(selectedType, info.name, aspect, fallback)}
                                    bound={aspect => boundFor(canonical[selectedType], info.name, aspect)}
                                    onChange={(aspect, v) => setAspect(selectedType, info.name, aspect, v)} />
                            ))}
                        </div>
                    )}
                </div>
            </Popup>
            {confirm && (
                <Popup title={t('ui.confirm_reload_title')}
                    onClose={() => setConfirm(null)}
                    onConfirm={confirmSave}
                    confirmLabel={t('ui.save_quick')}>
                    <p>{t('ui.confirm_reload_body')}</p>
                </Popup>
            )}
        </>,
        document.body,
    );
}
