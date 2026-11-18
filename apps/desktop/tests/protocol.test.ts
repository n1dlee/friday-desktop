import assert from 'node:assert/strict';
import test from 'node:test';
import { acceptedSnapshot, initialSnapshot, isActive, validInput } from '../src/protocol.ts';

test('applies snapshots monotonically and keeps cancellation active until acknowledged', () => {
  const previous = { ...initialSnapshot, revision: 6, phase: 'cancelling' as const };
  const late = { ...initialSnapshot, revision: 5, phase: 'capturing' as const };
  assert.equal(acceptedSnapshot(previous, late), previous);
  assert.equal(isActive(previous.phase), true);
  assert.equal(isActive('cancelled'), false);
});

test('measures commands in UTF-8 bytes', () => {
  assert.equal(validInput(''), false);
  assert.equal(validInput('open\0browser'), false);
  assert.equal(validInput('я'.repeat(4096)), true);
  assert.equal(validInput('я'.repeat(4097)), false);
});
