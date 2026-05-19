import { useEffect, useRef, useState } from 'react';
import type { DeviceMap, AudioConfig, ParamInfo } from '../types';
import { fetchConfig, fetchDevices, fetchCanonical, createWs } from '../api';

const DEFAULT_CONFIG: AudioConfig = {
    in_channels: 2, out_channels: 2, sample_rate: 48000,
    buffer_size: 256, audio_device: 'default'
};

export function useConnection(onMessage: (msg: string, param: string) => void) {
    const [connected, setConnected] = useState(false);
    const [audioConfig, setAudioConfig] = useState<AudioConfig>(DEFAULT_CONFIG);
    const [devices, setDevices] = useState<DeviceMap>({});
    // Canonical (firmware-declared) `ParamInfo` per effect type. Re-fetched on
    // each connect so a reload (which may change the firmware build) resyncs.
    // Single source of truth for the effect-type list — ChainView's
    // "+ new effect" picker derives from it.
    const [canonical, setCanonical] = useState<Record<string, ParamInfo[]>>({});

    // Ref so the WS callback always sees the latest handler
    const onMessageRef = useRef(onMessage);
    onMessageRef.current = onMessage;

    useEffect(() => {
        const cleanup = createWs(
            (msg, param) => onMessageRef.current(msg, param),
            () => {
                // Connection just opened — refresh config, devices, and canonical.
                // Also re-fires after a reconnect, so the UI re-syncs after a reload.
                setConnected(true);
                fetchDevices().then(devs => devs && setDevices(devs));
                fetchConfig().then(cfg => cfg && setAudioConfig(cfg));
                fetchCanonical().then(can => can && setCanonical(can));
            },
            () => setConnected(false),
        );
        return cleanup;
    }, []);

    return { connected, audioConfig, setAudioConfig, devices, canonical };
}
