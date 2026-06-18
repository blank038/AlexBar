import type { Text } from '../lib/i18n';
import { formatFetchedAt } from '../lib/format';
import type { Locale } from '../lib/types';

interface HeaderProps {
  loading: boolean;
  lastFetchedAt: number | null;
  locale: Locale;
  text: Text;
  onRefresh: () => void;
  onOpenSettings: () => void;
}

export function Header({ loading, lastFetchedAt, locale, text, onRefresh, onOpenSettings }: HeaderProps) {
  return (
    <header className="header">
      <div>
        <p className="eyebrow">AlexBar</p>
        <h1>{text.headerTitle}</h1>
        <p className="header__meta">{text.lastSample} · {formatFetchedAt(lastFetchedAt, locale, text)}</p>
      </div>
      <div className="header__actions">
        <button className="pill-button" type="button" onClick={onRefresh} disabled={loading}>
          {loading ? text.refreshing : text.refresh}
        </button>
        <button className="square-button" type="button" onClick={onOpenSettings}>
          ⚙
        </button>
      </div>
    </header>
  );
}
