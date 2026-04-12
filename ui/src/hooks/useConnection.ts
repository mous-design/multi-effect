import { useEffect, useRef, useState } from 'react';
import { DeviceMap } from '../types';
import { fetchConfig, fetchDevices, createWs } from '../api';

export interface AudioConfig {
    in_channels: number;
    out_channels: number;
    sample_rate: number;
    buffer_size: number;
    audio_device: string;
    delay_max_seconds: number;
}

const DEFAULT_CONFIG: AudioConfig = {
    in_channels: 2, out_channels: 2, sample_rate: 48000,
    buffer_size: 256, audio_device: 'default', delay_max_seconds: 2.0,
};

export function useConnection(onMessage: (msg: any) => void) {
    const [connected, setConnected] = useState(false);
    const [audioConfig, setAudioConfig] = useState<AudioConfig>(DEFAULT_CONFIG);
    const [devices, setDevices] = useState<DeviceMap>({});

    // Ref so the WS callback always sees the latest handler
    const onMessageRef = useRef(onMessage);
    onMessageRef.current = onMessage;

    useEffect(() => {
        fetchDevices().then(setDevices);
        fetchConfig().then(cfg => setAudioConfig({
            in_channels: cfg.in_channels, out_channels: cfg.out_channels,
            sample_rate: cfg.sample_rate, buffer_size: cfg.buffer_size,
            audio_device: cfg.audio_device, delay_max_seconds: cfg.delay_max_seconds,
        }));

        const cleanup = createWs(
            (msg) => onMessageRef.current(msg),
            () => setConnected(true),
            () => setConnected(false),
        );
        return cleanup;
    }, []);

    return { connected, audioConfig, setAudioConfig, devices };
}
