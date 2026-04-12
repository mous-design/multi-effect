import { useState } from 'react';
import { t } from '../i18n';

interface Props {
    hasChains: boolean;
    onNewChain: (input: [number, number], output: [number, number]) => void;
    onDeletePreset: () => void;
}

function parseChannels(s: string): [number, number] {
    const parts = s.split(',').map(p => parseInt(p.trim(), 10)).filter(n => !isNaN(n));
    if (parts.length === 1) return [parts[0], parts[0]];
    if (parts.length >= 2) return [parts[0], parts[1]];
    return [1, 1];
}

export function BottomBar({ hasChains, onNewChain, onDeletePreset }: Props) {
    const [showForm, setShowForm] = useState(false);
    const [input, setInput] = useState('1,1');
    const [output, setOutput] = useState('1,2');
    const [confirmDelete, setConfirmDelete] = useState(false);

    const handleCreate = () => {
        onNewChain(parseChannels(input), parseChannels(output));
        setShowForm(false);
    };

    const handleOpenForm = () => {
        setInput('1,1');
        setOutput('1,2');
        setShowForm(true);
    };

    return (
        <div className="preset-bottom-bar">
            {!showForm ? (
                <button className="new-chain-btn" onClick={handleOpenForm}>{t('ui.new_chain')}</button>
            ) : (
                <div className="new-chain-form">
                    <label>{t('ui.input_ch')}</label>
                    <input type="text" value={input} onChange={e => setInput(e.target.value)} placeholder="1,1" />
                    <label>{t('ui.output_ch')}</label>
                    <input type="text" value={output} onChange={e => setOutput(e.target.value)} placeholder="1,2" />
                    <button onClick={handleCreate}>{t('ui.create')}</button>
                    <button onClick={() => setShowForm(false)}>{t('ui.cancel')}</button>
                </div>
            )}
            {confirmDelete ? (
                <div className="chain-confirm-group">
                    <span className="chain-confirm-text">{t('ui.confirm_delete_preset')}</span>
                    <button className="chain-confirm-yes" onClick={onDeletePreset}>✓</button>
                    <button className="chain-confirm-no" onClick={() => setConfirmDelete(false)}>✗</button>
                </div>
            ) : (
                <button
                    className="new-chain-btn"
                    onClick={() => !hasChains ? onDeletePreset() : setConfirmDelete(true)}
                >{t('ui.delete_preset')}</button>
            )}
        </div>
    );
}
