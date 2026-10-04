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

globalThis.IS_REACT_ACT_ENVIRONMENT = true;

const rawSong = {
  title: 'T',
  caption: 'pop',
  lyrics: '[Verse]\nx',
  metadata: {},
  audio_path: null,
  source: 'local_generation',
  engine_id: 'e',
  created_at: '1',
  updated_at: '2',
};

function stubFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({ ok: true, json: async () => rawSong })),
  );
}

async function settle() {
  await act(async () => {});
}

async function fieldOfDialog(): Promise<HTMLInputElement | null> {
  for (let attempt = 0; attempt < 20; attempt++) {
    await settle();
    const field = document.querySelector('[role="dialog"] input') as HTMLInputElement | null;
    if (field) return field;
  }
  return null;
}

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
  vi.unstubAllGlobals();
});

describe('song menu metadata dialog', () => {
  it('opens on the first click and survives the menu closing', async () => {
    stubFetch();
    let open = true;
    const onClose = () => {
      open = false;
      rerender(open, onClose);
    };
    mount(open, onClose);
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    clickEditMetadata();
    // the menu closed itself, like handleAction does; the dialog stays
    expect(open).toBe(false);
    expect(document.querySelector('[role="dialog"]')).not.toBeNull();
  });

  it('clicks inside the dialog do not close it', async () => {
    stubFetch();
    let open = true;
    const onClose = () => {
      open = false;
      rerender(open, onClose);
    };
    mount(open, onClose);
    clickEditMetadata();
    const field = await fieldOfDialog();
    expect(field).not.toBeNull();
    act(() => {
      field!.dispatchEvent(new MouseEvent('mousedown', { bubbles: true }));
      field!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      field!.focus();
    });
    expect(document.querySelector('[role="dialog"]')).not.toBeNull();
  });

  it('clicks inside the dialog do not reach the song row behind it', async () => {
    stubFetch();
    let plays = 0;
    function RowHarness() {
      const [open, setOpen] = React.useState(true);
      return (
        <div onClick={() => { plays += 1; }}>
          <SongDropdownMenu song={song} isOpen={open} onClose={() => setOpen(false)} position="right" direction="down" />
        </div>
      );
    }
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    // the library row: any bubbled click starts playback
    act(() => {
      root.render(<RowHarness />);
    });
    clickEditMetadata();
    const field = await fieldOfDialog();
    expect(field).not.toBeNull();
    act(() => (field as HTMLInputElement).click());
    expect(plays).toBe(0);
    expect(document.querySelector('[role="dialog"]')).not.toBeNull();
  });
});
