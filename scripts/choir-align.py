#!/usr/bin/env python3
"""Choir alignment Phase 1: anchor extraction (DEV-ONLY, never called by the
service, NOT part of the runtime path — see AGENTS.md: no Python in the
runtime path). Implements docs/plans/2026-10-10_choir.md Phase 1.

What it does
------------
For each take of the take set (the four SATB solo renders from
docs/plans/2026-10-05_ensemble.md Phase 2):
1. ensures the six stems exist (POST /v1/library/songs/{id}/stems, waits),
2. transcribes the *vocals stem* — never the full mix, which transcribes as
   chords (choir plan §3, trap 1) — via POST /v1/midi/transcribe {path},
3. writes a manifest of note lists: workdir/choir-manifest.json.

The manifest is the input to Phase 2 (correspondence and residuals, no audio
touched), which lives in crates/music-server/src/align.rs and its tests.

Usage
-----
    scripts/choir-align.py [--base http://127.0.0.1:8791] [--size medium]

The take set is pinned below with the song IDs from the ensemble plan, so a
re-run measures the same renders.
"""

from __future__ import annotations

import argparse
import glob
import json
import re
import sys
import time
import urllib.request

# The four solo renders, ensemble plan Phase 2 (2026-10-07). Same seeds,
# same ABCs, same playlist — re-running this script must measure these.
TAKES = [
    {"part": "S", "song_id": "01a11751-f714-773f-b859-fbe672172be1"},
    {"part": "A", "song_id": "01a11762-eb23-708a-afc5-071c2e2447f9"},
    {"part": "T", "song_id": "01a11766-1fc9-702c-8ed0-7d11701be54f"},
    {"part": "B", "song_id": "01a11769-9caf-72c2-be25-4683b8edf7a5"},
]

# Sections of docs/plans/ensemble-prototype/satb-example.json: 4 bars verse,
# 4 bars chorus, 4/4 at 90 BPM. Fitted separately (choir plan §4).
SECTIONS = [
    {"label": "verse", "bar_from": 0, "bar_to": 4},
    {"label": "chorus", "bar_from": 4, "bar_to": 8},
]


def call(base: str, method: str, path: str, body=None, timeout=60):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(base + path, data=data, method=method,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode() or "null")


def wait_stems(base: str, song_id: str) -> list[str]:
    call(base, "POST", f"/v1/library/songs/{song_id}/stems", {})
    for _ in range(120):
        state = call(base, "GET", f"/v1/library/songs/{song_id}/stems")
        run = state.get("run") or {}
        if run.get("done"):
            if run.get("error"):
                raise SystemExit(f"separation failed for {song_id}: {run['error']}")
            return state.get("stems") or []
        time.sleep(15)
    raise SystemExit(f"separation timed out for {song_id}")


def stem_seconds(path: str) -> float:
    """Length of a WAV file in seconds, from its own header."""
    with open(path, "rb") as handle:
        raw = handle.read()
    pos = 12
    while pos + 8 <= len(raw):
        cid = raw[pos:pos + 4]
        size = int.from_bytes(raw[pos + 4:pos + 8], "little")
        if cid == b"fmt ":
            rate = int.from_bytes(raw[pos + 12:pos + 16], "little")
            channels = int.from_bytes(raw[pos + 10:pos + 12], "little")
            bits = int.from_bytes(raw[pos + 22:pos + 24], "little")
        if cid == b"data":
            return size / (channels * bits // 8) / rate
        pos += 8 + size + (size & 1)
    raise SystemExit(f"no audio length in {path}")


def transcribe_path(base: str, path: str, size: str) -> list[dict]:
    try:
        call(base, "POST", "/v1/midi/transcribe",
             {"path": path, "size": size}, timeout=120)
    except Exception as exc:
        if "already being turned into MIDI" not in str(exc):
            raise
    for _ in range(120):
        time.sleep(15)
        status = call(base, "GET", "/v1/midi")
        run = status.get("run") or {}
        if run.get("done"):
            if run.get("error"):
                raise SystemExit(f"transcription failed for {path}: {run['error']}")
            notes = call(base, "GET", "/v1/midi/notes").get("notes") or []
            if not notes:
                raise SystemExit(f"transcription done but no notes for {path}")
            return notes
    raise SystemExit(f"transcription timed out for {path}")


def chorus_entry(base: str, song_id: str, song: dict) -> tuple[float, str]:
    """The take's observed section split: when its chorus starts.

    Read off the word clocks (the take's LRC): the first word of the [Chorus]
    lyric section, timed where it is sung. Without this the assignment lets a
    late chorus entry match its bars to verse-tail onsets, because hymn
    sections share pitch material and the time cost saturates.
    """
    lyrics = song.get("lyrics") or ""
    match = re.search(r"\[Chorus\]\s*\n(\S+)", lyrics)
    if not match:
        raise SystemExit(f"no [Chorus] section in the lyrics of {song_id}")
    word = match.group(1)
    lrc = (song.get("metadata") or {}).get("lrc") or ""
    timed = re.search(r"\[(\d+):(\d+\.\d+)\]<\d+:[\d.]+>" + re.escape(word), lrc)
    if not timed:
        raise SystemExit(f"the chorus word {word!r} has no time in the LRC of {song_id}")
    return int(timed.group(1)) * 60 + float(timed.group(2)), word


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Choir alignment Phase 1 (dev-only).")
    parser.add_argument("--base", default="http://127.0.0.1:8791")
    parser.add_argument("--size", default="medium")
    parser.add_argument("--out", default="workdir/choir-manifest.json")
    args = parser.parse_args(argv)

    manifest = {"plan": "docs/plans/2026-10-10_choir.md",
                "phase": 1,
                "sections": SECTIONS,
                "takes": []}
    for take in TAKES:
        song_id = take["song_id"]
        print(f"[{take['part']}] stems for {song_id} ...", flush=True)
        stems = wait_stems(args.base, song_id)
        if "vocals" not in stems:
            raise SystemExit(f"no vocals stem for {song_id}: {stems}")
        song = call(args.base, "GET", f"/v1/library/songs/{song_id}")
        entry, word = chorus_entry(args.base, song_id, song)
        print(f"[{take['part']}] chorus entry at {entry:.2f}s ({word!r} in LRC) ...", flush=True)
        state = call(args.base, "GET", f"/v1/library/songs/{song_id}/stems")
        library = {str(entry.get("stem")): entry.get("song_id")
                   for entry in (state.get("library_songs") or [])}
        print(f"[{take['part']}] transcribing the vocals stem ...", flush=True)
        # transcribe the stem file on disk, not the library song: the song's
        # own MIDI would be the full mix again
        candidates = glob.glob(f"workdir/media/{song_id}-vocals.wav")
        if not candidates:
            raise SystemExit(f"vocals stem file missing for {song_id}")
        notes = transcribe_path(args.base, candidates[0], args.size)
        # Notes starting past the end of the stem are transcriber
        # hallucinations, not anchors: MuScriptor sang 3.2 s past the end of
        # one 21.5 s stem (60 phantom notes), and Parakeet timed a word past
        # it too. A warp fitted to phantoms is a warp to nothing.
        length = stem_seconds(candidates[0])
        phantoms = [note for note in notes if note["start"] > length]
        notes = [note for note in notes if note["start"] <= length]
        if not notes:
            raise SystemExit(f"no notes inside the audio for {song_id}")
        onsets = len({round(note["start"], 2) for note in notes})
        print(f"[{take['part']}] {len(notes)} notes ({len(phantoms)} past-end phantoms dropped), ~{onsets} distinct onsets", flush=True)
        manifest["takes"].append({"part": take["part"], "song_id": song_id,
                                  "vocals_stem": candidates[0],
                                  "stem_seconds": round(length, 3),
                                  "dropped_past_end": len(phantoms),
                                  "library_stems": library,
                                  "transcription_size": args.size,
                                  "anchors": [entry],
                                  "anchor_words": [f"{word} (first chorus word in LRC)"],
                                  "notes": notes})
    with open(args.out, "w", encoding="utf-8") as handle:
        json.dump(manifest, handle, ensure_ascii=False, indent=1)
    print(f"wrote {args.out}: {sum(len(t['notes']) for t in manifest['takes'])} notes total")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
