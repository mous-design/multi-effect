interface Props {
    message: string;
    onConfirm: () => void;
    onCancel: () => void;
}

export function ConfirmDelete({ message, onConfirm, onCancel }: Props) {
    return (
        <div className="chain-confirm-group">
            <span className="chain-confirm-text">{message}</span>
            <button className="chain-confirm-yes" onClick={onConfirm}>✓</button>
            <button className="chain-confirm-no" onClick={onCancel}>✗</button>
        </div>
    );
}
