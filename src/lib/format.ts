import type { Text } from './i18n';
import type { Bucket, CountUnit, Locale, Progress, Quota } from './types';

export function formatUsedAmount(progress: Progress, locale: Locale, text: Text): string {
  if (progress.kind === 'ratio') {
    return `${Math.round(normalizePercent(progress.usedPercent))}% ${text.usedSuffix}`;
  }

  const used = formatAbsoluteAmount(progress.used, progress.unit, locale);
  if (used) return `${used} ${text.usedSuffix}`;

  const usedPercent = usedPercentFromProgress(progress);
  return typeof usedPercent === 'number'
    ? `${Math.round(usedPercent)}% ${text.usedSuffix}`
    : text.usageUnavailable;
}

export function formatRemainingAmount(progress: Progress, locale: Locale, text: Text): string {
  if (progress.kind === 'ratio') {
    const remaining = 100 - normalizePercent(progress.usedPercent);
    return `${Math.round(Math.max(remaining, 0))}% ${text.leftSuffix}`;
  }

  const remaining = formatAbsoluteAmount(progress.remaining, progress.unit, locale);
  if (remaining) return `${remaining} ${text.leftSuffix}`;

  const remainingPercent = remainingPercentFromProgress(progress);
  if (typeof remainingPercent === 'number') {
    return `${Math.round(remainingPercent)}% ${text.leftSuffix}`;
  }
  return text.quotaUnknown;
}

export function formatLimitLabel(quota: Quota, locale: Locale, text: Text): string {
  const windowName = formatWindowName(quota.bucket, locale, text);
  if (windowName) return windowName;

  const displayName = quota.displayName.trim();
  return displayName || quota.key;
}

export function formatReset(bucket: Bucket | null, text: Text): string {
  if (!bucket?.resetsAt) return text.resetUnknown;
  const deltaMs = bucket.resetsAt - Date.now();
  if (deltaMs <= 0) return text.resetDueNow;

  const totalMinutes = Math.ceil(deltaMs / 60_000);
  if (totalMinutes < 60) return `${text.resetsIn} ${totalMinutes}${text.minutesUnit}`;

  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours < 24) {
    return minutes
      ? `${text.resetsIn} ${hours}${text.hoursUnit} ${minutes}${text.minutesUnit}`
      : `${text.resetsIn} ${hours}${text.hoursUnit}`;
  }

  const days = Math.floor(hours / 24);
  const remainingHours = hours % 24;
  return remainingHours
    ? `${text.resetsIn} ${days}${text.daysUnit} ${remainingHours}${text.hoursUnit}`
    : `${text.resetsIn} ${days}${text.daysUnit}`;
}

export function formatFetchedAt(value: number | null | undefined, locale: Locale, text: Text): string {
  if (typeof value !== 'number' || !Number.isFinite(value)) return text.notFetchedYet;
  return new Intl.DateTimeFormat(locale, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).format(new Date(value));
}

export function progressRemainingFraction(progress: Progress): number | null {
  const usedPercent = usedPercentFromProgress(progress);
  if (typeof usedPercent !== 'number') return null;
  return Math.max(1 - normalizePercent(usedPercent) / 100, 0);
}

function usedPercentFromProgress(progress: Progress): number | null {
  if (progress.kind === 'ratio') return normalizePercent(progress.usedPercent);
  if (typeof progress.usedPercent === 'number' && Number.isFinite(progress.usedPercent)) {
    return normalizePercent(progress.usedPercent);
  }
  return usedPercentFromCounts(progress.used, progress.total, progress.remaining);
}

function usedPercentFromCounts(
  used: number | null,
  total: number | null,
  remaining: number | null,
): number | null {
  if (
    typeof used === 'number'
    && Number.isFinite(used)
    && typeof total === 'number'
    && Number.isFinite(total)
    && total > 0
  ) {
    return clampPercent((used / total) * 100);
  }

  if (
    typeof total === 'number'
    && Number.isFinite(total)
    && total > 0
    && typeof remaining === 'number'
    && Number.isFinite(remaining)
  ) {
    return clampPercent(((total - remaining) / total) * 100);
  }

  if (
    typeof used === 'number'
    && Number.isFinite(used)
    && typeof remaining === 'number'
    && Number.isFinite(remaining)
  ) {
    const inferredTotal = used + remaining;
    if (inferredTotal > 0) return clampPercent((used / inferredTotal) * 100);
  }

  return null;
}

function remainingPercentFromProgress(progress: Progress): number | null {
  const fraction = progressRemainingFraction(progress);
  return typeof fraction === 'number' ? fraction * 100 : null;
}

function formatWindowName(bucket: Bucket | null, locale: Locale, text: Text): string | null {
  if (!bucket) return null;
  if (bucket.kind === 'rolling') {
    const durationMs = bucket.durationMs;
    if (typeof durationMs === 'number' && Number.isFinite(durationMs) && durationMs > 0) {
      const totalHours = Math.round(durationMs / 3_600_000);
      if (totalHours >= 24 && totalHours % 24 === 0) return formatDuration(totalHours / 24, 'day', locale, text);
      if (totalHours >= 1) return formatDuration(totalHours, 'hour', locale, text);
    }
  }

  const label = bucket.label.trim();
  return label || null;
}

function formatDuration(value: number, unit: 'day' | 'hour', locale: Locale, text: Text): string {
  if (locale === 'zh-CN') {
    return unit === 'day' ? `${value}${text.daysUnit}` : `${value}${text.hoursUnit}`;
  }
  return unit === 'day' ? `${value} ${text.daysUnit}` : `${value} ${text.hoursUnit}`;
}

function formatAbsoluteAmount(value: number | null, unit: CountUnit, locale: Locale): string | null {
  if (typeof value !== 'number' || !Number.isFinite(value)) return null;
  if (unit === 'dollars') {
    return new Intl.NumberFormat(locale, {
      style: 'currency',
      currency: 'USD',
      maximumFractionDigits: 2,
    }).format(value);
  }

  const formatted = new Intl.NumberFormat(locale, {
    notation: value >= 10_000 ? 'compact' : 'standard',
    maximumFractionDigits: value >= 100 ? 0 : 1,
  }).format(value);
  if (unit === 'tokens') return `${formatted} tok`;
  if (unit === 'requests') return locale === 'zh-CN' ? `${formatted}次` : `${formatted} req`;
  return formatted;
}

function normalizePercent(value: number): number {
  if (Number.isFinite(value) && value > 0 && value < 1) return clampPercent(value * 100);
  return clampPercent(value);
}

function clampPercent(value: number): number {
  return Number.isFinite(value) ? Math.min(Math.max(value, 0), 100) : 0;
}