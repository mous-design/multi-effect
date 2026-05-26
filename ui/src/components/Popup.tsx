import { ReactNode } from 'react';
import { t } from '../i18n';

interface Props {
    title: string;
    onClose: () => void;
    confirmLabel?: string;
    onConfirm?: () => void;
    confirmDisabled?: boolean;
    /// Optional secondary action(s) — rendered in the right-hand group of the
    /// actions row, before the confirm button. Use for things like
    /// "Effect bounds…" that lead into a sibling popup.
    extraAction?: ReactNode;
    children: ReactNode;
}

/// Layout: Cancel on the left, secondary action(s) + confirm on the right.
/// Children render in the body.
export function Popup({ title, onClose, confirmLabel, onConfirm, confirmDisabled, extraAction, children }: Props) {
    return (
        <div className="popup-overlay" onClick={onClose}>
            <div className="popup" onClick={e => e.stopPropagation()}>
                <p className="popup-title">{title}</p>
                <div className="popup-body">{children}</div>
                <div className="popup-actions">
                    <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
                    <div className="popup-actions-right">
                        {extraAction}
                        {onConfirm && (
                            <button className="popup-confirm" onClick={onConfirm} disabled={confirmDisabled}>
                                {confirmLabel ?? t('ui.apply')}
                            </button>
                        )}
                    </div>
                </div>
            </div>
        </div>
    );
}
