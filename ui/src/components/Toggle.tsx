interface ToggleProps {
  value: boolean;
  label: string;
  title?: string;
  onSet: (value: boolean) => void;
  labelPos?: 'left' | 'right' | 'top' | 'bottom'
}

export function Toggle({value, label, title, onSet, labelPos = 'bottom'}: ToggleProps) {

  return (
    <div className={`toggle toggle-${labelPos}`} onClick={() => onSet(!value)} title={title}>
      <div className={`toggle-track ${value ? 'on' : 'off'}`}>
        <div className="toggle-thumb" />
      </div>
      {label && <div className="toggle-label">{label}</div>}
    </div>
  );
}
