# Friday Desktop

Friday — настольный ассистент на Rust и Tauri для Windows 11 x64. Он принимает команды с клавиатуры и по микрофону, открывает браузер и калькулятор, управляет громкостью и отвечает на вопросы через Groq при включённой облачной обработке.

## Использование

Локальные команды работают сразу: «открой браузер и сделай тише», «открой калькулятор», «установи громкость на 35 процентов» или «выключи звук».

Голос и свободный диалог доступны после включения Groq и сохранения своего API-ключа в настройках. Нажмите «Говорить» или `Ctrl+Shift+Space` для записи и нажмите ещё раз, чтобы завершить её. Кнопка «Стоп» отменяет текущую обработку. Короткий результат действия сообщает, что подтвердила Windows. Если подтверждение потеряно, приложение показывает неопределённый результат и не повторяет действие само.

Ключ хранится в Windows Credential Manager. История выключена по умолчанию. Аудио передаётся в Groq только в голосовом режиме после завершения захвата. Для озвучки требуется установленный голос Windows выбранного языка.

## Устройство проекта

- `friday-core` разбирает локальные команды и задаёт типы действий и результатов.
- `friday-engine` управляет жизненным циклом запроса, отменой и последовательным выполнением действий. Квитанция сохраняется в SQLite до вызова Windows и уточняется после него.
- `friday-adapters` связывает движок с Windows, SQLite и Groq. Движок и хранилище можно проверять на Linux.
- `apps/desktop` содержит панель Svelte и оболочку Tauri для Windows. Панель вызывает команды через типизированный IPC.

## Сборка

На Windows нужны Rust из `rust-toolchain.toml`, MSVC Build Tools с Windows SDK, Node 26.13.1 и WebView2 Evergreen. Из `apps/desktop`:

```powershell
npm ci
npm run tauri -- dev
npm run tauri -- build --bundles nsis --target x86_64-pc-windows-msvc -- --locked
```

Установщик без подписи появится в `target/x86_64-pc-windows-msvc/release/bundle/nsis/`. Для подписи и канала обновлений используется отдельный workflow.

Минимальная версия Rust — 1.95.0. Сборки используют зафиксированные зависимости из `Cargo.lock` и `package-lock.json`. Изменения lock-файлов проверяются в CI по целевым графам Windows и Linux и по базе npm advisories.

## Проверки

```text
cargo fmt --all --check
cargo test --locked -p friday-core -p friday-engine -p friday-adapters
cargo clippy --locked -p friday-core -p friday-engine -p friday-adapters --all-targets -- -D warnings
```

Проверки панели из `apps/desktop`: `npm run check`, `npm test`, `npm run build` и `npm run test:ui`. Для browser smoke используйте установленный Chromium через `FRIDAY_TEST_BROWSER` или браузер Playwright из `npx playwright-core install chromium`.

[Выпуск, диагностика и проверка на устройстве](docs/release.md)
