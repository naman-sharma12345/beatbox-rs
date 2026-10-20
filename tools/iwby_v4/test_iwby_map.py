"""Checks for iwby_map (no audio): python3 test_iwby_map.py  (or pytest)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import iwby_map as m


def proj(chorus2_bars, repeats_verse=1):
    pats = [('intro', 4), ('verse1', 8), ('chorus1', 8), ('chorus2', chorus2_bars), ('outro', 4)]
    return {'bpm': 67.1, 'patterns': [{'name': n, 'bars': b} for n, b in pats],
            'arrangement': [{'pattern': n, 'repeats': repeats_verse if n == 'verse1' else 1} for n, _ in pats]}


def raises(f, *a):
    try:
        f(*a)
    except ValueError:
        return True
    return False


def test_v4b_map():  # legacy plan: chorus2 6 bars -> the map iwby_v4_post.py hardcoded
    assert m.section_bars(proj(6)) == dict(intro=[0, 4], verse=[4, 12], chorus1=[12, 20], chorus2=[20, 26], outro=[26, 30], breakdown=[18, 20])
    assert m.song_end_bar(proj(6)) == 30


def test_v5_map():  # yin_gated plan: chorus2 5 bars
    sb = m.section_bars(proj(5))
    assert sb['chorus2'] == [20, 25] and sb['outro'] == [25, 29] and sb['breakdown'] == [18, 20]


def test_repeats_and_verses():
    p = proj(6, repeats_verse=2)
    assert m.section_bars(p)['verse'] == [4, 20] and m.song_end_bar(p) == 38
    p = proj(6)
    p['patterns'].insert(2, {'name': 'verse2', 'bars': 8}); p['arrangement'].insert(2, {'pattern': 'verse2'})
    assert m.section_bars(p)['verse'] == [4, 20]


def test_refusals():
    p = proj(6); p['arrangement'] = [a for a in p['arrangement'] if a['pattern'] != 'chorus2']
    assert raises(m.section_bars, p)
    p = proj(6); p['arrangement'].append({'pattern': 'bridge'})
    assert raises(m.section_bars, p)


def test_end_times():  # v4b's absolute 105.0 / 108.6 / 110.0 s at 30 bars; B50's v5 values at 29 bars
    bar = 4 * 60 / 67.1
    assert m.end_times(30 * bar, [-2.3025, 1.2975], 2.6975) == ([105.0, 108.6], 110.0)
    assert m.end_times(29 * bar, [-2.3025, 1.2975], 2.6975) == ([101.423, 105.023], 106.42)


def test_throws():
    ph = [(i * 4.0, i * 4.0 + 2.5) for i in range(20)]
    assert m.throw_ends(ph, [2, 6, 19], 20) == [10.5, 26.5, 78.5]
    assert m.throw_ends(ph, [-1], 20) == [78.5]
    assert m.throw_ends(ph, None, 20) is None
    assert raises(m.throw_ends, ph[:19], [2], 20)
    assert raises(m.throw_ends, ph, [20], None)


if __name__ == '__main__':
    n = 0
    for k, f in sorted(globals().items()):
        if k.startswith('test_'):
            f(); n += 1
    print(f'{n} passed')
