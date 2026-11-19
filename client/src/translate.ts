export type Translate = (key: string, params?: Record<string, number | string>) => string;

export function translateFrom(dictionary: Readonly<Record<string, string>>): Translate {
  return (key, params) => (dictionary[key] ?? key).replace(/\{(\w+)\}/g, (token, name: string) => String(params?.[name] ?? token));
}
