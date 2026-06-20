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
const PROVIDER_VISIBLE_HEIGHT_DATASET_KEY = 'visibleHeight';

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

    const panel = document.querySelector<HTMLElement>(PANEL_SELECTOR)!;
    const observer = new ResizeObserver(syncProviderListHeight);
    observer.observe(providerList);
    observer.observe(panel);
    for (const provider of providerList.querySelectorAll<HTMLElement>('.provider')) {
      observer.observe(provider);
    }
    window.addEventListener('resize', syncProviderListHeight);
    syncProviderListHeight();

    return () => {
      observer.disconnect();
      window.removeEventListener('resize', syncProviderListHeight);
      providerList.style.maxHeight = '';
      delete providerList.dataset[PROVIDER_VISIBLE_HEIGHT_DATASET_KEY];
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
  const providerList = panelInner.querySelector<HTMLElement>('.provider-list');
  const desiredProviderListHeight = Number(providerList?.dataset[PROVIDER_VISIBLE_HEIGHT_DATASET_KEY] ?? 0);
  const currentProviderListHeight = providerList?.offsetHeight ?? 0;
  const clampedProviderListHeight = Math.max(0, desiredProviderListHeight - currentProviderListHeight);

  return Math.ceil(
    panelInner.offsetHeight
      + clampedProviderListHeight
      + Number.parseFloat(panelStyle.paddingTop)
      + Number.parseFloat(panelStyle.paddingBottom)
      + Number.parseFloat(panelStyle.borderTopWidth)
      + Number.parseFloat(panelStyle.borderBottomWidth),
  );
}

function applyProviderListLimit(providerList: HTMLElement, visibleProviderLimit: number) {
  const providers = providerList.querySelectorAll<HTMLElement>('.provider');
  const visibleCount = Math.min(visibleProviderLimit, providers.length);
  if (visibleCount === 0) {
    providerList.style.maxHeight = '';
    delete providerList.dataset[PROVIDER_VISIBLE_HEIGHT_DATASET_KEY];
    return;
  }

  const rowGap = Number.parseFloat(window.getComputedStyle(providerList).rowGap);
  let visibleHeight = 0;
  for (let index = 0; index < visibleCount; index += 1) {
    visibleHeight += providers[index].offsetHeight;
  }
  visibleHeight += (visibleCount - 1) * rowGap;

  const availableHeight = measureProviderListAvailableHeight(providerList);
  const maxHeight = Math.min(visibleHeight, availableHeight);
  providerList.dataset[PROVIDER_VISIBLE_HEIGHT_DATASET_KEY] = `${Math.ceil(visibleHeight)}`;

  const nextMaxHeight = `${Math.ceil(maxHeight)}px`;
  if (providerList.style.maxHeight !== nextMaxHeight) {
    providerList.style.maxHeight = nextMaxHeight;
  }
}

function measureProviderListAvailableHeight(providerList: HTMLElement): number {
  const panel = providerList.closest<HTMLElement>(PANEL_SELECTOR)!;
  const panelStyle = window.getComputedStyle(panel);
  const panelRect = panel.getBoundingClientRect();
  const providerListRect = providerList.getBoundingClientRect();
  const panelContentBottom =
    panelRect.bottom
    - Number.parseFloat(panelStyle.borderBottomWidth)
    - Number.parseFloat(panelStyle.paddingBottom);

  return Math.max(0, panelContentBottom - providerListRect.top);
}
