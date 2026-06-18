import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart';
import { useEffect, useState } from 'react';
import * as api from './lib/api';
import { getText, LOCALES } from './lib/i18n';
import type { AppSettings, ProviderId } from './lib/types';
import { PROVIDERS } from './lib/types';


const API_KEY_FIELD = 'api_key';
const DEFAULT_SETTINGS: AppSettings = {
  enabledProviders: PROVIDERS.map((provider) => provider.id),
  refreshIntervalSecs: 60,
  visibleProviderLimit: 2,
  locale: 'zh-CN',
};

const INTERVALS: AppSettings['refreshIntervalSecs'][] = [30, 60, 120, 300];
const VISIBLE_PROVIDER_LIMITS = [1, 2, 3, 4] as const;
type SettingsCategory = 'provider' | 'system';

export default function SettingsApp() {
  const [settings, setSettings] = useState<AppSettings>(DEFAULT_SETTINGS);
  const [saving, setSaving] = useState(false);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const [secretStatus, setSecretStatus] = useState<Record<ProviderId, boolean>>({});
  const [secretInputs, setSecretInputs] = useState<Record<ProviderId, string>>({});
  const [activeCategory, setActiveCategory] = useState<SettingsCategory>('provider');
  const [appError, setAppError] = useState<string | null>(null);
  const text = getText(settings.locale);
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
                {PROVIDERS.map((provider) => {
                  const checked = settings.enabledProviders.includes(provider.id);
                  const apiKeyConfigured = secretStatus[provider.id] === true;
                  return (
                    <div className="provider-setting" key={provider.id}>
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
