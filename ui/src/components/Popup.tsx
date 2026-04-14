import { ReactNode } from 'react';
import { t } from '../i18n';

interface Props {
    title: string;
    onClose: () => void;
    confirmLabel?: string;
    onConfirm?: () => void;
    confirmDisabled?: boolean;
    children: ReactNode;
}

export function Popup({ title, onClose, confirmLabel, onConfirm, confirmDisabled, children }: Props) {
    return (
        <div className="popup-overlay" onClick={onClose}>
            <div className="popup" onClick={e => e.stopPropagation()}>
                <p className="popup-title">{title}</p>
                {children}
                <div className="popup-actions">
                    {onConfirm && (
                        <button className="popup-confirm" onClick={onConfirm} disabled={confirmDisabled}>
                            {confirmLabel ?? t('ui.apply')}
                        </button>
                    )}
                    <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
                </div>
            </div>
        </div>
    );
}
