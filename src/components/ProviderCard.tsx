import type { CSSProperties } from 'react';
import { formatFetchedAt } from '../lib/format';
import type { Text } from '../lib/i18n';
import type { Locale, ProviderDefinition, ProviderSnapshot } from '../lib/types';
import { UsageBar } from './UsageBar';

interface ProviderCardProps {
  provider: ProviderDefinition;
  snapshot: ProviderSnapshot | undefined;
  enabled: boolean;
  refreshing: boolean;
  locale: Locale;
  text: Text;
  onRefresh: () => void;
}

export function ProviderCard({ provider, snapshot, enabled, refreshing, locale, text, onRefresh }: ProviderCardProps) {
  const status = snapshot?.note ? 'error' : snapshot?.quotas.length ? 'live' : enabled ? 'waiting' : 'disabled';

  return (
    <article className={`provider provider--${status}`} style={{ '--accent': provider.accent } as CSSProperties}>
      <div className="provider__header">
        <div>
          <div className="provider__kicker">{provider.shortName}</div>
          <h2>{provider.name}</h2>
        </div>
        <button className="icon-button" type="button" onClick={onRefresh} disabled={!enabled || refreshing}>
          {refreshing ? '…' : '↻'}
        </button>
      </div>

      <div className="provider__status">
        <span className="provider__dot" />
        <span>{statusLabel(status, text)}</span>
        <span className="provider__time">{formatFetchedAt(snapshot?.refreshedAt, locale, text)}</span>
      </div>

      {snapshot?.note ? <p className="provider__error">{snapshot.note}</p> : null}

      {!snapshot?.note && snapshot?.quotas.length ? (
        <div className="provider__limits">
          {snapshot.quotas.map((quota) => (
            <UsageBar key={quota.key} quota={quota} accent={provider.accent} locale={locale} text={text} />
          ))}
        </div>
      ) : null}

      {!snapshot && enabled ? (
        <p className="provider__empty">
          {text.credentialPrefix} {provider.credentialPath}{locale === 'zh-CN' ? '。' : '.'}
        </p>
      ) : null}

      {!enabled ? <p className="provider__empty">{text.providerDisabled}</p> : null}
    </article>
  );
}

function statusLabel(status: 'error' | 'live' | 'waiting' | 'disabled', text: Text): string {
  switch (status) {
    case 'live':
      return text.liveQuota;
    case 'error':
      return text.needsAttention;
    case 'disabled':
      return text.disabled;
    case 'waiting':
      return text.waiting;
  }
}
