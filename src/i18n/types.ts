export type SupportedLocale = "zh-TW" | "zh-CN" | "en" | "ja";

export type LanguagePreference = "system" | SupportedLocale;

export type TranslationKey = keyof typeof import("./locales/zh-TW").default;

export type TranslationSchema = Record<TranslationKey, string>;

export type TranslationParams = Record<string, string | number>;
