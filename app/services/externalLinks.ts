/**
 * Links that leave the studio.
 *
 * The fork has no desktop shell: every external link behaves as a browser
 * link always did - a new tab through the system browser.
 *
 * One listener on the document covers every link in the interface, including
 * the ones inside news items, so nothing has to remember to be special.
 */

import { API_BASE } from './apiBase';

/** Opens one URL in a new tab. */
export async function openExternal(url: string): Promise<void> {
  window.open(url, '_blank', 'noopener');
}

/**
 * True when the studio service runs on this computer: a loopback API base
 * or a same-origin page on localhost. Only then do "show in folder / play
 * in VLC" make sense — anywhere else they would act on the studio's
 * computer, not the viewer's.
 */
export function isLocalService(): boolean {
  if (typeof location === 'undefined') return false;
  let host: string;
  try {
    host = API_BASE ? new URL(API_BASE).hostname : location.hostname;
  } catch {
    return false;
  }
  return host === '' || host === 'localhost' || host === '127.0.0.1' || host === '::1' || host === '[::1]';
}

/** Sends every external link click to the system browser. Call once. */
export function installExternalLinkHandler(): void {
  // Capture, not bubble: dialogs stop propagation on their own container to
  // keep a click inside from closing them, and that also hid every link in
  // Settings and the news panel from a listener on the document.
  document.addEventListener('click', (event) => {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey) return;
    const anchor = (event.target as HTMLElement | null)?.closest?.('a');
    const href = anchor?.getAttribute('href');
    if (!href || !/^https?:\/\//i.test(href)) return;
    event.preventDefault();
    void openExternal(href);
  }, true);
}
