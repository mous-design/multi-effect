import { useEffect, useState } from 'react';

export function useTheme() {
    const [theme, setTheme] = useState(() =>
        localStorage.getItem('theme') === 'light' ? 'light' : 'dark'
    );

    useEffect(() => {
        document.body.classList.toggle('light', theme === 'light');
        localStorage.setItem('theme', theme);
    }, [theme]);

    const toggleTheme = () => setTheme(t => t === 'dark' ? 'light' : 'dark');

    return { theme, toggleTheme };
}
