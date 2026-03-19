import { useState } from 'react';
import { reloadConfig } from '../api';
import { t } from '../i18n';

const SAMPLE_RATES = [44100, 48000, 96000, 192000];
const BUFFER_SIZES = [64, 128, 256, 512, 1024];

interface AudioConfig {
  sample_rate: number;
  buffer_size: number;
  device: string;
  in_channels: number;
  out_channels: number;
}

interface Props {
  config: AudioConfig;
  onSave: (cfg: AudioConfig) => Promise<boolean>;
  onClose: () => void;
}

export function SettingsPopup({ config, onSave, onClose }: Props) {
  const [sample_rate,  setSampleRate]  = useState(config.sample_rate);
  const [buffer_size,  setBufferSize]  = useState(config.buffer_size);
  const [device,       setDevice]      = useState(config.device);
  const [in_channels,  setInChannels]  = useState(config.in_channels);
  const [out_channels, setOutChannels] = useState(config.out_channels);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState(false);

  async function handleSave() {
    const ok = await onSave({ sample_rate, buffer_size, device, in_channels, out_channels });
    if (ok) { setSaved(true); setError(false); }
    else    { setError(true); }
  }

  return (
    <div className="popup-overlay" onClick={onClose}>
      <div className="popup" onClick={e => e.stopPropagation()}>
        <p className="popup-title">{t('ui.settings')}</p>
        <table className="routing-table">
          <tbody>
            <tr>
              <td className="routing-label">{t('ui.device')}</td>
              <td colSpan={2}>
                <input
                  type="text"
                  value={device}
                  onChange={e => setDevice(e.target.value)}
                  className="settings-text-input"
                />
              </td>
            </tr>
            <tr>
              <td className="routing-label">{t('ui.sample_rate')}</td>
              <td colSpan={2}>
                <select value={sample_rate} onChange={e => setSampleRate(Number(e.target.value))} className="preset-select">
                  {SAMPLE_RATES.map(r => <option key={r} value={r}>{r} Hz</option>)}
                </select>
              </td>
            </tr>
            <tr>
              <td className="routing-label">{t('ui.buffer_size')}</td>
              <td colSpan={2}>
                <select value={buffer_size} onChange={e => setBufferSize(Number(e.target.value))} className="preset-select">
                  {BUFFER_SIZES.map(b => <option key={b} value={b}>{b}</option>)}
                </select>
              </td>
            </tr>
            <tr>
              <td className="routing-label">{t('ui.in_channels')}</td>
              <td colSpan={2}>
                <input type="number" min={1} max={32} value={in_channels}
                  onChange={e => setInChannels(Number(e.target.value))}
                  className="preset-input" />
              </td>
            </tr>
            <tr>
              <td className="routing-label">{t('ui.out_channels')}</td>
              <td colSpan={2}>
                <input type="number" min={1} max={32} value={out_channels}
                  onChange={e => setOutChannels(Number(e.target.value))}
                  className="preset-input" />
              </td>
            </tr>
          </tbody>
        </table>
        {saved && (
          <p className="settings-restart-warning">
            {t('ui.restart_required')}
            {' '}
            <button className="settings-reload-btn" onClick={() => { reloadConfig(); onClose(); }}>{t('ui.reload')}</button>
          </p>
        )}
        {error && <p className="settings-error">{t('error.save_config')}</p>}
        <div className="popup-actions">
          <button className="popup-confirm" onClick={handleSave}>{t('ui.save')}</button>
          <button className="popup-cancel" onClick={onClose}>{t('ui.cancel')}</button>
        </div>
      </div>
    </div>
  );
}
