import { useEffect, useRef, useState } from 'react';
import { Toast } from '../components/Toasts';
import { setApiErrorHandler } from '../api';

export function useToasts() {
    const [toasts, setToasts] = useState<Toast[]>([]);
    const toastId = useRef(0);

    const addToast = (msg: string) => {
        const id = ++toastId.current;
        setToasts(prev => [...prev, { id, msg, fading: false }]);
        setTimeout(() => setToasts(prev => prev.map(t => t.id === id ? { ...t, fading: true } : t)), 9500);
        setTimeout(() => setToasts(prev => prev.filter(t => t.id !== id)), 10000);
    };

    const dismissToast = (id: number) => {
        setToasts(prev => prev.filter(t => t.id !== id));
    };

    // Wire API errors to toasts
    const addToastRef = useRef(addToast);
    addToastRef.current = addToast;
    useEffect(() => { setApiErrorHandler(msg => addToastRef.current(msg)); }, []);

    return { toasts, addToast, dismissToast };
}
