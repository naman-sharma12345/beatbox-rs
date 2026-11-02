"""Pure tests only: no subprocess, sockets, Xlib, application or native execution."""
import copy
import importlib.util
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import zlib

MODULE = Path(__file__).with_name('p0a_native_frame_probe.py')
spec = importlib.util.spec_from_file_location('frame_probe', MODULE)
p = importlib.util.module_from_spec(spec)
spec.loader.exec_module(p)


def ledger():
    def entry(seq, tool, source='ai'):
        return {'seq':seq,'tool':tool,'args':json.dumps({'history':{'limit':100},'get_project':{'detail':'full'},'get_history':{'limit':500,'errors_only':False}}.get(tool,{})),'ok':True,'source':source}
    h0={'revision':1,'undo_depth':1,'redo_depth':0,'log_size':1,'snapshots':[],
        'entries':[entry(0,'load_project','cli')]}
    h1=copy.deepcopy(h0); h1['log_size']=4
    h1['entries']=[entry(3,'history'),entry(2,'get_project'),entry(1,'get_history')]+h0['entries']
    h2=copy.deepcopy(h1); h2['log_size']=7
    h2['entries']=[entry(6,'history'),entry(5,'get_project'),entry(4,'get_history')]+h1['entries']
    u={'done':['Loaded project'],'undone':[],'can_undo':1,'can_redo':0}
    return [h0,{'tracks':[]},u,h1,{'tracks':[]},copy.deepcopy(u),h2]


class Pure(unittest.TestCase):
    def reject(self, f, *args):
        with self.assertRaises((RuntimeError,ValueError,TypeError)):
            f(*args)

    def test_duplicate_json(self):
        self.reject(p.strict,b'{"a":1,"a":2}')

    def test_nonfinite_json(self):
        for x in ('NaN','Infinity','-Infinity','1e999','-1e999'):
            self.reject(p.strict,x.encode())

    def test_cap(self):
        self.reject(p.strict,b'{}',1)

    def test_strict_valid(self):
        self.assertEqual(p.strict(b'{"a":[1,true]}'),{'a':[1,True]})

    def test_integer_not_bool_or_float(self):
        for v in (True,False,1.0,-1,11,'1'):
            self.reject(p.integer,v,0,10)
        self.assertEqual(p.integer(10,0,10),10)

    def test_exact_keys(self):
        self.reject(p.exact,{'a':1,'extra':2},['a'])
        self.reject(p.exact,{},['a'])

    def test_accounted_reads(self):
        result=p.validate_ledger(ledger())
        self.assertEqual(result['http_calls'],7)
        self.assertFalse(result['native_edit_undo_verified'])
        self.assertFalse(result['atomic_frame_ledger_snapshot'])

    def test_wrong_read_count(self):
        self.reject(p.validate_ledger,ledger()[:-1])

    def test_project_mutation(self):
        d=ledger(); d[4]={'tracks':['unintended']}; self.reject(p.validate_ledger,d)

    def test_history_mutation(self):
        d=ledger(); d[5]['done']=['other']; self.reject(p.validate_ledger,d)

    def test_unaccounted_gui_command(self):
        d=ledger(); d[3]['entries'][0]['source']='you'; self.reject(p.validate_ledger,d)

    def test_read_log_wrong_tool(self):
        d=ledger(); d[3]['entries'][0]['tool']='undo'; self.reject(p.validate_ledger,d)

    def test_log_saturation(self):
        d=ledger(); d[6]['log_size']=500; self.reject(p.validate_ledger,d)

    def test_log_missing_entry(self):
        d=ledger(); d[6]['entries'].pop(); self.reject(p.validate_ledger,d)

    def test_log_sequence_bool(self):
        d=ledger(); d[0]['entries'][0]['seq']=False; self.reject(p.validate_ledger,d)

    def test_snapshots_not_assumed_complete(self):
        d=ledger(); d[0]['snapshots']=[{'name':'x'}]; self.reject(p.validate_ledger,d)

    def test_revision_not_restored_by_assumption(self):
        d=ledger(); d[6]['revision']=2; self.reject(p.validate_ledger,d)

    def test_history_truncation(self):
        d=ledger(); d[2]['can_undo']=100; self.reject(p.validate_ledger,d)

    def test_entry_schema(self):
        d=ledger(); d[0]['entries'][0]['unknown']=1; self.reject(p.validate_ledger,d)

    def test_loopback_listener_parser(self):
        data='header\n 0: 0100007F:9C40 00000000:0000 0A 0 0 0 1000 0 123\n'
        self.assertEqual(p.listener_rows(data,40000),[{'address':'0100007F','inode':'123'}])
        self.assertEqual(p.listener_rows(data,40001),[])
        self.assertEqual(len(p.listener_rows(data+data.splitlines()[1]+'\n',40000)),2)

    def test_wrong_birth_fails_before_signal(self):
        ident={'pid':123,'start_ticks':10,'state':'S','pgrp':123}
        with patch.object(p,'birth',return_value={**ident,'start_ticks':11}):
            self.reject(p.same_process,ident)

    def test_dead_birth(self):
        ident={'pid':123,'start_ticks':10,'state':'S','pgrp':123}
        with patch.object(p,'birth',return_value={**ident,'state':'Z'}):
            self.reject(p.same_process,ident)
            self.assertEqual(p.same_process(ident,False)['state'],'Z')

    def test_png_real_bytes_crc_rgb_padding(self):
        raw=bytes([0,0,255,0,0,255,0,0,7,7,7,7, 255,0,0,0,255,255,255,0,7,7,7,7])
        encoded=p.encode_frame(raw,2,2,12)
        self.assertEqual(encoded[:8],b'\x89PNG\r\n\x1a\n')
        chunks=[]; pos=8
        while pos<len(encoded):
            size=struct.unpack('>I',encoded[pos:pos+4])[0]
            kind=encoded[pos+4:pos+8]; data=encoded[pos+8:pos+8+size]
            crc=struct.unpack('>I',encoded[pos+8+size:pos+12+size])[0]
            self.assertEqual(zlib.crc32(kind+data)&0xffffffff,crc)
            chunks.append((kind,data)); pos+=12+size
        self.assertEqual(pos,len(encoded))
        self.assertEqual([c[0] for c in chunks],[b'IHDR',b'IDAT',b'IEND'])
        self.assertEqual(zlib.decompress(chunks[1][1]),
                         b'\x00\xff\x00\x00\x00\xff\x00\x00\x00\x00\xff\xff\xff\xff')

    def test_png_bounds(self):
        self.reject(p.encode_frame,b'',1481,1,5924)
        self.reject(p.encode_frame,b'',1,1,4)
        self.reject(p.encode_frame,b'xxxx',True,1,4)

    def test_environment_not_inherited(self):
        with patch.dict(p.os.environ,{'DISPLAY':':666','XAUTHORITY':'secret','BEATBOX_P0A_CAPTURE':'1',
                                      'HTTP_PROXY':'secret','GITHUB_TOKEN':'secret'}):
            env=p.clean_environment(Path('/mock-owned-home'))
        for name in ('DISPLAY','XAUTHORITY','BEATBOX_P0A_CAPTURE','HTTP_PROXY','GITHUB_TOKEN'):
            self.assertNotIn(name,env)

    def test_fsynced_exclusive_evidence(self):
        # Pure local file test only, always under caller's workspace scratch directory.
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            path=Path(t)/'receipt.json'; p.put(path,{'a':1})
            self.assertEqual(p.strict(path.read_bytes()),{'a':1})
            with self.assertRaises(FileExistsError): p.put(path,{'a':2})

    def test_put_cap(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            path=Path(t)/'receipt.json'
            self.reject(p.put,path,{'huge':'x'*100},10)
            self.assertFalse(path.exists())

    def test_worker_deadline_does_not_retry(self):
        # Mock Popen handle only; no process is launched.
        sup=p.Supervisor(Path('/mock'))
        fake=unittest.mock.Mock()
        fake.wait.side_effect=p.subprocess.TimeoutExpired('mock',3)
        with self.assertRaises(p.subprocess.TimeoutExpired): sup.wait(fake)
        self.assertEqual(fake.wait.call_count,1)
        self.assertLessEqual(fake.wait.call_args.kwargs['timeout'],3)

    def test_cleanup_one_aggregate_budget(self):
        sup=p.Supervisor(Path('/mock'))
        fake1=unittest.mock.Mock(pid=201,returncode=-9)
        fake2=unittest.mock.Mock(pid=202,returncode=-9)
        fake1.wait.side_effect=[p.subprocess.TimeoutExpired('mock',1),0]
        fake2.wait.side_effect=[p.subprocess.TimeoutExpired('mock',1),0]
        sup.children=[{'p':f,'birth':{'pid':f.pid,'start_ticks':100},'reaped':False,'pidfd':i,'label':str(i)}
                      for i,f in enumerate((fake1,fake2))]
        receipts=[]
        with patch.object(p,'same_process'), patch.object(p.os,'killpg') as kill, \
             patch.object(p.os,'close'), patch.object(p.signal,'signal'), \
             patch.object(p,'put',side_effect=lambda path,value:receipts.append(value)):
            self.assertTrue(sup.cleanup())
        self.assertEqual(kill.call_count,4)
        self.assertEqual(receipts[0]['status'],'reaped')
        self.assertTrue(all(row['reaped'] for row in receipts[0]['children']))
        self.assertTrue(all(0<=call.kwargs['timeout']<=5 for f in (fake1,fake2) for call in f.wait.call_args_list))

    def test_cleanup_ownership_loss_never_signals(self):
        sup=p.Supervisor(Path('/mock'))
        f=unittest.mock.Mock(pid=201,returncode=0)
        sup.children=[{'p':f,'birth':{'pid':f.pid,'start_ticks':100},'reaped':False,'pidfd':3,'label':'mock'}]
        with patch.object(p,'same_process',side_effect=RuntimeError('birth changed')), \
             patch.object(p.os,'killpg') as kill,patch.object(p.os,'close'), \
             patch.object(p.signal,'signal'),patch.object(p,'put'):
            self.assertFalse(sup.cleanup())
            kill.assert_not_called()

    def test_facility_receipt_schema(self):
        row={'schema':p.SCHEMA,'kind':'installed_facilities',
             'tools':{n:None for n in ('Xvfb','xauth','cargo','rustc','pkg-config')},
             'libraries':{n:{'soname':None,'loadable':False,'load_error':None,'mapped_paths':[]} for n in ('X11','Xtst','GL','EGL','asound')},
             'kernel':'mock','python':'mock','pidfd_open':True,'native_execution':False,'installed_nothing':True}
        self.assertEqual(p.checked_facilities(row),row)
        extra=copy.deepcopy(row); extra['unknown']=False
        self.reject(p.checked_facilities,extra)
        extra=copy.deepcopy(row); extra['libraries']['X11']['loadable']=1
        self.reject(p.checked_facilities,extra)

    def test_frame_receipt_source_hash_geometry_and_review(self):
        app={'pid':123,'start_ticks':10,'pgrp':123,'state':'S'}
        geom={'x':0,'y':0,'width':1480,'height':920,'border':0,'depth':24,'root':1}
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            image=Path(t)/'mock.png'; image.write_bytes(b'mock, not a native frame')
            row={'schema':p.SCHEMA,'kind':'same_window_frame','window':2,'title':'Beatbox Studio',
                 'app':app,'geometry_before':geom,'geometry_after':copy.deepcopy(geom),
                 'focus_window':2,'focus_revert':0,'xtest_extension':True,
                 'frame_sha256':p.digest(image),'frame_bytes':image.stat().st_size,
                 'capture_started_ns':1,'capture_finished_ns':2,'visual_status':'executed_unreviewed',
                 'native_gestures':'UNRUN',
                 'pixel_source':'XGetImage exact ordinary app client, not screenshot-tool Studio'}
            self.assertEqual(p.checked_frame(row,app,image),row)
            for key,value in [('visual_status','accepted'),('native_gestures','PASS'),
                              ('frame_sha256','0'*64),('frame_bytes',True),
                              ('pixel_source','screenshot-tool separate Studio')]:
                bad=copy.deepcopy(row); bad[key]=value
                self.reject(p.checked_frame,bad,app,image)
            bad=copy.deepcopy(row); bad['geometry_after']['width']=1100
            self.reject(p.checked_frame,bad,app,image)

    def test_final_receipt_cannot_claim_semantic_pass(self):
        row={'schema':p.SCHEMA,'status':'failed','source':{},'visual_status':'UNRUN',
             'native_gestures':'UNRUN','cases':{case:'UNRUN' for case in p.CASES},
             'semantic_pass':False,'error':'mock failed preflight','bounds':{
                'campaign_seconds':90,'child_seconds':3,'cleanup_seconds':5,'http_calls':7,
                'frames':1,'native_gestures':0},'build':None,'facilities':None,'fixture':None,
             'display':None,'listener':None,'app':None,'xvfb':None,'frame':None,'reads':[],
             'ledger':None,'cleanup':'reaped','finished_monotonic_ns':1}
        self.assertEqual(p.checked_final(row),row)
        for key,value in [('semantic_pass',True),('status','PASS'),('visual_status','accepted'),
                          ('native_gestures','PASS'),('status','probe_complete_not_semantic_pass')]:
            bad=copy.deepcopy(row); bad[key]=value; self.reject(p.checked_final,bad)
        bad=copy.deepcopy(row); bad['cases']['focus']='PASS'; self.reject(p.checked_final,bad)
        bad=copy.deepcopy(row); bad['extra']=True; self.reject(p.checked_final,bad)
        bad=copy.deepcopy(row); bad['bounds']['frames']=True; self.reject(p.checked_final,bad)


    def test_pidfd_failure_registered_before_unwind(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            sup=p.Supervisor(Path(t)); fake=unittest.mock.Mock(pid=321)
            ident={'pid':321,'start_ticks':99,'pgrp':321,'state':'S'}
            with patch.object(p.subprocess,'Popen',return_value=fake), \
                 patch.object(p,'birth',return_value=ident), \
                 patch.object(p.os,'pidfd_open',side_effect=OSError('mock kernel refusal')), \
                 patch.object(p.signal,'pthread_sigmask',return_value=set()):
                with self.assertRaises(OSError): sup.start('mock',['NO PROCESS EXECUTED'])
            self.assertEqual(len(sup.children),1)
            self.assertIsNone(sup.children[0]['birth'])
            self.assertIsNone(sup.children[0]['pidfd'])


    def test_binding_requires_exact_new_head_tree_clean_checkout(self):
        with patch.dict(p.os.environ,{'REVIEWED_HEAD':'a'*40,'REVIEWED_TREE':'b'*40}), \
             patch.object(p.subprocess,'check_output',side_effect=[b'a'*40,b'b'*40,b'']), \
             patch.object(p,'digest',return_value='c'*64):
            row=p.binding(); self.assertEqual(row['tree'],'b'*40)
        with patch.dict(p.os.environ,{'REVIEWED_HEAD':'a'*40,'REVIEWED_TREE':'b'*40}), \
             patch.object(p.subprocess,'check_output',side_effect=[b'a'*40,b'b'*40,b'?? unexpected']), \
             patch.object(p,'digest',return_value='c'*64):
            self.reject(p.binding)

    def test_full_capture_orchestration_pure_mock(self):
        self.mock_capture(False)

    def test_failed_ledger_preserves_executed_unreviewed_frame(self):
        self.mock_capture(True)

    def mock_capture(self, corrupt):
        # Every native, HTTP and process dependency is mocked; only local files/PNG encoder run.
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            root=Path(t).resolve(); out=root/'out'; out.mkdir()
            binary=root/'beatbox'; binary.write_bytes(b'mock, never executable')
            fixture=root/'fixture.json'; fixture.write_text('{"tracks":[]}')
            bind={'head':'a'*40,'tree':'b'*40}
            build={'schema':p.SCHEMA,**bind,'binary_sha256':p.digest(binary),'rustc':'rustc 1.99.0 (abc 2026-01-01)'}
            p.put(out/'source.json',{**bind,'runner_sha256':'c'*64,'workflow_sha256':'d'*64})
            p.put(out/'build.json',build)
            aid={'pid':321,'start_ticks':99,'pgrp':321,'state':'S'}
            xid={'pid':322,'start_ticks':100,'pgrp':322,'state':'S'}
            observed=[]; docs=ledger()
            if corrupt: docs[4]={'unexpected':'change'}
            facilities={'schema':p.SCHEMA,'kind':'installed_facilities',
                'tools':{n:'/mock/'+n for n in ('Xvfb','xauth','cargo','rustc','pkg-config')},
                'libraries':{n:{'soname':'mock','loadable':True,'load_error':None,'mapped_paths':[]}
                             for n in ('X11','Xtst','GL','EGL','asound')},
                'kernel':'mock','python':'mock','pidfd_open':True,
                'native_execution':False,'installed_nothing':True}
            listen={'address':'0100007F','inode':'123'}
            sup=unittest.mock.Mock()
            sup.remaining.side_effect=lambda maximum=p.CHILD_SECONDS: maximum
            def cleanup():
                p.put(out/'cleanup.json',{'status':'reaped','claim':'direct_children_reaped_groups_best_effort_not_descendant_emptiness',
                    'children':[{'label':label,'pid':ident['pid'],'start_ticks':ident['start_ticks'],'reaped':True,'exit_code':0}
                                for label,ident in [('studio',aid),('xvfb',xid)]],'errors':[],'budget_seconds':5})
                return True
            sup.cleanup.side_effect=cleanup
            def start(label, argv, env=None, pass_fds=()):
                observed.append(label)
                if label=='xvfb':
                    p.os.write(pass_fds[0],b'77\n'); return object(),xid
                return object(),aid
            sup.start.side_effect=start
            def worker(kind,spec):
                observed.append(kind)
                if kind=='facilities': return facilities
                if kind=='http':
                    i=spec['index']; body=Path(spec['body_path'])
                    body.write_text(json.dumps({'ok':True,'result':docs[i]}))
                    name,args=p.READS[i]
                    return {'schema':p.SCHEMA,'kind':'existing_http_read','index':i,'name':name,
                            'arguments':args,'source':'ai','listener':listen,'body_sha256':p.digest(body),
                            'body_bytes':body.stat().st_size,'self_log_entry':'mock'}
                path=Path(spec['frame_path'])
                path.write_bytes(p.encode_frame(bytes(1480*920*4),1480,920,1480*4))
                geom={'x':0,'y':0,'width':1480,'height':920,'border':0,'depth':24,'root':1}
                return {'schema':p.SCHEMA,'kind':'same_window_frame','window':2,'title':'Beatbox Studio',
                        'app':aid,'geometry_before':geom,'geometry_after':geom,'focus_window':2,
                        'focus_revert':0,'xtest_extension':True,'frame_sha256':p.digest(path),
                        'frame_bytes':path.stat().st_size,'capture_started_ns':1,'capture_finished_ns':2,
                        'visual_status':'executed_unreviewed','native_gestures':'UNRUN',
                        'pixel_source':'XGetImage exact ordinary app client, not screenshot-tool Studio'}
            sup.worker.side_effect=worker
            original_digest=p.digest; original_resolve=Path.resolve; original_stat=Path.stat
            def fake_digest(path):
                if str(path)=='/proc/321/exe': return build['binary_sha256']
                if str(path)=='examples/beats/rt_lofi.json': return original_digest(fixture)
                return original_digest(path)
            def fake_resolve(path,*a,**kw):
                return binary if str(path)=='/proc/321/exe' else original_resolve(path,*a,**kw)
            def fake_stat(path,*a,**kw):
                if str(path)=='examples/beats/rt_lofi.json': return original_stat(fixture,*a,**kw)
                return original_stat(path,*a,**kw)
            orig_read=Path.read_bytes
            def fake_read(path):
                return orig_read(fixture) if str(path)=='examples/beats/rt_lofi.json' else orig_read(path)
            with patch.object(p,'Supervisor',return_value=sup),patch.object(p,'same_process'), \
                 patch.object(p,'display_owner',return_value={'display':':77','tcp':False,'unix_inode':'123','socket_device':1,'socket_inode':2}), \
                 patch.object(p,'listener',return_value=listen),patch.object(p.time,'sleep'), \
                 patch.object(p,'digest',side_effect=fake_digest), \
                 patch.object(Path,'resolve',fake_resolve),patch.object(Path,'stat',fake_stat), \
                 patch.object(Path,'read_bytes',fake_read):
                result=p.capture(out,binary,bind)
                self.assertEqual(result,not corrupt,(out/'receipt.json').read_text())
            receipt=p.strict((out/'receipt.json').read_bytes())
            self.assertEqual(receipt['status'],'failed' if corrupt else 'probe_complete_not_semantic_pass')
            self.assertEqual(receipt['visual_status'],'executed_unreviewed')
            self.assertFalse(receipt['semantic_pass'])
            self.assertEqual(receipt['native_gestures'],'UNRUN')
            self.assertEqual(observed,['facilities','xvfb','studio']+['http']*4+['frame']+['http']*3)
            sup.cleanup.assert_called_once()
            parent=root/'published'; parent.mkdir()
            (parent/'source.json').write_bytes((out/'source.json').read_bytes())
            import shutil
            shutil.copytree(out,parent/'campaign')
            with patch.dict(p.os.environ,{'CAPTURE_OUTCOME':'success'}): p.stage_evidence(parent)
            public=p.strict((parent/'upload/public-receipt.json').read_bytes())
            self.assertEqual(public['status'],'failed' if corrupt else 'probe_complete_not_semantic_pass')
            if not corrupt:
                self.assertEqual(public['linkage']['app'],aid)
                self.assertEqual(public['linkage']['frame']['frame_sha256'],receipt['frame']['frame_sha256'])
                self.assertEqual(public['linkage']['ledger']['observed_appended_reads'],6)
            self.assertNotIn('mock', (parent/'upload/public-receipt.json').read_text())
            for outcome,marker in [('failure',False),('success',True)]:
                failed=root/('failed-stage-'+outcome); failed.mkdir()
                (failed/'source.json').write_bytes((out/'source.json').read_bytes())
                shutil.copytree(out,failed/'campaign')
                if marker: (failed/'campaign/receipt-finalization-failure.json').write_text('{}')
                with patch.dict(p.os.environ,{'CAPTURE_OUTCOME':outcome}): p.stage_evidence(failed)
                self.assertEqual(p.strict((failed/'upload/public-receipt.json').read_bytes())['status'],'failed')


    def test_no_coordinate_or_gesture_interface(self):
        # Source assertion, NOT proof of successful OS operation.
        source=MODULE.read_text()
        self.assertNotIn('XTestFake',source)
        self.assertNotIn('XSetInputFocus',source)
        self.assertNotIn('RawInput',source)
        self.assertNotIn('call screenshot',source)
        self.assertEqual(len(p.READS),7)
        self.assertEqual([a[0] for a in p.READS],
                         ['get_history','get_project','history','get_history','get_project','history','get_history'])


class ReviewFix(unittest.TestCase):
    def test_terminal_argument_rejections(self):
        for offset, bad in [(0,'{"limit":99}'),(0,'{"action":"history","limit":100}'),
                            (1,'{"detail":"summary"}'),(1,'{"detail":"full","extra":1}'),
                            (2,'{"limit":499,"errors_only":false}'),
                            (2,'{"limit":500,"errors_only":0}'),
                            (0,'{"limit":100.0}'),(0,'{"limit":true}'),
                            (0,'{"limit":100,"limit":100}'),(0,'{"limit":NaN}'),
                            (0,'{"limit":1e999}'),(0,'not JSON'),(0,'[]')]:
            with self.subTest(bad=bad):
                d=ledger(); d[3]['entries'][offset]['args']=bad
                d[6]['entries'][offset+3]['args']=bad  # preserve later prefix
                with self.assertRaises((RuntimeError,ValueError,TypeError)): p.validate_ledger(d)

    def test_each_observed_group_arguments_checked(self):
        for group in (3,6):
            d=ledger(); d[group]['entries'][0]['args']='{"limit":99}'
            with self.assertRaises(RuntimeError): p.validate_ledger(d)

    def test_terminal_normalization_accepts_whitespace_not_action(self):
        d=ledger()
        d[3]['entries'][0]['args']='{ "limit" : 100 }'
        d[6]['entries'][3]['args']=d[3]['entries'][0]['args']
        self.assertEqual(p.validate_ledger(d)['observed_appended_reads'],6)
        self.assertIn('not recursively observed',p.validate_ledger(d)['final_get_history_append'])

    def partial_start(self, failure):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            sup=p.Supervisor(Path(t)); fake=unittest.mock.Mock(pid=321,returncode=0)
            events=[]; ident={'pid':321,'start_ticks':99,'pgrp':321,'state':'S'}
            def mask(how,values):
                events.append(how)
                if how==p.signal.SIG_SETMASK and failure=='interrupt':
                    self.assertEqual(len(sup.children),1)
                    raise RuntimeError('simulated pending TERM at mask restore')
                return set()
            def getbirth(pid):
                self.assertEqual(len(sup.children),1)
                if failure=='birth': raise FileNotFoundError('simulated proc race')
                if failure=='group': return {**ident,'pgrp':999}
                return ident
            with patch.object(p.subprocess,'Popen',return_value=fake) as popen, \
                 patch.object(p.signal,'pthread_sigmask',side_effect=mask), \
                 patch.object(p.os,'pidfd_open',return_value=7),patch.object(p,'birth',side_effect=getbirth):
                with self.assertRaises((RuntimeError,FileNotFoundError)): sup.start('mock',['NEVER EXECUTED'])
            self.assertEqual(len(sup.children),1)
            self.assertIsNone(sup.children[0]['birth'])
            self.assertEqual(events,[p.signal.SIG_BLOCK,p.signal.SIG_SETMASK])
            self.assertTrue(popen.call_args.kwargs['start_new_session'])
            with patch.object(p.signal,'signal'),patch.object(p.signal,'pidfd_send_signal',create=True) as direct, \
                 patch.object(p.os,'killpg') as group,patch.object(p.os,'close'),patch.object(p,'put') as put:
                self.assertFalse(sup.cleanup())  # reaped handle does not erase identity failure
            group.assert_not_called(); fake.wait.assert_called_once()
            self.assertEqual(direct.call_count,0 if failure=='interrupt' else 1)
            self.assertEqual(put.call_args.args[1]['status'],'failed')
            self.assertTrue(put.call_args.args[1]['children'][0]['reaped'])

    def test_birth_failure_registered_and_pidfd_reaped_honestly(self): self.partial_start('birth')
    def test_registration_pending_signal_registered_and_bounded_reap(self): self.partial_start('interrupt')
    def test_group_identity_failure_never_signals_discovered_group(self): self.partial_start('group')

    def test_missing_identity_unreapable_is_failure_no_signal(self):
        sup=p.Supervisor(Path('/mock')); fake=unittest.mock.Mock(pid=321,returncode=None)
        fake.wait.side_effect=p.subprocess.TimeoutExpired('mock',1)
        sup.children=[{'p':fake,'birth':None,'label':'mock','reaped':False,'pidfd':None}]
        with patch.object(p.signal,'signal'),patch.object(p.os,'killpg') as group, \
             patch.object(p.signal,'pidfd_send_signal',create=True) as direct,patch.object(p,'put') as put:
            self.assertFalse(sup.cleanup())
        group.assert_not_called(); direct.assert_not_called()
        self.assertEqual(fake.wait.call_count,2)
        self.assertTrue(all(0<=c.kwargs['timeout']<=5 for c in fake.wait.call_args_list))
        self.assertFalse(put.call_args.args[1]['children'][0]['reaped'])

    def test_popen_failure_restores_mask_without_phantom_child(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            sup=p.Supervisor(Path(t))
            with patch.object(p.subprocess,'Popen',side_effect=OSError('mock')), \
                 patch.object(p.signal,'pthread_sigmask',return_value=set()) as mask:
                with self.assertRaises(OSError): sup.start('mock',['NEVER EXECUTED'])
            self.assertEqual(sup.children,[])
            self.assertEqual([c.args[0] for c in mask.call_args_list],[p.signal.SIG_BLOCK,p.signal.SIG_SETMASK])

    def test_cleanup_fd_and_receipt_errors_restore_handlers(self):
        sup=p.Supervisor(Path('/mock')); fake=unittest.mock.Mock(pid=321,returncode=0)
        sup.children=[{'p':fake,'birth':None,'label':'mock','reaped':True,'pidfd':7}]
        with patch.object(p.signal,'signal',return_value='old') as handlers, \
             patch.object(p.os,'close',side_effect=OSError('mock')), \
             patch.object(p,'put',side_effect=OSError('mock')) as put:
            self.assertFalse(sup.cleanup())
        self.assertEqual(put.call_count,2)
        self.assertIn('pidfd_close_failed',put.call_args.args[1]['errors'])
        self.assertEqual(handlers.call_args_list[-2:].copy(),[
            unittest.mock.call(p.signal.SIGTERM,'old'),unittest.mock.call(p.signal.SIGINT,'old')])

    def test_cleanup_receipt_write_failure_cannot_be_clean(self):
        sup=p.Supervisor(Path('/mock'))
        with patch.object(p.signal,'signal'),patch.object(p,'put',side_effect=OSError('mock')):
            self.assertFalse(sup.cleanup())

    def test_fd_close_attempts_every_resource(self):
        with patch.object(p.os,'close',side_effect=[OSError('mock'),None]) as close:
            with self.assertRaises(RuntimeError): p.close_descriptors([3,4])
        self.assertEqual(close.call_args_list,[unittest.mock.call(3),unittest.mock.call(4)])

    def test_capture_finalization_failure_has_best_effort_failure(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            out=Path(t); sup=unittest.mock.Mock(); sup.cleanup.side_effect=OSError('mock')
            original=p.put
            def put(path,row,*args):
                if path.name=='receipt.json': raise OSError('mock fsync failure')
                return original(path,row,*args)
            with patch.object(p,'Supervisor',return_value=sup),patch.object(p,'put',side_effect=put):
                self.assertFalse(p.capture(out,Path('/never-executed'),{}))
            evidence=p.strict((out/'receipt-finalization-failure.json').read_bytes())
            self.assertEqual(evidence['status'],'failed'); self.assertEqual(evidence['cleanup'],'failed')
            self.assertFalse(evidence['semantic_pass'])

    def test_main_restores_original_handlers_on_campaign_exception(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            out=Path(t); (out/'source.json').write_text('{}')
            binary=out/'mock'; binary.write_text('not executable, access mocked')
            (out/'build.json').write_text('{}')
            with patch.object(p.sys,'argv',['probe','capture',str(out),str(binary)]), \
                 patch.object(p,'binding',return_value={}),patch.object(p.os,'access',return_value=True), \
                 patch.object(p.signal,'signal',return_value='original') as handlers, \
                 patch.object(p,'capture',side_effect=RuntimeError('mock')):
                with self.assertRaises(RuntimeError): p.main()
            self.assertEqual(handlers.call_args_list[-2:],[unittest.mock.call(p.signal.SIGTERM,'original'),
                                                           unittest.mock.call(p.signal.SIGINT,'original')])

    def test_toolchain_missing_and_symlink_proxies_rejected_without_execution(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            home=Path(t)/'.rustup'; bin=home/'toolchains/1.99.0-x86_64-unknown-linux-gnu/bin'
            with self.assertRaises(RuntimeError): p.real_toolchain(home)
            bin.mkdir(parents=True)
            for name in ('rustc','cargo','rustdoc'):
                f=bin/name; f.write_text('never executed'); f.chmod(0o700)
            self.assertEqual(len(p.real_toolchain(home)),3)
            proxy=Path(t)/'rustup'; proxy.write_text('never executed'); proxy.chmod(0o700)
            (bin/'rustc').unlink(); (bin/'rustc').symlink_to(proxy.resolve())
            with self.assertRaises(RuntimeError): p.real_toolchain(home)

    def test_hosted_build_exact_real_tools_bounds_compiler_and_no_fallback(self):
        for bad_version in (False,True):
            with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
                out=Path(t); source={'head':'a'*40,'tree':'b'*40}; p.put(out/'source.json',source)
                tools={n:'/installed/bin/'+n for n in ('rustc','cargo','rustdoc')}
                calls=[]
                def run(argv,**kw):
                    calls.append((argv,kw))
                    ver='rustc 1.98.0 (abc 2026-01-01)' if bad_version else 'rustc 1.99.0 (abc 2026-01-01)'
                    return unittest.mock.Mock(stdout=(ver if argv[0].endswith('rustc') else 'cargo 1.99.0 (abc 2026-01-01)').encode())
                with patch.object(p,'binding',return_value=source),patch.object(p,'real_toolchain',return_value=tools), \
                     patch.object(p.subprocess,'run',side_effect=run),patch.object(p,'digest',return_value='c'*64), \
                     patch('shutil.which',return_value='/usr/bin/pkg-config'):
                    self.assertEqual(p.hosted_build(out),1 if bad_version else 0)
                self.assertEqual(calls[0][0],['/installed/bin/rustc','--version'])
                if bad_version:
                    self.assertEqual(len(calls),1); self.assertTrue((out/'preparation-failure.json').exists())
                else:
                    self.assertEqual([k['timeout'] for _,k in calls],[3,3,3,610])
                    self.assertEqual(calls[3][0][-4:],['/installed/bin/cargo','build','--locked','--release'])
                    self.assertEqual(calls[3][1]['env']['RUSTC'],'/installed/bin/rustc')
                    self.assertEqual(calls[3][1]['env']['RUSTDOC'],'/installed/bin/rustdoc')
                    self.assertEqual(calls[3][1]['env']['RUSTUP_TOOLCHAIN'],'/installed')
                    self.assertTrue((out/'build.json').exists())

    def test_exact_upload_manifest_excludes_logs_args_cookies_and_unexpected_json(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp')) as t:
            out=Path(t); campaign=out/'campaign'; campaign.mkdir()
            for name in ['secret.log','unexpected.json','anything.txt','01-http.args.json']:
                (campaign/name).write_text('RAW SECRET MOCK')
            owned=campaign/'owned'; owned.mkdir(); (owned/'Xauthority').write_text('RAW COOKIE MOCK')
            (campaign/'read-0.json').write_text('{"ok":true,"result":{}}')
            (campaign/'current-client.png').write_bytes(b'mock frame, not native proof')
            self.assertEqual(p.stage_evidence(out),0)
            files={f.name for f in (out/'upload').iterdir()}
            self.assertEqual(files,{'manifest.json','public-receipt.json','read-0.json','current-client.png'})
            manifest=p.strict((out/'upload/manifest.json').read_bytes())
            self.assertEqual({f['path'] for f in manifest['files']},files-{'manifest.json'})
            for f in (out/'upload').iterdir(): self.assertNotIn(b'RAW SECRET MOCK',f.read_bytes())
            self.assertFalse(manifest['raw_diagnostics_uploaded'])

    def test_geometry_after_bool_rejected_even_when_equal(self):
        # Existing checked_frame test validates bytes, here early geometry rejection only.
        app={}; geom={'x':0,'y':0,'width':1480,'height':920,'border':0,'depth':24,'root':1}
        row={'schema':p.SCHEMA,'kind':'same_window_frame','window':2,'title':'Beatbox Studio','app':app,
             'geometry_before':geom,'geometry_after':{**geom,'root':True},'focus_window':2,'focus_revert':0,
             'xtest_extension':True,'frame_sha256':'a'*64,'frame_bytes':1,'capture_started_ns':1,
             'capture_finished_ns':2,'visual_status':'executed_unreviewed','native_gestures':'UNRUN',
             'pixel_source':'XGetImage exact ordinary app client, not screenshot-tool Studio'}
        with self.assertRaises(RuntimeError): p.checked_frame(row,app,Path('/never-read'))

    def test_hosted_prep_exact_existing_and_upload_no_glob(self):
        workflow=MODULE.parent.parent/'.github/workflows/p0a-native-frame-probe.yml'
        shots=workflow.with_name('screenshots.yml').read_text()
        begin=shots.index('      - uses: dtolnay/rust-toolchain@')
        end=shots.index('      - name: build (release',begin)
        self.assertIn(shots[begin:end],workflow.read_text())
        uploads=[line for line in workflow.read_text().split('          path: |')[1].splitlines() if line.strip()]
        self.assertEqual(len(uploads),10)
        self.assertTrue(all('*' not in line and 'upload/' in line for line in uploads))
        self.assertNotIn('--offline',workflow.read_text())

if __name__=='__main__':
    unittest.main(verbosity=2)
