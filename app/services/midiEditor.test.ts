import { afterEach, describe, expect, it } from 'vitest';
import { audioWorkletsAvailable } from './midiEditor';

describe('whether the embedded piano-roll editor can boot its synth', () => {
  const real = (window as unknown as Record<string, unknown>).AudioContext;
  afterEach(() => {
    (window as unknown as Record<string, unknown>).AudioContext = real;
  });

  it('is false with no audio at all', () => {
    (window as unknown as Record<string, unknown>).AudioContext = undefined;
    expect(audioWorkletsAvailable()).toBe(false);
  });

  it('is false when the context exposes no worklets (plain LAN http)', () => {
    (window as unknown as Record<string, unknown>).AudioContext = class {};
    expect(audioWorkletsAvailable()).toBe(false);
  });

  it('is true when worklets are exposed (localhost, HTTPS)', () => {
    class FakeContext {}
    (FakeContext as unknown as Record<string, unknown>).prototype ??= {};
    Object.defineProperty(FakeContext.prototype, 'audioWorklet', { value: {}, configurable: true });
    (window as unknown as Record<string, unknown>).AudioContext = FakeContext;
    expect(audioWorkletsAvailable()).toBe(true);
  });
});
