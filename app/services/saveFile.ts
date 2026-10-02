import { apiUrl } from './apiBase';

/**
 * "Save as" for anything the window hands the user.
 *
 * The fork has no desktop shell with a native Save dialog, so the browser
 * always downloads the file itself. (The service keeps its own choose/write
 * flow for agents over MCP.)
 */

export type SaveSource = { url: string } | { blob: Blob };

export interface SavingFile {
  id: string;
  name: string;
  path?: string;
  state: 'saving' | 'done' | 'error';
  written?: number;
  total?: number | null;
  error?: string;
}

const EVENT = 'studio:saving';

function tell(file: Partial<SavingFile> & { id: string }): void {
  window.dispatchEvent(new CustomEvent(EVENT, { detail: file }));
}

/** Every change to a save, from this window or the service's reports. */
export function onSaving(listener: (file: Partial<SavingFile> & { id: string }) => void): () => void {
  const handler = (event: Event) => listener((event as CustomEvent<Partial<SavingFile> & { id: string }>).detail);
  window.addEventListener(EVENT, handler);
  return () => window.removeEventListener(EVENT, handler);
}

async function answer(response: Response): Promise<Record<string, unknown>> {
  const body = await response.json().catch(() => null);
  if (!response.ok) throw new Error(body?.error || `HTTP ${response.status}`);
  return body ?? {};
}

/**
 * Asks where, then saves. A cancelled dialog saves nothing; anything that goes
 * wrong is shown in the files panel with its reason, so callers have nothing
 * to catch.
 */
export async function saveFile(name: string, source: SaveSource): Promise<void> {
  const link = document.createElement('a');
  link.href = 'url' in source ? apiUrl(source.url) : URL.createObjectURL(source.blob);
  link.download = name;
  document.body.appendChild(link);
  link.click();
  link.remove();
  const bytes = 'url' in source ? null : source.blob.size;
  if (!('url' in source)) window.setTimeout(() => URL.revokeObjectURL(link.href), 60_000);
  // the files panel keeps a history of downloads, like it kept saves
  tell({ id: `dl-${Date.now()}`, name, state: 'done', written: bytes ?? undefined, total: bytes });
}

/** A saved file, opened in the studio computer's native media player (VLC, mpv, ...). Returns the player it opened in. */
export async function playSaved(path: string): Promise<string> {
  const body = await answer(await fetch(apiUrl('/v1/files/play'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path }),
  }));
  return String(body.player ?? '');
}

/** The file manager, open on a saved file. */
export async function revealSaved(path: string): Promise<void> {
  await answer(await fetch(apiUrl('/v1/files/reveal'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ path }),
  }));
}
