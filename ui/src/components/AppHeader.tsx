import { Toasts, Toast } from './Toasts';
import { t } from '../i18n';

function SpeakerIcon() {
    return (
        <svg width="13" height="13" viewBox="0 0 13 13" fill="currentColor">
            <path d="M1 4.5h2.5L7 2v9L3.5 8.5H1z" />
            <path d="M9 3.5a4 4 0 010 6" stroke="currentColor" strokeWidth="1.5" fill="none" strokeLinecap="round" />
        </svg>
    );
}

function DevicesIcon() {
    return (
        <svg width="22" height="22" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <rect x="1" y="4" width="14" height="8" rx="1.5" />
            <line x1="4" y1="4" x2="4" y2="12" />
            <circle cx="8" cy="8" r="1.5" fill="currentColor" stroke="none" />
        </svg>
    );
}

interface Props {
    connected: boolean;
    toasts: Toast[];
    onDismissToast: (id: number) => void;
    presets: number[];
    activePreset: number;
    isDirty: boolean;
    isComparing: boolean;
    savedFeedback: boolean;
    theme: string;
    onSwitchPreset: (n: number) => void;
    onCompare: () => void;
    onQuickSave: () => void;
    onOpenSave: () => void;
    onOpenSettings: () => void;
    onNavigateDevices: () => void;
    onNavigateHome: () => void;
    onToggleTheme: () => void;
}

export function AppHeader(p: Props) {
    return (
        <header className="app-header">
            <h1 className="app-title-link" onClick={p.onNavigateHome}>multi-effect</h1>
            <div className={`status ${p.connected ? 'connected' : 'disconnected'}`}>
                <span className="status-dot" />
                {p.connected ? t('ui.live') : t('ui.reconnecting')}
            </div>
            <Toasts toasts={p.toasts} onDismiss={p.onDismissToast} />
            <div className="header-preset">
                <label className="preset-label">{t('ui.preset')}</label>
                <select
                    value={p.activePreset}
                    onChange={e => p.onSwitchPreset(Number(e.target.value))}
                    className="preset-select"
                >
                    {p.presets.map(n => (
                        <option key={n} value={n}>{n === p.activePreset && p.isDirty ? `${n}*` : n}</option>
                    ))}
                </select>
                <button
                    className={`compare-btn${p.isComparing ? ' compare-btn-active' : ''}`}
                    onClick={p.onCompare}
                    disabled={!p.isDirty && !p.isComparing}
                    title={p.isComparing ? `Comparing with preset ${p.activePreset} — click to restore edits` : `Compare with saved preset ${p.activePreset}`}
                >
                    <SpeakerIcon /> {p.activePreset}
                </button>
                <button className="preset-save-btn" onClick={p.onQuickSave} disabled={p.activePreset === 0 || !p.isDirty} title={t('ui.save_quick')}>
                    {p.savedFeedback ? t('ui.saved') : t('ui.save_quick')}
                </button>
                <button className="preset-save-btn" onClick={p.onOpenSave} title={t('ui.save')}>
                    {t('ui.save')}
                </button>
                <button className="devices-btn" onClick={p.onNavigateDevices} title={t('ui.devices')}>
                    <DevicesIcon />
                </button>
                <button className="settings-btn" onClick={p.onOpenSettings} title={t('ui.settings')}>⚙</button>
                <button className="theme-btn" onClick={p.onToggleTheme}>
                    {p.theme === 'dark' ? '🌙' : '☀️'}
                </button>
            </div>
        </header>
    );
}
