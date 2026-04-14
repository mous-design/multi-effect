import { t } from '../i18n';

interface Props {
    input: [number, number];
    output: [number, number];
    inChannels: number;
    outChannels: number;
    onChange: (input: [number, number], output: [number, number]) => void;
}

function chOptions(n: number) {
    return [0, ...Array.from({ length: n }, (_, i) => i + 1)];
}

function chLabel(n: number) {
    return n === 0 ? '–' : String(n);
}

function ChannelPair({ label, pair, channels, onChange }: {
    label: string;
    pair: [number, number];
    channels: number;
    onChange: (pair: [number, number]) => void;
}) {
    return (
        <tr>
            <td className="routing-label">{label}</td>
            <td>
                <label className="routing-ch-label">L</label>
                <select value={pair[0]} onChange={e => onChange([Number(e.target.value), pair[1]])} className="preset-select">
                    {chOptions(channels).map(n => <option key={n} value={n}>{chLabel(n)}</option>)}
                </select>
            </td>
            <td>
                <label className="routing-ch-label">R</label>
                <select value={pair[1]} onChange={e => onChange([pair[0], Number(e.target.value)])} className="preset-select">
                    {chOptions(channels).map(n => <option key={n} value={n}>{chLabel(n)}</option>)}
                </select>
            </td>
        </tr>
    );
}

export function RoutingSelect({ input, output, inChannels, outChannels, onChange }: Props) {
    return (
        <table className="routing-table">
            <tbody>
                <ChannelPair label={t('ui.routing_input')} pair={input} channels={inChannels}
                    onChange={inp => onChange(inp, output)} />
                <ChannelPair label={t('ui.routing_output')} pair={output} channels={outChannels}
                    onChange={out => onChange(input, out)} />
            </tbody>
        </table>
    );
}
