import { useState } from 'react';
import { sendReload } from '../api';
import { t } from '../i18n';
import { Popup } from './Popup';
import { FormRow } from './FormRow';
import type { AudioConfig } from '../types';


const SAMPLE_RATES = [44100, 48000, 96000, 192000];
const BUFFER_SIZES = [64, 128, 256, 512];

interface Props {
    config: AudioConfig;
    onSave: (cfg: AudioConfig) => Promise<boolean>;
    onClose: () => void;
}

export function SettingsPopup({ config, onSave, onClose }: Props) {
    const [sample_rate, setSampleRate] = useState(config.sample_rate);
    const [buffer_size, setBufferSize] = useState(config.buffer_size);
    const [audio_device, setDevice] = useState(config.audio_device);
    const [in_channels, setInChannels] = useState(config.in_channels);
    const [out_channels, setOutChannels] = useState(config.out_channels);
    const [delay_max_seconds, setDelayMaxSeconds] = useState(config.delay_max_seconds);
    const [saved, setSaved] = useState(false);
    const [error, setError] = useState(false);

    async function handleSave() {
        const looper_max_seconds = 30; // @todo must be in the form?
        const ok = await onSave({ sample_rate, buffer_size, audio_device: audio_device,
            in_channels, out_channels, delay_max_seconds, looper_max_seconds });
        if (ok) { setSaved(true); setError(false); }
        else { setError(true); }
    }

    return (
        <Popup title={t('ui.settings')} onClose={onClose} confirmLabel={t('ui.save')} onConfirm={handleSave}>
            <table className="routing-table">
                <tbody>
                    <FormRow label={t('ui.audio_device')}>
                        <input type="text" value={audio_device} onChange={e => setDevice(e.target.value)} className="settings-text-input" />
                    </FormRow>
                    <FormRow label={t('ui.sample_rate')}>
                        <select value={sample_rate} onChange={e => setSampleRate(Number(e.target.value))} className="preset-select">
                            {SAMPLE_RATES.map(r => <option key={r} value={r}>{r} Hz</option>)}
                        </select>
                    </FormRow>
                    <FormRow label={t('ui.buffer_size')}>
                        <select value={buffer_size} onChange={e => setBufferSize(Number(e.target.value))} className="preset-select">
                            {BUFFER_SIZES.map(b => <option key={b} value={b}>{t('ui.buffer_size_' + b)}</option>)}
                        </select>
                    </FormRow>
                    <FormRow label={t('ui.in_channels')}>
                        <input type="number" min={1} max={32} value={in_channels}
                            onChange={e => setInChannels(Number(e.target.value))} className="preset-input" />
                    </FormRow>
                    <FormRow label={t('ui.out_channels')}>
                        <input type="number" min={1} max={32} value={out_channels}
                            onChange={e => setOutChannels(Number(e.target.value))} className="preset-input" />
                    </FormRow>
                    <FormRow label={t('ui.delay_max')}>
                        <input type="number" min={0.1} max={30} step={0.1} value={delay_max_seconds}
                            onChange={e => setDelayMaxSeconds(Number(e.target.value))} className="preset-input" />
                    </FormRow>
                </tbody>
            </table>
            {saved && (
                <p className="settings-restart-warning">
                    {t('ui.restart_required')}
                    {' '}
                    <button className="settings-reload-btn" onClick={() => { sendReload(); onClose(); }}>{t('ui.reload')}</button>
                </p>
            )}
            {error && <p className="settings-error">{t('error.save_config')}</p>}
        </Popup>
    );
}
