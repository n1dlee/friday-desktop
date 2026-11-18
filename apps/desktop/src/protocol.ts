import { invoke, isTauri } from '@tauri-apps/api/core';

export type Locale = 'ru' | 'en';
export type Session = 'unknown' | 'unlocked' | 'locked' | 'suspended';
export type Phase = 'idle' | 'preparing_audio' | 'capturing' | 'transcribing' | 'routing'
  | 'planning' | 'executing' | 'speaking' | 'cancelling' | 'completed' | 'failed' | 'cancelled';

export interface Settings {
  locale: Locale;
  cloud_enabled: boolean;
  history_enabled: boolean;
  speak_answers: boolean;
  shortcut: string;
}

export type Action = { kind: 'open_app'; app: 'browser' | 'calculator' }
  | { kind: 'output_volume' };

export type ActionOutcome = { status: 'accepted_by_os' | 'blocked' | 'failed' | 'indeterminate' }
  | { status: 'volume_applied'; volume: number; muted: boolean };

export interface Receipt { id: string; action: Action; outcome: ActionOutcome }

export interface Snapshot {
  revision: number;
  session: Session;
  turn_id: string | null;
  phase: Phase;
  transcript: string | null;
  answer: string | null;
  receipts: Receipt[];
  failure: string | null;
  speech_warning: string | null;
}

export interface Bootstrap {
  protocol: number;
  version: string;
  snapshot: Snapshot;
  settings: Settings;
  key_present: boolean;
  key_warning: string | null;
  shortcut_registered: boolean;
  updates_configured: boolean;
}

export interface HistoryEntry {
  turn_id: string;
  created_at: number;
  input: string;
  answer: string | null;
  receipts: Receipt[];
}

export type Diagnostics = {
  app_version: string;
  protocol: number;
  session: Session;
  shortcut_registered: boolean;
  cloud_enabled: boolean;
  last_phase: Phase;
  microphone_available: boolean | null;
  local_voice_available: boolean | null;
  last_failure: string | null;
  unresolved_actions: number;
  updates_configured: boolean;
};

export const initialSettings: Settings = {
  locale: 'ru',
  cloud_enabled: false,
  history_enabled: false,
  speak_answers: false,
  shortcut: 'Ctrl+Shift+Space',
};

export const initialSnapshot: Snapshot = {
  revision: 0, session: 'unknown', turn_id: null, phase: 'idle',
  transcript: null, answer: null, receipts: [], failure: null, speech_warning: null,
};

const terminalPhases = new Set<Phase>(['idle', 'completed', 'failed', 'cancelled']);
export const isActive = (phase: Phase) => !terminalPhases.has(phase);

export const acceptedSnapshot = (previous: Snapshot, incoming: Snapshot): Snapshot =>
  incoming.revision >= previous.revision ? incoming : previous;

export const maxInputBytes = 8192;
export const validInput = (input: string) =>
  input.trim().length > 0 && !input.includes('\0') && new TextEncoder().encode(input).byteLength <= maxInputBytes;

export const api = {
  connected: isTauri,
  bootstrap: () => invoke<Bootstrap>('bootstrap'),
  observe: (since: number) => invoke<Snapshot>('observe_state', { since }),
  submit: (intent: { kind: 'start_capture' } | { kind: 'submit_text'; text: string }) =>
    invoke<{ turn_id: string }>('submit', { requestId: crypto.randomUUID(), intent }),
  finish: (turnId: string) => invoke<void>('finish_capture', { turnId }),
  cancel: (turnId: string) => invoke<void>('cancel_turn', { turnId }),
  saveSettings: (settings: Settings) => invoke<Bootstrap>('update_settings', { settings }),
  setKey: (key: string) => invoke<void>('set_groq_key', { key }),
  deleteKey: () => invoke<void>('delete_groq_key'),
  history: () => invoke<HistoryEntry[]>('load_history'),
  clearHistory: () => invoke<void>('clear_history'),
  diagnostics: () => invoke<Diagnostics>('diagnostics'),
  checkUpdate: () => invoke<{ version: string } | null>('check_update'),
  installUpdate: (version: string) => invoke<void>('install_update', { version }),
};
