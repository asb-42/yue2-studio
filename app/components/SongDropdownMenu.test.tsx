import React, { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { SongDropdownMenu } from './SongDropdownMenu';
import type { Song } from '../types';

vi.mock('../context/I18nContext', () => ({ useI18n: () => ({ t: (key: string) => key, language: 'en' }) }));
vi.mock('../context/AuthContext', () => ({ useAuth: () => ({ user: { id: 'u' } }) }));
vi.mock('../context/SongActionsContext', () => ({
  useSongActions: () => ({ update: () => undefined }),
  ownsSong: () => true,
}));
vi.mock('../services/studioQueries', () => ({ useKaraokeStatus: () => ({ data: null }) }));
vi.mock('./SongMetadataDialog', () => ({
  SongMetadataDialog: () => (
    <div role="dialog">
      <input aria-label="title" defaultValue="x" />
    </div>
  ),
}));

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const song = { id: 's1', title: 'T', audioUrl: '/a.mp3', lyrics: '[Verse]\nx', style: 'pop' } as Song;

let root: Root;
let host: HTMLDivElement;
function mount(isOpen: boolean, onClose: () => void) {
  host = document.createElement('div');
  document.body.append(host);
  root = createRoot(host);
  act(() => root.render(<SongDropdownMenu song={song} isOpen={isOpen} onClose={onClose} position="right" direction="down" />));
}
function rerender(isOpen: boolean, onClose: () => void) {
  act(() => root.render(<SongDropdownMenu song={song} isOpen={isOpen} onClose={onClose} position="right" direction="down" />));
}
function clickEditMetadata() {
  const buttons = [...host.querySelectorAll('button')];
  const edit = buttons.find((button) => button.textContent === 'songMetaEdit');
  if (!edit) throw new Error(`no Edit metadata button among: ${buttons.map((b) => b.textContent).join(' | ')}`);
  act(() => (edit as HTMLButtonElement).click());
}
afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  vi.restoreAllMocks();
});

describe('song menu metadata dialog', () => {
  it('opens on the first click and survives the menu closing', () => {
    let open = true;
    const onClose = () => {
      open = false;
      rerender(open, onClose);
    };
    mount(open, onClose);
    expect(host.querySelector('[role="dialog"]')).toBeNull();
    clickEditMetadata();
    // the menu closed itself, like handleAction does; the dialog stays
    expect(open).toBe(false);
    expect(host.querySelector('[role="dialog"]')).not.toBeNull();
  });

  it('clicks inside the dialog do not close it', () => {
    let open = true;
    const onClose = () => {
      open = false;
      rerender(open, onClose);
    };
    mount(open, onClose);
    clickEditMetadata();
    const field = host.querySelector('[role="dialog"] input') as HTMLInputElement;
    expect(field).not.toBeNull();
    act(() => {
      field.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
      field.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      field.focus();
    });
    expect(host.querySelector('[role="dialog"]')).not.toBeNull();
  });
});
