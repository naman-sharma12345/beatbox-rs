#!/usr/bin/env python3
"""Finite P0A appearance capture. No input injection, retries or visual acceptance."""
import ctypes as C
import hashlib
import json
import math
import re
import os
from pathlib import Path
import secrets
import signal
import struct
import subprocess as S
import sys
import tempfile
import time
import zlib

MODES = [('pattern', 'sequencer', 0, 1), ('song', 'playlist', 3, 2), ('mix', 'mixer', 1, 3)]
CASES = [(state, mode, view, index, workspace, w, h, scale, False)
         for state in ('empty', 'populated') for mode, view, index, workspace in MODES
         for w, h, scale in ((1100, 700, 1), (1440, 900, 1), (1100, 700, 2))]
CASES += [('populated', mode, view, index, workspace, w, h, 1, True)
          for mode, view, index, workspace in MODES for w, h in ((1100, 700), (1440, 900))]
LEGACY = ('BEATBOX_NARROW_SHOT', 'BEATBOX_AUDIO_SCENARIO', 'BEATBOX_AUDIO_SCALE',
          'BEATBOX_ARRANGEMENT_CAPTURE', 'BEATBOX_ARRANGEMENT_CLIP_ID', 'BEATBOX_ARRANGEMENT_ACTION_HASH')
RECIPE = {'brief': 'dark desi hip-hop with vocal chops, dark plucks and a sliding 808, for a gully rap verse',
          'genre': 'desi_hiphop', 'bpm': 92, 'seed': 21, 'max_iterations': 2}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write(path, doc):
    path.write_text(json.dumps(doc, indent=2) + '\n')


def name(case):
    state, mode, _, _, _, w, h, scale, inspector = case
    return f'p0a_{state}_{mode}_{w}x{h}_{scale}x_{"inspector" if inspector else "base"}'


def png(path, expected):
    require(8 <= path.stat().st_size <= 32_000_000, 'PNG file bound')
    data = path.read_bytes()
    require(data[:8] == b'\x89PNG\r\n\x1a\n', 'PNG signature/bound')
    pos, payload, kinds = 8, bytearray(), []
    while pos < len(data):
        require(pos + 12 <= len(data), 'truncated chunk')
        n, = struct.unpack_from('>I', data, pos)
        require(n <= 24_000_000 and pos + 12 + n <= len(data), 'chunk bound')
        kind, chunk = data[pos+4:pos+8], data[pos+8:pos+8+n]
        crc, = struct.unpack_from('>I', data, pos+8+n)
        require(zlib.crc32(kind+chunk) & 0xffffffff == crc, 'PNG CRC')
        if kind == b'IHDR':
            require(not kinds and n == 13, 'single first IHDR')
            w, h, *fmt = struct.unpack('>IIBBBBB', chunk)
            require((w, h) == expected and fmt == [8, 6, 0, 0, 0], 'RGBA8 dimensions/format')
        elif kind == b'IDAT':
            require(kinds and kinds[0] == b'IHDR' and b'IEND' not in kinds, 'IDAT order')
            require(not (b'IDAT' in kinds and kinds[-1] != b'IDAT'), 'nonconsecutive IDAT')
            payload.extend(chunk)
        elif kind == b'IEND':
            require(n == 0 and b'IDAT' in kinds, 'IEND')
        else:
            require(kind[0] & 32 and b'IEND' not in kinds, 'unknown critical chunk')
        kinds.append(kind)
        pos += n+12
        if kind == b'IEND':
            break
    require(pos == len(data) and kinds[-1:] == [b'IEND'], 'complete PNG consumption')
    stride = expected[0]*4
    limit = (stride+1)*expected[1]
    decoder = zlib.decompressobj()
    raw = decoder.decompress(payload, limit+1)
    require(len(raw) == limit and decoder.eof and not decoder.unused_data
            and not decoder.unconsumed_tail, 'bounded complete IDAT stream')
    previous, pixels = bytearray(stride), hashlib.sha256()
    for y in range(expected[1]):
        start = y*(stride+1)
        mode, row = raw[start], bytearray(raw[start+1:start+1+stride])
        require(mode <= 4, 'PNG filter')
        for x in range(stride):
            a, b, c = row[x-4] if x >= 4 else 0, previous[x], previous[x-4] if x >= 4 else 0
            p = a+b-c
            da, db, dc = abs(p-a), abs(p-b), abs(p-c)
            paeth = a if da <= db and da <= dc else b if db <= dc else c
            predictor = (0, a, b, (a+b)//2, paeth)[mode]
            row[x] = (row[x]+predictor) & 255
        pixels.update(row)
        previous = row
    return pixels.hexdigest()


def integer(value, minimum=0):
    require(type(value) is int and value >= minimum, 'strict nonboolean integer')


def boolean(value):
    require(type(value) is bool, 'strict boolean')


def finite(value):
    require(type(value) in (int, float) and math.isfinite(value), 'finite number')


def strict_json(text):
    def constant(value):
        raise ValueError('nonstandard JSON constant: '+value)
    def pairs(items):
        result = {}
        for key, value in items:
            require(key not in result, 'duplicate JSON key')
            result[key] = value
        return result
    doc = json.loads(text, parse_constant=constant, object_pairs_hook=pairs)
    def walk(value):
        if type(value) is float:
            finite(value)
        elif type(value) is dict:
            for v in value.values(): walk(v)
        elif type(value) is list:
            for v in value: walk(v)
    walk(doc)
    return doc


def shape(value, keys):
    require(type(value) is dict and set(value) == set(keys.split()), 'complete versioned object: '+keys)


def rectangle(value):
    require(type(value) is list and len(value) == 4, 'rectangle shape')
    for v in value: finite(v)
    require(value[2] > value[0] and value[3] > value[1], 'positive rectangle')


PANE_LABELS = dict(Sequencer='Channel rack', Mixer='Mixer', Automation='Automation',
                   Playlist='Playlist', Vocal='Vocal', Browser='Browser', Rack='FX rack',
                   Library='Library', Inspector='Channel', Notes='Piano roll', Console='Console',
                   Transport='Transport', Analysis='Analysis', History='History')
CENTRAL = {'Sequencer', 'Playlist', 'Mixer', 'Notes', 'Automation', 'Vocal', 'Rack'}


def snapshot_project(project):
    required = set('name bpm swing key_root scale tracks patterns arrangement master_effects master_volume_db samples schema_version id_seq'.split())
    optional_arrays = set('buses automation macros playlist automation_clips audio_clips markers tempo_points seed_log'.split())
    require(type(project) is dict and required <= set(project)
            and set(project) <= required | optional_arrays | {'vocal_map','fx_chains','midi','song','tuning_cents'}, 'serialized snapshot Project envelope')
    for key in ('name','key_root','scale'): require(type(project[key]) is str, 'snapshot project string')
    for key in ('bpm','swing','master_volume_db'): finite(project[key])
    for key in ('schema_version','id_seq'): integer(project[key])
    for key in {'tracks','patterns','arrangement','master_effects','samples'} | optional_arrays:
        if key in project: require(type(project[key]) is list, 'snapshot project array')
    for key in ('vocal_map','fx_chains','midi','song'):
        if key in project: require(type(project[key]) is dict, 'snapshot project object')
    if 'tuning_cents' in project: finite(project['tuning_cents'])


def observation(doc):
    shape(doc, 'project_serialized_sha256 project_serialized_bytes public_ledger')
    require(type(doc['project_serialized_sha256']) is str and re.fullmatch('[0-9a-f]{64}', doc['project_serialized_sha256']), 'project hash')
    integer(doc['project_serialized_bytes'], 1)
    ledger = doc['public_ledger']
    shape(ledger, 'revision load_generation depths labels log snapshots')
    for key in ('revision', 'load_generation'): integer(ledger[key])
    require(type(ledger['depths']) is list and len(ledger['depths']) == 2, 'depths shape')
    require(type(ledger['labels']) is list and len(ledger['labels']) == 2, 'labels shape')
    for depth, labels in zip(ledger['depths'], ledger['labels']):
        integer(depth)
        require(type(labels) is list and len(labels) == depth and all(type(v) is str for v in labels), 'public history labels/depths')
    require(type(ledger['log']) is list and type(ledger['snapshots']) is list, 'public ledger arrays')
    for entry in ledger['log']:
        shape(entry, 'tool summary ok source')
        boolean(entry['ok'])
        require(all(type(entry[k]) is str for k in ('tool', 'summary', 'source')), 'log strings')
    for entry in ledger['snapshots']:
        shape(entry, 'name project note revision')
        integer(entry['revision'])
        require(type(entry['name']) is str and type(entry['note']) is str and type(entry['project']) is dict, 'public snapshot shape')
        snapshot_project(entry['project'])


def sidecar(path, case, head):
    doc = strict_json(path.read_text())
    shape(doc, 'schema build_sha status native_input autoplay state_origin view_index view_name workspace project_file runner_case fixture_kind_source seeded_presentation requested viewport_pixels pixels_per_point logical_viewport native_viewport_observations capture_frame shot_requested render_revision render_load_generation baseline final invariance transport transport_position_policy')
    _, _, _, index, workspace, w, h, scale, inspector = case
    integer(doc['schema'], 1)
    require(doc['schema'] == 1 and type(doc['build_sha']) is str and doc['build_sha'] == head, 'app head/schema')
    require(type(doc['project_file']) is str and doc['runner_case'] == name(case)
            and Path(doc['project_file']).resolve() == (path.parent/'fixtures'/(case[0]+'.json')).resolve(), 'fixture linkage')
    integer(doc['view_index'])
    require(doc['view_index'] == index and doc['view_name'] == case[2].upper(), 'app view')
    shape(doc['requested'], 'logical_size scale')
    integer(doc['requested']['scale'], 1)
    for key, expected in [('viewport_pixels', [w*scale, h*scale]), ('logical_viewport', [w, h])]:
        require(type(doc[key]) is list and len(doc[key]) == 2, 'viewport shape')
        for v in doc[key]: finite(v)
        if key == 'viewport_pixels':
            for v in doc[key]: integer(v, 1)
            require(doc[key] == expected, 'app pixels')
        else: require(all(abs(a-b) < 1.1 for a,b in zip(doc[key], expected)), 'logical viewport')
    size = doc['requested']['logical_size']
    require(type(size) is list and len(size) == 2, 'requested size')
    for v in size: integer(v, 1)
    require(doc['requested'] == {'logical_size':[w,h], 'scale':scale}, 'requested config')
    finite(doc['pixels_per_point'])
    require(abs(doc['pixels_per_point']-scale) < .01, 'app logical geometry')
    ws = doc['workspace']
    shape(ws, 'mode pinned_transport panes central_rect inspector_count inspector_right_of_entire_central')
    integer(ws['mode'], 1); boolean(ws['pinned_transport']); integer(ws['inspector_count'])
    boolean(ws['inspector_right_of_entire_central'])
    require(ws['mode'] == workspace and ws['pinned_transport'], 'workspace mode/pinned')
    require(type(ws['panes']) is list and ws['panes'], 'observed panes')
    central, channels, identities = [], [], []
    for pane in ws['panes']:
        shape(pane, 'pane label main_surface rect')
        require(type(pane['pane']) is str and pane['pane'] in PANE_LABELS and pane['label'] == PANE_LABELS[pane['pane']], 'pane identity/label')
        boolean(pane['main_surface']); rectangle(pane['rect'])
        require(pane['main_surface'], 'cold capture main surface')
        identities.append(pane['pane'])
        if pane['pane'] in CENTRAL: central.append(pane['rect'])
        if pane['pane'] == 'Inspector': channels.append(pane['rect'])
    require(len(identities) == len(set(identities)), 'unique pane identities')
    expected_panes = {'Library', 'Browser', {0:'Sequencer',3:'Playlist',1:'Mixer'}[index]}
    if index == 0: expected_panes.add('Notes')
    if inspector: expected_panes.add('Inspector')
    require(set(identities) == expected_panes and central, 'cold capture pane set')
    union = [min(r[0] for r in central), min(r[1] for r in central), max(r[2] for r in central), max(r[3] for r in central)]
    rectangle(ws['central_rect'])
    require(ws['central_rect'] == union, 'observed central union')
    right = len(channels) == 1 and channels[0][0] >= union[2] and channels[0][1] <= union[1]+1.1 and channels[0][3] >= union[3]-1.1
    require(len(channels) == int(inspector) == ws['inspector_count'] and right is inspector
            and ws['inspector_right_of_entire_central'] is right, 'Inspector observed relation')
    shape(doc['seeded_presentation'], 'inspector'); boolean(doc['seeded_presentation']['inspector'])
    require(doc['seeded_presentation']['inspector'] is inspector, 'presentation seed')
    for key in ('native_input', 'autoplay', 'shot_requested'): boolean(doc[key])
    require(not doc['native_input'] and not doc['autoplay'] and doc['shot_requested'] and doc['status'] == 'captured_uninspected', 'capture status')
    require(doc['state_origin'] == ['project_file_loaded','capture_first_viewport','presentation_seed_only'], 'state origin')
    require(doc['native_viewport_observations'] is None, 'version 1 optional native observations')
    require(doc['fixture_kind_source'] == 'runner manifest cases[].fixture; not an app UI assertion'
            and doc['invariance'] == 'project serialized bytes + complete public ledger; private history payloads not exposed'
            and doc['transport_position_policy'] == 'ordinary frame 2 seek to 38 percent retained; not frozen', 'evidence policies')
    integer(doc['capture_frame'], 31)
    for key in ('baseline', 'final'): observation(doc[key])
    require(doc['baseline'] == doc['final'], 'project/public-ledger invariance')
    for key, field in [('render_revision','revision'), ('render_load_generation','load_generation')]:
        integer(doc[key]); require(doc[key] == doc['final']['public_ledger'][field], 'current render')
    t = doc['transport']
    shape(t, 'playing auditioning launch_pending position_s engine_playing engine_auditioning commands_pending notes_pending audition_pending launch_command_pending perf_playing perf_queued')
    for key in ('playing','auditioning','launch_pending','engine_playing','engine_auditioning','audition_pending','launch_command_pending'):
        boolean(t[key]); require(not t[key], 'inactive transport')
    for key in ('commands_pending','notes_pending'):
        integer(t[key]); require(t[key] == 0, 'empty pending transport')
    require(t['perf_playing'] is None and t['perf_queued'] is None, 'empty performance transport')
    finite(t['position_s']); require(t['position_s'] >= 0, 'observed position')
    return doc


def cleanup(process, deadline=None):
    if process is None:
        return
    # Always attempt both bounded waits, even if signalling itself fails.
    errors = []
    for sig, seconds in ((signal.SIGTERM, 2), (signal.SIGKILL, 3)):
        try:
            os.killpg(process.pid, sig)
        except ProcessLookupError:
            pass
        except Exception as error:
            errors.append(str(error))
        try:
            process.wait(timeout=seconds if deadline is None else max(0, min(seconds, deadline-time.monotonic())))
        except S.TimeoutExpired:
            if sig == signal.SIGKILL:
                errors.append('owned process did not reap within cleanup deadline')
        except Exception as error:
            errors.append(str(error))
    if errors:
        raise RuntimeError('; '.join(errors))


def command(argv, env, log, seconds, record_argv=True, stderr=S.STDOUT):
    process = None
    try:
        if record_argv:
            log.write((json.dumps(argv)+'\n').encode()); log.flush()
        process = S.Popen(argv, env=env, stdout=log, stderr=stderr, start_new_session=True)
        return process.wait(timeout=seconds)
    finally:
        cleanup(process)


class XReadout:
    """Read-only Xlib: bounded root-child queries, no input or remote endpoint."""
    def __init__(self):
        self.x = C.CDLL('libX11.so.6')
        P, U, I = C.c_void_p, C.c_ulong, C.c_int
        signatures = {'XOpenDisplay': ([C.c_char_p], P), 'XDefaultRootWindow': ([P], U),
                      'XGetGeometry': ([P, U, C.POINTER(U), C.POINTER(I), C.POINTER(I)] + [C.POINTER(C.c_uint)]*4, I),
                      'XQueryTree': ([P, U, C.POINTER(U), C.POINTER(U), C.POINTER(C.POINTER(U)), C.POINTER(C.c_uint)], I),
                      'XFetchName': ([P, U, C.POINTER(P)], I), 'XFree': ([P], I), 'XCloseDisplay': ([P], I)}
        for key, (args, result) in signatures.items():
            fn = getattr(self.x, key); fn.argtypes = args; fn.restype = result
        self.display = self.x.XOpenDisplay(None)
        require(self.display, 'XOpenDisplay not ready')
        self.root = self.x.XDefaultRootWindow(self.display)

    def geometry(self, window):
        root, x, y = C.c_ulong(), C.c_int(), C.c_int()
        w, h, border, depth = (C.c_uint() for _ in range(4))
        require(self.x.XGetGeometry(self.display, window, C.byref(root), C.byref(x), C.byref(y),
                                   C.byref(w), C.byref(h), C.byref(border), C.byref(depth)), 'XGetGeometry')
        require(w.value > 0 and h.value > 0, 'positive X geometry')
        return [w.value, h.value]

    def studio(self):
        root, parent, count, children = C.c_ulong(), C.c_ulong(), C.c_uint(), C.POINTER(C.c_ulong)()
        require(self.x.XQueryTree(self.display, self.root, C.byref(root), C.byref(parent),
                                 C.byref(children), C.byref(count)), 'XQueryTree')
        found = []
        try:
            require(count.value <= 256, 'bounded root children')
            for i in range(count.value):
                title = C.c_void_p()
                if self.x.XFetchName(self.display, children[i], C.byref(title)) and title.value:
                    try:
                        if C.string_at(title) == b'Beatbox Studio':
                            found.append({'window': children[i], 'title': 'Beatbox Studio',
                                          'client_pixels': self.geometry(children[i])})
                    finally:
                        self.x.XFree(title)
        finally:
            if children:
                self.x.XFree(children)
        require(len(found) <= 1, 'ambiguous Studio windows')
        return found[0] if found else None

    def close(self):
        self.x.XCloseDisplay(self.display)


def observer_main(mode):
    # The only process that loads/calls Xlib. Fatal C exits cannot kill the supervisor.
    readout = XReadout()
    try:
        result = readout.geometry(readout.root) if mode == 'root' else readout.studio()
    finally:
        readout.close()
    print(json.dumps(result, allow_nan=False))


class Observer:
    """One finite read-only child per observation, no command DSL or input API."""
    def __init__(self, env):
        self.env, self.process = env, None
        self.failure_deadline = None

    def read(self, mode, deadline):
        require(mode in ('root', 'studio'), 'internal observer mode')
        remaining = deadline-time.monotonic()
        require(remaining > 0, 'observer deadline')
        with tempfile.TemporaryFile() as output:
            try:
                self.process = S.Popen([sys.executable, str(Path(__file__).resolve()), '--internal-observer', mode],
                                       env=self.env, stdout=output, stderr=S.DEVNULL, start_new_session=True)
                require(self.process.wait(timeout=max(0, deadline-time.monotonic())) == 0, 'observer fatal exit')
                require(time.monotonic() < deadline, 'observer deadline')
                output.seek(0)
                raw = output.read(4097)
                require(len(raw) <= 4096, 'observer result bound')
                result = strict_json(raw.decode())
                if mode == 'root':
                    size = result
                elif result is not None:
                    shape(result, 'window title client_pixels')
                    integer(result['window'], 1)
                    require(result['title'] == 'Beatbox Studio', 'observer title')
                    size = result['client_pixels']
                if result is not None or mode == 'root':
                    require(type(size) is list and len(size) == 2, 'observer geometry shape')
                    for v in size: integer(v, 1)
            except BaseException:
                self.failure_deadline = time.monotonic()+5
                raise
            finally:
                self.close(min(deadline+5, time.monotonic()+5))
            require(time.monotonic() < deadline, 'observer deadline including close')
            return result

    def close(self, deadline=None):
        process, self.process = self.process, None
        limit = self.failure_deadline or deadline or time.monotonic()+5
        try:
            cleanup(process, limit)
        except BaseException:
            self.failure_deadline = limit
            raise


def validate_empty(doc):
    require(not doc['tracks'] and doc['bpm'] == 120 and len(doc['patterns']) == 1
            and doc['patterns'][0]['name'] == 'A' and doc['patterns'][0]['bars'] == 4
            and not doc['patterns'][0]['clips'], 'real default empty Pattern A')
    for field in ('playlist', 'audio_clips', 'arrangement', 'automation', 'automation_clips', 'vocal_map', 'samples'):
        require(not doc.get(field), 'empty musical content: '+field)


def complete(folder, expected):
    actual = {p.name for p in folder.iterdir() if p.suffix in ('.png', '.json', '.log') and p.name.startswith('p0a_')}
    require(actual == {n+s for n in expected for s in ('.png', '.json', '.log')}, 'exact triplet set')
    require(all((folder/(n+s)).stat().st_size > 0 for n in expected for s in ('.png', '.json', '.log')), 'empty output')


def run(binary, folder):
    require(not folder.exists(), 'fresh output root required; stale outputs not accepted')
    folder.mkdir(parents=True)
    expected = [name(c) for c in CASES]
    require(len(expected) == len(set(expected)) == 24, 'exact 24-cell matrix')
    git = lambda arg: S.check_output(['git', 'rev-parse', arg], text=True).strip()
    manifest = {'schema': 1, 'expected': expected, 'cases': [], 'head': git('HEAD'), 'tree': git('HEAD^{tree}'),
                'rust_toolchain_manifest_sha256': digest(Path('rust-toolchain.toml')),
                'rust_toolchain_manifest': Path('rust-toolchain.toml').read_text(),
                'binary_sha256': digest(binary), 'rustc': S.check_output(['rustc', '-vV'], text=True),
                'workflow': {k: os.environ.get(k) for k in ('GITHUB_RUN_ID', 'GITHUB_EVENT_NAME', 'GITHUB_REF', 'GITHUB_WORKFLOW', 'GITHUB_WORKFLOW_REF')},
                'native_input': False, 'human_inspection': 'outstanding',
                'unrun': ['1440x900@2x', 'native input/focus/save/ordinary Close', 'human visual inspection'], 'status': 'incomplete'}
    save = lambda: write(folder/'manifest.json', manifest)
    save()
    xvfb, app, readout = None, None, None
    cleanup_deadline = None
    original_x = {k: os.environ.get(k) for k in ('DISPLAY', 'XAUTHORITY')}
    env = os.environ.copy()
    for key in LEGACY + ('BEATBOX_CAPTURE_SIZE', 'BEATBOX_CAPTURE_UI_SCALE', 'BEATBOX_CAPTURE_INSPECTOR', 'DISPLAY', 'WAYLAND_DISPLAY'):
        env.pop(key, None)
    env['BEATBOX_NO_LISTEN'] = '1'
    # Never inherit a user history override into fixture production.
    env.pop('BEATBOX_NOVELTY_HISTORY', None)
    try:
        with tempfile.TemporaryDirectory(prefix='p0a-') as temporary:
            home = Path(temporary)
            for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR'):
                path = home/key; path.mkdir(mode=0o700); env[key] = str(path)
            fixtures = folder/'fixtures'; fixtures.mkdir()
            empty, populated = fixtures/'empty.json', fixtures/'populated.json'
            history = (fixtures/'novelty-history.jsonl').resolve()
            env['BEATBOX_NOVELTY_HISTORY'] = str(history)
            manifest['producer_history'] = {'path': str(history), 'source': 'capture-owned BEATBOX_NOVELTY_HISTORY; inherited override discarded'}
            inputs = [['new_project', {'name': 'P0A empty musical content', 'bpm': 120}],
                      ['produce_track', dict(RECIPE, out_dir=str(fixtures/'desi'))]]
            write(fixtures/'inputs.json', {'actions': inputs, 'argv': [
                [str(binary), 'call', inputs[0][0], json.dumps(inputs[0][1]), '--project', str(empty)],
                [str(binary), 'call', inputs[1][0], json.dumps(inputs[1][1])]]})
            with (fixtures/'actions.log').open('wb') as log:
                require(command([str(binary), 'call', inputs[0][0], json.dumps(inputs[0][1]), '--project', str(empty)], env, log, 60) == 0, 'empty fixture')
            with (fixtures/'producer.json').open('wb') as log, (fixtures/'producer.log').open('wb') as errors:
                require(command([str(binary), 'call', inputs[1][0], json.dumps(inputs[1][1])], env, log, 180, record_argv=False, stderr=errors) == 0, 'producer fixture')
            result = json.loads((fixtures/'producer.json').read_text())
            for key in ('project', 'plan', 'audio'):
                if key not in result['files']:
                    continue
                product = result['files'][key]
                require(type(product) is str and Path(product).resolve().is_relative_to(fixtures.resolve()), 'producer products must be capture-owned')
            populated.write_bytes(Path(result['files']['project']).read_bytes())
            validate_empty(json.loads(empty.read_text()))
            populated_doc = json.loads(populated.read_text())
            require(populated_doc['tracks'], 'populated tracks required')
            paths = {p.resolve() for p in fixtures.rglob('*') if p.is_file()}
            for sample in populated_doc.get('samples', []):
                asset = Path(sample['path']).resolve()
                require(asset.is_file(), 'missing referenced local asset')
                paths.add(asset)
            hashes = {str(p): digest(p) for p in sorted(paths)}
            manifest['fixture_sha256'] = hashes
            require(history.is_file() and str(history) in hashes, 'recorded producer history required')
            manifest['producer_history']['sha256'] = hashes[str(history)]
            require(Path(result['files']['project']).resolve().is_relative_to(fixtures.resolve()), 'producer project must be capture-owned')
            authority = home/'authority'
            S.run(['xauth', '-f', str(authority), 'add', ':0', '.', secrets.token_hex(16)], check=True, timeout=5, env=env)
            env['XAUTHORITY'] = str(authority)
            x_deadline = time.monotonic()+5
            read_fd, write_fd = os.pipe()
            try:
                with (folder/'xvfb.log').open('wb') as log:
                    xvfb = S.Popen(['Xvfb', '-displayfd', str(write_fd), '-screen', '0', '2880x1800x24', '-nolisten', 'tcp', '-auth', str(authority)],
                                   pass_fds=(write_fd,), stdout=log, stderr=S.STDOUT, env=env, start_new_session=True)
                os.close(write_fd); write_fd = None
                import select
                require(select.select([read_fd], [], [], max(0, x_deadline-time.monotonic()))[0], 'X display allocation deadline')
                display = os.read(read_fd, 32).decode().strip()
                require(display.isdecimal(), 'allocated X display')
            finally:
                os.close(read_fd)
                if write_fd is not None:
                    os.close(write_fd)
            # Server consumes the same cookie, independent of its display number.
            listing = S.check_output(['xauth', '-f', str(authority), 'list'], env=env, text=True, timeout=max(.001, x_deadline-time.monotonic())).split()
            S.run(['xauth', '-f', str(authority), 'add', ':'+display, '.', listing[-1]], env=env, check=True, timeout=max(.001, x_deadline-time.monotonic()))
            env['DISPLAY'] = ':'+display
            readout = Observer(env)
            root = readout.read('root', x_deadline)
            manifest['x_root_pixels'] = root
            require(root == [2880, 1800], 'X framebuffer geometry')
            start = time.monotonic()
            for case in CASES:
                require(time.monotonic()-start < 720, '12 minute campaign budget')
                state, _, view, _, _, w, h, scale, inspector = case
                stem = folder/name(case); image = stem.with_suffix('.png')
                for suffix in ('.png', '.json', '.log'):
                    stem.with_suffix(suffix).unlink(missing_ok=True)
                selectors = {'BEATBOX_CAPTURE_SIZE': f'{w}x{h}', 'BEATBOX_CAPTURE_UI_SCALE': str(scale),
                             'BEATBOX_CAPTURE_INSPECTOR': str(int(inspector)), 'BEATBOX_NO_LISTEN': '1'}
                argv = [str(binary), 'studio', str(empty if state == 'empty' else populated), '--screenshot', str(image), '--view', view]
                row = {'name': name(case), 'fixture': state, 'argv': argv, 'selectors': selectors, 'exit_code': None, 'timeout': False, 'status': 'incomplete'}
                manifest['cases'].append(row); save()
                observed = None
                with stem.with_suffix('.log').open('wb') as log:
                    log.write((json.dumps(argv)+'\n').encode()); log.flush()
                    until = min(time.monotonic()+110, start+720)
                    app = S.Popen(argv, env=dict(env, **selectors), stdout=log, stderr=S.STDOUT, start_new_session=True)
                    try:
                        while app.poll() is None:
                            if time.monotonic() >= until:
                                row.update(timeout=True, error='Studio timeout'); save()
                                raise TimeoutError('Studio timeout')
                            if observed is None or observed['client_pixels'] != [w*scale, h*scale]:
                                previous = observed
                                observed = readout.read('studio', until)
                                require(previous is None or observed is not None, 'Studio window disappeared')
                            time.sleep(.05)
                        require(time.monotonic() < until, 'Studio case deadline')
                        row['exit_code'] = app.wait(timeout=max(0, until-time.monotonic()))
                        require(row['exit_code'] == 0 and observed is not None, 'Studio exit/window readout')
                    finally:
                        if sys.exc_info()[0] is not None:
                            cleanup_deadline = readout.failure_deadline or time.monotonic()+5
                        owned, app = app, None
                        cleanup(owned, cleanup_deadline or time.monotonic()+5)
                row['x_client'] = observed
                require(observed['client_pixels'] == [w*scale, h*scale], 'native client dimensions')
                row['decoded_rgba_sha256'] = png(image, (w*scale, h*scale))
                sidecar(stem.with_suffix('.json'), case, manifest['head'])
                require(time.monotonic() < start+720, '12 minute campaign budget')
                row.update(status='captured_uninspected', **{suffix[1:]+'_sha256': digest(stem.with_suffix(suffix)) for suffix in ('.png', '.json', '.log')})
                save()
            require(hashes == {p: digest(Path(p)) for p in hashes}, 'fixtures changed')
            require(digest(binary) == manifest['binary_sha256'], 'binary changed')
            complete(folder, expected)
            manifest['fixture_after_sha256'] = hashes
            manifest['status'] = 'captured_uninspected'; save()
    except Exception as error:
        manifest['error'] = str(error)
        manifest['error_type'] = type(error).__name__
        if manifest['cases'] and manifest['cases'][-1]['status'] == 'incomplete':
            manifest['cases'][-1]['error'] = str(error)
            if isinstance(error, (TimeoutError, S.TimeoutExpired)):
                manifest['cases'][-1]['timeout'] = True
        raise
    finally:
        active_error = sys.exc_info()[0] is not None
        cleanup_errors = []
        cleanup_deadline = cleanup_deadline or (readout.failure_deadline if readout is not None else None) or time.monotonic()+5
        for label, action in [('Studio', lambda: cleanup(app, cleanup_deadline)),
                              ('X observer', lambda: readout.close(cleanup_deadline) if readout is not None else None),
                              ('Xvfb', lambda: cleanup(xvfb, cleanup_deadline))]:
            try:
                action()
            except Exception as error:
                cleanup_errors.append(label+': '+str(error))
        for key, value in original_x.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        manifest['unattempted'] = expected[len(manifest['cases']):]
        if cleanup_errors:
            manifest.update(status='incomplete', cleanup_errors=cleanup_errors)
        save()
        if cleanup_errors and not active_error:
            raise RuntimeError('; '.join(cleanup_errors))


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] == '--internal-observer' and sys.argv[2] in ('root', 'studio'):
        observer_main(sys.argv[2])
    else:
        require(len(sys.argv) == 3 and not sys.argv[1].startswith('--'), 'binary and output root required')
        run(Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve())
