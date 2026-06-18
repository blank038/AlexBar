import { invoke } from '@tauri-apps/api/core';
import type { AppSettings, ProviderId, ProviderSnapshot } from './types';

export function getReports(): Promise<ProviderSnapshot[]> {
  return invoke<ProviderSnapshot[]>('get_reports');
}

export function refreshAll(): Promise<ProviderSnapshot[]> {
  return invoke<ProviderSnapshot[]>('refresh_all');
}

export function refreshProvider(provider: ProviderId): Promise<ProviderSnapshot> {
  return invoke<ProviderSnapshot>('refresh_provider', { provider });
}

export function setProviderSecret(provider: ProviderId, field: string, value: string | null): Promise<void> {
  return invoke<void>('set_provider_secret', { provider, field, value });
}

export function getProviderSecretStatus(provider: ProviderId): Promise<boolean> {
  return invoke<boolean>('get_provider_secret_status', { provider });
}

export function getSettings(): Promise<AppSettings> {
  return invoke<AppSettings>('get_settings');
}

export function updateSettings(settings: AppSettings): Promise<AppSettings> {
  return invoke<AppSettings>('update_settings', { settings });
}

export function setPopoverHeight(height: number): Promise<void> {
  return invoke<void>('set_popover_height', { height });
}

export function openSettingsWindow(): Promise<void> {
  return invoke<void>('open_settings_window');
}
