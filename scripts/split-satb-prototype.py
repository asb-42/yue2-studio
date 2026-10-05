#!/usr/bin/env python3
"""SATB -> 4x native two-voice ABC splitter + render-manifest generator (PROTOTYPE).

Dev-only helper for the ensemble plan in docs/plans/2026-10-05_ensemble.md.
It is NEVER called by music-server and is NOT part of the runtime path
(see AGENTS.md: no Python in the runtime path). Run it by hand while
preparing a quartet production.

What it does
------------
Reads one SATB arrangement in a small JSON format (4 monophonic lines sharing
one header, one bar structure and one section layout) and emits four scores in
YuE2's native two-voice ABC dialect (``V: Vocal`` + ``V: Ins``), one per part,
plus a ``manifest.json`` describing the 4 (+1 instrumental) render jobs to run
through the existing ``song_create`` API / MCP tools. Each emitted part is
self-checked with a subset validator of the native dialect rules
(``crates/music-server/src/score/abc.rs``).

Usage
-----
    scripts/split-satb-prototype.py split --in INPUT.json --out-dir OUT \\
        --style-skeleton "English, warm chamber folk, {vocal}, soft piano and strings, ..." \\
        --vocal-s "airy soprano lead vocal" --vocal-a "warm alto vocal" \\
        --vocal-t "clear tenor vocal" --vocal-b "deep bass vocal" \\
        --instrumental-style "instrumental, warm chamber folk, soft piano and strings, 90 BPM"

    scripts/split-satb-prototype.py check part_S.abc [part_A.abc ...]

Input JSON schema (v1)
----------------------
{
  "header": {"M": "4/4", "L": 32, "bpm": 90, "K": "C"},
  "sections": [
    {"label": "verse",
     "bars": {"S": ["E8G8A8G8", ...], "A": [...], "T": [...], "B": [...]}},
    ...
  ],
  "chords": {"verse": ["C", "F", ...], "chorus": [...]},   // optional, one per bar
  "lyrics": {"S": "[Verse]\\n...\\n\\n[Chorus]\\n...", ...}  // optional, per part
}

Rules enforced (v1): every part has the same bar count in every section;
every section has 1+ bars; bar bodies use only the native subset (note/rest
tokens with durations from the native set, quoted chord symbols via the
"chords" map, whole-bar Z rests); no mid-score M:/K: changes, no tuplets,
grace notes, stacked chords, repeats or decorations. The final authority is
always the studio's own parser (score/abc::parse); this checker is a subset
and says so where it stops.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path

PARTS = ("S", "A", "T", "B")

DURATIONS = (1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48)

MAJOR_KEYS = ["Cb", "Gb", "Db", "Ab", "Eb", "Bb", "F", "C", "G", "D", "A",
              "E", "B", "F#", "C#"]
MINOR_KEYS = ["Abm", "Ebm", "Bbm", "Fm", "Cm", "Gm", "Dm", "Am", "Em", "Bm",
              "F#m", "C#m", "G#m", "D#m", "A#m"]

QUALITIES = ["", "m", "dim", "aug", "7", "maj7", "m7", "dim7", "m7b5",
             "sus4", "sus2", "6", "m6", "7sus4", "m(maj7)"]

VOICE_LINES = (
    'V: Vocal clef=treble name="Vocal Melody" snm="Vocal"',
    'V: Ins clef=treble name="Ins Melody" snm="Inst."',
)

WHOLE_RESTS = {"Z": 1, "Z2": 2, "Z3": 3, "Z4": 4}

TOKEN_RE = re.compile(
    r'"(?P<chord>[^"\n]*)"'
    r"|\[K:(?P<key>[^\]\n]+)\]"
    r"|(?P<acc>\^\^|__|\^|_|=)?(?P<note>[A-Ga-gz])(?P<oct>[,']*)"
    r"(?P<duration>[0-9]*)(?P<tie>-?)"
)
CHORD_RE_TMPL = r"^[A-G](?:bb|##|b|#)?(?:%s)(?:/[A-G](?:bb|##|b|#)?)?$"
CHORD_RE = re.compile(CHORD_RE_TMPL % "|".join(re.escape(q) for q in QUALITIES))

UNSUPPORTED_HINTS = ("(", ")", "{", "}", "!", "|:", ":|", "[", "]", "\\")


class SplitError(Exception):
    pass


def is_power_of_two(n: int) -> bool:
    return n > 0 and (n & (n - 1)) == 0


def check_header(header: dict) -> tuple[int, int, int, int, str]:
    try:
        meter = header["M"]
        l_den = int(header["L"])
        bpm = int(header["bpm"])
        key = header["K"]
    except (KeyError, TypeError, ValueError) as exc:
        raise SplitError(f"header needs M/L/bpm/K: {exc}")
    m = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", str(meter))
    if not m:
        raise SplitError(f"unsupported meter {meter!r}; write an explicit fraction")
    num, mden = int(m.group(1)), int(m.group(2))
    if mden > 1024 or not is_power_of_two(mden):
        raise SplitError(f"unsupported meter denominator {mden}")
    if l_den > 1024 or not is_power_of_two(l_den):
        raise SplitError(f"unsupported L: denominator {l_den}")
    if bpm <= 0:
        raise SplitError(f"bpm must be positive, got {bpm}")
    if key not in MAJOR_KEYS + MINOR_KEYS:
        raise SplitError(f"unsupported key {key!r}; use a standard major or minor K: field")
    return num, mden, l_den, bpm, key


def bar_units(num: int, mden: int, l_den: int) -> int:
    total, rem = divmod(num * l_den, mden)
    if rem:
        raise SplitError(
            f"meter {num}/{mden} with L:1/{l_den} is fractional "
            f"({num * l_den}/{mden} units); prototype v1 needs whole units"
        )
    return total


def check_bar_body(body: str, expect_units: int, where: str) -> None:
    """Subset of score/abc::parse_bar: whole rests or token sum == bar length."""
    text = body.strip()
    if not text:
        raise SplitError(f"{where}: empty measure")
    if text in WHOLE_RESTS:
        return
    if text.startswith("%") or text.startswith("V:") or text.startswith(("M:", "K:")):
        raise SplitError(f"{where}: field/comment lines are not bar bodies: {text!r}")
    for hint in UNSUPPORTED_HINTS:
        if hint in text and not (hint in "[]" and text.startswith('"')):
            # '[' / ']' only occur in the [K:] form, handled below as unsupported.
            if hint in "[]":
                continue
            raise SplitError(
                f"{where}: unsupported construct {hint!r} in {text!r}; "
                f"native subset has no tuplets, grace notes, decorations or repeats"
            )
    pos, offset = 0, 0
    n = len(text)
    while pos < n:
        if text[pos].isspace():
            pos += 1
            continue
        if text.startswith("[K:", pos):
            end = text.find("]", pos)
            key = text[pos + 3:end if end != -1 else n]
            raise SplitError(
                f"{where}: inline key change [K:{key}] unsupported in prototype v1 "
                f"(single global K: only)"
            )
        m = TOKEN_RE.match(text, pos)
        if not m:
            shown = text[pos:pos + 24]
            raise SplitError(f"{where}: unsupported token at {shown!r} in {text!r}")
        pos = m.end()
        if m.group("chord") is not None:
            chord = m.group("chord")
            if not CHORD_RE.match(chord):
                raise SplitError(f"{where}: unsupported chord {chord!r}")
            continue
        note = m.group("note")
        acc = m.group("acc") or ""
        oct_marks = m.group("oct") or ""
        tie = m.group("tie") or ""
        digits = m.group("duration") or ""
        units = int(digits) if digits else 1
        if units not in DURATIONS:
            raise SplitError(
                f"{where}: unsupported duration {units}; split it into tied "
                f"supported lengths"
            )
        if "," in oct_marks and "'" in oct_marks:
            raise SplitError(f"{where}: mixed octave marks in {text!r}")
        if note == "z":
            if acc or oct_marks or tie:
                raise SplitError(
                    f"{where}: a rest cannot have accidentals, octave marks or ties"
                )
        if offset + units > expect_units:
            raise SplitError(f"{where}: note/rest exceeds meter duration in {text!r}")
        offset += units
    if offset != expect_units:
        raise SplitError(
            f"{where}: duration {offset} units != meter duration {expect_units} "
            f"in {text!r}"
        )


def check_native(text: str, name: str = "<score>") -> None:
    """Subset of score/abc::parse for files this prototype emits."""
    lines = text.splitlines()
    if len(lines) < 12:
        raise SplitError(f"{name}: incomplete native two-voice ABC")
    if lines[0] != "X:1" or lines[1] != "T:":
        raise SplitError(f"{name}: expected native X:1 and blank T: header")
    if not lines[2].startswith("M:"):
        raise SplitError(f"{name}: missing header M:")
    m = re.fullmatch(r"([1-9][0-9]*)/([1-9][0-9]*)", lines[2][2:])
    if not m:
        raise SplitError(f"{name}: bad header M:")
    num, mden = int(m.group(1)), int(m.group(2))
    m = re.fullmatch(r"L:1/([1-9][0-9]*)", lines[3])
    if not m:
        raise SplitError(f"{name}: expected L:1/<power of two>")
    l_den = int(m.group(1))
    if not re.fullmatch(r"Q:1/4=([1-9][0-9]*)", lines[4]):
        raise SplitError(f"{name}: expected integer quarter-note tempo Q:1/4=<BPM>")
    if lines[5] != VOICE_LINES[0] or lines[6] != VOICE_LINES[1]:
        raise SplitError(f"{name}: preserve native Vocal and Ins voice definitions")
    if not lines[7].startswith("K:"):
        raise SplitError(f"{name}: missing header K:")
    expect = bar_units(num, mden, l_den)
    cursor, group = 8, 0
    while cursor < len(lines):
        while cursor < len(lines) and lines[cursor].startswith("% "):
            cursor += 1
        if cursor == len(lines):
            raise SplitError(f"{name}: dangling section comment without music")
        group += 1
        counts = []
        for voice in ("Vocal", "Ins"):
            ctx = f"{name} group {group}, {voice}"
            if cursor >= len(lines) or lines[cursor] != f"V: {voice}":
                raise SplitError(f"{ctx}: expected V: {voice}")
            cursor += 1
            if cursor < len(lines) and (lines[cursor].startswith("M:")
                                         or lines[cursor].startswith("K:")):
                raise SplitError(
                    f"{ctx}: mid-score M:/K: changes unsupported by the prototype "
                    f"checker (the studio parser accepts them)"
                )
            if cursor >= len(lines):
                raise SplitError(f"{ctx}: missing music line")
            line = lines[cursor]
            if not line.endswith("|"):
                raise SplitError(f"{ctx}: music line must end with a plain barline")
            cursor += 1
            expanded = 0
            for bar in line[:-1].split("|"):
                bar = bar.strip()
                if not bar:
                    raise SplitError(
                        f"{ctx}: empty measure or unsupported double/repeat barline"
                    )
                if bar in WHOLE_RESTS:
                    for _ in range(WHOLE_RESTS[bar]):
                        expanded += 1
                else:
                    check_bar_body(bar, expect, ctx)
                    expanded += 1
            if not 1 <= expanded <= 4:
                raise SplitError(f"{ctx}: expected 1-4 measures after expanding Z rests")
            counts.append(expanded)
        if counts[0] != counts[1]:
            raise SplitError(f"{name} group {group}: voices have different measure counts")


def rest_line(count: int) -> str:
    if count == 1:
        return "Z|"
    return f"Z{count}|"


def build_part_abc(header_lines: list[str], sections: list[dict],
                   part: str, expect_units: int, chords: dict) -> str:
    out = list(header_lines)
    for section in sections:
        label = section["label"]
        bars = section["bars"][part]
        clist = chords.get(label, [None] * len(bars))
        first_group = True
        for start in range(0, len(bars), 4):
            chunk = bars[start:start + 4]
            cchunk = clist[start:start + 4] if clist else [None] * len(chunk)
            bodies = []
            for body, chord in zip(chunk, cchunk):
                body = body.strip()
                if body in WHOLE_RESTS:
                    bodies.append(body)
                elif chord:
                    if body.startswith('"'):
                        raise SplitError(
                            f"{label} {part}: bar already carries a chord symbol; "
                            f"use either inline chords or the chords map, not both"
                        )
                    bodies.append(f'"{chord}"{body}')
                else:
                    bodies.append(body)
            if first_group:
                out.append(f"% {label}")
                first_group = False
            out.append("V: Vocal")
            out.append("|".join(bodies) + "|")
            out.append("V: Ins")
            out.append(rest_line(len(chunk)))
    return "\n".join(out) + "\n"


def load_arrangement(path: Path) -> dict:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise SplitError(f"cannot read {path}: {exc}")
    if not isinstance(data, dict):
        raise SplitError(f"{path}: top level must be an object")
    header = data.get("header")
    sections = data.get("sections")
    if not isinstance(header, dict) or not isinstance(sections, list) or not sections:
        raise SplitError(f"{path}: need header {{M,L,bpm,K}} and a non-empty sections list")
    num, mden, l_den, bpm, key = check_header(header)
    expect = bar_units(num, mden, l_den)
    if not all(isinstance(s, dict) and isinstance(s.get("label"), str)
               and isinstance(s.get("bars"), dict) for s in sections):
        raise SplitError(f"{path}: each section needs a label and bars {{S,A,T,B}}")
    for section in sections:
        label = section["label"]
        bars = section["bars"]
        if set(bars.keys()) != set(PARTS):
            raise SplitError(
                f"{path}: section {label!r} must name exactly S/A/T/B "
                f"(got {sorted(bars.keys())})"
            )
        counts = {}
        for part in PARTS:
            blist = bars[part]
            if not isinstance(blist, list) or not blist:
                raise SplitError(f"{path}: section {label!r} part {part} needs 1+ bars")
            counts[part] = len(blist)
            for i, body in enumerate(blist):
                if not isinstance(body, str):
                    raise SplitError(
                        f"{path}: section {label!r} part {part} bar {i + 1} must be a string"
                    )
                check_bar_body(body, expect,
                               f"{path} section {label!r} part {part} bar {i + 1}")
        if len(set(counts.values())) != 1:
            raise SplitError(
                f"{path}: section {label!r} has uneven bar counts {counts}; "
                f"all four parts must share the bar structure"
            )
    chords = data.get("chords", {})
    if chords:
        if not isinstance(chords, dict):
            raise SplitError(f"{path}: chords must map section label -> chord per bar")
        for section in sections:
            label = section["label"]
            if label not in chords:
                continue
            clist = chords[label]
            want = len(section["bars"]["S"])
            if not isinstance(clist, list) or len(clist) != want:
                raise SplitError(
                    f"{path}: chords[{label!r}] needs one entry per bar ({want})"
                )
            for chord in clist:
                if chord is not None and not (isinstance(chord, str)
                                              and CHORD_RE.match(chord)):
                    raise SplitError(
                        f"{path}: unsupported chord {chord!r} in chords[{label!r}]"
                    )
    lyrics = data.get("lyrics", {})
    if lyrics:
        if not isinstance(lyrics, dict):
            raise SplitError(f"{path}: lyrics must map part -> lyric sheet text")
        for part, text in lyrics.items():
            if part not in PARTS or not isinstance(text, str):
                raise SplitError(f"{path}: lyrics keys must be a subset of S/A/T/B")
    total_bars = sum(len(s["bars"]["S"]) for s in sections)
    return {"header": {"num": num, "mden": mden, "l_den": l_den, "bpm": bpm,
                       "key": key, "M": f"{num}/{mden}"},
            "sections": sections, "chords": chords, "lyrics": lyrics,
            "total_bars": total_bars, "bar_units": expect}


def skeleton_lyrics(sections: list[dict]) -> str:
    tag_of = {"verse": "Verse", "chorus": "Chorus", "bridge": "Bridge",
              "intro": "Intro", "outro": "Outro", "pre-chorus": "Pre-Chorus",
              "interlude": "Interlude"}
    return "\n\n".join(f"[{tag_of.get(s['label'], 'Verse')}]" for s in sections)


def estimate_seconds(total_bars: int, num: int, mden: int, bpm: int) -> float:
    quarters = total_bars * 4 * num / mden + 4 * num / mden  # +1 empty bar, cf. phrasing.lay
    return quarters * 60.0 / bpm


def cmd_split(args: argparse.Namespace) -> int:
    arr = load_arrangement(Path(args.in_file))
    header = arr["header"]
    header_lines = [
        "X:1",
        "T:",
        f"M:{header['M']}",
        f"L:1/{header['l_den']}",
        f"Q:1/4={header['bpm']}",
        VOICE_LINES[0],
        VOICE_LINES[1],
        f"K:{header['key']}",
    ]
    skeleton = args.style_skeleton
    if "{vocal}" not in skeleton:
        raise SplitError("style-skeleton needs a {vocal} placeholder for the per-part voice")
    vocals = {"S": args.vocal_s, "A": args.vocal_a, "T": args.vocal_t, "B": args.vocal_b}
    if any(not v for v in vocals.values()):
        raise SplitError("all four --vocal-s/a/t/b descriptors are required")
    style_bpm = re.search(r"(\d+)\s*BPM", skeleton)
    if style_bpm and int(style_bpm.group(1)) != header["bpm"]:
        print(f"warning: style skeleton says {style_bpm.group(1)} BPM but score is "
              f"Q:1/4={header['bpm']}; the engine follows the score, fix the style",
              file=sys.stderr)
    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    seconds = estimate_seconds(arr["total_bars"], header["num"], header["mden"],
                               header["bpm"])
    duration = min(360.0, math.ceil((seconds * 1.1 + 2.0) * 10) / 10)
    jobs = []
    for part in PARTS:
        abc = build_part_abc(header_lines, arr["sections"], part, arr["bar_units"],
                             arr["chords"])
        check_native(abc, f"part_{part}.abc")  # self-check before writing
        abc_path = out_dir / f"part_{part}.abc"
        abc_path.write_text(abc, encoding="utf-8")
        style = skeleton.replace("{vocal}", vocals[part])
        lyrics = arr["lyrics"].get(part, skeleton_lyrics(arr["sections"]))
        jobs.append({
            "part": part,
            "title": f"{args.title_prefix} ({part})" if args.title_prefix else None,
            "style": style,
            "lyrics": lyrics,
            "lyrics_complete": part in arr["lyrics"],
            "abc_file": abc_path.name,
            "cot": args.cot,
            "duration_seconds": duration,
            "transpose": 0,
            "lm_seed": args.lm_seed,
            "seed": args.seed,
            "vocals_only": True,
            "output_format": None,
            "notes": ("isolated vocal run; accompaniment discarded at mixdown. "
                      "Same seeds across parts keep timing closer but also keep "
                      "timbre closer; expect drift either way (see plan)."),
        })
    jobs.append({
        "part": "bed",
        "title": f"{args.title_prefix} (bed)" if args.title_prefix else None,
        "style": args.instrumental_style,
        "lyrics": "",
        "lyrics_complete": True,
        "abc_file": None,
        "cot": args.cot,
        "duration_seconds": duration,
        "transpose": 0,
        "lm_seed": args.lm_seed,
        "seed": args.seed,
        "vocals_only": False,
        "output_format": None,
        "notes": "instrumental bed; model composes its own score (no abc).",
    })
    manifest = {
        "prototype": "split-satb-prototype v1 (dev-only; not part of the runtime)",
        "plan": "docs/plans/2026-10-05_ensemble.md",
        "header": {k: header[k] for k in ("M", "bpm", "key")},
        "L": f"1/{header['l_den']}",
        "sections": [s["label"] for s in arr["sections"]],
        "bars_per_section": [len(s["bars"]["S"]) for s in arr["sections"]],
        "total_bars": arr["total_bars"],
        "estimated_seconds": round(seconds, 1),
        "how_to_render": ("Run jobs S/A/T/B then bed through song_create (MCP) or "
                          "the Create panel, one run per job; link them via playlist "
                          "or cover_of/made_from. Align the 5 tracks in a DAW, "
                          "mix, then master against a reference."),
        "jobs": jobs,
    }
    (out_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(f"parts: 4 x {arr['total_bars']} bars "
          f"({', '.join(s['label'] for s in arr['sections'])})")
    print(f"tempo: Q:1/4={header['bpm']}, estimated song {seconds:.1f}s "
          f"-> duration_seconds {duration}")
    print(f"wrote: {[f'part_{p}.abc' for p in PARTS] + ['manifest.json']} in {out_dir}")
    if any(j["part"] in PARTS and not j["lyrics_complete"] for j in jobs):
        print("note: no per-part lyrics in input; manifest carries tag skeletons only. "
              "Fill lyric lines before rendering.", file=sys.stderr)
    return 0


def cmd_check(args: argparse.Namespace) -> int:
    failed = False
    for name in args.files:
        try:
            check_native(Path(name).read_text(encoding="utf-8"), name)
            print(f"OK   {name}")
        except (OSError, SplitError) as exc:
            print(f"FAIL {name}: {exc}", file=sys.stderr)
            failed = True
    return 1 if failed else 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Prototype SATB splitter for the ensemble plan (dev-only).")
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("split", help="SATB JSON -> 4 native ABCs + manifest.json")
    p.add_argument("--in", dest="in_file", required=True)
    p.add_argument("--out-dir", required=True)
    p.add_argument("--style-skeleton", required=True,
                   help="shared style with a {vocal} placeholder")
    p.add_argument("--vocal-s", required=True)
    p.add_argument("--vocal-a", required=True)
    p.add_argument("--vocal-t", required=True)
    p.add_argument("--vocal-b", required=True)
    p.add_argument("--instrumental-style", required=True)
    p.add_argument("--title-prefix", default="")
    p.add_argument("--cot", default="full", choices=("full", "melody"))
    p.add_argument("--lm-seed", type=int, default=None)
    p.add_argument("--seed", type=int, default=None)
    p.add_argument("--ins-mode", default="rest", choices=("rest",),
                   help="v1 supports rests only; the bed run covers accompaniment")
    p.set_defaults(func=cmd_split)
    c = sub.add_parser("check", help="validate native two-voice ABC file(s)")
    c.add_argument("files", nargs="+")
    c.set_defaults(func=cmd_check)
    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except SplitError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
