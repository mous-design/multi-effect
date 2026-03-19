import en from './lang/en';

type LangMap = Record<string, string>;

let current: LangMap = en;

/** Switch to a different language map at runtime. */
export function setLang(map: LangMap): void {
  current = map;
}

/**
 * Translate a key, replacing `{}` placeholders with args in order.
 * Falls back to the key itself when not found in the active language.
 */
export function t(key: string, ...args: (string | number)[]): string {
  let msg = current[key] ?? key;
  for (const arg of args) {
    const idx = msg.indexOf('{}');
    if (idx === -1) break;
    msg = msg.slice(0, idx) + String(arg) + msg.slice(idx + 2);
  }
  return msg;
}
