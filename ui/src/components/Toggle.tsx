interface ToggleProps {
  nodeKey: string;
  param: string;
  value: boolean;
  label: string;
  onSet: (path: string, value: boolean) => void;
}

export function Toggle({ nodeKey, param, value, label, onSet }: ToggleProps) {
  return (
    <div className="toggle" onClick={() => onSet(`${nodeKey}.${param}`, !value)}>
      <div className={`toggle-track ${value ? 'on' : 'off'}`}>
        <div className="toggle-thumb" />
      </div>
      {label && <div className="toggle-label">{label}</div>}
    </div>
  );
}
