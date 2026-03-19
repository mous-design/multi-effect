import { useRef } from 'react';

interface KnobProps {
  nodeKey: string;
  param: string;
  value: number;
  min: number;
  max: number;
  label: string;
  unit?: string;
  onSet: (path: string, value: number) => void;
}

function toRad(deg: number) { return deg * Math.PI / 180; }

function arcPath(cx: number, cy: number, r: number, startDeg: number, endDeg: number) {
  const x1 = cx + r * Math.sin(toRad(startDeg));
  const y1 = cy - r * Math.cos(toRad(startDeg));
  const x2 = cx + r * Math.sin(toRad(endDeg));
  const y2 = cy - r * Math.cos(toRad(endDeg));
  const span = endDeg - startDeg;
  const large = Math.abs(span) > 180 ? 1 : 0;
  const sweep = span > 0 ? 1 : 0;
  return `M ${x1.toFixed(2)} ${y1.toFixed(2)} A ${r} ${r} 0 ${large} ${sweep} ${x2.toFixed(2)} ${y2.toFixed(2)}`;
}

export function Knob({ nodeKey, param, value, min, max, label, unit, onSet }: KnobProps) {
  const dragRef = useRef<{ startY: number; startVal: number } | null>(null);
  const cx = 40, cy = 40, r = 28;
  const START = -135, END = 135, RANGE = 270;
  const norm = Math.max(0, Math.min(1, (value - min) / (max - min)));
  const valueAngle = START + norm * RANGE;

  const trackPath = arcPath(cx, cy, r, START, END);
  const valuePath = norm > 0.001 ? arcPath(cx, cy, r, START, valueAngle) : null;
  const tipX = cx + r * 0.6 * Math.sin(toRad(valueAngle));
  const tipY = cy - r * 0.6 * Math.cos(toRad(valueAngle));

  const fmt = (v: number) => {
    if (Math.abs(max - min) <= 1) return v.toFixed(2);
    if (Math.abs(max - min) < 10) return v.toFixed(1);
    return Math.round(v).toString();
  };

  const onPointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    dragRef.current = { startY: e.clientY, startVal: value };
  };
  const onPointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    if (!dragRef.current) return;
    const dy = dragRef.current.startY - e.clientY;
    const newVal = Math.max(min, Math.min(max, dragRef.current.startVal + dy / 150 * (max - min)));
    onSet(`${nodeKey}.${param}`, newVal);
  };
  const onPointerUp = () => { dragRef.current = null; };

  return (
    <div className="knob">
      <svg width="80" height="80"
        onPointerDown={onPointerDown} onPointerMove={onPointerMove} onPointerUp={onPointerUp}
        style={{ cursor: 'ns-resize', userSelect: 'none' }}>
        <path d={trackPath} fill="none" stroke="var(--knob-track)" strokeWidth="4" strokeLinecap="round" />
        {valuePath && <path d={valuePath} fill="none" stroke="var(--accent)" strokeWidth="4" strokeLinecap="round" />}
        <circle cx={cx} cy={cy} r="12" fill="var(--bg-tile)" stroke="var(--border-tile)" strokeWidth="1.5" />
        <line x1={cx} y1={cy} x2={tipX.toFixed(2)} y2={tipY.toFixed(2)}
          stroke="var(--text-strong)" strokeWidth="2" strokeLinecap="round" />
      </svg>
      <div className="knob-value">{fmt(value)}{unit ?? ''}</div>
      <div className="knob-label">{label}</div>
    </div>
  );
}
