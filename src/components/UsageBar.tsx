import type { Text } from '../lib/i18n';
import { formatLimitLabel, formatRemainingAmount, formatReset, formatUsedAmount, progressRemainingFraction } from '../lib/format';
import type { Locale, Quota, Urgency } from '../lib/types';

interface UsageBarProps {
  quota: Quota;
  accent: string;
  locale: Locale;
  text: Text;
}

const URGENCY_CLASS: Record<Urgency, 'ok' | 'warning' | 'exhausted' | 'unknown'> = {
  calm: 'ok',
  tense: 'warning',
  capped: 'exhausted',
  unknown: 'unknown',
};

export function UsageBar({ quota, accent, locale, text }: UsageBarProps) {
  const remainingFraction = progressRemainingFraction(quota.progress) ?? 0;
  const label = formatLimitLabel(quota, locale, text);
  const usedLabel = formatUsedAmount(quota.progress, locale, text);
  const remainingLabel = formatRemainingAmount(quota.progress, locale, text);
  const width = `${Math.round(Math.min(Math.max(remainingFraction, 0), 1) * 1000) / 10}%`;

  return (
    <div className={`usage usage--${URGENCY_CLASS[quota.urgency]}`}>
      <div className="usage__row">
        <span className="usage__label">{label}</span>
        <span className="usage__remaining">{remainingLabel}</span>
      </div>
      <div className="usage__track" aria-label={`${label} ${remainingLabel}`}>
        <span className="usage__fill" style={{ width, background: accent }} />
      </div>
      <div className="usage__meta">
        <span>{usedLabel}</span>
        <span>{formatReset(quota.bucket, text)}</span>
      </div>
    </div>
  );
}
