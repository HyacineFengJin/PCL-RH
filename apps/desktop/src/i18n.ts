/** Launcher messages and regional formatting have separate owners: language
 * chooses our message catalog, region only controls Intl. User/service strings
 * are interpolated verbatim and are never looked up as translation keys.
 * Configure once in the app render; isolated panels use createTranslator so a
 * preview or a delayed callback cannot change another panel's locale.
 */
import zhCN from "./locales/zh-CN";
import enUS from "./locales/en-US";
import type { LauncherLocale, LauncherPreferences } from "./launcherTypes";
import { nodeRuntimeError } from "./nodeRuntimeError";

export type LocalizationPreferences = LauncherPreferences["localization"];
export type MessageKey = keyof typeof zhCN;
export type MessageParams = Readonly<Record<string, string | number>>;
export type Translator = ReturnType<typeof createTranslator>;

function browserLocale(): string {
  return typeof navigator === "undefined"
    ? "zh-CN"
    : navigator.languages?.[0] || navigator.language || "zh-CN";
}

export function resolveLanguage(
  language: LauncherLocale,
  systemLocale = browserLocale(),
): "zh-CN" | "en-US" {
  const locale = language === "system" ? systemLocale : language;
  return /^en(?:-|$)/i.test(locale) ? "en-US" : "zh-CN";
}

function resolveRegion(region: LauncherLocale, systemLocale: string): string {
  const locale = region === "system" ? systemLocale : region;
  try {
    return Intl.getCanonicalLocales(locale)[0] || "zh-CN";
  } catch {
    return "zh-CN";
  }
}

export function createTranslator(
  localization: LocalizationPreferences = {
    language: "zh-CN",
    region: "zh-CN",
  },
  systemLocale = browserLocale(),
) {
  const language = resolveLanguage(localization.language, systemLocale);
  const region = resolveRegion(localization.region, systemLocale);
  const catalog: Partial<Record<MessageKey, string>> =
    language === "en-US" ? enUS : zhCN;
  function t<K extends MessageKey>(key: K, params?: MessageParams): string {
    const message = catalog[key] ?? zhCN[key];
    return message.replace(/\{(\w+)\}/g, (token, name: string) =>
      params && Object.prototype.hasOwnProperty.call(params, name)
        ? String(params[name])
        : token,
    );
  }
  function formatDate(
    value: Date | number | string,
    options?: Intl.DateTimeFormatOptions,
  ): string {
    const date = value instanceof Date ? value : new Date(value);
    if (!Number.isFinite(date.getTime())) return String(value);
    return new Intl.DateTimeFormat(
      region,
      options || { dateStyle: "short", timeStyle: "medium" },
    ).format(date);
  }
  function formatNumber(
    value: number,
    options?: Intl.NumberFormatOptions,
  ): string {
    return new Intl.NumberFormat(region, options).format(value);
  }
  function serviceError(error: unknown): string {
    const dependency = nodeRuntimeError(error);
    if (dependency)
      return t(
        dependency === "missing"
          ? "experimental.nodeMissing"
          : "experimental.nodeUnsupported",
      );
    return t("common.serviceError", { error: String(error) });
  }
  function formatRelativeDate(
    value: string | number | Date,
    now = Date.now(),
  ): string {
    const date = value instanceof Date ? value : new Date(value);
    if (!Number.isFinite(date.getTime())) return "—";
    const seconds = Math.max(0, Math.floor((now - date.getTime()) / 1000));
    // Relative words belong to the interface language; absolute dates and
    // numeric values above belong to the independently selected region.
    for (const [duration, unit] of [
      [31536000, "year"],
      [2592000, "month"],
      [604800, "week"],
      [86400, "day"],
      [3600, "hour"],
      [60, "minute"],
    ] as const) {
      if (seconds >= duration)
        return new Intl.RelativeTimeFormat(language, {
          numeric: "auto",
        }).format(-Math.floor(seconds / duration), unit);
    }
    return t("ui.justNow");
  }
  return {
    t,
    formatDate,
    formatNumber,
    formatRelativeDate,
    serviceError,
    language,
    region,
  };
}

let active = createTranslator();
export function configureLocale(
  localization: LocalizationPreferences,
  systemLocale?: string,
): void {
  active = createTranslator(localization, systemLocale);
  // Keep screen-reader language and language-specific layout in sync with UI text.
  if (
    typeof document !== "undefined" &&
    document.documentElement.lang !== active.language
  )
    document.documentElement.lang = active.language;
}
export function t<K extends MessageKey>(
  key: K,
  params?: MessageParams,
): string {
  return active.t(key, params);
}
export function formatDate(
  value: Date | number | string,
  options?: Intl.DateTimeFormatOptions,
): string {
  return active.formatDate(value, options);
}
export function formatNumber(
  value: number,
  options?: Intl.NumberFormatOptions,
): string {
  return active.formatNumber(value, options);
}
export function serviceError(error: unknown): string {
  return active.serviceError(error);
}
export function formatRelativeDate(
  value: string | number | Date,
  now?: number,
): string {
  return active.formatRelativeDate(value, now);
}
export function currentLocale(): {
  language: "zh-CN" | "en-US";
  region: string;
} {
  return { language: active.language, region: active.region };
}
