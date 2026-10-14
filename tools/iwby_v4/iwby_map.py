"""Song map for the IWBY post-process, read from the project instead of hardcoded (B51, 2026-10-14).

Pure functions, no audio: iwby_post.py calls them, test_iwby_map.py checks them.

- sections: the project's arrangement (pattern bars x repeats, in order) gives the bar span of
  intro, verse (every verse* entry), chorus1, chorus2 and outro; the breakdown is the last
  `breakdown_bars` of the section before chorus2. Seconds = bars * BAR at the project's bpm.
- throws: phrase indices (0-based; negative counts from the end) into the phrases found on the dry
  vocal stem, so they follow the take wherever the plan puts it. `expect_phrases` guards the index
  list: a take that splits into a different number of phrases is refused rather than thrown blind.
- end fade and length: offsets from the song end (the last arrangement bar), rounded to 1 ms (fade)
  and 10 ms (length) like the v4b params they replace.
"""

ROLES = ('intro', 'verse', 'chorus1', 'chorus2', 'outro')


def arrangement_bars(project):
    """[(pattern name, start bar, end bar)] in arrangement order."""
    bars_of = {p['name']: int(p['bars']) for p in project['patterns']}
    out, b = [], 0
    for entry in project['arrangement']:
        name = entry['pattern']
        if name not in bars_of:
            raise ValueError(f'arrangement names pattern {name!r}, which the project does not have')
        n = bars_of[name] * max(1, int(entry.get('repeats', 1)))
        out.append((name, b, b + n))
        b += n
    return out


def section_bars(project, breakdown_bars=2):
    """{role: [bar0, bar1]} for intro, verse, chorus1, breakdown, chorus2, outro."""
    spans = {}
    for name, b0, b1 in arrangement_bars(project):
        role = 'verse' if name.startswith('verse') else name
        if role in spans:
            if spans[role][1] != b0:
                raise ValueError(f'section {role!r} is not contiguous in the arrangement')
            spans[role][1] = b1
        else:
            spans[role] = [b0, b1]
    missing = [r for r in ROLES if r not in spans]
    if missing:
        raise ValueError(f'the arrangement has no {", ".join(missing)} (has {", ".join(spans)})')
    d0 = spans['chorus2'][0]
    if breakdown_bars > 0 and d0 - breakdown_bars < spans['chorus1'][0]:
        raise ValueError('chorus1 is shorter than the breakdown')
    sb = {r: spans[r] for r in ROLES}
    sb['breakdown'] = [d0 - breakdown_bars, d0]
    return sb


def song_end_bar(project):
    return arrangement_bars(project)[-1][2]


def end_times(end_s, fade_from_end_s, length_from_end_s):
    """(end fade [start, end] in s, render length in s) from the song end."""
    fade = [round(end_s + fade_from_end_s[0], 3), round(end_s + fade_from_end_s[1], 3)]
    return fade, round(end_s + length_from_end_s, 2)


def throw_ends(phrases, throw_phrases, expect_phrases=None):
    """Phrase-end times (s) of the thrown phrases; None = a throw on every phrase end."""
    if throw_phrases is None:
        return None
    n = len(phrases)
    if expect_phrases is not None and n != expect_phrases:
        raise ValueError(f'the vocal stem splits into {n} phrases, the throw list was written for {expect_phrases}')
    out = []
    for i in throw_phrases:
        if not -n <= i < n:
            raise ValueError(f'throw phrase {i} is outside the {n} phrases')
        out.append(phrases[i][1])
    return sorted(set(out))
