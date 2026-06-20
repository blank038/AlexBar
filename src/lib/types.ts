export type ProviderId = string;
export type Locale = 'zh-CN' | 'en-US';

export type Urgency = 'calm' | 'tense' | 'capped' | 'unknown';
export type CountUnit = 'tokens' | 'requests';

export interface ProviderSnapshot {
  provider: ProviderId;
  refreshedAt: number;
  account: AccountInfo | null;
  metrics: ProviderMetric[];
  note: string | null;
}

export interface AccountInfo {
  identifier: string | null;
  email: string | null;
  plan: string | null;
}

export type ProviderMetric = QuotaMetric | BalanceMetric;

export interface QuotaMetric {
  kind: 'quota';
  key: string;
  displayName: string;
  bucket: Bucket;
  progress: Progress;
  urgency: Urgency;
}

export interface BalanceMetric {
  kind: 'balance';
  key: string;
  displayName: string;
  amount: number;
  currency: string;
  granted: number | null;
  toppedUp: number | null;
  isAvailable: boolean;
  urgency: Urgency;
}

export type Bucket =
  | { kind: 'rolling'; durationMs: number; label: string; resetsAt: number | null }
  | { kind: 'openEnded'; label: string; resetsAt: number | null };

export type Progress =
  | { kind: 'ratio'; usedPercent: number }
  | {
      kind: 'counted';
      used: number | null;
      total: number | null;
      remaining: number | null;
      usedPercent: number | null;
      unit: CountUnit;
    };

export interface AppSettings {
  enabledProviders: ProviderId[];
  providerOrder: ProviderId[];
  refreshIntervalSecs: 30 | 60 | 120 | 300;
  visibleProviderLimit: number;
  locale: Locale;
}

export interface ProviderDefinition {
  id: ProviderId;
  name: string;
  shortName: string;
  credentialPath: string;
  accent: string;
  requiresApiKey: boolean;
}

export const PROVIDERS: ProviderDefinition[] = [
  {
    id: 'openai-codex',
    name: 'Codex Plus / Pro',
    shortName: 'Codex',
    credentialPath: '~/.codex/auth.json',
    accent: '#c8ff5f',
    requiresApiKey: false,
  },
  {
    id: 'anthropic',
    name: 'Claude Pro / Max',
    shortName: 'Claude',
    credentialPath: '~/.claude/.credentials.json',
    accent: '#5ac4ff',
    requiresApiKey: false,
  },
  {
    id: 'deepseek',
    name: 'DeepSeek API Balance',
    shortName: 'DeepSeek',
    credentialPath: 'AlexBar secrets.json',
    accent: '#34d399',
    requiresApiKey: true,
  },
  {
    id: 'zai',
    name: 'z.ai Coding Plan',
    shortName: 'z.ai',
    credentialPath: 'AlexBar secrets.json',
    accent: '#7c5cff',
    requiresApiKey: true,
  },
];
