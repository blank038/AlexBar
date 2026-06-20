import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart';
import { type PointerEvent, useEffect, useMemo, useRef, useState } from 'react';
import * as api from './lib/api';
import { getText, LOCALES } from './lib/i18n';
import type { AppSettings, ProviderDefinition, ProviderId } from './lib/types';
import { PROVIDERS } from './lib/types';


const API_KEY_FIELD = 'api_key';
const DEFAULT_SETTINGS: AppSettings = {
  enabledProviders: PROVIDERS.map((provider) => provider.id),
  providerOrder: PROVIDERS.map((provider) => provider.id),
  refreshIntervalSecs: 60,
  visibleProviderLimit: 2,
  locale: 'zh-CN',
};

const INTERVALS: AppSettings['refreshIntervalSecs'][] = [30, 60, 120, 300];
const VISIBLE_PROVIDER_LIMITS = [1, 2, 3, 4, 5, 6, 7, 8] as const;
type SettingsCategory = 'provider' | 'system';
type DropPosition = 'before' | 'after';
const PROVIDER_DRAG_THRESHOLD_PX = 6;

interface ProviderDropTarget {
  provider: ProviderId;
  position: DropPosition;
}

interface ProviderDragState {
  provider: ProviderId;
  pointerId: number;
  startX: number;
  startY: number;
  active: boolean;
}

export default function SettingsApp() {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [saving, setSaving] = useState(false);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const [secretStatus, setSecretStatus] = useState<Record<ProviderId, boolean>>({});
  const [secretInputs, setSecretInputs] = useState<Record<ProviderId, string>>({});
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>('provider');
  const [appError, setAppError] = useState<string | null>(null);
  const [draggedProvider, setDraggedProvider] = useState<ProviderId | null>(null);
  const [dropTarget, setDropTarget] = useState<ProviderDropTarget | null>(null);
  const [draftProviderOrder, setDraftProviderOrder] = useState<ProviderId[] | null>(null);
  const dragStateRef = useRef<ProviderDragState | null>(null);
  const draftProviderOrderRef = useRef<ProviderId[] | null>(null);
  const text = getText(settings.locale);
  const providerOrder = draftProviderOrder ?? settings.providerOrder;
  const orderedProviders = useMemo(
    () => orderProviders(providerOrder),
    [providerOrder],
  );
  const providerCategoryClass =
    activeCategory === 'provider'
      ? 'settings-window__category settings-window__category--active'
      : 'settings-window__category';
  const systemCategoryClass =
    activeCategory === 'system'
      ? 'settings-window__category settings-window__category--active'
      : 'settings-window__category';
  const activeCategoryLabel = activeCategory === 'provider' ? text.categoryProvider : text.categorySystem;

  useEffect(() => {
    let disposed = false;

    async function boot() {
      try {
        const apiKeyProviders = PROVIDERS.filter((provider) => provider.requiresApiKey);
        const [nextSettings, nextAutostartEnabled, secretStatusEntries] = await Promise.all([
          api.getSettings(),
          isEnabled(),
          Promise.all(
            apiKeyProviders.map(async (provider) => [
              provider.id,
              await api.getProviderSecretStatus(provider.id),
            ] as const),
          ),
        ]);
        if (disposed) return;
        setSettings(nextSettings);
        setAutostartEnabled(nextAutostartEnabled);
        const nextSecretStatus: Record<ProviderId, boolean> = {};
        for (const [providerId, configured] of secretStatusEntries) {
          nextSecretStatus[providerId] = configured;
        }
        setSecretStatus(nextSecretStatus);
      } catch (error) {
        if (!disposed) setAppError(String(error));
      }
    }

    void boot();

    return () => {
      disposed = true;
    };
  }, []);

  async function persistSettings(nextSettings: AppSettings) {
    const previousSettings = settings;
    let settingsSaved = false;
    setSaving(true);
    setAppError(null);
    setSettings(nextSettings);
    try {
      const saved = await api.updateSettings(nextSettings);
      settingsSaved = true;
      setSettings(saved);
      await api.refreshAll();
    } catch (error) {
      setAppError(String(error));
      if (!settingsSaved) setSettings(previousSettings);
    } finally {
      setSaving(false);
    }
  }

  function setProvider(provider: ProviderId, enabled: boolean) {
    const currentlyEnabled = settings.enabledProviders.includes(provider);
    if (currentlyEnabled === enabled) return;

    const enabledProviders = enabled
      ? [...settings.enabledProviders, provider]
      : settings.enabledProviders.filter((value) => value !== provider);
    void persistSettings({ ...settings, enabledProviders });
  }

  function startProviderDrag(event: PointerEvent<HTMLDivElement>, provider: ProviderId) {
    if (saving || isBlockedDragTarget(event.target)) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;

    dragStateRef.current = {
      provider,
      pointerId: event.pointerId,
      startX: event.clientX,
      startY: event.clientY,
      active: false,
    };
    event.currentTarget.setPointerCapture(event.pointerId);
  }

  function moveProviderDrag(event: PointerEvent<HTMLDivElement>) {
    const dragState = dragStateRef.current;
    if (!dragState || dragState.pointerId !== event.pointerId || saving) return;

    if (!dragState.active) {
      const moved =
        Math.abs(event.clientX - dragState.startX) >= PROVIDER_DRAG_THRESHOLD_PX
        || Math.abs(event.clientY - dragState.startY) >= PROVIDER_DRAG_THRESHOLD_PX;
      if (!moved) return;

      dragState.active = true;
      const initialOrder = draftProviderOrderRef.current ?? settings.providerOrder;
      draftProviderOrderRef.current = initialOrder;
      setDraftProviderOrder(initialOrder);
      setDraggedProvider(dragState.provider);
    }

    event.preventDefault();
    const target = providerTargetFromPoint(event.clientX, event.clientY);
    if (!target || target.provider === dragState.provider) {
      setDropTarget(null);
      return;
    }

    const position = getDropPosition(event.clientY, target.element);
    setDropTarget({ provider: target.provider, position });
    setDraftProviderOrder((current) => {
      const currentOrder = current ?? settings.providerOrder;
      const nextOrder = moveProvider(currentOrder, dragState.provider, target.provider, position);
      draftProviderOrderRef.current = nextOrder;
      return sameProviderOrder(nextOrder, currentOrder) ? currentOrder : nextOrder;
    });
  }

  function finishProviderDrag(event: PointerEvent<HTMLDivElement>) {
    const dragState = dragStateRef.current;
    if (!dragState || dragState.pointerId !== event.pointerId) return;

    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }

    const finalOrder = draftProviderOrderRef.current;
    dragStateRef.current = null;
    draftProviderOrderRef.current = null;
    setDraggedProvider(null);
    setDropTarget(null);
    setDraftProviderOrder(null);

    if (dragState.active && finalOrder && !sameProviderOrder(finalOrder, settings.providerOrder)) {
      void persistSettings({ ...settings, providerOrder: finalOrder });
    }
  }

  function cancelProviderDrag(event: PointerEvent<HTMLDivElement>) {
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    dragStateRef.current = null;
    draftProviderOrderRef.current = null;
    setDraggedProvider(null);
    setDropTarget(null);
    setDraftProviderOrder(null);
  }

  async function saveApiKey(provider: ProviderId) {
    const value = (secretInputs[provider] ?? '').trim();
    setSaving(true);
    setAppError(null);
    try {
      await api.setProviderSecret(provider, API_KEY_FIELD, value || null);
      setSecretStatus((current) => ({ ...current, [provider]: value.length > 0 }));
      setSecretInputs((current) => ({ ...current, [provider]: '' }));
    } catch (error) {
      setAppError(String(error));
    } finally {
      setSaving(false);
    }
  }

  async function setAutoStart(enabled: boolean) {
    if (autostartEnabled === enabled) return;

    const previousAutostartEnabled = autostartEnabled;
    setSaving(true);
    setAppError(null);
    setAutostartEnabled(enabled);
    try {
      if (enabled) {
        await enable();
      } else {
        await disable();
      }
      setAutostartEnabled(await isEnabled());
    } catch (error) {
      setAppError(String(error));
      setAutostartEnabled(previousAutostartEnabled);
    } finally {
      setSaving(false);
    }
  }

  return (
    <main className="settings-window">
      <header className="settings-window__titlebar">
        <div>
          <p className="eyebrow">AlexBar</p>
          <h1>{text.settingsWindowTitle}</h1>
        </div>
        <span className="settings-window__save-state">
          {saving ? text.saving : text.persistedLocally}
        </span>
      </header>

      {appError ? <div className="app-error settings-window__error">{appError}</div> : null}

      <div className="settings-window__body">
        <nav className="settings-window__nav" aria-label={text.settings} role="tablist">
          <button
            className={providerCategoryClass}
            type="button"
            role="tab"
            aria-selected={activeCategory === 'provider'}
            onClick={() => setActiveCategory('provider')}
          >
            {text.categoryProvider}
          </button>
          <button
            className={systemCategoryClass}
            type="button"
            role="tab"
            aria-selected={activeCategory === 'system'}
            onClick={() => setActiveCategory('system')}
          >
            {text.categorySystem}
          </button>
        </nav>

        <section className="settings-window__content" role="tabpanel" aria-label={activeCategoryLabel}>
          {activeCategory === 'provider' ? (
            <>
              <div className="settings-window__group">
                <p className="settings-window__label">{text.providers}</p>
                {orderedProviders.map((provider) => {
                  const checked = settings.enabledProviders.includes(provider.id);
                  const apiKeyConfigured = secretStatus[provider.id] === true;
                  const dropPosition = dropTarget?.provider === provider.id ? dropTarget.position : null;
                  const providerSettingClass = [
                    'provider-setting',
                    draggedProvider === provider.id ? 'provider-setting--dragging' : '',
                    dropPosition ? `provider-setting--drop-${dropPosition}` : '',
                  ]
                    .filter(Boolean)
                    .join(' ');
                  return (
                    <div
                      className={providerSettingClass}
                      key={provider.id}
                      data-provider-id={provider.id}
                      aria-grabbed={draggedProvider === provider.id ? true : undefined}
                      onPointerDown={(event) => startProviderDrag(event, provider.id)}
                      onPointerMove={moveProviderDrag}
                      onPointerUp={finishProviderDrag}
                      onPointerCancel={cancelProviderDrag}
                    >
                      <div className="provider-setting__row">
                        <button
                          className="provider-setting__drag-handle"
                          type="button"
                          disabled={saving}
                          aria-label={`${text.reorderProvider}: ${provider.shortName}`}
                          title={text.reorderProvider}
                        >
                          ::
                        </button>

                        <div className="provider-setting__body">
                          <label className="toggle">
                            <input
                              type="checkbox"
                              checked={checked}
                              disabled={saving}
                              onChange={(event) => setProvider(provider.id, event.currentTarget.checked)}
                            />
                            <span className="toggle__visual" />
                            <span>
                              <strong>{provider.shortName}</strong>
                              <em>{provider.credentialPath}</em>
                            </span>
                          </label>

                          {provider.requiresApiKey ? (
                            <div className="secret-field">
                              <p className="secret-field__status">
                                {apiKeyConfigured ? text.apiKeyConfigured : text.apiKeyNotConfigured}
                              </p>
                              <div className="secret-field__controls">
                                <input
                                  className="secret-field__input"
                                  type="password"
                                  autoComplete="off"
                                  aria-label={`${provider.shortName} ${text.enterApiKey}`}
                                  placeholder={text.enterApiKey}
                                  value={secretInputs[provider.id] ?? ''}
                                  disabled={saving}
                                  onChange={(event) => {
                                    const value = event.currentTarget.value;
                                    setSecretInputs((current) => ({
                                      ...current,
                                      [provider.id]: value,
                                    }));
                                  }}
                                />
                                <button
                                  className="secret-field__button"
                                  type="button"
                                  disabled={saving}
                                  onClick={() => void saveApiKey(provider.id)}
                                >
                                  {text.saveApiKey}
                                </button>
                              </div>
                            </div>
                          ) : null}
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>

              <div className="settings-window__group">
                <p className="settings-window__label">{text.refreshInterval}</p>
                <div className="intervals">
                  {INTERVALS.map((interval) => (
                    <button
                      key={interval}
                      className={settings.refreshIntervalSecs === interval ? 'interval interval--active' : 'interval'}
                      type="button"
                      disabled={saving}
                      onClick={() => void persistSettings({ ...settings, refreshIntervalSecs: interval })}
                    >
                      {interval}s
                    </button>
                  ))}
                </div>
              </div>

              <div className="settings-window__group">
                <p className="settings-window__label">{text.visibleProviderLimit}</p>
                <div className="intervals">
                  {VISIBLE_PROVIDER_LIMITS.map((limit) => (
                    <button
                      key={limit}
                      className={settings.visibleProviderLimit === limit ? 'interval interval--active' : 'interval'}
                      type="button"
                      disabled={saving}
                      onClick={() => void persistSettings({ ...settings, visibleProviderLimit: limit })}
                    >
                      {limit}
                    </button>
                  ))}
                </div>
              </div>
            </>
          ) : (
            <>
              <div className="settings-window__group">
                <p className="settings-window__label">{text.categorySystem}</p>
                <label className="toggle">
                  <input
                    type="checkbox"
                    checked={autostartEnabled === true}
                    disabled={saving || autostartEnabled === null}
                    onChange={(event) => void setAutoStart(event.currentTarget.checked)}
                  />
                  <span className="toggle__visual" />
                  <span>
                    <strong>{text.autoStart}</strong>
                    <em>{autostartEnabled === null ? text.autoStartLoading : text.autoStartHint}</em>
                  </span>
                </label>
              </div>

              <div className="settings-window__group">
                <p className="settings-window__label">{text.language}</p>
                <div className="language-options">
                  {LOCALES.map((option) => (
                    <button
                      key={option.id}
                      className={settings.locale === option.id ? 'language-option language-option--active' : 'language-option'}
                      type="button"
                      disabled={saving}
                      onClick={() => void persistSettings({ ...settings, locale: option.id })}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              </div>
            </>
          )}
        </section>
      </div>
    </main>
  );
}

function orderProviders(providerOrder: ProviderId[]): ProviderDefinition[] {
  const rank = new Map(providerOrder.map((provider, index) => [provider, index]));
  return [...PROVIDERS].sort((left, right) => {
    const leftRank = rank.get(left.id) ?? Number.MAX_SAFE_INTEGER;
    const rightRank = rank.get(right.id) ?? Number.MAX_SAFE_INTEGER;
    return leftRank - rightRank;
  });
}

function getDropPosition(pointerY: number, element: HTMLElement): DropPosition {
  const rect = element.getBoundingClientRect();
  return pointerY >= rect.top + rect.height / 2 ? 'after' : 'before';
}

function providerTargetFromPoint(x: number, y: number): { provider: ProviderId; element: HTMLElement } | null {
  const element = document.elementFromPoint(x, y)?.closest<HTMLElement>('[data-provider-id]');
  const provider = element?.dataset.providerId;
  return element && provider ? { provider, element } : null;
}

function moveProvider(
  providerOrder: ProviderId[],
  draggedProvider: ProviderId,
  targetProvider: ProviderId,
  position: DropPosition,
): ProviderId[] {
  const nextOrder = providerOrder.filter((provider) => provider !== draggedProvider);
  const targetIndex = nextOrder.indexOf(targetProvider);
  if (targetIndex < 0 || nextOrder.length === providerOrder.length) return providerOrder;

  nextOrder.splice(position === 'after' ? targetIndex + 1 : targetIndex, 0, draggedProvider);
  return nextOrder;
}

function sameProviderOrder(left: ProviderId[], right: ProviderId[]): boolean {
  return left.length === right.length && left.every((provider, index) => provider === right[index]);
}

function isBlockedDragTarget(target: EventTarget): boolean {
  return target instanceof HTMLElement && Boolean(target.closest('input, .secret-field__button'));
}
