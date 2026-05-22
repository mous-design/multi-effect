import { useState } from 'react';
import { createPortal } from 'react-dom';
import type { NodeDef } from '../types';
import { sendParamMeta } from '../api';
import { Popup } from './Popup';
import { ParamRow, boundFor } from './ParamAspectsRow';
import { t } from '../i18n';

interface Props {
    node: NodeDef;
    onClose: () => void;
}

type Pending = Record<string, Record<string, number | boolean>>;
type Edit    = { param: string; aspect: string; value: number | boolean };

/// Per-tile Instance overrides editor.
///
/// Stages edits locally and commits all of them on Save (one `SET` per
/// aspect). Cancel discards. The popup is portalled to `document.body` so
/// parent-tile styles (e.g. the `inactive` opacity) don't bleed onto it.
///
/// Lists each live param (`ParamKind::ParamMeta`) with its editable aspects.
/// The optional `BoundMeta` entries on the same canonical declare the
/// envelope each aspect override can take; the row consults them via
/// `boundFor` for input-range hints.
/// `DiscreteFloat` / `Event` params are skipped — no useful Instance bounds.
export function TileSettingsPopup({ node, onClose }: Props) {
    const params = (node.params_info ?? []).filter(i =>
        i.kind?.tag === 'ParamMeta' || i.kind?.tag === 'Setting');
    const [pending, setPending] = useState<Pending>({});
    // Edits the server refused pending reload acknowledgement. Non-null →
    // confirm popup is showing; on confirm we replay these with the flag set,
    // on cancel we drop them (already-applied edits stay applied).
    const [needConfirm, setNeedConfirm] = useState<Edit[] | null>(null);

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

    async function save() {
        // Fire each pending aspect edit; server applies in-bounds ones and
        // refuses bound-grow ones with `confirm_required:`. Collect the
        // refused ones, then surface a single reload-acknowledgement popup
        // so the user can replay them with the flag set.
        const edits: Edit[] = Object.entries(pending).flatMap(([param, aspects]) =>
            Object.entries(aspects).map(([aspect, value]) => ({ param, aspect, value })));
        const refused: Edit[] = [];
        for (const e of edits) {
            const { ok, confirmRequired } = await sendParamMeta(node.key, e.param, e.aspect, e.value);
            if (confirmRequired) refused.push(e);
            else if (!ok)        return;  // hard error — toast already fired
        }
        if (refused.length === 0) { onClose(); return; }
        setNeedConfirm(refused);
    }

    async function confirmSave() {
        if (!needConfirm) return;
        for (const e of needConfirm) {
            const { ok } = await sendParamMeta(node.key, e.param, e.aspect, e.value, true);
            if (!ok) { setNeedConfirm(null); return; }
        }
        onClose();
    }

    return createPortal(
        <>
            <Popup title={`${t(`type.${node.type}`)} — ${t('ui.settings')}`}
                onClose={onClose}
                onConfirm={save}
                confirmLabel={t('ui.apply')}>
                <div className="tile-settings">
                    {params.map(info => (
                        <ParamRow key={info.name} info={info}
                            effective={(aspect, fallback) => effective(info.name, aspect, fallback)}
                            bound={aspect => boundFor(node.params_info, info.name, aspect)}
                            onChange={(aspect, v) => setAspect(info.name, aspect, v)} />
                    ))}
                </div>
            </Popup>
            {needConfirm && (
                <Popup title={t('ui.confirm_reload_title')}
                    onClose={() => setNeedConfirm(null)}
                    onConfirm={confirmSave}
                    confirmLabel={t('ui.apply')}>
                    <p>{t('ui.confirm_reload_body')}</p>
                </Popup>
            )}
        </>,
        document.body,
    );
}
