import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { chromium } from 'playwright-core';

const dist = fileURLToPath(new URL('../dist/', import.meta.url));

test('the packaged panel runs offline actions, honours privacy and controls capture', async (t) => {
  const server = createServer(async (request, response) => {
    const pathname = new URL(request.url, 'http://localhost').pathname;
    const name = normalize(pathname === '/' ? 'index.html' : pathname.slice(1));
    if (name.startsWith('..')) {
      response.writeHead(400).end();
      return;
    }
    try {
      const bytes = await readFile(join(dist, name));
      const type = name.endsWith('.js') ? 'text/javascript' : name.endsWith('.css') ? 'text/css' : 'text/html';
      response.writeHead(200, { 'content-type': `${type}; charset=utf-8` }).end(bytes);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => server.close());
  const executablePath = process.env.FRIDAY_TEST_BROWSER || (existsSync('/usr/bin/chromium') ? '/usr/bin/chromium' : undefined);
  const browser = await chromium.launch({ executablePath, args: ['--no-sandbox'] });
  t.after(() => browser.close());
  const page = await browser.newPage({ viewport: { width: 960, height: 720 }, acceptDownloads: true });
  const errors = [];
  page.setDefaultTimeout(7000);
  page.on('pageerror', error => errors.push(error.message));

  await page.addInitScript(() => {
    let settings = { locale: 'ru', cloud_enabled: false, history_enabled: false, speak_answers: false, shortcut: 'Ctrl+Shift+Space' };
    let snapshot = {
      revision: 0, session: 'unlocked', turn_id: null, phase: 'idle', transcript: null, answer: null,
      receipts: [], failure: null, speech_warning: null,
    };
    let keyPresent = false;
    const commands = [];
    const publish = (fields) => { snapshot = { ...snapshot, ...fields, revision: snapshot.revision + 1 }; };
    const boot = () => ({
      protocol: 1, version: '0.1.0', snapshot, settings, key_present: keyPresent, key_warning: null,
      shortcut_registered: true, updates_configured: false,
    });
    window.__fridayTest = {
      commands,
      lock: () => publish({ session: 'locked', receipts: [], transcript: null, answer: null, phase: 'cancelled' }),
    };
    window.isTauri = true;
    window.__TAURI_INTERNALS__ = {
      invoke: async (name, args) => {
        if (name !== 'observe_state') commands.push(name);
        switch (name) {
          case 'bootstrap': return boot();
          case 'observe_state':
            if (args.since === snapshot.revision) await new Promise(resolve => setTimeout(resolve, 25));
            return snapshot;
          case 'submit':
            if (args.intent.kind === 'start_capture') {
              publish({ turn_id: 'voice', phase: 'preparing_audio', receipts: [], transcript: null });
              setTimeout(() => publish({ phase: 'capturing' }), 40);
            } else {
              publish({
                turn_id: 'keyboard', phase: 'completed', transcript: null,
                receipts: [
                  { id: '1', action: { kind: 'open_app', app: 'browser' }, outcome: { status: 'accepted_by_os' } },
                  { id: '2', action: { kind: 'output_volume' }, outcome: { status: 'volume_applied', volume: 30, muted: false } },
                ],
              });
            }
            return { turn_id: snapshot.turn_id };
          case 'finish_capture':
            publish({ phase: 'transcribing' });
            return;
          case 'cancel_turn':
            publish({ phase: 'cancelled' });
            return;
          case 'update_settings':
            settings = args.settings;
            return boot();
          case 'set_groq_key':
            if (args.key !== 'gsk_example_only') throw 'invalid_input';
            keyPresent = true;
            return;
          case 'load_history':
            return [{ turn_id: 'archived', created_at: 1760000000, input: 'Сохранённый запрос', answer: null, receipts: [] }];
          case 'diagnostics':
            return { app_version: '0.1.0', protocol: 1, session: 'unlocked', unresolved_actions: 0 };
          default: throw new Error(`unexpected IPC call ${name}`);
        }
      },
    };
  });

  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.getByRole('status').getByText('Готова', { exact: true }).waitFor();
  const composer = page.getByRole('textbox', { name: 'Напишите команду или задайте вопрос' });
  await composer.fill('Открой браузер и сделай тише');
  await composer.press('Enter');
  await page.getByText('Запуск принят Windows').waitFor();
  assert.equal(await page.getByText('30%', { exact: true }).count(), 1);
  assert.equal(await page.getByRole('button', { name: 'Говорить', exact: true }).isDisabled(), true);
  if (process.env.FRIDAY_UI_SCREENSHOT) {
    await page.screenshot({ path: process.env.FRIDAY_UI_SCREENSHOT });
  }

  await page.getByRole('button', { name: 'Настройки', exact: true }).click();
  await page.getByLabel('Разрешить обработку в Groq').check();
  await page.getByRole('button', { name: 'Сохранить', exact: true }).click();
  await page.getByText('Настройки сохранены').waitFor();
  await page.getByLabel('Ключ Groq API').fill('gsk_example_only');
  await page.getByRole('button', { name: 'Сохранить ключ' }).click();
  await page.getByText('Ключ сохранён в Windows Credential Manager.').waitFor();
  assert.equal(await page.getByLabel('Ключ Groq API').inputValue(), '');

  await page.getByRole('button', { name: 'Ассистент', exact: true }).click();
  await page.getByRole('button', { name: 'Говорить', exact: true }).click();
  await page.getByRole('status').getByText('Слушаю', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Завершить запись' }).click();
  await page.getByRole('status').getByText('Распознаю речь', { exact: true }).waitFor();
  await page.getByRole('button', { name: 'Стоп', exact: true }).click();
  await page.getByRole('status').getByText('Остановлено', { exact: true }).waitFor();

  await page.getByRole('button', { name: 'История', exact: true }).click();
  await page.getByText('Сохранённый запрос').waitFor();
  await page.evaluate(() => window.__fridayTest.lock());
  await page.getByText('Разблокируйте Windows, чтобы продолжить.').waitFor();
  assert.equal(await page.getByText('Сохранённый запрос').count(), 0);
  assert.deepEqual(errors, []);
});
