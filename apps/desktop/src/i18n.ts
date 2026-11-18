import type { Action, Locale, Phase, Receipt, Session } from './protocol';

export const translations = {
  ru: {
    assistant: 'Ассистент', history: 'История', settings: 'Настройки', available: 'Готова к работе',
    connect: 'Подключение', inputPlaceholder: 'Напишите команду или задайте вопрос', send: 'Отправить',
    start: 'Говорить', finish: 'Завершить запись', cancel: 'Стоп', about: 'Friday Desktop',
    greeting: 'Что сделать?', textOnly: 'Локальные команды работают без сети.',
    workspace: 'Ассистент', locked: 'Разблокируйте Windows, чтобы продолжить.',
    cloudOff: 'Включите Groq в настройках для голосового ввода и диалога.',
    keyNeeded: 'Добавьте ключ Groq в настройках для голосового ввода.',
    nativeOnly: 'Откройте установленное приложение Friday Desktop.',
    browser: 'Открой браузер', volume: 'Сделай тише', calculator: 'Открой калькулятор',
    actions: 'Действия', assistantText: 'Friday', transcript: 'Распознано', ownText: 'Вы',
    partial: 'Ответ формируется', settingsGeneral: 'Общие', language: 'Язык',
    shortcut: 'Горячая клавиша', save: 'Сохранить', saved: 'Настройки сохранены',
    shortcutHelp: 'Нажмите для начала и ещё раз для завершения записи.',
    voice: 'Голос и облако', enableCloud: 'Разрешить обработку в Groq',
    cloudConsent: 'При записи аудио отправляется в Groq. Для диалога отправляется текст запроса и, если включена история, недавние сообщения.',
    enableTts: 'Озвучивать ответы локальным голосом Windows',
    keyLabel: 'Ключ Groq API', keyStored: 'Ключ сохранён в Windows Credential Manager.',
    keyMissing: 'Ключ не задан.', keySave: 'Сохранить ключ', keyRemove: 'Удалить ключ',
    keyRemoved: 'Ключ удалён', privacy: 'Данные', enableHistory: 'Сохранять историю на этом компьютере',
    privacyHelp: 'История хранится до 30 дней. Аудиозаписи на диск не сохраняются.',
    clearHistory: 'Удалить историю', confirmDelete: 'Удалить всю локальную историю?', historyCleared: 'История удалена',
    noHistory: 'Сохранённых диалогов пока нет.', historyHelp: 'Показаны последние 100 сохранённых запросов.',
    diagnosis: 'Диагностика', loadDiagnosis: 'Сформировать отчёт', exportDiagnosis: 'Экспортировать JSON',
    diagnosisHelp: 'Отчёт содержит версии и коды состояния. Здесь нет записей, переписки и ключей.',
    updates: 'Обновления', checkUpdate: 'Проверить', currentVersion: 'Установлена текущая версия',
    updateOff: 'Канал подписанных обновлений для этой сборки не задан.', install: 'Установить',
    installing: 'Загрузка и проверка обновления', shortcutConflict: 'Горячая клавиша недоступна. Выберите другую.',
    offline: 'Локальный режим', micReady: 'Голос доступен', micOff: 'Голос выключен',
  },
  en: {
    assistant: 'Assistant', history: 'History', settings: 'Settings', available: 'Ready',
    connect: 'Connecting', inputPlaceholder: 'Enter a command or ask a question', send: 'Send',
    start: 'Talk', finish: 'Finish recording', cancel: 'Stop', about: 'Friday Desktop',
    greeting: 'What should I do?', textOnly: 'Local commands work offline.',
    workspace: 'Assistant', locked: 'Unlock Windows to continue.',
    cloudOff: 'Enable Groq in settings for voice input and conversation.',
    keyNeeded: 'Add a Groq key in settings for voice input.',
    nativeOnly: 'Open the installed Friday Desktop application.',
    browser: 'Open the browser', volume: 'Turn down the volume', calculator: 'Open the calculator',
    actions: 'Actions', assistantText: 'Friday', transcript: 'Transcript', ownText: 'You',
    partial: 'Preparing a response', settingsGeneral: 'General', language: 'Language',
    shortcut: 'Global shortcut', save: 'Save', saved: 'Settings saved',
    shortcutHelp: 'Press to start recording and press again to finish.',
    voice: 'Voice and cloud', enableCloud: 'Allow Groq processing',
    cloudConsent: 'Groq receives audio for transcription and text for conversation. Recent messages are included when history is enabled.',
    enableTts: 'Read responses with a local Windows voice',
    keyLabel: 'Groq API key', keyStored: 'Key is stored in Windows Credential Manager.',
    keyMissing: 'No key configured.', keySave: 'Save key', keyRemove: 'Remove key',
    keyRemoved: 'Key removed', privacy: 'Data', enableHistory: 'Save history on this computer',
    privacyHelp: 'History is kept for up to 30 days. Audio clips are kept in memory.',
    clearHistory: 'Delete history', confirmDelete: 'Delete all local history?', historyCleared: 'History deleted',
    noHistory: 'No saved conversations yet.', historyHelp: 'Showing the 100 most recent saved requests.',
    diagnosis: 'Diagnostics', loadDiagnosis: 'Prepare report', exportDiagnosis: 'Export JSON',
    diagnosisHelp: 'The report includes versions and status codes. Voice, conversations and keys are excluded.',
    updates: 'Updates', checkUpdate: 'Check', currentVersion: 'You are up to date',
    updateOff: 'This build has no signed update channel configured.', install: 'Install',
    installing: 'Downloading and verifying the update', shortcutConflict: 'Shortcut unavailable. Choose a different shortcut.',
    offline: 'Local mode', micReady: 'Voice available', micOff: 'Voice off',
  },
} satisfies Record<Locale, Record<string, string>>;

const phases: Record<Locale, Record<Phase, string>> = {
  ru: {
    idle: 'Готова', preparing_audio: 'Подключаю микрофон', capturing: 'Слушаю',
    transcribing: 'Распознаю речь', routing: 'Разбираю команду', planning: 'Думаю',
    executing: 'Выполняю', speaking: 'Озвучиваю', cancelling: 'Останавливаюсь',
    completed: 'Ход завершён', failed: 'Не удалось завершить ход', cancelled: 'Остановлено',
  },
  en: {
    idle: 'Ready', preparing_audio: 'Opening microphone', capturing: 'Listening',
    transcribing: 'Transcribing', routing: 'Reading command', planning: 'Thinking',
    executing: 'Running actions', speaking: 'Speaking', cancelling: 'Stopping',
    completed: 'Turn complete', failed: 'Could not complete the turn', cancelled: 'Stopped',
  },
};

const errors: Record<string, [string, string]> = {
  busy: ['Дождитесь завершения текущего хода.', 'Wait for the current turn to finish.'],
  duplicate: ['Этот запрос уже принят.', 'This request was already accepted.'],
  locked: ['Откройте панель Friday в разблокированной сессии Windows.', 'Open the Friday panel in an unlocked Windows session.'],
  invalid_turn: ['Этот ход уже завершился.', 'This turn has already ended.'],
  stopped: ['Friday завершает работу.', 'Friday is shutting down.'],
  timeout: ['Истекло время ожидания.', 'The operation timed out.'],
  invalid_input: ['Проверьте формат и длину запроса.', 'Check the format and length of your request.'],
  clarify: ['Уточните команду. Доступны браузер, калькулятор и громкость.', 'Clarify the command. Browser, calculator and volume are supported.'],
  cloud_disabled: ['Облачная обработка выключена.', 'Cloud processing is disabled.'],
  missing_key: ['Добавьте ключ Groq в настройках.', 'Add a Groq key in settings.'],
  credential_store: ['Windows не предоставила доступ к ключу.', 'Could not access Windows Credential Manager.'],
  unauthorized: ['Groq отклонил ключ. Проверьте его в настройках.', 'Groq rejected the key. Check it in settings.'],
  rate_limited: ['Достигнут лимит Groq. Попробуйте позже.', 'Groq rate limit reached. Try again later.'],
  provider_unavailable: ['Не удалось подключиться к Groq или выбрать доступную модель.', 'Could not reach Groq or select a supported model.'],
  invalid_response: ['Не удалось обработать ответ Groq.', 'Could not read the Groq response.'],
  action_limit: ['Превышен лимит действий за запрос.', 'This request exceeded the action limit.'],
  action_failed: ['Проверьте результаты отдельных действий.', 'Review the individual action results.'],
  action_uncertain: ['Проверьте результат в Windows перед повтором команды.', 'Check the result in Windows before trying again.'],
  audio_unavailable: ['Микрофон недоступен. Проверьте разрешения Windows.', 'Microphone unavailable. Check Windows permissions.'],
  audio_lost: ['Микрофон отключился во время записи.', 'The microphone disconnected during recording.'],
  audio_overflow: ['Захват не успевал за аудиоустройством. Попробуйте ещё раз.', 'Capture could not keep up with the audio device. Try again.'],
  audio_stalled: ['Ожидаю освобождения аудиоустройства.', 'Waiting for the audio device to release.'],
  empty_capture: ['Не удалось услышать речь. Попробуйте снова.', 'No speech was heard. Try again.'],
  speech_unavailable: ['Для этого языка недоступен локальный голос Windows.', 'A local Windows voice for this language is unavailable.'],
  storage: ['Не удалось открыть или обновить локальный профиль.', 'Could not read or update the local profile.'],
  shortcut_unavailable: ['Горячая клавиша занята или недоступна.', 'The shortcut is in use or unavailable.'],
  update_unavailable: ['Подписанное обновление недоступно.', 'A signed update is unavailable.'],
  update_failed: ['Проверка или установка обновления не удалась.', 'Could not check or install the update.'],
};

export const phaseLabel = (locale: Locale, phase: Phase) => phases[locale][phase];

export function sessionLabel(locale: Locale, session: Session) {
  if (session === 'unlocked') return translations[locale].available;
  if (session === 'locked') return locale === 'ru' ? 'Windows заблокирована' : 'Windows is locked';
  if (session === 'suspended') return locale === 'ru' ? 'Сеанс приостановлен' : 'Session suspended';
  return locale === 'ru' ? 'Проверяю сеанс Windows' : 'Checking the Windows session';
}

export function failure(locale: Locale, value: unknown) {
  const code = typeof value === 'string' ? value : '';
  const pair = errors[code];
  return pair?.[locale === 'ru' ? 0 : 1] ??
    (locale === 'ru' ? 'Не удалось выполнить операцию.' : 'The operation could not be completed.');
}

export function actionLabel(locale: Locale, action: Action) {
  if (action.kind === 'output_volume') return locale === 'ru' ? 'Громкость' : 'Volume';
  if (action.app === 'browser') return locale === 'ru' ? 'Браузер' : 'Browser';
  return locale === 'ru' ? 'Калькулятор' : 'Calculator';
}

export function resultLabel(locale: Locale, receipt: Receipt) {
  const result = receipt.outcome;
  const ru = locale === 'ru';
  switch (result.status) {
    case 'accepted_by_os': return ru ? 'Запуск принят Windows' : 'Launch accepted by Windows';
    case 'volume_applied': return result.muted
      ? (ru ? `Звук выключен · ${result.volume}%` : `Muted · ${result.volume}%`)
      : `${result.volume}%`;
    case 'indeterminate': return ru ? 'Результат не подтверждён' : 'Result unconfirmed';
    case 'blocked': return ru ? 'Действие остановлено' : 'Action blocked';
    case 'failed': return ru ? 'Действие не выполнено' : 'Action failed';
  }
}
