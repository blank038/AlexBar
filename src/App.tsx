import { listen } from '@tauri-apps/api/event';
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { Header } from './components/Header';
import { ProviderCard } from './components/ProviderCard';
import * as api from './lib/api';
import { getText } from './lib/i18n';
import type { AppSettings, ProviderId, ProviderSnapshot } from './lib/types';
import { PROVIDERS } from './lib/types';

const DEFAULT_SETTINGS: AppSettings = {
  enabledProviders: PROVIDERS.map((provider) => provider.id),
  refreshIntervalSecs: 60,
  visibleProviderLimit: 2,
  locale: 'zh-CN',
};

const POPOVER_RESIZE_DEBOUNCE_MS = 60;
const SHELL_SELECTOR = '.shell';
const PANEL_SELECTOR = '.panel';
const PANEL_INNER_SELECTOR = '.panel__inner';

export default function App() {
  const [snapshots, setSnapshots] = useState<ProviderSnapshot[]>([]);
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [loading, setLoading] = useState(true);
  const [refreshingProvider, setRefreshingProvider] = useState<ProviderId | null>(null);
  const [appError, setAppError] = useState<string | null>(null);
  const text = getText(settings.locale);
  const providerListRef = useRef<HTMLElement>(null);
  const enabledProviders = useMemo(
    () => PROVIDERS.filter((provider) => settings.enabledProviders.includes(provider.id)),
    [settings.enabledProviders],
  );

  useEffect(() => {
    let disposed = false;

    async function boot() {
      setLoading(true);
      try {
        const [nextSettings, cachedSnapshots] = await Promise.all([api.getSettings(), api.getReports()]);
        if (disposed) return;
        setSettings(nextSettings);
        setSnapshots(orderSnapshots(cachedSnapshots));
      } catch (error) {
        if (!disposed) setAppError(String(error));
      } finally {
        if (!disposed) setLoading(false);
      }
    }

    const usageListener = listen<ProviderSnapshot[]>('usage://updated', (event) => {
      setSnapshots(orderSnapshots(event.payload));
    });
    const settingsListener = listen<AppSettings>('settings://updated', (event) => {
      setSettings(event.payload);
    });
    void boot();

    return () => {
      disposed = true;
      void usageListener.then((unlisten) => unlisten());
      void settingsListener.then((unlisten) => unlisten());
    };
  }, []);

  useEffect(() => {
    const shell = document.querySelector<HTMLElement>(SHELL_SELECTOR)!;
    const panel = shell.querySelector<HTMLElement>(PANEL_SELECTOR)!;
    const panelInner = panel.querySelector<HTMLElement>(PANEL_INNER_SELECTOR)!;
    let resizeTimer: number | undefined;
    let lastHeight = 0;

    const syncHeight = () => {
      window.clearTimeout(resizeTimer);
      resizeTimer = window.setTimeout(() => {
        const height = measurePopoverHeight(panel, panelInner);
        if (height === lastHeight) return;

        lastHeight = height;
        void api.setPopoverHeight(height);
      }, POPOVER_RESIZE_DEBOUNCE_MS);
    };

    const observer = new ResizeObserver(syncHeight);
    observer.observe(shell);
    observer.observe(panelInner);
    syncHeight();

    return () => {
      observer.disconnect();
      window.clearTimeout(resizeTimer);
    };
  }, []);

  useLayoutEffect(() => {
    const providerList = providerListRef.current;
    if (!providerList) return;

    const syncProviderListHeight = () => {
      applyProviderListLimit(providerList, settings.visibleProviderLimit);
    };

    const observer = new ResizeObserver(syncProviderListHeight);
    observer.observe(providerList);
    for (const provider of providerList.querySelectorAll<HTMLElement>('.provider')) {
      observer.observe(provider);
    }
    syncProviderListHeight();

    return () => {
      observer.disconnect();
      providerList.style.maxHeight = '';
      providerList.style.overflowY = '';
    };
  }, [enabledProviders, snapshots, settings.visibleProviderLimit]);

  const lastFetchedAt = useMemo(() => {
    const refreshed = snapshots.map((snapshot) => snapshot.refreshedAt).filter(Number.isFinite);
    return refreshed.length ? Math.max(...refreshed) : null;
  }, [snapshots]);

  async function refreshAllProviders() {
    setLoading(true);
    setAppError(null);
    try {
      setSnapshots(orderSnapshots(await api.refreshAll()));
    } catch (error) {
      setAppError(String(error));
    } finally {
      setLoading(false);
    }
  }

  async function refreshOne(provider: ProviderId) {
    setRefreshingProvider(provider);
    setAppError(null);
    try {
      const snapshot = await api.refreshProvider(provider);
      setSnapshots((current) => orderSnapshots(upsertSnapshot(current, snapshot)));
    } catch (error) {
      setAppError(String(error));
    } finally {
      setRefreshingProvider(null);
    }
  }

  async function openSettingsWindow() {
    setAppError(null);
    try {
      await api.openSettingsWindow();
    } catch (error) {
      setAppError(String(error));
    }
  }

  return (
    <main className="shell">
      <div className="panel">
        <div className="panel__inner">
          <Header
            loading={loading}
            lastFetchedAt={lastFetchedAt}
            locale={settings.locale}
            text={text}
            onRefresh={refreshAllProviders}
            onOpenSettings={openSettingsWindow}
          />

          {appError ? <div className="app-error">{appError}</div> : null}

          <section ref={providerListRef} className="provider-list" aria-label={text.providerUsage}>
            {enabledProviders.map((provider) => (
              <ProviderCard
                key={provider.id}
                provider={provider}
                snapshot={snapshots.find((snapshot) => snapshot.provider === provider.id)}
                enabled={true}
                refreshing={refreshingProvider === provider.id}
                onRefresh={() => refreshOne(provider.id)}
                locale={settings.locale}
                text={text}
              />
            ))}
          </section>
        </div>
      </div>
    </main>
  );
}

function orderSnapshots(snapshots: ProviderSnapshot[]): ProviderSnapshot[] {
  const rank = new Map(PROVIDERS.map((provider, index) => [provider.id, index]));
  return [...snapshots].sort((left, right) => {
    const leftRank = rank.get(left.provider) ?? Number.MAX_SAFE_INTEGER;
    const rightRank = rank.get(right.provider) ?? Number.MAX_SAFE_INTEGER;
    return leftRank - rightRank;
  });
}

function upsertSnapshot(snapshots: ProviderSnapshot[], snapshot: ProviderSnapshot): ProviderSnapshot[] {
  const next = snapshots.filter((current) => current.provider !== snapshot.provider);
  next.push(snapshot);
  return next;
}

function measurePopoverHeight(panel: HTMLElement, panelInner: HTMLElement): number {
  const panelStyle = window.getComputedStyle(panel);
  return Math.ceil(
    panelInner.offsetHeight
      + Number.parseFloat(panelStyle.paddingTop)
      + Number.parseFloat(panelStyle.paddingBottom)
      + Number.parseFloat(panelStyle.borderTopWidth)
      + Number.parseFloat(panelStyle.borderBottomWidth),
  );
}

function applyProviderListLimit(providerList: HTMLElement, visibleProviderLimit: number) {
  const providers = providerList.querySelectorAll<HTMLElement>('.provider');
  const visibleCount = providers.length;
  if (visibleProviderLimit >= visibleCount) {
    providerList.style.maxHeight = '';
    providerList.style.overflowY = '';
    return;
  }

  const rowGap = Number.parseFloat(window.getComputedStyle(providerList).rowGap);
  let maxHeight = 0;
  for (let index = 0; index < visibleProviderLimit; index += 1) {
    maxHeight += providers[index].offsetHeight;
  }
  maxHeight += (visibleProviderLimit - 1) * rowGap;

  const nextMaxHeight = `${Math.ceil(maxHeight)}px`;
  if (providerList.style.maxHeight !== nextMaxHeight) {
    providerList.style.maxHeight = nextMaxHeight;
  }
  if (providerList.style.overflowY !== 'auto') {
    providerList.style.overflowY = 'auto';
  }
}
