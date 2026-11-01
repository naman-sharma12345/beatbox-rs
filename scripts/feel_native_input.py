#!/usr/bin/env python3
"""Finite selected_dialog calibration only. No scored controls or acceptance path."""
import ctypes as C
import hashlib
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import struct
import subprocess
import time
import threading
import zlib

ROOT = Path.cwd()
OUT = ROOT / 'native-calibration'
DEADLINE = 0
STAGE = 'selected_dialog'
APPROVAL = {'head': '438f038576409328a6e34862ed98290e69febc51', 'parent_review_sha256': '9bce4b34fafea9a27ca4f84a61d2b277182f5f0c9712551d6a55abfa005d57f5', 'original_png_sha256': ['483bce5ef62dd455cc0f1749f8ccb8f81abbc92318c7ec0b6abf029222eb4d98', '483bce5ef62dd455cc0f1749f8ccb8f81abbc92318c7ec0b6abf029222eb4d98'], 'fixture_sha256': 'd7375111eb87c320bbd2cf48823979b201d0904baa85b467cf1d44643974b7cd', 'full': [30, 263, 1070, 676], 'roll': [82, 325, 1066, 624], 'tolerance_px': 1, 'native_scale': 1, 'ordinary_hits': {'n3':[131,549],'n4':[193,464],'n5':[377,400],'Feel':[362,275]}}


def require(ok, why):
    if not ok:
        raise RuntimeError(why)

def bounded(path, cap=1048576):
    with Path(path).open('rb') as f: data = f.read(cap+1)
    require(len(data) <= cap, 'file cap: ' + str(path))
    return data


def digest(path):
    return hashlib.sha256(bounded(path, 100 * 1048576)).hexdigest()


def atomic(name, value):
    data = json.dumps(value, indent=2, sort_keys=True).encode()
    require(len(data) <= 4 * 1048576, 'JSON evidence cap')
    temp = OUT / (name + '.tmp')
    temp.write_bytes(data)
    temp.replace(OUT / name)


def identity(pid):
    fields = bounded(f'/proc/{pid}/stat', 4096).rsplit(b') ', 1)[1].split()
    require(fields[0] not in (b'Z', b'X', b'x'), 'dead owner')
    return [int(fields[1]), int(fields[2]), int(fields[3]), int(fields[19])]


def display_ready(p, r, write_fd, alive):
    import select
    started = time.monotonic()
    budget = min(10, max(0, DEADLINE-started))
    receipt = {'budget_seconds': budget, 'pipe_fd': r, 'child_write_fd': write_fd, 'pid': p.pid}
    def observe():
        fields = bounded(f'/proc/{p.pid}/stat', 4096).rsplit(b') ', 1)[1].split()
        prior = os.waitid(os.P_PID, p.pid, os.WEXITED|os.WNOHANG|os.WNOWAIT)
        return {'state': fields[0].decode(), 'ppid_pgid_sid_start': [int(fields[i]) for i in (1,2,3,19)],
                'waitid': None if prior is None else {k: getattr(prior,k) for k in ('si_pid','si_code','si_status')},
                'log_bytes': (OUT/'xvfb.log').stat().st_size}
    try:
        alive(p)
        pipe = os.fstat(r)
        receipt['pipe_identity'] = [pipe.st_dev, pipe.st_ino, pipe.st_mode]
        receipt.update(before=observe(), outcome='waiting', elapsed_seconds=time.monotonic()-started)
        atomic('readiness.json', receipt)
        os.set_blocking(r, False)
        number = b''
        while True:
            remaining = started+budget-time.monotonic()
            require(remaining>0, 'display readiness timeout')
            readable = select.select([r], [], [], remaining)[0]
            alive(p)
            require(readable and time.monotonic()<started+budget, 'display readiness timeout')
            try: part = os.read(r, 32-len(number))
            except BlockingIOError: continue
            require(part, 'displayfd EOF')
            number += part
            if b'\n' in number: break
            require(len(number)<32, 'displayfd response cap')
        require(number.endswith(b'\n') and number[:-1].isdigit(), 'displayfd response')
        alive(p)
        require(time.monotonic()<started+budget, 'display readiness timeout')
        receipt['outcome'] = 'ready'
        return number[:-1]
    except BaseException as error:
        receipt.update(outcome='STOP', error=type(error).__name__+': '+str(error)[:300])
        raise
    finally:
        receipt['elapsed_seconds'] = time.monotonic()-started
        try: receipt['after'] = observe()
        except (OSError,RuntimeError) as error: receipt['after_error'] = type(error).__name__+': '+str(error)[:300]
        atomic('readiness.json', receipt)


def png(w, h, rgb):
    require(0 < w <= 1100 and 0 < h <= 700 and len(rgb) == w*h*3, 'RGB shape')
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind+data) & 0xffffffff)
    raw = b''.join(b'\0'+rgb[y*w*3:(y+1)*w*3] for y in range(h))
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w,h,8,2,0,0,0)) + chunk(b'IDAT', zlib.compress(raw)) + chunk(b'IEND', b'')


class Attr(C.Structure):
    _fields_ = [(n, C.c_int) for n in ('x','y','width','height','border','depth')] + [
        ('visual',C.c_void_p),('root',C.c_ulong)] + [(n,C.c_int) for n in ('cls','bit','win','backing')] + [
        ('planes',C.c_ulong),('pixel',C.c_ulong),('save',C.c_int),('colormap',C.c_ulong),
        ('installed',C.c_int),('mapped',C.c_int),('all',C.c_long),('your',C.c_long),
        ('propagate',C.c_long),('override',C.c_int),('screen',C.c_void_p)]
class Image(C.Structure):
    _fields_ = [(n,C.c_int) for n in ('width','height','offset','format')] + [('data',C.c_void_p)] + [
        (n,C.c_int) for n in ('order','unit','bit_order','pad','depth','stride','bpp')] + [
        (n,C.c_ulong) for n in ('red','green','blue')]
class X11:
    def __init__(self):
        self.d = None
        self.held = set()
        self.buttons = set()
        self.events = []
        self.errors = []
        require(C.sizeof(Attr)==136 and C.sizeof(C.c_long)==8 and Image.red.offset==56, 'unsupported LP64 ABI')
        self.x = C.CDLL('libX11.so.6')
        self.t = C.CDLL('libXtst.so.6')
        P,U,I = C.c_void_p,C.c_ulong,C.c_int
        UP,IP = C.POINTER(U),C.POINTER(I)
        specs = {
            'XOpenDisplay':([C.c_char_p],P), 'XCloseDisplay':([P],I), 'XDefaultRootWindow':([P],U),
            'XSync':([P,I],I), 'XFree':([P],I), 'XSetErrorHandler':([P],P),
            'XQueryTree':([P,U,UP,UP,C.POINTER(UP),C.POINTER(C.c_uint)],I),
            'XGetWindowAttributes':([P,U,C.POINTER(Attr)],I), 'XInternAtom':([P,C.c_char_p,I],U),
            'XGetWindowProperty':([P,U,U,C.c_long,C.c_long,I,U,UP,IP,UP,UP,C.POINTER(P)],I),
            'XResizeWindow':([P,U,C.c_uint,C.c_uint],I), 'XMoveWindow':([P,U,I,I],I),
            'XTranslateCoordinates':([P,U,U,I,I,IP,IP,UP],I),
            'XSetInputFocus':([P,U,I,U],I), 'XGetInputFocus':([P,UP,IP],I),
            'XQueryPointer':([P,U,UP,UP,IP,IP,IP,IP,C.POINTER(C.c_uint)],I),
            'XQueryKeymap':([P,C.c_char_p],I), 'XKeysymToKeycode':([P,U],C.c_ubyte),
            'XGetKeyboardMapping':([P,C.c_ubyte,I,IP],UP),
            'XGetImage':([P,U,I,I,C.c_uint,C.c_uint,U,I],P),
            'XGetPixel':([P,I,I],U), 'XDestroyImage':([P],I)}
        for name,(args,ret) in specs.items():
            f=getattr(self.x,name); f.argtypes=args; f.restype=ret
        for name,args in [('XTestQueryExtension',[P,IP,IP,IP,IP]),('XTestFakeKeyEvent',[P,C.c_uint,I,U]),
                          ('XTestFakeMotionEvent',[P,I,I,I,U]),('XTestFakeButtonEvent',[P,C.c_uint,I,U])]:
            f=getattr(self.t,name); f.argtypes=args; f.restype=I
        self.callback = C.CFUNCTYPE(I,P,P)(lambda d,e: (self.errors.append('X protocol error') if len(self.errors)<16 else None) or 0)
        self.old = self.x.XSetErrorHandler(C.cast(self.callback,P))
        self.d = self.x.XOpenDisplay(None)
        require(self.d, 'XOpenDisplay')
        self.root = self.x.XDefaultRootWindow(self.d)
        a,b,c,d = I(),I(),I(),I()
        require(self.t.XTestQueryExtension(self.d,C.byref(a),C.byref(b),C.byref(c),C.byref(d)), 'XTEST unavailable')
        self.code = self.x.XKeysymToKeycode(self.d,0xffc4)  # XK_F7
        count = I()
        keys = self.x.XGetKeyboardMapping(self.d,self.code,1,C.byref(count))
        try:
            require(self.code and keys and 0<count.value<=16 and keys[0]==0xffc4, 'F7 keymap')
        finally:
            if keys: self.x.XFree(keys)
        self.shift = self.x.XKeysymToKeycode(self.d,0xffe1)  # XK_Shift_L
        keys = self.x.XGetKeyboardMapping(self.d,self.shift,1,C.byref(count))
        try:
            require(self.shift and keys and 0<count.value<=16 and keys[0]==0xffe1, 'Shift keymap')
        finally:
            if keys: self.x.XFree(keys)
        self.sync()
    def sync(self):
        self.x.XSync(self.d,0)
        require(not self.errors, 'X protocol error')
    def windows(self, pid):
        root,parent,count = C.c_ulong(),C.c_ulong(),C.c_uint()
        children = C.POINTER(C.c_ulong)()
        require(self.x.XQueryTree(self.d,self.root,C.byref(root),C.byref(parent),C.byref(children),C.byref(count)), 'root tree')
        candidates = []
        try:
            require(root.value==self.root and count.value<=32, 'root/tree cap')
            for w in list(children[:count.value]):
                attr = Attr()
                require(self.x.XGetWindowAttributes(self.d,w,C.byref(attr)), 'attributes')
                if attr.cls!=1 or attr.mapped!=2: continue
                actual,n,after,fmt,data = C.c_ulong(),C.c_ulong(),C.c_ulong(),C.c_int(),C.c_void_p()
                atom=self.x.XInternAtom(self.d,b'_NET_WM_PID',1)
                require(atom, 'PID atom missing')
                try:
                    rc=self.x.XGetWindowProperty(self.d,w,atom,0,1,0,6,C.byref(actual),C.byref(fmt),C.byref(n),C.byref(after),C.byref(data))
                    require(rc==0 and actual.value==6 and fmt.value==32 and n.value==1 and after.value==0 and data, 'PID property')
                    require(C.cast(data,C.POINTER(C.c_ulong))[0]==pid, 'competing visible window')
                finally:
                    if data: self.x.XFree(data)
                require(attr.root==self.root and attr.override==0, 'window root/override')
                candidates.append((w,attr))
        finally:
            if children: self.x.XFree(children)
        self.sync()
        require(len(candidates)<=1, 'ambiguous owned windows')
        return candidates
    def geometry(self, pid, target):
        found=self.windows(pid)
        require(len(found)==1 and found[0][0]==target, 'owned XID changed')
        a=found[0][1]
        x,y,child = C.c_int(),C.c_int(),C.c_ulong()
        require(self.x.XTranslateCoordinates(self.d,target,self.root,0,0,C.byref(x),C.byref(y),C.byref(child)), 'root translation')
        self.sync()
        require((a.width,a.height,a.depth,a.border)==(1100,700,24,0) and (x.value,y.value)==(0,0), 'geometry/1x prerequisite')
        return [x.value,y.value,a.width,a.height,a.depth]
    def focus(self, target):
        w,r=C.c_ulong(),C.c_int()
        self.x.XGetInputFocus(self.d,C.byref(w),C.byref(r)); self.sync()
        require(w.value==target, 'focus mismatch')
    def f7(self):
        state=C.create_string_buffer(32)
        self.x.XQueryKeymap(self.d,state); self.sync()
        require(not any(state.raw), 'preexisting held key')
        self.events.append(['F7',1])
        self.held.add(self.code)
        require(self.t.XTestFakeKeyEvent(self.d,self.code,1,0), 'F7 press')
        self.sync()
        self.release()
    def pointer(self):
        root,child=C.c_ulong(),C.c_ulong()
        rx,ry,wx,wy=C.c_int(),C.c_int(),C.c_int(),C.c_int(); mask=C.c_uint()
        require(self.x.XQueryPointer(self.d,self.root,C.byref(root),C.byref(child),C.byref(rx),C.byref(ry),C.byref(wx),C.byref(wy),C.byref(mask)), 'query pointer')
        self.sync()
        require(root.value==self.root, 'pointer root')
        return mask.value & 0x1f00
    def neutral(self):
        state=C.create_string_buffer(32); self.x.XQueryKeymap(self.d,state); self.sync()
        require(not any(state.raw) and self.pointer()==0 and not self.held and not self.buttons, 'held input at checkpoint')
    def motion(self, x, y):
        require(len(self.events)<16 and (x,y) in ((131,549),(193,464),(362,275)), 'unapproved motion/event cap')
        self.events.append(['move',x,y])
        require(self.t.XTestFakeMotionEvent(self.d,0,x,y,0), 'native motion'); self.sync()
    def button(self, down):
        require(len(self.events)<16 and down in (0,1), 'button/event cap')
        self.events.append(['button',1,down])
        if down: self.buttons.add(1)
        ok=self.t.XTestFakeButtonEvent(self.d,1,down,0)
        if ok and not down: self.buttons.discard(1)
        require(ok, 'native button'); self.sync()
    def shift_press(self):
        require(len(self.events)<16, 'Shift event cap')
        self.events.append(['key',self.shift,1]); self.held.add(self.shift)
        require(self.t.XTestFakeKeyEvent(self.d,self.shift,1,0), 'native Shift'); self.sync()
    def release(self):
        errors=[]
        for held,fn in ((self.buttons,self.t.XTestFakeButtonEvent),(self.held,self.t.XTestFakeKeyEvent)):
            for code in list(held):
                try:
                    if len(self.events)<16: self.events.append(['release_button' if held is self.buttons else 'release_key',code])
                    else: errors.append(RuntimeError('release event cap'))
                    require(fn(self.d,code,0,0), 'input release')
                    held.remove(code)
                except BaseException as error: errors.append(error)
        self.neutral()
        require(not errors, 'input release failure')
    def capture(self, target):
        image=self.x.XGetImage(self.d,target,0,0,1100,700,C.c_ulong(-1).value,2)
        require(image, 'XGetImage')
        try:
            a=C.cast(image,C.POINTER(Image)).contents
            require((a.width,a.height,a.format,a.depth)==(1100,700,2,24) and a.bpp in (24,32) and a.order in (0,1), 'XImage format')
            masks=[a.red,a.green,a.blue]
            require(all(m>0 and m<=0xffffffff for m in masks) and not any(masks[i]&masks[j] for i,j in ((0,1),(0,2),(1,2))), 'RGB masks')
            channels=[]
            for mask in masks:
                shift=(mask & -mask).bit_length()-1; maximum=mask>>shift
                require(maximum & (maximum+1)==0, 'noncontiguous channel')
                channels.append((mask,shift,maximum))
            rgb=bytearray()
            for y in range(700):
                for x in range(1100):
                    p=self.x.XGetPixel(image,x,y)
                    rgb.extend(((p&m)>>s)*255//v for m,s,v in channels)
            self.sync()
            return png(1100,700,rgb)
        finally:
            self.x.XDestroyImage(image)
def run():
    global DEADLINE
    import resource
    resource.setrlimit(resource.RLIMIT_FSIZE, (4*1048576,4*1048576))
    DEADLINE=time.monotonic()+180
    OUT.mkdir(exist_ok=True)
    doc={'schema':1,'status':'STOP','acceptance':False,'case_A':'UNRUN','controls':'UNRUN','stage':STAGE,'approval':APPROVAL,'native_calibration':True,'final_popup_bounds':None,'actions':[],'reads':[],'cleanup':[]}
    groups=[]; x=None; code=2
    guard=threading.Lock(); finished=threading.Event()
    registering=False; pending=None
    def stop(sig, frame):
        nonlocal pending
        if registering:
            pending=sig
            return
        raise RuntimeError('deadline/cancellation '+str(sig))
    for sig in (signal.SIGALRM,signal.SIGTERM,signal.SIGINT,signal.SIGHUP): signal.signal(sig,stop)
    signal.alarm(180)
    def launch(argv, env, name, **kw):
        nonlocal registering
        log=(OUT/(name+'.log')).open('wb')
        with guard:
            registering=True
            try:
                try:
                    p=subprocess.Popen(argv,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True,**kw)
                    groups.append((p,None,argv,env.get('DISPLAY')))
                finally: log.close()
            finally:
                registering=False
                if pending is not None: stop(pending,None)
            ident=identity(p.pid); groups[-1]=(p,ident,argv,env.get('DISPLAY'))
            doc.setdefault('owners',[]).append({'pid':p.pid,'ppid_pgid_sid_start':ident,'argv':argv,'display':env.get('DISPLAY')})
            require(ident[:3]==[os.getpid(),p.pid,p.pid], 'owned group/session')
            return p
    def alive(p):
        item=next(g for g in groups if g[0] is p)
        require(identity(p.pid)==item[1] and os.waitid(os.P_PID,p.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT) is None, 'owner identity/exit')
        require(os.path.samefile(f'/proc/{p.pid}/exe',item[2][0]), 'executable identity')
        raw=bounded(f'/proc/{p.pid}/cmdline',8192)
        parsed=raw.split(b'\0')[:-1]
        expected=[os.fsencode(a) for a in item[2]]
        if parsed!=expected:
            try:
                stat=bounded(f'/proc/{p.pid}/stat',4096)
                atomic('argv-mismatch.json',{
                    'pid':p.pid, 'monotonic':time.monotonic(), 'stage':STAGE,
                    'raw_hex':raw.hex(), 'raw_length':len(raw), 'raw_sha256':hashlib.sha256(raw).hexdigest(),
                    'parsed_argv_hex':[a.hex() for a in parsed], 'expected_argv_hex':[a.hex() for a in expected],
                    'expected_argv':item[2], 'retained_identity':item[1],
                    'stat_hex':stat.hex(), 'stat_sha256':hashlib.sha256(stat).hexdigest(),
                    'identity_at_diagnostic':identity(p.pid),
                    'exe_samefile_at_diagnostic':os.path.samefile(f'/proc/{p.pid}/exe',item[2][0]),
                    'precheck_identity_exit_exe_passed':True,
                    'scope':'sequential mismatch observations, not an atomic process snapshot'})
            except Exception:
                pass  # Evidence failure must not replace or bypass the original guard.
        require(parsed==expected, 'argv drift')
    def expire():
        if not finished.wait(max(0,DEADLINE-time.monotonic())):
            with guard:
                if finished.is_set(): return
                try: atomic('deadline.json',{'status':'STOP','reason':'fixed 180-second deadline; killing unreaped owned groups'})
                finally:
                    for p,*rest in groups:
                        try: os.killpg(p.pid,signal.SIGKILL)
                        except ProcessLookupError: pass
    threading.Thread(target=expire,daemon=True).start()
    try:
        require(signal.getsignal(signal.SIGCHLD) in (signal.SIG_DFL,None), 'SIGCHLD ownership')
        binary=(ROOT/'target/release/beatbox').resolve(); fixture=OUT/'project.json'
        startup=[('cli','load_project',{'path':str(fixture)}),('ai','get_project',{'detail':'full'})]
        require(all(len(json.dumps(a,ensure_ascii=False,separators=(',',':')).encode('utf-8'))<=140 for _,_,a in startup), 'startup args exceed 140 UTF-8 bytes')
        def cleaned(v):
            if isinstance(v,float): return int(v*10000+.5)/10000 if v>=0 else -int(-v*10000+.5)/10000
            if isinstance(v,list): return [cleaned(x) for x in v]
            return {k:cleaned(x) for k,x in v.items()} if isinstance(v,dict) else v
        require(digest(fixture)==APPROVAL['fixture_sha256'], 'approved fixture bytes mismatch')
        original=cleaned(json.loads(bounded(fixture))); notes=original['patterns'][0]['clips']['k']
        playlist=original.get('playlist', [])
        require(len(original['tracks'])==len(original['patterns'])==1 and isinstance(playlist,list) and not playlist, 'fixture shape')
        require(original['tracks'][0]['name']=='K' and original['patterns'][0]['name']=='A' and original['patterns'][0]['bars']==1, 'fixture names')
        require(original['tracks'][0]['id'] and original['patterns'][0]['id'] and len(notes)==3 and len({n['id'] for n in notes})==3 and all(n['id'] for n in notes), 'fixture IDs')
        for n,p,s,o in zip(notes,[60,64,67],[.3,1.3,4.3],[.12,-.1,0]):
            require(n['pitch']==p and abs(n['start']-s)<1e-6 and abs(n.get('offset',0)-o)<1e-6 and n['len']==1, 'fixture note payload')
        env={k:v for k,v in os.environ.items() if not k.startswith(('BEATBOX_','XDG_','WAYLAND_')) and k not in ('DISPLAY','HOME','GDK_SCALE','GDK_DPI_SCALE','QT_SCALE_FACTOR')}
        for name in ('home','config','data','cache','work','runtime'): (OUT/name).mkdir(mode=0o700)
        env.update(HOME=str(OUT/'home'),XDG_CONFIG_HOME=str(OUT/'config'),XDG_DATA_HOME=str(OUT/'data'),XDG_CACHE_HOME=str(OUT/'cache'),XDG_RUNTIME_DIR=str(OUT/'runtime'))
        # Xvfb chooses a free private display; no title search or shared DISPLAY.
        r,w=os.pipe()
        try:
            xv=launch(['/usr/bin/Xvfb','-displayfd',str(w),'-screen','0','1100x700x24','-nolisten','tcp','-dpi','96'],env,'xvfb',pass_fds=(w,))
            write_fd=w
            os.close(w); w=-1
            number=display_ready(xv,r,write_fd,alive)
        finally:
            os.close(r)
            if w!=-1: os.close(w)
        env['DISPLAY']=':'+number.decode(); os.environ['DISPLAY']=env['DISPLAY']
        sock=socket.socket(); sock.bind(('127.0.0.1',0)); port=sock.getsockname()[1]; sock.close()
        studio=launch([str(binary),'--workdir',str(OUT/'work'),'studio',str(fixture),'--listen',f'127.0.0.1:{port}'],env,'studio')
        def owned():
            alive(xv); alive(studio)
            raw=bounded(f'/proc/{studio.pid}/environ',65536).split(b'\0')
            require(b'DISPLAY='+env['DISPLAY'].encode() in raw and not any(v.startswith(b'BEATBOX_') for v in raw), 'display/selectors')
        x=X11.__new__(X11); x.__init__()
        doc['native_event_attempts']=x.events
        ready=time.monotonic()+10; target=None
        for attempt in range(20):
            owned(); found=x.windows(studio.pid)
            if found: target=found[0][0]; break
            if time.monotonic()+.5>=ready: break
            time.sleep(.5)
        require(target, '20 probes/10s readiness exhausted')
        def listener():
            owned()
            rows=[l.split() for l in bounded('/proc/net/tcp',1048576).decode().splitlines()[1:]]
            rows=[r for r in rows if r[1]==f'0100007F:{port:04X}' and r[3]=='0A']
            require(len(rows)==1, 'unique loopback listener')
            inode=rows[0][9]
            fds=list(Path(f'/proc/{studio.pid}/fd').iterdir()); require(len(fds)<=256, 'fd cap')
            require(any(os.readlink(f)==f'socket:[{inode}]' for f in fds), 'Studio socket ownership')
        prefix=None; boundary=0
        def read(name,args):
            nonlocal prefix,boundary
            require((name,args) in [('get_project',{'detail':'full'}),('get_history',{'limit':500}),('transport',{'action':'status'})], 'observer allowlist')
            listener(); require(time.monotonic()<DEADLINE and len(doc['reads'])<24, 'read budget/deadline')
            conn=http.client.HTTPConnection('127.0.0.1',port,timeout=2)
            try:
                conn.request('POST','/call',json.dumps({'name':name,'arguments':args}),{'Content-Type':'application/json'})
                response=conn.getresponse(); raw=response.read(1048577)
                (OUT/f'observer-{len(doc["reads"]):02}.json').write_bytes(raw)
                require(response.status==200 and len(raw)<=1048576, 'observer response cap/status')
                result=json.loads(raw); require(result.get('ok') is True, 'observer tool error')
                if name=='get_history':
                    h=result['result']; entries=list(reversed(h['entries']))
                    require(h['log_size']==len(entries)<400 and [e['seq'] for e in entries]==list(range(len(entries))), 'complete history/sequence cap')
                    if prefix is None:
                        require(len(entries)==len(startup), 'unexpected startup count')
                        for entry,(source,tool,arguments) in zip(entries,startup):
                            require(entry['ok'] is True and entry['source']==source and entry['tool']==tool and json.loads(entry['args'])==arguments, 'unexpected startup activity')
                        prefix=entries; boundary=len(doc['reads'])
                    else:
                        additions=doc['reads'][boundary:]
                        require(entries[:len(prefix)]==prefix and len(entries)==len(prefix)+len(additions), 'history prefix/count drift')
                        for entry,known in zip(entries[len(prefix):],additions):
                            require(entry['ok'] is True and entry['source']=='ai' and entry['tool']==known['name'] and json.loads(entry['args'])==known['arguments'], 'unexpected Engine activity')
                doc['reads'].append({'name':name,'arguments':args,'response':result,'monotonic':time.monotonic()})
                atomic('manifest.json',doc)
                return result['result']
            finally: conn.close()
        x.x.XMoveWindow(x.d,target,0,0); x.x.XResizeWindow(x.d,target,1100,700)
        x.x.XSetInputFocus(x.d,target,2,0); x.sync(); time.sleep(.25)
        geometry=x.geometry(studio.pid,target); x.focus(target); owned(); listener()
        approved_sources={'src/gui/piano.rs': 'fb394cdab8d608b1564c4406347923d9f2bfc46cee17e4b0626b622242674a19', 'src/gui/shortcuts.rs': '52a1d53778b60aeda903345081597819fd2f95f555256c6c61392b9006c73911', 'src/gui/workspace.rs': '514ac4e773b919c2007abf03d24fbd8ef5bf518b698c42a3b56abde83665f37d', 'src/gui/mod.rs': '31b5565dc5eb8ca58568ed9d1b1ed33225ab99a4b24e4661c6b5556fd857e166'}
        require(all(digest(ROOT/p)==h for p,h in approved_sources.items()), 'approved layout source mismatch')
        before=read('get_project',{'detail':'full'}); require(before==original, 'full live fixture mismatch')
        history=read('get_history',{'limit':500}); transport=read('transport',{'action':'status'})
        require(transport['studio_attached'] and not transport['playing'] and transport['pending']==0, 'transport prerequisite')
        x.geometry(studio.pid,target); x.focus(target); owned(); listener(); x.neutral(); x.f7(); doc['actions'].append('native F7 press/release only')
        time.sleep(.5)
        def validate():
            owned(); listener(); x.focus(target)
            require(x.geometry(studio.pid,target)==geometry, 'geometry drift')
            x.neutral()
        def checkpoint(name):
            validate()
            image=x.capture(target); (OUT/name).write_bytes(image)
            doc.setdefault('captures',[]).append({'file':name,'sha256':hashlib.sha256(image).hexdigest(),'xid':target,'geometry':geometry,'monotonic':time.monotonic()})
            require(read('get_project',{'detail':'full'})==original, 'project mutation')
            now=read('get_history',{'limit':500}); t=read('transport',{'action':'status'})
            require(all(now[k]==history[k] for k in ('revision','undo_depth','redo_depth','snapshots')) and t==transport, 'history/transport drift')
        checkpoint('ordinary-baseline.png')
        # Literal approved hits only. No target arguments, locator, controls or retry.
        validate()
        x.motion(131,549); x.button(1); x.button(0); x.release()
        time.sleep(.25); checkpoint('selected-first.png')
        validate()
        x.shift_press()
        x.motion(193,464); x.button(1); x.button(0); x.release()
        time.sleep(.25); checkpoint('selected-second.png')
        validate()
        x.motion(362,275); x.button(1); x.button(0); x.release()
        time.sleep(.25); checkpoint('selected-dialog.png')
        time.sleep(.25); checkpoint('selected-dialog-steady.png')
        require(doc['captures'][-2]['sha256']==doc['captures'][-1]['sha256'], 'unsettled final framebuffer; no retry')
        final=read('get_history',{'limit':500})
        require(all(final[k]==history[k] for k in ('revision','undo_depth','redo_depth','snapshots')), 'terminal history drift')
        require(len(x.events)==13, 'native event count: F7 pair + three motion/clicks + Shift pair')
        require(len(doc['reads'])==19, 'observer ledger: initial3 + five checkpoints*3 + terminal1')
        atomic('target-map.json',{'status':'CALIBRATION_REVIEW_REQUIRED','acceptance':False,'stage':STAGE,'approval':APPROVAL,'final_popup_bounds':None,'controls':'UNRUN','case_A':'UNRUN','coordinates_approved':'ordinary targets only','review':'Human must compare baseline to BOTH approved originals and inspect actual first/second selection and popup. Unexpected layout rejects attempt. No automated semantic selection/popup assertion.'})
        atomic('source-bindings.json',{str(p):digest(ROOT/p) for p in ['src/gui/piano.rs','src/gui/shortcuts.rs','src/gui/workspace.rs','src/gui/mod.rs','src/server.rs','src/tools.rs','src/tools_parity.rs','src/engine.rs']})
        doc.update(status='CALIBRATION_REVIEW_REQUIRED',binary_sha256=digest(binary),fixture_sha256=digest(fixture),helper_sha256=digest(Path(__file__)),display=env['DISPLAY'],xid=target,listener_port=port,final_history=final,terminal_read_own_entry_outside_snapshot=True)
        code=3  # Intentional non-success: acceptance pending, NOT runtime FAIL.
    except BaseException as error:
        doc['error']=type(error).__name__+': '+str(error)[:300]
    finally:
        doc['native_event_attempts']=[] if x is None else x.events
        signal.alarm(0)
        for sig in (signal.SIGTERM,signal.SIGINT,signal.SIGHUP): signal.signal(sig,signal.SIG_IGN)
        try:
            if x is not None and x.d:
                x.release(); x.x.XCloseDisplay(x.d); x.d=None
        except BaseException as error:
            doc['cleanup'].append(str(error)); doc['status']='STOP'; code=2
        with guard:
            for p,ident,argv,display in reversed(groups):
                receipt={'pid':p.pid,'reaped':False,'group_kill':'not_attempted'}
                doc['cleanup'].append(receipt)
                try:
                    # WNOWAIT retains the anchor through the kill decision. Never poll/reap first.
                    prior=os.waitid(os.P_PID,p.pid,os.WEXITED|os.WNOHANG|os.WNOWAIT)
                    receipt['preexisting_exit']=None if prior is None else {k:getattr(prior,k) for k in ('si_pid','si_code','si_status')}
                    try:
                        os.killpg(p.pid,signal.SIGKILL)
                        receipt['group_kill']='issued'
                    except ProcessLookupError: receipt['group_kill']='absent'
                    receipt['wait_return']=p.wait(timeout=2)
                    receipt['reaped']=True
                    if prior is not None or receipt['group_kill']!='issued' or receipt['wait_return']!=-signal.SIGKILL:
                        doc['status']='STOP'; code=2
                except BaseException as error:
                    receipt['error']=str(error); doc['status']='STOP'; code=2
            finished.set()
            if time.monotonic()>=DEADLINE: doc['status']='STOP'; code=2
        atomic('manifest.json',doc)
    return code
if __name__=='__main__':
    raise SystemExit(run())
