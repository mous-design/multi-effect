import { t } from '../i18n';

interface Props {
    presetNum: number;
    onChangeNum: (n: number) => void;
    onConfirm: () => void;
    onClose: () => void;
}

export function SavePresetPopup({ presetNum, onChangeNum, onConfirm, onClose }: Props) {
    return (
        <div className="popup-overlay" onClick={onClose}>
            <div className="popup" onClick={e => e.stopPropagation()}>
                <p className="popup-title">{t('ui.save_preset_title')}</p>
                <div className="popup-row">
                    <label>{t('ui.preset_number')}</label>
                    <input
                        type="number"
                        min={1}
                        max={127}
                        value={presetNum}
                        onChange={e => onChangeNum(Number(e.target.value))}
                        className="preset-input"
                        autoFocus
                    />
                </div>
                <div className="popup-actions">
                    <button className="popup-confirm" onClick={onConfirm}>{t('ui.save')}</button>
                    <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
                </div>
            </div>
        </div>
    );
}
