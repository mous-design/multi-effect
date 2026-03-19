export interface Toast {
  id: number;
  msg: string;
  fading: boolean;
}

interface Props {
  toasts: Toast[];
  onDismiss: (id: number) => void;
}

export function Toasts({ toasts, onDismiss }: Props) {
  if (toasts.length === 0) return null;
  return (
    <div className="toast-area">
      {toasts.map(t => (
        <div
          key={t.id}
          className={`toast ${t.fading ? 'fading' : ''}`}
          onClick={() => onDismiss(t.id)}
          title="Click to dismiss"
        >
          <span>{t.msg}</span>
          <span className="toast-x">×</span>
        </div>
      ))}
    </div>
  );
}
