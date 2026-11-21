import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import zhTW from "./locales/zh-TW";
import zhCN from "./locales/zh-CN";
import en from "./locales/en";
import ja from "./locales/ja";
import type {
  LanguagePreference,
  SupportedLocale,
  TranslationKey,
  TranslationParams,
  TranslationSchema,
} from "./types";

const LOCALE_DICTS: Record<SupportedLocale, TranslationSchema> = {
  "zh-TW": zhTW,
  "zh-CN": zhCN,
  en,
  ja,
};

export const LANGUAGE_OPTIONS: { value: LanguagePreference; nativeLabel?: string }[] = [
  { value: "system" },
  { value: "zh-TW", nativeLabel: "繁體中文" },
  { value: "zh-CN", nativeLabel: "简体中文" },
  { value: "en", nativeLabel: "English" },
  { value: "ja", nativeLabel: "日本語" },
];

export function isValidLanguagePreference(value: unknown): value is LanguagePreference {
  return (
    value === "system" ||
    value === "zh-TW" ||
    value === "zh-CN" ||
    value === "en" ||
    value === "ja"
  );
}

export function detectSystemLocale(): SupportedLocale {
  if (typeof navigator === "undefined") return "zh-TW";
  const raw = navigator.language || navigator.languages?.[0] || "zh-TW";
  const normalized = raw.trim().replace(/_/g, "-").toLowerCase();
  if (
    normalized.startsWith("zh-cn") ||
    normalized.startsWith("zh-sg") ||
    normalized.startsWith("zh-hans")
  ) {
    return "zh-CN";
  }
  if (normalized.startsWith("zh")) {
    return "zh-TW";
  }
  if (normalized.startsWith("ja")) {
    return "ja";
  }
  return "en";
}

export function resolveLocale(preference: LanguagePreference): SupportedLocale {
  return preference === "system" ? detectSystemLocale() : preference;
}

function formatMessage(template: string, params?: TranslationParams): string {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (match, key) => {
    if (Object.prototype.hasOwnProperty.call(params, key)) {
      return String(params[key]);
    }
    return match;
  });
}

type I18nContextValue = {
  language: LanguagePreference;
  resolvedLocale: SupportedLocale;
  setLanguage: (next: LanguagePreference) => void;
  t: (key: TranslationKey, params?: TranslationParams) => string;
};

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({
  initialLanguage = "system",
  children,
}: {
  initialLanguage?: LanguagePreference;
  children: ReactNode;
}) {
  const [language, setLanguage] = useState<LanguagePreference>(initialLanguage);
  const resolvedLocale = resolveLocale(language);

  useEffect(() => {
    if (typeof document !== "undefined") {
      document.documentElement.lang = resolvedLocale;
    }
  }, [resolvedLocale]);

  const t = useCallback(
    (key: TranslationKey, params?: TranslationParams) => {
      const dict = LOCALE_DICTS[resolvedLocale] ?? LOCALE_DICTS["zh-TW"];
      const template = dict[key] ?? LOCALE_DICTS["zh-TW"][key] ?? key;
      return formatMessage(template, params);
    },
    [resolvedLocale],
  );

  const value = useMemo(
    () => ({ language, resolvedLocale, setLanguage, t }),
    [language, resolvedLocale, t],
  );

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (!ctx) {
    throw new Error("useI18n must be used within an I18nProvider");
  }
  return ctx;
}
