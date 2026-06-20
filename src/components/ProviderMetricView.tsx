import type { Text } from '../lib/i18n';
import { formatBalanceAmount, formatBalanceBreakdown } from '../lib/format';
import type { BalanceMetric, Locale, ProviderMetric, Urgency } from '../lib/types';
import { UsageBar } from './UsageBar';

interface ProviderMetricViewProps {
  metric: ProviderMetric;
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

export function ProviderMetricView({ metric, accent, locale, text }: ProviderMetricViewProps) {
  if (metric.kind === 'quota') {
    return <UsageBar quota={metric} accent={accent} locale={locale} text={text} />;
  }

  return <BalanceMetricView metric={metric} locale={locale} text={text} />;
}

function BalanceMetricView({ metric, locale, text }: { metric: BalanceMetric; locale: Locale; text: Text }) {
  const breakdown = formatBalanceBreakdown(metric, locale, text);
  const status = metric.isAvailable ? text.balanceAvailable : text.balanceUnavailable;

  return (
    <div className={`usage balance usage--${URGENCY_CLASS[metric.urgency]}`}>
      <div className="usage__row">
        <span className="usage__label">{metric.displayName}</span>
        <span className="usage__remaining">{status}</span>
      </div>
      <div className="balance__amount">{formatBalanceAmount(metric, locale)}</div>
      {breakdown ? (
        <div className="usage__meta">
          <span>{breakdown}</span>
        </div>
      ) : null}
    </div>
  );
}
