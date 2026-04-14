import { ReactNode } from 'react';

interface Props {
    label: string;
    wide?: boolean;
    children: ReactNode;
}

export function DeviceField({ label, wide, children }: Props) {
    return (
        <div className="device-field">
            <label className={wide ? 'wide' : undefined}>{label}</label>
            {children}
        </div>
    );
}
