#!/usr/bin/env python3
"""Finite ordinary Notes calibration only. No scored controls or acceptance path."""
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
            'XQueryKeymap':([P,C.c_char_p],I), 'XKeysymToKeycode':([P,U],C.c_ubyte),
            'XGetKeyboardMapping':([P,C.c_ubyte,I,IP],UP),
            'XGetImage':([P,U,I,I,C.c_uint,C.c_uint,U,I],P),
            'XGetPixel':([P,I,I],U), 'XDestroyImage':([P],I)}
        for name,(args,ret) in specs.items():
            f=getattr(self.x,name); f.argtypes=args; f.restype=ret
        for name,args in [('XTestQueryExtension',[P,IP,IP,IP,IP]),('XTestFakeKeyEvent',[P,C.c_uint,I,U])]:
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
        self.held.add(self.code)
        require(self.t.XTestFakeKeyEvent(self.d,self.code,1,0), 'F7 press')
        self.sync()
        self.release()
    def release(self):
        for code in list(self.held):
            require(self.t.XTestFakeKeyEvent(self.d,code,0,0), 'key release')
            self.held.remove(code)
        state=C.create_string_buffer(32); self.x.XQueryKeymap(self.d,state); self.sync()
        require(not any(state.raw), 'held key after release')
        self.sync()
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
    doc={'schema':1,'status':'STOP','acceptance':False,'case_A':'UNRUN','controls':'UNRUN','actions':[],'reads':[],'cleanup':[]}
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
        require(bounded(f'/proc/{p.pid}/cmdline',8192).split(b'\0')[:-1]==[os.fsencode(a) for a in item[2]], 'argv drift')
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
        original=cleaned(json.loads(bounded(fixture))); notes=original['patterns'][0]['clips']['k']
        require(len(original['tracks'])==len(original['patterns'])==1 and not original['playlist'], 'fixture shape')
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
            os.close(w); w=-1
            import select
            require(select.select([r],[],[],2)[0], 'display readiness')
            number=os.read(r,32).strip(); require(number.isdigit(), 'displayfd response')
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
            listener(); require(time.monotonic()<DEADLINE and len(doc['reads'])<16, 'read budget/deadline')
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
        before=read('get_project',{'detail':'full'}); require(before==original, 'full live fixture mismatch')
        history=read('get_history',{'limit':500}); transport=read('transport',{'action':'status'})
        require(transport['studio_attached'] and not transport['playing'] and transport['pending']==0, 'transport prerequisite')
        x.geometry(studio.pid,target); x.focus(target); owned(); x.f7(); doc['actions'].append('native F7 press/release only')
        time.sleep(.5)
        for i in range(2):
            owned(); x.focus(target); require(x.geometry(studio.pid,target)==geometry, 'geometry drift')
            image=x.capture(target); name=f'ordinary-notes-{i}.png'; (OUT/name).write_bytes(image)
            doc.setdefault('captures',[]).append({'file':name,'sha256':hashlib.sha256(image).hexdigest(),'xid':target,'geometry':geometry,'monotonic':time.monotonic()})
            require(read('get_project',{'detail':'full'})==original, 'project mutation')
            now=read('get_history',{'limit':500}); t=read('transport',{'action':'status'})
            require(all(now[k]==history[k] for k in ('revision','undo_depth','redo_depth','snapshots')) and t==transport, 'history/transport drift')
            time.sleep(.25)
        require(doc['captures'][0]['sha256']==doc['captures'][1]['sha256'], 'unsettled framebuffer; no retry')
        final=read('get_history',{'limit':500})
        atomic('target-map.json',{'status':'CALIBRATION_REVIEW_REQUIRED','dock_bounds':None,'coordinates_approved':False,'source':'src/gui/piano.rs:436-448,1607-1685,2610-2638,2780-2829','pitch_span':[57,70],'steps':16,'notes':notes,'formula':{'toolbar':'[L+100,T+1,R-4,T+21]','velocity_height':'clamp((B-T)-44-16-48,16,48)','roll':'[L+52,T+62,R-4,B-velocity_height-4]','cell':'roll.width/16','row_h':'roll.height/14','note':'[roll.left+start*cell,roll.top+(70-pitch)*row_h+0.5,max(roll.left+(start+len)*cell,roll.left+start*cell+3),roll.top+(70-pitch+1)*row_h-0.5]'},'review':'Both reviewers must identify actual full-panel L,T,R,B and toolbar/Snap/three notes on these ordinary images. No numeric targets guessed; no clicks supported.'})
        atomic('source-bindings.json',{str(p):digest(ROOT/p) for p in ['src/gui/piano.rs','src/gui/shortcuts.rs','src/gui/workspace.rs','src/gui/mod.rs','src/server.rs','src/tools.rs','src/tools_parity.rs','src/engine.rs']})
        doc.update(status='CALIBRATION_REVIEW_REQUIRED',binary_sha256=digest(binary),fixture_sha256=digest(fixture),helper_sha256=digest(Path(__file__)),display=env['DISPLAY'],xid=target,listener_port=port,final_history=final,terminal_read_own_entry_outside_snapshot=True)
        code=3  # Intentional non-success: acceptance pending, NOT runtime FAIL.
    except BaseException as error:
        doc['error']=type(error).__name__+': '+str(error)[:300]
    finally:
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
