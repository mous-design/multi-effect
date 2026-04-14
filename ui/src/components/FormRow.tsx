import { ReactNode } from 'react';

interface Props {
    label: string;
    children: ReactNode;
}

export function FormRow({ label, children }: Props) {
    return (
        <tr>
            <td className="routing-label">{label}</td>
            <td colSpan={2}>{children}</td>
        </tr>
    );
}
