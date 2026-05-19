import { useEffect, useRef, useState } from 'react';
import { Toast } from '../components/Toasts';
import { setApiErrorHandler } from '../api';
import { t } from '../i18n';

export function useToasts() {
    const [toasts, setToasts] = useState<Toast[]>([]);
    const toastId = useRef(0);

    // `addToast` accepts a translation key (or any string) and optional
    // placeholder args for `{}` substitution. Unknown keys fall through to
    // the string itself — that's `t()`'s default behaviour, so call sites
    // that already pass already-translated strings (e.g. server error text)
    // keep working.
    const addToast = (key: string, ...args: (string | number)[]) => {
        const id = ++toastId.current;
        const msg = t(key, ...args);
        setToasts(prev => [...prev, { id, msg, fading: false }]);
        setTimeout(() => setToasts(prev => prev.map(t => t.id === id ? { ...t, fading: true } : t)), 9500);
        setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 10000);
    };

    const dismissToast = (id: number) => {
        setToasts(prev => prev.filter(t => t.id !== id));
    };

    // Wire API errors to toasts. api.ts emits keys; we translate here.
    const addToastRef = useRef(addToast);
    addToastRef.current = addToast;
    useEffect(() => { setApiErrorHandler((key, ...args) => addToastRef.current(key, ...args)); }, []);

    return { toasts, addToast, dismissToast };
}
