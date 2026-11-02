#!/usr/bin/env python3
"""Fixed-purpose hosted facility/frame probe. NO native gesture implementation.
Only facilities and capture are public entry points. Internal workers are killable.
No application screenshot API, selectors, coordinates, replay or semantic oracle.
"""
import ctypes as C
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import re
import resource
import secrets
import select
import signal
import stat
import struct
import subprocess
import sys
import time
import zlib

SCHEMA = 'beatbox.native-frame-probe.v1'
BASE_HEAD = '577ef9f56248a11c948240bf9bc32c8494f17ccc'
BASE_TREE = '6653887d8fa749950d1177dd60ce31fd3e914417'
RECEIPT_CAP = 65536
READ_CAP = 8 * 1024 * 1024
FRAME_CAP = 32 * 1024 * 1024
CAMPAIGN_SECONDS = 90
CHILD_SECONDS = 3
CLEANUP_SECONDS = 5
READS = [('get_history', {'limit': 500, 'errors_only': False}),
         ('get_project', {'detail': 'full'}),
         ('history', {'action': 'history', 'limit': 100}),
         ('get_history', {'limit': 500, 'errors_only': False}),
         ('get_project', {'detail': 'full'}),
         ('history', {'action': 'history', 'limit': 100}),
         ('get_history', {'limit': 500, 'errors_only': False})]
CASES = ['focus', 'filter_mode', 'osc1', 'osc2', 'inspector_scroll',
         'knob_drag_release_undo', 'song_scroll', 'mix_final_channel',
         'floating_ownership', 'save_reopen', 'ordinary_close_guard']


def require(ok, message):
    if not ok:
        raise RuntimeError(message)


def integer(value, lo, hi):
    require(type(value) is int and lo <= value <= hi, 'integer outside bounds')
    return value


def exact(value, keys):
    require(type(value) is dict and set(value) == set(keys), 'unexpected receipt keys')
    return value


def strict(data, cap=RECEIPT_CAP):
    require(len(data) <= cap, 'JSON byte cap')
    def pairs(items):
        result = {}
        for key, val in items:
            require(key not in result, 'duplicate JSON key')
            result[key] = val
        return result
    def constant(_):
        raise RuntimeError('nonfinite JSON')
    def finite(value):
        number=float(value)
        require(math.isfinite(number),'nonfinite numeric JSON')
        return number
    return json.loads(data, object_pairs_hook=pairs, parse_constant=constant, parse_float=finite)


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()


def put(path, value, cap=RECEIPT_CAP):
    data = (json.dumps(value, sort_keys=True, allow_nan=False, indent=2) + '\n').encode()
    require(len(data) <= cap, 'output receipt byte cap')
    # Never replace evidence, including an attempted operation's marker.
    with Path(path).open('xb') as f:
        f.write(data); f.flush(); os.fsync(f.fileno())
    fd = os.open(str(Path(path).parent), os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def birth(pid):
    text = Path(f'/proc/{pid}/stat').read_text()
    fields = text[text.rfind(')') + 2:].split()
    return {'pid': pid, 'start_ticks': int(fields[19]), 'state': fields[0],
            'pgrp': int(fields[2])}


def same_process(expected, live=True):
    got = birth(expected['pid'])
    require(got['start_ticks'] == expected['start_ticks'] and got['pgrp'] == expected['pgrp'],
            'process birth/group changed')
    if live:
        require(got['state'] not in ('Z', 'X'), 'owned process exited')
    return got


def sockets(pid):
    out = set()
    for fd in Path(f'/proc/{pid}/fd').iterdir():
        try:
            target = os.readlink(fd)
        except FileNotFoundError:
            continue
        match = re.fullmatch(r'socket:\[(\d+)\]', target)
        if match:
            out.add(match[1])
    return out


def listener_rows(text, port):
    rows = []
    for line in text.splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 10 and fields[3] == '0A' and int(fields[1].split(':')[1], 16) == port:
            rows.append({'address': fields[1].split(':')[0], 'inode': fields[9]})
    return rows


def listener(app, port):
    same_process(app)
    rows = listener_rows(Path('/proc/net/tcp').read_text(), port)
    other = listener_rows(Path('/proc/net/tcp6').read_text(), port)
    require(len(rows) == 1 and not other and rows[0]['address'] == '0100007F',
            'ambiguous or non-loopback listener')
    require(rows[0]['inode'] in sockets(app['pid']), 'listener not owned by exact app')
    same_process(app)
    return rows[0]


def display_owner(xvfb, number):
    same_process(xvfb)
    name = f'/tmp/.X11-unix/X{number}'
    require(stat.S_ISSOCK(os.stat(name).st_mode), 'X socket absent')
    rows = [line.split() for line in Path('/proc/net/unix').read_text().splitlines()[1:]]
    matched = [r for r in rows if len(r) == 8 and r[7] == name]
    require(len(matched) == 1 and matched[0][6] in sockets(xvfb['pid']), 'X socket ownership')
    # Xvfb explicitly has -nolisten tcp; reject any TCP listener owned by it.
    owned = sockets(xvfb['pid'])
    for table in ('tcp', 'tcp6'):
        for line in Path('/proc/net/' + table).read_text().splitlines()[1:]:
            f = line.split()
            require(not (f[3] == '0A' and f[9] in owned), 'X server has TCP listener')
    same_process(xvfb)
    return {'display': ':' + str(number), 'unix_inode': matched[0][6],
            'socket_device': os.stat(name).st_dev, 'socket_inode': os.stat(name).st_ino,
            'tcp': False}


def limits():
    resource.setrlimit(resource.RLIMIT_FSIZE, (READ_CAP, READ_CAP))
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))


class Supervisor:
    def __init__(self, out):
        self.out = out
        self.deadline = time.monotonic() + CAMPAIGN_SECONDS
        self.children = []
        self.sequence = 0

    def remaining(self, maximum=CHILD_SECONDS):
        remaining = self.deadline - time.monotonic()
        require(remaining > 0, 'campaign deadline')
        return min(maximum, remaining)

    def start(self, label, argv, env=None, pass_fds=()):
        self.remaining()
        put(self.out / (label + '.attempt.json'), {'schema': SCHEMA, 'label': label,
            'sequence': self.sequence, 'argv': argv, 'monotonic_ns': time.monotonic_ns()})
        self.sequence += 1
        # Block delivery through Popen + registration. The child restores the inherited
        # mask before exec. No wait/poll/reap occurs before ownership signaling.
        blocked = (signal.SIGINT, signal.SIGTERM)
        old_mask = signal.pthread_sigmask(signal.SIG_BLOCK, blocked)
        def child_setup():
            signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
            limits()
        try:
            with (self.out / (label + '.log')).open('xb') as log:
                p = subprocess.Popen(argv, env=env, stdout=log, stderr=subprocess.STDOUT,
                                     stdin=subprocess.DEVNULL, start_new_session=True,
                                     pass_fds=pass_fds, preexec_fn=child_setup)
                row = {'p':p, 'birth':None, 'label':label, 'reaped':False, 'pidfd':None}
                self.children.append(row)  # first operation after returned Popen
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, old_mask)
        # Partial row survives pending-signal delivery, pidfd failure and birth failure.
        row['pidfd'] = os.pidfd_open(p.pid)
        ident = birth(p.pid)
        require(ident['pgrp'] == p.pid, 'child not separate process group')
        row['birth'] = ident
        return p, ident

    def wait(self, p, maximum=CHILD_SECONDS):
        code = p.wait(timeout=self.remaining(maximum))
        next(row for row in self.children if row['p'] is p)['reaped'] = True
        require(code == 0, 'child failed: ' + str(code))

    def worker(self, kind, spec):
        label = f'{self.sequence:02d}-{kind}'
        args_path = self.out / (label + '.args.json')
        result_path = self.out / (label + '.result.json')
        put(args_path, spec)
        p, _ = self.start(label, [sys.executable, str(Path(__file__).resolve()),
                                '_worker', kind, str(args_path), str(result_path)], spec.get('env'))
        self.wait(p)
        return strict(result_path.read_bytes())

    def cleanup(self):
        # Direct-child reap only; owned groups best effort, NOT descendant emptiness.
        # pidfds bind direct-child signals, not killpg. Leaders remain unreaped
        # until wait; a birth check is not an atomic group-signaling guarantee.
        end = time.monotonic() + CLEANUP_SECONDS
        handlers = {}; errors = []
        def error(code):
            errors.append(code)  # no raw exception/credential-bearing diagnostic text
        try:
            for sig in (signal.SIGTERM, signal.SIGINT):
                try: handlers[sig] = signal.signal(sig, signal.SIG_IGN)
                except Exception: error('handler_install_failed')
            pending = [r for r in reversed(self.children) if not r['reaped']]
            for row in pending:
                if row['birth'] is None: error('identity_unacquired')
            for sig in (signal.SIGTERM, signal.SIGKILL):
                for row in pending:
                    if row['reaped']: continue
                    try:
                        if row['birth'] is not None:
                            same_process(row['birth'], live=False)
                            os.killpg(row['p'].pid, sig)
                        elif row['pidfd'] is not None and hasattr(signal, 'pidfd_send_signal'):
                            # No discovered PID/group authority when identity is missing.
                            signal.pidfd_send_signal(row['pidfd'], sig)
                    except (ProcessLookupError, FileNotFoundError):
                        pass
                    except Exception: error('owned_signal_failed')
                phase_end = min(end, time.monotonic() + 0.75) if sig == signal.SIGTERM else end
                for row in pending:
                    if row['reaped']: continue
                    try:
                        row['p'].wait(timeout=max(0, phase_end - time.monotonic()))
                        row['reaped'] = True
                    except subprocess.TimeoutExpired: pass
                    except Exception: error('wait_failed')
        finally:
            for row in self.children:
                if row['pidfd'] is not None:
                    try: os.close(row['pidfd'])
                    except Exception: error('pidfd_close_failed')
                    row['pidfd'] = None
            # Restore even if receipt writing or another finalizer fails.
            for sig, handler in handlers.items():
                try: signal.signal(sig, handler)
                except Exception: error('handler_restore_failed')
        result = {'status':'reaped' if not errors and all(r['reaped'] for r in self.children) else 'failed',
                  'claim':'direct_children_reaped_groups_best_effort_not_descendant_emptiness',
                  'children':[{'label':r['label'], 'pid':r['p'].pid,
                               'start_ticks':None if r['birth'] is None else r['birth']['start_ticks'],
                               'reaped':r['reaped'], 'exit_code':r['p'].returncode} for r in self.children],
                  'errors':errors, 'budget_seconds':CLEANUP_SECONDS}
        try: put(self.out / 'cleanup.json', result)
        except Exception:
            result['status'] = 'failed'; error('cleanup_receipt_failed')
            try: put(self.out / 'cleanup-finalization-failure.json', result)
            except Exception: pass
        return result['status'] == 'reaped'


def encode_frame(raw, width, height, stride):
    # Pure fixed framebuffer decoder, not a widget oracle or target locator.
    integer(width,1,1480); integer(height,1,920); integer(stride,width*4,width*4+256)
    require(len(raw)==stride*height and len(raw)<=FRAME_CAP,'raw frame length')
    scan=bytearray()
    for y in range(height):
        row=raw[y*stride:y*stride+width*4]
        rgb=bytearray(width*3); rgb[0::3]=row[2::4]; rgb[1::3]=row[1::4]; rgb[2::3]=row[0::4]
        scan.extend(b'\x00'); scan.extend(rgb)
    def chunk(kind, data):
        return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
    png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',width,height,8,2,0,0,0))
    return png+chunk(b'IDAT',zlib.compress(scan,3))+chunk(b'IEND',b'')


def checked_facilities(row):
    exact(row,['schema','kind','tools','libraries','kernel','python','pidfd_open',
               'native_execution','installed_nothing'])
    require(row['schema']==SCHEMA and row['kind']=='installed_facilities' and
            row['native_execution'] is False and row['installed_nothing'] is True,'facility identity')
    exact(row['tools'],['Xvfb','xauth','cargo','rustc','pkg-config'])
    require(all(v is None or type(v) is str for v in row['tools'].values()),'facility tool types')
    exact(row['libraries'],['X11','Xtst','GL','EGL','asound'])
    for val in row['libraries'].values():
        exact(val,['soname','loadable','load_error','mapped_paths'])
        require((val['soname'] is None or type(val['soname']) is str) and
                type(val['loadable']) is bool and
                (val['load_error'] is None or type(val['load_error']) is str) and
                type(val['mapped_paths']) is list and len(val['mapped_paths'])<=8 and
                all(type(path) is str and len(path)<=4096 for path in val['mapped_paths']),
                'facility library types')
    require(type(row['pidfd_open']) is bool and type(row['kernel']) is str and
            type(row['python']) is str,'facility metadata types')
    return row


def x_worker(spec):
    # All X calls, even library load/property/frame access, are confined here.
    exact(spec, ['env', 'app', 'xvfb', 'display_number', 'frame_path'])
    same_process(spec['app']); display_owner(spec['xvfb'], spec['display_number'])
    x = C.CDLL('libX11.so.6')
    P = C.c_void_p; U = C.c_ulong; I = C.c_int
    def bind(name, result, args):
        f = getattr(x, name); f.restype = result; f.argtypes = args; return f
    open_display = bind('XOpenDisplay', P, [C.c_char_p])
    d = open_display(spec['env']['DISPLAY'].encode()); require(d, 'cannot open owned display')
    root = bind('XDefaultRootWindow', U, [P])(d)
    intern = bind('XInternAtom', U, [P, C.c_char_p, I])
    prop = bind('XGetWindowProperty', I, [P,U,U,C.c_long,C.c_long,I,U,C.POINTER(U),
                C.POINTER(I),C.POINTER(U),C.POINTER(U),C.POINTER(P)])
    free = bind('XFree', I, [P])
    def property_value(window, name, expected_type, expected_format, max_items):
        atom = intern(d, name.encode(), 1); require(atom, 'property atom absent')
        typ=U(); fmt=I(); count=U(); rest=U(); data=P()
        rc=prop(d,window,atom,0,max_items,0,expected_type,C.byref(typ),C.byref(fmt),
                C.byref(count),C.byref(rest),C.byref(data))
        try:
            require(rc==0 and typ.value==expected_type and fmt.value==expected_format and
                    0<count.value<=max_items and rest.value==0, 'window property invalid: '+name)
            if expected_format==32:
                return list(C.cast(data,C.POINTER(U))[:count.value])
            return C.string_at(data,count.value).decode('utf-8',errors='strict')
        finally:
            if data: free(data)
    parent=U(); rr=U(); children=C.POINTER(U)(); count=C.c_uint()
    query=bind('XQueryTree',I,[P,U,C.POINTER(U),C.POINTER(U),C.POINTER(C.POINTER(U)),C.POINTER(C.c_uint)])
    require(query(d,root,C.byref(rr),C.byref(parent),C.byref(children),C.byref(count)), 'query tree')
    require(count.value<=64, 'unexpected root child count')
    wins=list(children[:count.value]); free(children)
    matches=[]
    pid_atom=intern(d,b'_NET_WM_PID',1)
    require(pid_atom, 'no native PID property')
    # Missing properties on unrelated children do not create title-based fallbacks.
    for win in wins:
        try:
            pid=property_value(win,'_NET_WM_PID',6,32,1)
        except RuntimeError:
            continue
        if pid==[spec['app']['pid']]: matches.append(win)
    require(len(matches)==1, 'owned native client missing or ambiguous')
    win=matches[0]
    utf8=intern(d,b'UTF8_STRING',1); require(utf8, 'no UTF8 atom')
    title=property_value(win,'_NET_WM_NAME',utf8,8,128)
    require(title=='Beatbox Studio', 'unexpected exact owned window title')
    geometry=bind('XGetGeometry',I,[P,U,C.POINTER(U),C.POINTER(I),C.POINTER(I),
                    C.POINTER(C.c_uint),C.POINTER(C.c_uint),C.POINTER(C.c_uint),C.POINTER(C.c_uint)])
    def geom():
        r=U(); xx=I(); yy=I(); w=C.c_uint(); h=C.c_uint(); border=C.c_uint(); depth=C.c_uint()
        require(geometry(d,win,C.byref(r),C.byref(xx),C.byref(yy),C.byref(w),C.byref(h),
                         C.byref(border),C.byref(depth)), 'client geometry unavailable')
        require(r.value==root and (w.value,h.value)==(1480,920) and depth.value==24,
                'ordinary initial geometry/depth differs; do not infer scale')
        return {'x':xx.value,'y':yy.value,'width':w.value,'height':h.value,
                'border':border.value,'depth':depth.value,'root':r.value}
    before=geom()
    # XGetImage on a nonviewable client errors in this killable worker; no fallback.
    class XImage(C.Structure):
        _fields_=[('width',I),('height',I),('xoffset',I),('format',I),('data',P),
                  ('byte_order',I),('bitmap_unit',I),('bitmap_bit_order',I),('bitmap_pad',I),
                  ('depth',I),('bytes_per_line',I),('bits_per_pixel',I),('red_mask',U),
                  ('green_mask',U),('blue_mask',U),('obdata',P)]
    getimage=bind('XGetImage',C.POINTER(XImage),[P,U,I,I,C.c_uint,C.c_uint,U,I])
    started=time.monotonic_ns()
    image=getimage(d,win,0,0,1480,920,U(-1).value,2); require(image, 'live XGetImage failed')
    try:
        im=image.contents
        require((im.width,im.height,im.format,im.xoffset,im.byte_order,im.depth,im.bits_per_pixel,
                 im.red_mask,im.green_mask,im.blue_mask)==(1480,920,2,0,0,24,32,0xff0000,0xff00,0xff),
                'unsupported framebuffer format; no guessed decoder')
        require(1480*4<=im.bytes_per_line<=1480*4+256, 'frame stride')
        require(im.bytes_per_line*920<=FRAME_CAP, 'raw frame cap')
        raw=C.string_at(im.data,im.bytes_per_line*920)
        png=encode_frame(raw,1480,920,im.bytes_per_line)
        require(len(png)<=FRAME_CAP, 'PNG frame cap')
        path=Path(spec['frame_path'])
        with path.open('xb') as f:
            f.write(png); f.flush(); os.fsync(f.fileno())
    finally:
        bind('XDestroyImage',I,[C.POINTER(XImage)])(image)
    focus=U(); revert=I()
    bind('XGetInputFocus',I,[P,C.POINTER(U),C.POINTER(I)])(d,C.byref(focus),C.byref(revert))
    extension=bind('XQueryExtension',I,[P,C.c_char_p,C.POINTER(I),C.POINTER(I),C.POINTER(I)])
    op=I(); event=I(); error=I()
    xtest_extension=bool(extension(d,b'XTEST',C.byref(op),C.byref(event),C.byref(error)))
    after=geom(); require(after==before,'geometry changed across frame')
    require(property_value(win,'_NET_WM_PID',6,32,1)==[spec['app']['pid']], 'native PID drift')
    same_process(spec['app']); display_owner(spec['xvfb'],spec['display_number'])
    bind('XCloseDisplay',I,[P])(d)
    return {'schema':SCHEMA,'kind':'same_window_frame','window':win,'title':title,
            'app':spec['app'],'geometry_before':before,'geometry_after':after,
            'focus_window':focus.value,'focus_revert':revert.value,
            'xtest_extension':xtest_extension,'frame_sha256':digest(path),
            'frame_bytes':len(png),'capture_started_ns':started,'capture_finished_ns':time.monotonic_ns(),
            'visual_status':'executed_unreviewed','native_gestures':'UNRUN',
            'pixel_source':'XGetImage exact ordinary app client, not screenshot-tool Studio'}


def facilities_worker(spec):
    exact(spec, ['env'])
    import shutil
    tools = {name: shutil.which(name) for name in ('Xvfb','xauth','cargo','rustc','pkg-config')}
    libs = {}
    # Fixed Linux sonames are attempted, not presumed installed. No find_library:
    # that helper may invoke compiler/linker fallbacks, contrary to this read-only probe.
    sonames={'X11':'libX11.so.6','Xtst':'libXtst.so.6','GL':'libGL.so.1',
             'EGL':'libEGL.so.1','asound':'libasound.so.2'}
    for name,found in sonames.items():
        loaded=False; error=None
        try:
            C.CDLL(found); loaded=True
        except OSError as exc:
            error=str(exc)[:512]
        stem=found.split('.so')[0]+'.so'
        mapped=sorted({line.split()[-1] for line in Path('/proc/self/maps').read_text().splitlines()
                       if '/' in line and Path(line.split()[-1]).name.startswith(stem)})
        libs[name]={'soname':found,'loadable':loaded,'load_error':error,'mapped_paths':mapped}
    has_pidfd=False
    if hasattr(os,'pidfd_open'):
        try:
            fd=os.pidfd_open(os.getpid()); os.close(fd); has_pidfd=True
        except OSError:
            pass
    return {'schema':SCHEMA,'kind':'installed_facilities','tools':tools,'libraries':libs,
            'kernel':os.uname().release,'python':sys.version,'pidfd_open':has_pidfd,
            'native_execution':False,'installed_nothing':True}


def http_worker(spec):
    exact(spec,['env','app','port','index','body_path'])
    i=integer(spec['index'],0,6); port=integer(spec['port'],40000,60000)
    before=listener(spec['app'],port)
    name,args=READS[i]
    payload=json.dumps({'name':name,'arguments':args}).encode()
    conn=http.client.HTTPConnection('127.0.0.1',port,timeout=1.5)
    try:
        conn.request('POST','/call',body=payload,headers={'Content-Type':'application/json','Connection':'close'})
        response=conn.getresponse(); require(response.status==200,'HTTP status')
        body=response.read(READ_CAP+1)
        doc=strict(body,READ_CAP); exact(doc,['ok','result']); require(doc['ok'] is True,'read failed')
        require(type(doc['result']) is dict,'non-object tool result')
        with Path(spec['body_path']).open('xb') as f:
            f.write(body); f.flush(); os.fsync(f.fileno())
    finally:
        conn.close()
    require(listener(spec['app'],port)==before,'listener changed during read')
    return {'schema':SCHEMA,'kind':'existing_http_read','index':i,'name':name,'arguments':args,
            'source':'ai','listener':before,'body_sha256':digest(spec['body_path']),
            'body_bytes':len(body),'self_log_entry':'appended after returned result; retained and accounted'}


def validate_ledger(documents):
    require(len(documents)==7,'exact seven reads required')
    h0,p0,u0,h1,p1,u1,h2=documents
    for h in (h0,h1,h2):
        exact(h,['revision','undo_depth','redo_depth','log_size','snapshots','entries'])
        integer(h['revision'],0,2**53); integer(h['log_size'],0,499)
        integer(h['undo_depth'],0,99); integer(h['redo_depth'],0,99)
        require(h['snapshots']==[], 'fresh fixture must have no snapshots')
        require(type(h['entries']) is list and len(h['entries'])==h['log_size'],'log truncated')
        for seq,e in zip(range(h['log_size']-1,-1,-1),h['entries']):
            exact(e,['seq','tool','args','ok','source']); require(type(e['seq']) is int and e['seq']==seq,'log seq')
            require(type(e['tool']) is str and type(e['args']) is str and type(e['source']) is str
                    and type(e['ok']) is bool,'entry types')
    for u in (u0,u1):
        exact(u,['done','undone','can_undo','can_redo'])
        integer(u['can_undo'],0,99); integer(u['can_redo'],0,99)
        require(type(u['done']) is list and type(u['undone']) is list and
                len(u['done'])==u['can_undo'] and len(u['undone'])==u['can_redo'] and
                all(type(s) is str for s in u['done']+u['undone']), 'history truncated/types')
    require(type(p0) is dict and type(p1) is dict and p0 and p1, 'project object required')
    require(p0==p1 and u0==u1,'project/history changed without gestures')
    require(all((h['revision'],h['undo_depth'],h['redo_depth'])==
                (h0['revision'],u0['can_undo'],u0['can_redo']) for h in (h0,h1,h2)), 'ledger drift')
    for a,b in ((h0,h1),(h1,h2)):
        require(b['log_size']==a['log_size']+3 and b['entries'][3:]==a['entries'],'unexpected log changes')
        require([(e['tool'],e['source'],e['ok']) for e in b['entries'][:3]]==
                [('history','ai',True),('get_project','ai',True),('get_history','ai',True)],'unaccounted reads')
        expected = [{'limit':100}, {'detail':'full'}, {'limit':500,'errors_only':False}]
        for entry, args in zip(b['entries'][:3], expected):
            parsed = strict(entry['args'].encode(), 140)
            exact(parsed, args)
            require(all(type(parsed[k]) is type(v) and parsed[k] == v for k,v in args.items()),
                    'unexpected normalized terminal read arguments')
    return {'status':'finite_read_accounting_checked','http_calls':7,'observed_appended_reads':6,
            'final_get_history_append':'expected but not recursively observed',
            'returned_project_objects_equal_not_schema_validated':True,'public_history_equal':True,'revision':h0['revision'],
            'atomic_frame_ledger_snapshot':False,'native_edit_undo_verified':False}


def checked_frame(row, app, frame_path):
    exact(row,['schema','kind','window','title','app','geometry_before','geometry_after','focus_window',
               'focus_revert','xtest_extension','frame_sha256','frame_bytes','capture_started_ns',
               'capture_finished_ns','visual_status','native_gestures','pixel_source'])
    require(row['schema']==SCHEMA and row['kind']=='same_window_frame' and row['app']==app,
            'frame identity receipt')
    integer(row['window'],1,2**64-1); integer(row['frame_bytes'],1,FRAME_CAP)
    integer(row['capture_started_ns'],1,2**63-1)
    integer(row['capture_finished_ns'],row['capture_started_ns'],2**63-1)
    require(row['visual_status']=='executed_unreviewed' and row['native_gestures']=='UNRUN', 'verdict escalation')
    require(type(row['xtest_extension']) is bool and row['title']=='Beatbox Studio', 'frame types/title')
    integer(row['focus_window'],0,2**64-1); integer(row['focus_revert'],0,2)
    g=row['geometry_before']
    for geometry in (g,row['geometry_after']):
        exact(geometry,['x','y','width','height','border','depth','root'])
        for key in geometry: integer(geometry[key],-32768 if key in ('x','y') else 0,2**64-1)
    require(g==row['geometry_after'] and (g['width'],g['height'],g['depth'])==(1480,920,24), 'geometry receipt')
    require(row['frame_sha256']==digest(frame_path) and row['frame_bytes']==Path(frame_path).stat().st_size,
            'frame bytes mismatch')
    require(row['pixel_source']=='XGetImage exact ordinary app client, not screenshot-tool Studio', 'frame source')
    return row


def close_descriptors(fds):
    failed = False
    for fd in fds:
        try: os.close(fd)
        except Exception: failed = True
    require(not failed, 'descriptor close failed')


def clean_environment(home):
    # Deliberate allowlist; no inherited DISPLAY, auth, app capture selectors, proxies or user state.
    return {'PATH':os.environ.get('PATH','/usr/bin:/bin'),'LANG':'C.UTF-8','HOME':str(home),
            'XDG_CONFIG_HOME':str(home/'config'),'XDG_CACHE_HOME':str(home/'cache'),
            'XDG_DATA_HOME':str(home/'data'),'XDG_STATE_HOME':str(home/'state'),
            'XDG_RUNTIME_DIR':str(home/'runtime'),'LIBGL_ALWAYS_SOFTWARE':'1',
            'WINIT_UNIX_BACKEND':'x11'}


def capture(out, binary, binding):
    supervisor=Supervisor(out); result={'schema':SCHEMA,'status':'failed','source':binding,
        'visual_status':'UNRUN','native_gestures':'UNRUN','cases':{case:'UNRUN' for case in CASES},
        'semantic_pass':False,'error':None,'build':None,'facilities':None,'fixture':None,
        'display':None,'listener':None,'app':None,'xvfb':None,'frame':None,'reads':[],
        'ledger':None,'bounds':{'campaign_seconds':90,'child_seconds':3,
        'cleanup_seconds':5,'http_calls':7,'frames':1,'native_gestures':0}}
    try:
        # No binary guessing or artifact download. Hosted build record is mandatory.
        build=strict((out/'build.json').read_bytes())
        exact(build,['schema','head','tree','binary_sha256','rustc'])
        require(build['schema']==SCHEMA and build['head']==binding['head'] and
                build['tree']==binding['tree'] and build['binary_sha256']==digest(binary),'binary/source binding')
        result['build']=build
        scratch=out/'owned'; scratch.mkdir(mode=0o700)
        home=scratch/'home'; home.mkdir(mode=0o700)
        for sub in ('config','cache','data','state','runtime'):
            (home/sub).mkdir(mode=0o700)
        env=clean_environment(home)
        facility=supervisor.worker('facilities',{'env':env})
        checked_facilities(facility)
        result['facilities']=facility
        require(all(facility['tools'][n] for n in ('Xvfb',)), 'missing installed Xvfb')
        require(facility['libraries']['X11']['loadable'], 'missing X11')
        # Existing small recorded fixture, copied verbatim; no fixture tools or musical preparation calls.
        source=Path('examples/beats/rt_lofi.json')
        require(source.stat().st_size<=READ_CAP,'fixture cap')
        fixture=scratch/'project.json'; fixture.write_bytes(source.read_bytes())
        result['fixture']={'source':str(source),'sha256':digest(source),'scratch_sha256':digest(fixture)}
        # Xorg reads this exact fresh server auth; FamilyWild avoids guessing the display number.
        auth=scratch/'Xauthority'; cookie=secrets.token_bytes(16)
        def field(data): return struct.pack('>H',len(data))+data
        auth.write_bytes(struct.pack('>H',65535)+field(b'')+field(b'')+
                         field(b'MIT-MAGIC-COOKIE-1')+field(cookie)); auth.chmod(0o600)
        env['XAUTHORITY']=str(auth.resolve())
        rfd,wfd=os.pipe()
        try:
            os.set_blocking(rfd,False)
            xvfb,xid=supervisor.start('xvfb',[facility['tools']['Xvfb'],'-displayfd',str(wfd),
                '-screen','0','1600x1000x24','-dpi','96','-nolisten','tcp','-auth',str(auth.resolve()),
                '-noreset'],env,(wfd,))
            os.close(wfd); wfd=-1
            ready=select.select([rfd],[],[],supervisor.remaining())[0]
            require(ready,'X displayfd deadline')
            number_data=os.read(rfd,32)
            require(re.fullmatch(rb'[0-9]{1,5}\n',number_data) is not None,'displayfd exact record')
            number=integer(int(number_data),0,65535)
        finally:
            close_descriptors([rfd] + ([wfd] if wfd!=-1 else []))
        env['DISPLAY']=':'+str(number)
        result['display']=display_owner(xid,number)
        port=40000+secrets.randbelow(20001)  # No free-port precheck trusted as ownership.
        app,aid=supervisor.start('studio',[str(binary),'--workdir',str(scratch.resolve()),
            'studio',str(fixture.resolve()),'--listen','127.0.0.1:'+str(port)],env)
        # One scheduled readiness observation, not adaptive polling or action retry.
        time.sleep(supervisor.remaining(2))
        same_process(aid)
        require(Path(f'/proc/{aid["pid"]}/exe').resolve()==binary and
                digest(f'/proc/{aid["pid"]}/exe')==build['binary_sha256'],'app executable ownership')
        result['listener']=listener(aid,port)
        result['app']=aid; result['xvfb']=xid
        docs=[]; reads=[]
        for i in range(7):
            if i==4:
                display_owner(xid,number); listener(aid,port)
                frame=out/'current-client.png'
                row=supervisor.worker('frame',{'env':env,'app':aid,'xvfb':xid,
                    'display_number':number,'frame_path':str(frame.resolve())})
                result['frame']=checked_frame(row,aid,frame)
                result['visual_status']='executed_unreviewed'
            body=out/f'read-{i}.json'
            row=supervisor.worker('http',{'env':env,'app':aid,'port':port,'index':i,'body_path':str(body.resolve())})
            exact(row,['schema','kind','index','name','arguments','source','listener','body_sha256','body_bytes','self_log_entry'])
            require(row['schema']==SCHEMA and row['kind']=='existing_http_read' and row['index']==i and
                    (row['name'],row['arguments'])==READS[i] and row['source']=='ai' and
                    row['listener']==result['listener'] and row['body_sha256']==digest(body) and
                    row['body_bytes']==body.stat().st_size, 'read receipt mismatch')
            integer(row['index'],i,i); integer(row['body_bytes'],1,READ_CAP)
            docs.append(strict(body.read_bytes(),READ_CAP)['result']); reads.append(row)
        result['reads']=reads; result['ledger']=validate_ledger(docs)
        require(digest(binary)==build['binary_sha256'] and digest(fixture)==result['fixture']['sha256'] and
                digest(f'/proc/{aid["pid"]}/exe')==build['binary_sha256'], 'binary/fixture/executable changed')
        display_owner(xid,number); listener(aid,port)
        supervisor.remaining()
        result['status']='probe_complete_not_semantic_pass'
    except Exception as error:
        result['error']=type(error).__name__+': '+str(error)
    finally:
        try:
            cleaned=supervisor.cleanup()
        except Exception:
            cleaned=False
        result['cleanup']='reaped' if cleaned else 'failed'
        if not cleaned: result['status']='failed'
        result['finished_monotonic_ns']=time.monotonic_ns()
        try:
            checked_final(result); put(out/'receipt.json',result)
        except Exception:
            result['status']='failed'
            try:
                put(out/'receipt-finalization-failure.json',
                    {'schema':SCHEMA,'status':'failed','cleanup':result['cleanup'],
                     'semantic_pass':False,'native_gestures':'UNRUN'})
            except Exception: pass
    return result['status']=='probe_complete_not_semantic_pass'


def checked_final(row):
    # Verdict/envelope guard ONLY. Not a recursive public evidence validator.
    # Nested values are validated as constructed by capture; consumers must verify
    # source bindings, PNG decoding, reads and provenance independently.
    exact(row,['schema','status','source','visual_status','native_gestures','cases','semantic_pass',
               'error','bounds','build','facilities','fixture','display','listener','app','xvfb',
               'frame','reads','ledger','cleanup','finished_monotonic_ns'])
    require(row['schema']==SCHEMA and row['status'] in ('failed','probe_complete_not_semantic_pass'), 'final identity')
    require(row['semantic_pass'] is False and row['native_gestures']=='UNRUN', 'no semantic/input pass allowed')
    require(row['visual_status'] in ('UNRUN','executed_unreviewed'), 'visual review escalation')
    exact(row['cases'],CASES); require(all(v=='UNRUN' for v in row['cases'].values()),'gesture status escalation')
    require(row['bounds']=={'campaign_seconds':90,'child_seconds':3,'cleanup_seconds':5,
                            'http_calls':7,'frames':1,'native_gestures':0}, 'bounds drift')
    for key,value in row['bounds'].items():
        integer(value,row['bounds'][key],row['bounds'][key])
    integer(row['finished_monotonic_ns'],1,2**63-1)
    require(row['error'] is None or type(row['error']) is str,'error type')
    if row['status']=='probe_complete_not_semantic_pass':
        require(row['cleanup']=='reaped' and row['error'] is None and
                row['visual_status']=='executed_unreviewed' and len(row['reads'])==7 and
                all(row[key] is not None for key in ('build','facilities','fixture','display',
                    'listener','app','xvfb','frame','ledger')), 'incomplete success receipt')
    return row


def binding():
    # Exact NEW head/tree only. base_* constants are assertions, NOT ancestry proof.
    def git(*args):
        return subprocess.check_output(['git',*args],timeout=3,stderr=subprocess.DEVNULL).decode().strip()
    head=git('rev-parse','HEAD'); tree=git('rev-parse','HEAD^{tree}')
    require(re.fullmatch('[0-9a-f]{40}',os.environ.get('REVIEWED_HEAD','')) and
            re.fullmatch('[0-9a-f]{40}',os.environ.get('REVIEWED_TREE','')),'exact reviewed source required')
    require(head==os.environ['REVIEWED_HEAD'] and tree==os.environ['REVIEWED_TREE'], 'reviewed source mismatch')
    require(git('status','--porcelain','--untracked-files=all')=='','dirty source checkout')
    return {'head':head,'tree':tree,'base_head':BASE_HEAD,'base_tree':BASE_TREE,
            'runner_sha256':digest(__file__),'workflow_sha256':digest('.github/workflows/p0a-native-frame-probe.yml')}


def real_toolchain(home):
    # No rustup proxy is invoked, even for --version; exact installed Linux host.
    root = (Path(home)/'toolchains'/'1.99.0-x86_64-unknown-linux-gnu').resolve()
    tools = {}
    for name in ('rustc','cargo','rustdoc'):
        path = root/'bin'/name
        require(path.is_file() and os.access(path,os.X_OK) and
                path.resolve()==path and path.name!='rustup', 'exact real toolchain absent')
        proxy = Path(home).parent/'.cargo/bin/rustup'
        require(not (proxy.exists() and os.path.samefile(path,proxy)), 'rustup proxy rejected')
        tools[name] = str(path)
    return tools


def hosted_build(out):
    # Hosted ONLY. Existing approved preparation runs separately in workflow.
    # No installation/fallback here. Diagnostics are private and never uploaded.
    try:
        source = strict((out/'source.json').read_bytes())
        require(source==binding(), 'prebuild binding changed')
        tools=real_toolchain(os.environ.get('RUSTUP_HOME',str(Path.home()/'.rustup')))
        env=os.environ.copy()
        env.update(RUSTC=tools['rustc'],RUSTDOC=tools['rustdoc'],
                   CARGO_BUILD_RUSTC=tools['rustc'],BEATBOX_BUILD_SHA=source['head'],
                   PATH=str(Path(tools['rustc']).parent)+':'+env.get('PATH','/usr/bin:/bin'),
                   RUSTUP_TOOLCHAIN=str(Path(tools['rustc']).parent.parent),
                   RUSTC_WRAPPER='',RUSTC_WORKSPACE_WRAPPER='',
                   CARGO_BUILD_RUSTC_WRAPPER='',CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='')
        versions={}
        for name in ('rustc','cargo'):
            run=subprocess.run([tools[name],'--version'],env=env,check=True,
                               stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=3)
            version=run.stdout.decode().strip()
            require(re.fullmatch(name+r' 1\.99\.0 \([0-9a-f]+ [0-9-]+\)',version),'unexpected compiler/tool version')
            versions[name]=version
        import shutil
        pkg=shutil.which('pkg-config'); require(pkg is not None,'pkg-config absent')
        subprocess.run([pkg,'--modversion','alsa','x11','xkbcommon'],env=env,
                       stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,check=True,timeout=3)
        with (out/'build.log').open('xb') as log:
            subprocess.run(['timeout','--signal=TERM','--kill-after=5s','600s',tools['cargo'],
                            'build','--locked','--release'],env=env,stdout=log,
                           stderr=subprocess.STDOUT,check=True,timeout=610)
        put(out/'build.json',{'schema':SCHEMA,'head':source['head'],'tree':source['tree'],
                             'binary_sha256':digest('target/release/beatbox'),'rustc':versions['rustc']})
        return 0
    except Exception as exc:
        try: put(out/'preparation-failure.json',{'schema':SCHEMA,'status':'failed',
                                               'failure_type':type(exc).__name__})
        except Exception: pass
        return 1


# Exact finite publication set; no glob, raw diagnostics, args, owned/ or cookies.
UPLOAD_FILES = ['public-receipt.json','current-client.png'] + [f'read-{i}.json' for i in range(7)]


def public_linkage(row, campaign, source):
    # Fixed structured evidence only. No free-form diagnostics, paths, argv or env.
    def identity(value):
        exact(value,['pid','start_ticks','state','pgrp'])
        for key in ('pid','start_ticks','pgrp'): integer(value[key],1,2**63-1)
        require(value['state'] in ('R','S','D','Z','T','t','X','I'), 'process state')
        require(value['pgrp']==value['pid'],'owned group')
        return dict(value)
    def hashvalue(value):
        require(type(value) is str and re.fullmatch('[0-9a-f]{64}',value),'hash type')
        return value
    app=identity(row['app']); xvfb=identity(row['xvfb'])
    for key in ('head','tree'):
        require(type(source[key]) is str and re.fullmatch('[0-9a-f]{40}',source[key]), 'public source identity')
    build=row['build']; exact(build,['schema','head','tree','binary_sha256','rustc'])
    require(build['schema']==SCHEMA and build['head']==source['head'] and build['tree']==source['tree'], 'public build binding')
    # Compiler version text is private; only its digest crosses publication boundary.
    require(type(build['rustc']) is str and re.fullmatch(r'rustc 1\.99\.0 \([0-9a-f]+ [0-9-]+\)',build['rustc']), 'public compiler version')
    public_build={'head':build['head'],'tree':build['tree'],'binary_sha256':hashvalue(build['binary_sha256']),
                  'rustc_version_sha256':hashlib.sha256(build['rustc'].encode()).hexdigest()}
    frame=checked_frame(row['frame'],app,campaign/'current-client.png')
    clean=strict((campaign/'cleanup.json').read_bytes())
    exact(clean,['status','claim','children','errors','budget_seconds'])
    require(clean['status']=='reaped' and clean['errors']==[] and clean['budget_seconds']==5 and
            clean['claim']=='direct_children_reaped_groups_best_effort_not_descendant_emptiness', 'public cleanup failed')
    integer(clean['budget_seconds'],5,5)
    require(type(clean['children']) is list and len(clean['children'])<=11,'child count')
    children=[]
    for child in clean['children']:
        exact(child,['label','pid','start_ticks','reaped','exit_code'])
        require(type(child['label']) is str and re.fullmatch(r'(xvfb|studio|[0-9]{2}-(facilities|frame|http))',child['label']), 'child label')
        integer(child['pid'],1,2**63-1); integer(child['start_ticks'],1,2**63-1)
        integer(child['exit_code'],-128,255); require(child['reaped'] is True,'direct child not reaped')
        children.append(dict(child))
    require(any(c['pid']==app['pid'] and c['start_ticks']==app['start_ticks'] for c in children) and
            any(c['pid']==xvfb['pid'] and c['start_ticks']==xvfb['start_ticks'] for c in children),'cleanup target linkage')
    display=row['display']; exact(display,['display','unix_inode','socket_device','socket_inode','tcp'])
    require(type(display['display']) is str and re.fullmatch(':[0-9]{1,5}',display['display']) and
            type(display['unix_inode']) is str and re.fullmatch('[0-9]+',display['unix_inode']) and display['tcp'] is False, 'display types')
    for key in ('socket_device','socket_inode'): integer(display[key],0,2**64-1)
    listener=row['listener']; exact(listener,['address','inode'])
    require(listener['address']=='0100007F' and type(listener['inode']) is str and re.fullmatch('[0-9]+',listener['inode']), 'listener types')
    docs=[]; reads=[]
    for i,(name,args) in enumerate(READS):
        body=campaign/f'read-{i}.json'; doc=strict(body.read_bytes(),READ_CAP)
        exact(doc,['ok','result']); require(doc['ok'] is True,'public read failed')
        docs.append(doc['result'])
        reads.append({'index':i,'name':name,'arguments':args,'body_bytes':body.stat().st_size,'body_sha256':digest(body)})
    ledger=validate_ledger(docs)
    facility=checked_facilities(row['facilities'])
    return {'build':public_build,'app':app,'xvfb':xvfb,'frame':frame,
            'display':dict(display),'listener':dict(listener),'reads':reads,'ledger':ledger,
            'cleanup':'reaped','cleanup_children':children,
            'facility_libraries_loadable':{k:v['loadable'] for k,v in facility['libraries'].items()}}


def stage_evidence(out):
    stage=out/'upload'; stage.mkdir(mode=0o700,exist_ok=False)
    campaign=out/'campaign'; status='failed'; visual='UNRUN'; row=None
    receipt=campaign/'receipt.json'
    if receipt.is_file():
        try:
            row=checked_final(strict(receipt.read_bytes()))
            status=row['status']; visual=row['visual_status']
        except Exception: pass
    # Do not publish free-form errors, stderr, version text, environment or paths.
    public={'schema':SCHEMA,'status':status,'visual_status':visual,'native_gestures':'UNRUN',
            'semantic_pass':False,'cases':{case:'UNRUN' for case in CASES},
            'nested_schema_claim':'envelope_only_requires_independent_source_and_evidence_review',
            'cleanup_claim':'direct_children_only_groups_best_effort_not_descendant_emptiness'}
    for key in ('head','tree','runner_sha256','workflow_sha256'):
        try:
            val=strict((out/'source.json').read_bytes())[key]
            require(type(val) is str and re.fullmatch('[0-9a-f]{'+str(40 if key in ('head','tree') else 64)+'}',val),'hash')
            public[key]=val
        except Exception: public[key]=None
    public['linkage']=None
    if row is not None:
        try:
            public['linkage']=public_linkage(row,campaign,public)
        except Exception:
            public['status']='failed'
    if (public['linkage'] is None or os.environ.get('CAPTURE_OUTCOME')!='success' or
            (campaign/'receipt-finalization-failure.json').exists() or
            (campaign/'cleanup-finalization-failure.json').exists()):
        public['status']='failed'
    put(stage/'public-receipt.json',public)
    for name in UPLOAD_FILES[1:]:
        path=campaign/name
        if not path.exists(): continue
        cap=FRAME_CAP if name=='current-client.png' else READ_CAP
        require(path.is_file() and not path.is_symlink() and path.stat().st_size<=cap,'upload cap/type')
        data=path.read_bytes(); require(len(data)<=cap,'upload cap')
        with (stage/name).open('xb') as f: f.write(data)
    manifest=[{'path':name,'bytes':(stage/name).stat().st_size,'sha256':digest(stage/name)}
              for name in UPLOAD_FILES if (stage/name).exists()]
    put(stage/'manifest.json',{'schema':SCHEMA,'files':manifest,'expected_files':UPLOAD_FILES,
                             'missing_files':[name for name in UPLOAD_FILES if not (stage/name).exists()],
                             'raw_diagnostics_uploaded':False,'owned_credentials_uploaded':False})
    return 0


def main():
    if len(sys.argv)==5 and sys.argv[1]=='_worker':
        kind,args,output=sys.argv[2:]
        spec=strict(Path(args).read_bytes())
        result={'facilities':facilities_worker,'frame':x_worker,'http':http_worker}[kind](spec)
        put(output,result); return 0
    require(len(sys.argv) in (3,4),'usage: facilities OUT | build OUT | stage OUT | capture OUT BINARY')
    mode=sys.argv[1]; require(mode in ('facilities','build','stage','capture'),'fixed modes only')
    out=Path(sys.argv[2]).resolve(); os.umask(0o077)
    if mode=='build': return hosted_build(out)
    if mode=='stage': return stage_evidence(out)
    def stop(_sig,_frame): raise RuntimeError('supervisor interrupted')
    handlers={}
    try:
        for sig in (signal.SIGTERM,signal.SIGINT): handlers[sig]=signal.signal(sig,stop)
        if mode=='facilities':
            require(len(sys.argv)==3,'facility arguments')
            out.mkdir(mode=0o700,parents=True,exist_ok=False)
            owner=Supervisor(out)
            try:
                source=binding(); put(out/'source.json',source)
                facility=owner.worker('facilities',{'env':{'PATH':os.environ.get('PATH','/usr/bin:/bin'),
                                                          'LANG':'C.UTF-8'}})
                checked_facilities(facility); put(out/'facilities.json',facility)
                require(facility['tools']['Xvfb'] and facility['tools']['pkg-config'], 'required facility absent')
                require(facility['libraries']['X11']['loadable'] and facility['pidfd_open'], 'required library/process facility absent')
            finally:
                require(owner.cleanup(),'facility cleanup failed')
            return 0
        require(len(sys.argv)==4,'capture arguments')
        require(out.is_dir() and not (out/'receipt.json').exists(),'fresh output receipt required')
        source=binding(); require(strict((out/'source.json').read_bytes())==source,'prebuild binding changed')
        binary=Path(sys.argv[3]).resolve(); require(binary.is_file() and os.access(binary,os.X_OK),'missing binary')
        campaign=out/'campaign'; campaign.mkdir(mode=0o700)
        (campaign/'build.json').write_bytes((out/'build.json').read_bytes())
        return 0 if capture(campaign,binary,source) else 1
    finally:
        for sig,handler in handlers.items(): signal.signal(sig,handler)


if __name__=='__main__':
    sys.exit(main())
