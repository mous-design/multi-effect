import { useState } from 'react';
import { t } from '../i18n';
import { RoutingSelect } from './RoutingSelect';
import { ConfirmDelete } from './ConfirmDelete';

interface Props {
    hasChains: boolean;
    inChannels: number;
    outChannels: number;
    onNewChain: (input: [number, number], output: [number, number]) => void;
    onDeletePreset: () => void;
}

export function BottomBar({ hasChains, inChannels, outChannels, onNewChain, onDeletePreset }: Props) {
    const [showForm, setShowForm] = useState(false);
    const [input, setInput] = useState<[number, number]>([1, Math.min(2, inChannels)]);
    const [output, setOutput] = useState<[number, number]>([1, Math.min(2, outChannels)]);
    const [confirmDelete, setConfirmDelete] = useState(false);

    const handleCreate = () => {
        onNewChain(input, output);
        setShowForm(false);
    };

    const handleOpenForm = () => {
        setInput([1, Math.min(2, inChannels)]);
        setOutput([1, Math.min(2, outChannels)]);
        setShowForm(true);
    };

    return (
        <div className="preset-bottom-bar">
            {!showForm ? (
                <button className="new-chain-btn" onClick={handleOpenForm}>{t('ui.new_chain')}</button>
            ) : (
                <div className="new-chain-form">
                    <RoutingSelect
                        input={input} output={output}
                        inChannels={inChannels} outChannels={outChannels}
                        onChange={(inp, out) => { setInput(inp); setOutput(out); }}
                    />
                    <div className="new-chain-actions">
                        <button onClick={handleCreate}>{t('ui.create')}</button>
                        <button onClick={() => setShowForm(false)}>{t('ui.cancel')}</button>
                    </div>
                </div>
            )}
            {confirmDelete ? (
                <ConfirmDelete
                    message={t('ui.confirm_delete_preset')}
                    onConfirm={onDeletePreset}
                    onCancel={() => setConfirmDelete(false)}
                />
            ) : (
                <button
                    className="new-chain-btn"
                    onClick={() => !hasChains ? onDeletePreset() : setConfirmDelete(true)}
                >{t('ui.delete_preset')}</button>
            )}
        </div>
    );
}
