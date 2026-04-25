import { useEffect, useRef, useState } from 'react';
import type { DeviceMap, AudioConfig } from '../types';
import { fetchConfig, fetchDevices, createWs } from '../api';

const DEFAULT_CONFIG: AudioConfig = {
    in_channels: 2, out_channels: 2, sample_rate: 48000,
    buffer_size: 256, audio_device: 'default', delay_max_seconds: 2.0,
    looper_max_seconds: 30.0
};

export function useConnection(onMessage: (msg: string, param: string) => void) {
    const [connected, setConnected] = useState(false);
    const [audioConfig, setAudioConfig] = useState<AudioConfig>(DEFAULT_CONFIG);
    const [devices, setDevices] = useState<DeviceMap>({});

    // Ref so the WS callback always sees the latest handler
    const onMessageRef = useRef(onMessage);
    onMessageRef.current = onMessage;

    useEffect(() => {
        const cleanup = createWs(
            (msg, param) => onMessageRef.current(msg, param),
            () => setConnected(true),
            () => setConnected(false),
        );
        fetchDevices().then(devs => devs && setDevices(devs));
        fetchConfig().then(cfg => cfg && setAudioConfig(cfg));
        return cleanup;
    }, []);

    return { connected, audioConfig, setAudioConfig, devices };
}
