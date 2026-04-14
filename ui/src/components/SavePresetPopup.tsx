import { t } from '../i18n';
import { Popup } from './Popup';

interface Props {
    presetNum: number;
    onChangeNum: (n: number) => void;
    onConfirm: () => void;
    onClose: () => void;
}

export function SavePresetPopup({ presetNum, onChangeNum, onConfirm, onClose }: Props) {
    return (
        <Popup title={t('ui.save_preset_title')} onClose={onClose} confirmLabel={t('ui.save')} onConfirm={onConfirm}>
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
        </Popup>
    );
}
