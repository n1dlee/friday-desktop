<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    api, acceptedSnapshot, initialSettings, initialSnapshot, isActive, maxInputBytes, validInput,
    type Bootstrap, type Diagnostics, type HistoryEntry, type Receipt, type Settings, type Snapshot,
  } from './protocol';
  import { actionLabel, failure, phaseLabel, resultLabel, sessionLabel, translations } from './i18n';

  type Tab = 'assistant' | 'history' | 'settings';
  let tab = $state<Tab>('assistant');
  let snapshot = $state<Snapshot>({ ...initialSnapshot });
  let settings = $state<Settings>({ ...initialSettings });
  let savedSettings = $state<Settings>({ ...initialSettings });
  let booting = $state(true);
  let native = $state(false);
  let keyPresent = $state(false);
  let shortcutRegistered = $state(false);
  let updatesConfigured = $state(false);
  let version = $state('');
  let offer = $state('');
  let error = $state('');
  let notice = $state('');
  let draft = $state('');
  let lastInput = $state('');
  let key = $state('');
  let history = $state<HistoryEntry[]>([]);
  let report = $state<Diagnostics | null>(null);
  let pending = $state(false);
  let awaitingHistory = $state(false);
  let composer = $state<HTMLTextAreaElement>();
  let stateRunning = false;

  let t = $derived(translations[settings.locale]);
  let unlocked = $derived(snapshot.session === 'unlocked');
  let active = $derived(isActive(snapshot.phase));
  let voiceReady = $derived(savedSettings.cloud_enabled && keyPresent);
  let formDisabled = $derived(!native || !unlocked || pending || active);
  let responseShown = $derived(lastInput || snapshot.transcript || snapshot.answer || snapshot.receipts.length || snapshot.failure || active || snapshot.phase === 'cancelled');
  let phaseText = $derived(unlocked ? phaseLabel(settings.locale, snapshot.phase) : sessionLabel(settings.locale, snapshot.session));
  let nav = $derived([
    { id: 'assistant' as const, icon: '◈', label: t.assistant },
    { id: 'history' as const, icon: '☷', label: t.history },
    { id: 'settings' as const, icon: '⚙', label: t.settings },
  ]);

  function applyBoot(data: Bootstrap) {
    if (data.protocol !== 1) throw new Error('protocol');
    version = data.version;
    settings = { ...data.settings };
    savedSettings = { ...data.settings };
    document.documentElement.lang = data.settings.locale;
    keyPresent = data.key_present;
    shortcutRegistered = data.shortcut_registered;
    updatesConfigured = data.updates_configured;
    if (data.key_warning) error = failure(data.settings.locale, data.key_warning);
    applySnapshot(data.snapshot);
  }

  function applySnapshot(next: Snapshot) {
    if (next.revision < snapshot.revision) return;
    if (next.session !== 'unlocked') {
      history = [];
      key = '';
      draft = '';
      lastInput = '';
      report = null;
      error = '';
      notice = '';
    }
    if (next.turn_id && next.turn_id !== snapshot.turn_id && next.phase === 'preparing_audio') {
      lastInput = '';
    }
    snapshot = acceptedSnapshot(snapshot, next);
  }

  async function watchState() {
    while (stateRunning) {
      try {
        const session = snapshot.session;
        const next = await api.observe(snapshot.revision);
        if (!stateRunning) break;
        applySnapshot(next);
        if (session !== next.session && next.session === 'unlocked') {
          applyBoot(await api.bootstrap());
        }
      } catch {
        await new Promise(resolve => window.setTimeout(resolve, 350));
      }
    }
  }

  async function connect() {
    native = api.connected();
    if (!native) {
      booting = false;
      return;
    }
    try {
      applyBoot(await api.bootstrap());
      if (!stateRunning) return;
      void watchState();
    } catch (err) {
      error = failure(settings.locale, err);
    } finally {
      booting = false;
    }
  }

  onMount(() => {
    stateRunning = true;
    void connect();
    return () => { stateRunning = false; };
  });

  function resetMessages() {
    error = '';
    notice = '';
  }

  async function perform(work: () => Promise<void>) {
    if (pending) return;
    resetMessages();
    pending = true;
    try {
      await work();
    } catch (err) {
      error = failure(settings.locale, err);
    } finally {
      pending = false;
    }
  }

  function navigate(next: Tab) {
    tab = next;
    if (next === 'history' && unlocked) void loadHistory();
    if (next === 'settings') void refreshConfiguration();
  }

  async function refreshConfiguration() {
    if (!native || !unlocked) return;
    try {
      const state = await api.bootstrap();
      if (snapshot.session === 'unlocked') {
        keyPresent = state.key_present;
        shortcutRegistered = state.shortcut_registered;
      }
    } catch (err) {
      error = failure(settings.locale, err);
    }
  }

  async function loadHistory() {
    awaitingHistory = true;
    resetMessages();
    try {
      const items = await api.history();
      history = snapshot.session === 'unlocked' ? items : [];
    } catch (err) {
      error = failure(settings.locale, err);
    } finally {
      awaitingHistory = false;
    }
  }

  function fill(text: string) {
    draft = text;
    composer?.focus();
  }

  function sendText(event?: SubmitEvent) {
    event?.preventDefault();
    if (formDisabled || !validInput(draft)) return;
    const text = draft.trim();
    void perform(async () => {
      await api.submit({ kind: 'submit_text', text });
      if (snapshot.session !== 'unlocked') return;
      draft = '';
      lastInput = text;
    });
  }

  function composerKey(event: KeyboardEvent) {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      sendText();
    }
  }

  function beginRecording() {
    if (formDisabled || !voiceReady) return;
    void perform(async () => {
      await api.submit({ kind: 'start_capture' });
      lastInput = '';
    });
  }

  function finishRecording() {
    const id = snapshot.turn_id;
    if (id) void perform(() => api.finish(id));
  }

  function cancelTurn() {
    const id = snapshot.turn_id;
    if (!id) return;
    resetMessages();
    void api.cancel(id).catch(err => { error = failure(settings.locale, err); });
  }

  function save(event: SubmitEvent) {
    event.preventDefault();
    const proposed = { ...settings };
    void perform(async () => {
      applyBoot(await api.saveSettings(proposed));
      await tick();
      notice = t.saved;
    });
  }

  function saveKey(event: SubmitEvent) {
    event.preventDefault();
    const value = key.trim();
    key = '';
    void perform(async () => {
      await api.setKey(value);
      keyPresent = true;
    });
  }

  function eraseHistory() {
    if (!window.confirm(t.confirmDelete)) return;
    void perform(async () => {
      await api.clearHistory();
      history = [];
      lastInput = '';
      notice = t.historyCleared;
    });
  }

  function exportReport() {
    if (!report) return;
    const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }));
    const link = document.createElement('a');
    link.href = url;
    link.download = 'friday-diagnostics.json';
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 500);
  }

  function date(seconds: number) {
    return new Intl.DateTimeFormat(settings.locale, { dateStyle: 'medium', timeStyle: 'short' }).format(seconds * 1000);
  }
</script>

{#snippet receiptRows(rows: Receipt[])}
  <ul class="receipts" aria-label={t.actions}>
    {#each rows as receipt (receipt.id)}
      <li>
        <span>{actionLabel(settings.locale, receipt.action)}</span>
        <span class:success={receipt.outcome.status === 'accepted_by_os' || receipt.outcome.status === 'volume_applied'}>
          {resultLabel(settings.locale, receipt)}
        </span>
      </li>
    {/each}
  </ul>
{/snippet}

<div class="desktop">
  <aside class="sidebar">
    <div class="brand">
      <span class="brand-icon" aria-hidden="true">◈</span>
      <span>Friday</span>
    </div>
    <nav aria-label={t.about}>
      {#each nav as item (item.id)}
        <button class:current={tab === item.id} aria-current={tab === item.id ? 'page' : undefined} onclick={() => navigate(item.id)}>
          <span class="nav-icon" aria-hidden="true">{item.icon}</span>
          <span>{item.label}</span>
        </button>
      {/each}
    </nav>
    <div class="profile-label">
      <span class:live={voiceReady} class="dot" aria-hidden="true"></span>
      {voiceReady ? t.micReady : t.offline}
    </div>
  </aside>

  <main>
    <header class="top">
      <h1>{tab === 'assistant' ? t.workspace : tab === 'history' ? t.history : t.settings}</h1>
      <div class="top-status">
        <span class:live={unlocked} class="dot" aria-hidden="true"></span>
        <span>{booting ? t.connect : sessionLabel(settings.locale, snapshot.session)}</span>
      </div>
    </header>

    {#if !booting && !native}
      <p class="banner" role="status">{t.nativeOnly}</p>
    {/if}
    {#if native && !booting && !unlocked && snapshot.session !== 'unknown'}
      <p class="banner" role="status">{t.locked}</p>
    {/if}
    {#if error}
      <p class="banner error" role="alert">{error}</p>
    {:else if notice}
      <p class="banner" role="status">{notice}</p>
    {/if}

    {#if tab === 'assistant'}
      <div class="workspace">
        <section class="conversation" aria-label={t.assistant}>
          {#if !responseShown}
            <div class="empty">
              <div class="orb" aria-hidden="true"><span></span><b>◈</b></div>
              <h2>{t.greeting}</h2>
              <p>{t.textOnly}</p>
              <div class="suggestions">
                <button disabled={formDisabled} onclick={() => fill(t.browser)}>{t.browser}</button>
                <button disabled={formDisabled} onclick={() => fill(t.volume)}>{t.volume}</button>
                <button disabled={formDisabled} onclick={() => fill(t.calculator)}>{t.calculator}</button>
              </div>
            </div>
          {:else}
            <div class="turn">
              {#if unlocked && (lastInput || snapshot.transcript)}
                <div class="message user-message">
                  <span class="message-label">{snapshot.transcript ? t.transcript : t.ownText}</span>
                  <p>{snapshot.transcript ?? lastInput}</p>
                </div>
              {/if}
              {#if unlocked && snapshot.answer}
                <div class="message response"><span class="message-label">{t.assistantText}</span><p>{snapshot.answer}</p></div>
              {/if}
              {#if unlocked && snapshot.receipts.length}
                {@render receiptRows(snapshot.receipts)}
              {/if}
              {#if snapshot.failure}
                <p class="banner error" role="alert">{failure(settings.locale, snapshot.failure)}</p>
              {/if}
              {#if snapshot.speech_warning}
                <p class="banner">{failure(settings.locale, snapshot.speech_warning)}</p>
              {/if}
            </div>
          {/if}
        </section>
        <div class="controls">
          <div class="activity" class:recording={snapshot.phase === 'capturing'}>
            <div class="current-phase" role="status" aria-live="polite">
              <span class="phase-dot" aria-hidden="true"></span>{phaseText}
            </div>
            <div class="voice-buttons">
              {#if snapshot.phase === 'capturing'}
                <button class="button accent" disabled={pending || !unlocked} onclick={finishRecording}>{t.finish}</button>
              {:else if !active}
                <button class="button" disabled={formDisabled || !voiceReady} onclick={beginRecording}>{t.start}</button>
              {/if}
              {#if active}
                <button class="button stop" disabled={snapshot.phase === 'cancelling'} onclick={cancelTurn}>{t.cancel}</button>
              {/if}
            </div>
          </div>
          <form class="composer" onsubmit={sendText}>
            <textarea aria-label={t.inputPlaceholder} placeholder={t.inputPlaceholder}
              bind:this={composer} bind:value={draft} disabled={!native || !unlocked} maxlength={maxInputBytes}
              rows="2" onkeydown={composerKey}></textarea>
            <button type="submit" class="button primary" disabled={formDisabled || !validInput(draft)}>{t.send}<span aria-hidden="true">↗</span></button>
          </form>
          <div class="composer-help">
            <span>{!savedSettings.cloud_enabled ? t.cloudOff : !keyPresent ? t.keyNeeded : t.shortcutHelp}</span>
            {#if shortcutRegistered}<kbd>{savedSettings.shortcut}</kbd>{/if}
          </div>
        </div>
      </div>
    {:else if tab === 'history'}
      <section class="content">
        <p class="muted">{t.historyHelp}</p>
        {#if !awaitingHistory && history.length === 0}
          <p class="empty-history">{t.noHistory}</p>
        {/if}
        {#each history as entry (entry.turn_id)}
          <article class="history-card">
            <time datetime={new Date(entry.created_at * 1000).toISOString()}>{date(entry.created_at)}</time>
            <h2>{entry.input}</h2>
            {#if entry.answer}<p>{entry.answer}</p>{/if}
            {#if entry.receipts.length}{@render receiptRows(entry.receipts)}{/if}
          </article>
        {/each}
      </section>
    {:else}
      <div class="content settings">
        <form onsubmit={save}>
          <fieldset disabled={formDisabled}>
            <legend>{t.settingsGeneral}</legend>
            <div class="row"><label for="language">{t.language}</label>
              <select id="language" bind:value={settings.locale}><option value="ru">Русский</option><option value="en">English</option></select></div>
            <div class="row"><label for="shortcut">{t.shortcut}</label>
              <input id="shortcut" type="text" bind:value={settings.shortcut} maxlength="64" autocomplete="off" required /></div>
            {#if native && unlocked && !shortcutRegistered}<p class="muted">{t.shortcutConflict}</p>{/if}
          </fieldset>
          <fieldset disabled={formDisabled}>
            <legend>{t.voice}</legend>
            <label class="check"><input type="checkbox" bind:checked={settings.cloud_enabled} /> {t.enableCloud}</label>
            <p class="muted">{t.cloudConsent}</p>
            <label class="check"><input type="checkbox" bind:checked={settings.speak_answers} /> {t.enableTts}</label>
          </fieldset>
          <fieldset disabled={formDisabled}>
            <legend>{t.privacy}</legend>
            <label class="check"><input type="checkbox" bind:checked={settings.history_enabled} /> {t.enableHistory}</label>
            <p class="muted">{t.privacyHelp}</p>
            <button type="button" class="button" onclick={eraseHistory}>{t.clearHistory}</button>
          </fieldset>
          <button class="button primary" type="submit" disabled={formDisabled}>{t.save}</button>
        </form>
        <form class="settings-separate" onsubmit={saveKey}>
          <fieldset disabled={formDisabled}>
            <legend>{t.keyLabel}</legend>
            <p class="muted">{keyPresent ? t.keyStored : t.keyMissing}</p>
            <div class="row key-row">
              <input type="password" aria-label={t.keyLabel} placeholder="gsk_…" autocomplete="off" bind:value={key} minlength="8" maxlength="512" required />
              <button class="button" type="submit" disabled={key.trim().length < 8}>{t.keySave}</button>
            </div>
            {#if keyPresent}<button class="button stop" type="button" onclick={() => perform(async () => { await api.deleteKey(); keyPresent = false; notice = t.keyRemoved; })}>{t.keyRemove}</button>{/if}
          </fieldset>
        </form>
        <section class="settings-separate">
          <h2>{t.diagnosis}</h2>
          <p class="muted">{t.diagnosisHelp}</p>
          <button class="button" disabled={formDisabled} onclick={() => perform(async () => { report = await api.diagnostics(); })}>{t.loadDiagnosis}</button>
          {#if report}
            <pre>{JSON.stringify(report, null, 2)}</pre>
            <button class="button" onclick={exportReport}>{t.exportDiagnosis}</button>
          {/if}
        </section>
        <section class="settings-separate">
          <h2>{t.updates}{version ? ` · v${version}` : ''}</h2>
          {#if updatesConfigured}
            <button class="button" disabled={formDisabled} onclick={() => perform(async () => {
              offer = (await api.checkUpdate())?.version ?? '';
              if (!offer) notice = t.currentVersion;
            })}>{t.checkUpdate}</button>
            {#if offer}
              <button class="button primary" disabled={formDisabled} onclick={() => perform(async () => {
                notice = t.installing;
                await api.installUpdate(offer);
              })}>{t.install} {offer}</button>
            {/if}
          {:else}
            <p class="muted">{t.updateOff}</p>
          {/if}
        </section>
      </div>
    {/if}
  </main>
</div>
