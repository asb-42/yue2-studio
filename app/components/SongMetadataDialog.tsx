import React, { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import { Loader2, X } from 'lucide-react';
import { useI18n } from '../context/I18nContext';
import { updateNativeSong } from '../services/nativeLibrary';
import type { Song } from '../types';

/**
 * View and edit a library track's metadata: title, artist, style tags and
 * lyrics are editable (the file is retagged on save); everything the studio
 * recorded around them - seeds, engine, provenance, dates - is shown
 * read-only. Generation settings stay untouched so exact replay keeps
 * working.
 */

interface RawSong {
  title: string;
  caption: string;
  lyrics: string;
  metadata: Record<string, unknown>;
  audio_path?: string | null;
  source: string;
  engine_id: string;
  profile_id?: string | null;
  created_at: string;
  updated_at: string;
}

const str = (value: unknown): string => (typeof value === 'string' ? value : '');

const dateOf = (stamp: string): string => {
  const seconds = Number(stamp);
  if (!Number.isFinite(seconds) || seconds <= 0) return stamp;
  try {
    return new Date(seconds * 1000).toLocaleString();
  } catch {
    return stamp;
  }
};

export const SongMetadataDialog: React.FC<{ song: Song; onClose: () => void; onSaved: (song: Song) => void }> = ({ song, onClose, onSaved }) => {
  const { t } = useI18n();
  const [raw, setRaw] = useState<RawSong | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [title, setTitle] = useState(song.title);
  const [artist, setArtist] = useState('');
  const [style, setStyle] = useState(song.style);
  const [lyrics, setLyrics] = useState(song.lyrics);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetch(`/v1/library/songs/${encodeURIComponent(song.id)}`)
      .then(async (response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return (await response.json()) as RawSong;
      })
      .then((body) => {
        if (cancelled) return;
        setRaw(body);
        setTitle(body.title);
        setArtist(str((body.metadata as Record<string, unknown>)?.artist));
        setStyle(body.caption);
        setLyrics(body.lyrics);
      })
      .catch((problem) => {
        if (!cancelled) setFailed(problem instanceof Error ? problem.message : String(problem));
      });
    return () => {
      cancelled = true;
    };
  }, [song.id]);

  const save = async () => {
    if (!raw || busy) return;
    if (!title.trim()) {
      setError(t('songMetaTitleRequired'));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const metadata = Object.fromEntries(Object.entries((raw.metadata ?? {}) as Record<string, unknown>).filter(([, value]) => value !== null));
      if (artist.trim()) metadata.artist = artist.trim();
      else metadata.artist = null;
      const updated = await updateNativeSong(song, { title: title.trim(), caption: style, lyrics, metadata });
      onSaved(updated);
      onClose();
    } catch (problem) {
      setError(problem instanceof Error ? problem.message : String(problem));
    } finally {
      setBusy(false);
    }
  };

  const meta = (raw?.metadata ?? {}) as Record<string, unknown>;
  const derived = meta.derived as { from_title?: string; tool?: string } | null | undefined;
  const audioFile = typeof raw?.audio_path === 'string' ? raw.audio_path.split('/').pop() : null;
  const rows: [string, string][] = raw
    ? [
        [t('metaDuration'), song.duration || str(meta.duration_seconds)],
        [t('metaSource'), raw.source],
        [t('metaEngine'), raw.engine_id],
        [t('metaSeeds'), [meta.lm_seed, meta.seed].filter((seed) => seed !== undefined && seed !== null).join(' / ') || '—'],
        [t('metaCot'), str(meta.cot) || '—'],
        [t('metaFile'), audioFile || '—'],
        [t('metaDerived'), derived?.from_title ? `${derived.from_title}${derived.tool ? ` · ${derived.tool}` : ''}` : '—'],
        [t('metaKaraoke'), meta.lrc ? t('karaokeReady') : '—'],
        [t('metaCreated'), dateOf(raw.created_at)],
        [t('metaUpdated'), dateOf(raw.updated_at)],
      ]
    : [];
  const input = 'w-full rounded-lg border border-zinc-300 bg-white px-3 py-2 text-sm text-zinc-900 dark:border-white/15 dark:bg-black/30 dark:text-white';

  return createPortal(
    // Clicks bubble through the React tree, not the DOM tree: without this
    // stop they reach the song row this menu hangs off and start playback.
    <div className="fixed inset-0 z-70 flex items-center justify-center bg-black/60 p-2 sm:p-4" onClick={(event) => event.stopPropagation()} onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div role="dialog" aria-modal="true" aria-label={t('songMetaTitle')} className="flex max-h-[92vh] w-full max-w-xl flex-col overflow-hidden rounded-2xl bg-white shadow-2xl dark:bg-zinc-900">
        <div className="flex items-center gap-2 border-b border-zinc-200 px-4 py-3 dark:border-white/10 sm:px-5">
          <h3 className="min-w-0 flex-1 truncate text-base font-bold text-zinc-900 dark:text-white">{t('songMetaTitle')}</h3>
          <button type="button" onClick={onClose} aria-label={t('metaClose')} title={t('metaClose')} className="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200">
            <X size={18} />
          </button>
        </div>
        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4 sm:px-5">
          {failed ? (
            <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{failed}</p>
          ) : !raw ? (
            <p className="flex items-center gap-2 text-sm text-zinc-500"><Loader2 size={15} className="animate-spin text-pink-500" />{t('metaLoading')}</p>
          ) : (
            <>
              <label className="block">
                <span className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500">{t('songMetaTitleField')}</span>
                <input value={title} onChange={(event) => setTitle(event.target.value)} className={input} maxLength={200} />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500">{t('songMetaArtist')}</span>
                <input value={artist} onChange={(event) => setArtist(event.target.value)} className={input} maxLength={200} placeholder={t('songMetaArtistHint')} />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500">{t('songMetaStyle')}</span>
                <textarea value={style} onChange={(event) => setStyle(event.target.value)} rows={3} className={input} />
              </label>
              <label className="block">
                <span className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500">{t('songMetaLyrics')}</span>
                <textarea value={lyrics} onChange={(event) => setLyrics(event.target.value)} rows={8} className={`${input} font-mono text-[13px]`} />
              </label>
              <div>
                <span className="mb-1 block text-[11px] font-semibold uppercase tracking-wide text-zinc-500">{t('songMetaRecorded')}</span>
                <dl className="overflow-hidden rounded-lg border border-zinc-200 text-[13px] dark:border-white/10">
                  {rows.map(([label, value]) => (
                    <div key={label} className="flex gap-3 border-b border-zinc-100 px-3 py-1.5 last:border-0 dark:border-white/5">
                      <dt className="w-24 shrink-0 text-zinc-500">{label}</dt>
                      <dd className="min-w-0 flex-1 break-words text-zinc-800 dark:text-zinc-100">{value}</dd>
                    </div>
                  ))}
                </dl>
              </div>
            </>
          )}
          {error && <p role="alert" className="text-[13px] text-rose-600 dark:text-rose-300">{error}</p>}
        </div>
        <div className="flex justify-end gap-2 border-t border-zinc-200 px-4 py-3 dark:border-white/10 sm:px-5">
          <button type="button" onClick={onClose} className="rounded-lg border border-zinc-300 px-4 py-2 text-sm text-zinc-600 dark:border-white/15 dark:text-zinc-300">
            {t('metaCancel')}
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={!raw || busy}
            className="rounded-lg bg-pink-600 px-4 py-2 text-sm font-bold text-white hover:brightness-110 disabled:opacity-50"
          >
            {busy ? t('metaSaving') : t('metaSave')}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
};
