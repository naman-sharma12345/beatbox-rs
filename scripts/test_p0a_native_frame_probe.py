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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
            path=Path(t)/'receipt.json'; p.put(path,{'a':1})
            self.assertEqual(p.strict(path.read_bytes()),{'a':1})
            with self.assertRaises(FileExistsError): p.put(path,{'a':2})

    def test_put_cap(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
             'semantic_pass':False,'error':'mock failed preflight','diagnostic':p.new_diagnostic(),'bounds':{
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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

    def mock_capture(self, corrupt, fault=None, cleanup_mode=None, finalization_error=False):
        # Every native, HTTP and process dependency is mocked; only local files/PNG encoder run.
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
            if cleanup_mode=='unknown': sup.cleanup.side_effect=OSError('SECRET-DIAGNOSTIC-SENTINEL')
            if cleanup_mode=='failed': sup.cleanup.side_effect=None; sup.cleanup.return_value=False
            original_checkpoint=p.checkpoint; triggered=[]
            def checkpoint(value, stage):
                original_checkpoint(value,stage)
                if stage==fault and not triggered:
                    triggered.append(stage)
                    raise RuntimeError('SECRET-DIAGNOSTIC-SENTINEL')
            original_put=p.put
            def put(path, value, *args):
                if finalization_error and path.name=='receipt.json':
                    raise OSError('SECRET-DIAGNOSTIC-SENTINEL')
                return original_put(path,value,*args)
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
                 patch.object(Path,'read_bytes',fake_read), \
                 patch.object(p,'checkpoint',side_effect=checkpoint),patch.object(p,'put',side_effect=put):
                result=p.capture(out,binary,bind)
                if fault or cleanup_mode or finalization_error:
                    self.assertFalse(result)
                else:
                    self.assertEqual(result,not corrupt,(out/'receipt.json').read_text())
            if fault or cleanup_mode or finalization_error:
                import shutil
                failure=out/('receipt.json' if (out/'receipt.json').exists() else 'receipt-finalization-failure.json')
                receipt=p.strict(failure.read_bytes()); diag=receipt['diagnostic']
                expected=fault or ('cleanup' if cleanup_mode else 'final_receipt')
                self.assertEqual(diag['failure_stage'],expected)
                self.assertEqual(diag['cleanup'],cleanup_mode or 'reaped')
                if finalization_error: self.assertEqual(diag['finalization'],'failed')
                for f in out.rglob('*'):
                    if f.is_file(): self.assertNotIn(b'SECRET-DIAGNOSTIC-SENTINEL',f.read_bytes())
                parent=root/'stage-failure'; parent.mkdir()
                (parent/'source.json').write_bytes((out/'source.json').read_bytes())
                shutil.copytree(out,parent/'campaign')
                with patch.dict(p.os.environ,{'CAPTURE_OUTCOME':'failure'}): p.stage_evidence(parent)
                public=p.strict((parent/'upload/public-receipt.json').read_bytes())
                self.assertEqual(public['diagnostic']['failure_stage'],expected)
                self.assertEqual(public['status'],'failed')
                p.checked_public(public)
                return
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
            p.checked_public(public)
            if not corrupt:
                for key in ('path','env','argv','cookie','hostname','error','detail'):
                    for route in ((),('build',),('app',),('frame',),('frame','geometry_before'),
                                  ('display',),('listener',),('ledger',),('facility_libraries_loadable',)):
                        bad=copy.deepcopy(public); target=bad['linkage']
                        for name in route: target=target[name]
                        target[key]='SECRET-DIAGNOSTIC-SENTINEL'
                        with self.assertRaises(RuntimeError): p.checked_public(bad)
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
            out=Path(t); sup=unittest.mock.Mock(); sup.cleanup.side_effect=OSError('mock')
            original=p.put
            def put(path,row,*args):
                if path.name=='receipt.json': raise OSError('mock fsync failure')
                return original(path,row,*args)
            with patch.object(p,'Supervisor',return_value=sup),patch.object(p,'put',side_effect=put):
                self.assertFalse(p.capture(out,Path('/never-executed'),{}))
            evidence=p.strict((out/'receipt-finalization-failure.json').read_bytes())
            self.assertEqual(evidence['status'],'failed'); self.assertEqual(evidence['cleanup'],'unknown')
            self.assertFalse(evidence['semantic_pass'])

    def test_main_restores_original_handlers_on_campaign_exception(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
            out=Path(t); (out/'source.json').write_text('{}')
            binary=out/'mock'; binary.write_text('not executable, access mocked')
            (out/'build.json').write_text('{}')
            with patch.object(p.sys,'argv',['probe','capture',str(out),str(binary)]), \
                 patch.object(p,'binding',return_value={}),patch.object(p.os,'access',return_value=True), \
                 patch.object(p.signal,'signal',return_value='original') as handlers, \
                 patch.object(p,'capture',side_effect=RuntimeError('mock')):
                self.assertEqual(p.main(),1)
            self.assertEqual(handlers.call_args_list[-2:],[unittest.mock.call(p.signal.SIGTERM,'original'),
                                                           unittest.mock.call(p.signal.SIGINT,'original')])

    def test_toolchain_missing_and_symlink_proxies_rejected_without_execution(self):
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
            with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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
        with tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp')) as t:
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


class Diagnostics(unittest.TestCase):
    SENTINEL='SECRET-DIAGNOSTIC-SENTINEL'

    def temp(self):
        return tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-proof/test-tmp'))

    def public(self):
        with self.temp() as t:
            out=Path(t)
            p.stage_evidence(out)
            return p.strict((out/'upload/public-receipt.json').read_bytes())

    def test_each_stage_has_closed_validated_mapping(self):
        self.assertEqual(len(p.STAGES),len(set(p.STAGES)))
        for stage in p.STAGES:
            with self.subTest(stage=stage):
                value=p.new_diagnostic(); p.checkpoint(value,stage)
                p.diagnostic_failure(value,RuntimeError(self.SENTINEL))
                self.assertEqual(value['failure_stage'],stage)
                self.assertEqual(value['failure_code'],'operation_failed')
                self.assertEqual(p.checked_diagnostic(value),value)
                self.assertNotIn(self.SENTINEL,json.dumps(value))
                self.assertLess(len(json.dumps(value)),1024)

    def test_known_and_unknown_exceptions_never_formatted(self):
        class Unknown(Exception):
            def __str__(self): raise AssertionError('exception formatted')
            def __repr__(self): raise AssertionError('exception represented')
        for error,code in [(RuntimeError(self.SENTINEL),'operation_failed'),
                           (OSError(self.SENTINEL),'io_error'),
                           (ValueError(self.SENTINEL),'invalid_value'),
                           (p.subprocess.TimeoutExpired(self.SENTINEL,3),'timeout'),
                           (Unknown(self.SENTINEL),'unknown_exception')]:
            value=p.new_diagnostic(); p.diagnostic_failure(value,error)
            self.assertEqual(value['failure_code'],code)
            self.assertNotIn(self.SENTINEL,json.dumps(value))

    def test_first_failure_survives_cleanup_and_finalization_errors(self):
        value=p.new_diagnostic(); p.checkpoint(value,'frame_worker')
        p.diagnostic_failure(value,OSError(self.SENTINEL))
        for stage in ('cleanup','final_receipt','public_source','public_linkage'):
            p.checkpoint(value,stage); p.diagnostic_failure(value,ValueError(self.SENTINEL))
        self.assertEqual(value['failure_stage'],'frame_worker')
        self.assertEqual(value['failure_operation'],'frame')
        self.assertEqual(value['failure_code'],'io_error')

    def test_unknown_and_prohibited_diagnostic_fields_rejected(self):
        for key in ('error','path','env','argv','cookie','hostname','stdout','traceback','detail','unknown'):
            value=p.new_diagnostic(); value[key]=self.SENTINEL
            with self.assertRaises(RuntimeError): p.checked_diagnostic(value)
        for key in ('stage','failure_stage','failure_code','operation','failure_operation',
                    'xvfb_startup','studio_startup','cleanup','finalization'):
            value=p.new_diagnostic(); value[key]=self.SENTINEL
            with self.assertRaises(RuntimeError): p.checked_diagnostic(value)
        for key in ('stage','cleanup','finalization','xvfb_startup','studio_startup'):
            value=p.new_diagnostic(); value[key]=True
            with self.assertRaises(RuntimeError): p.checked_diagnostic(value)

    def test_actual_public_validator_version_fields_and_payload_cap(self):
        row=self.public(); self.assertEqual(row['schema'],p.PUBLIC_SCHEMA)
        p.checked_public(row)
        for key in ('path','env','argv','cookie','hostname','stdout','error','detail','unknown'):
            value=copy.deepcopy(row); value[key]=self.SENTINEL
            with self.assertRaises(RuntimeError): p.checked_public(value)
        for key,value in [('schema',p.SCHEMA),('status','PASS'),('visual_status','accepted'),
                          ('semantic_pass',True),('native_gestures','PASS'),('head',self.SENTINEL)]:
            bad=copy.deepcopy(row); bad[key]=value
            with self.assertRaises(RuntimeError): p.checked_public(bad)
        bad=copy.deepcopy(row);bad['diagnostic']['detail']='x'*p.RECEIPT_CAP
        with self.assertRaises(RuntimeError): p.checked_public(bad)
        self.assertLess(len(json.dumps(row)),4096)
        self.assertEqual(len(p.UPLOAD_FILES),9)

    def test_nullable_failure_requires_consistent_pairs(self):
        value=p.new_diagnostic();p.checked_diagnostic(value)
        for key,val in [('failure_stage','capture_entry'),('failure_code','io_error'),
                        ('failure_operation','capture')]:
            bad=copy.deepcopy(value);bad[key]=val
            with self.assertRaises(RuntimeError): p.checked_diagnostic(bad)

    def test_no_receipt_means_unknown_startup_not_cleanup_success(self):
        row=self.public(); diag=row['diagnostic']
        self.assertEqual(row['status'],'failed')
        self.assertEqual(diag['xvfb_startup'],'unknown')
        self.assertEqual(diag['studio_startup'],'unknown')
        self.assertEqual(diag['cleanup'],'unknown')
        self.assertIsNone(row['linkage'])

    def test_capture_constructor_failure_safe_and_no_cleanup_claim(self):
        with self.temp() as t:
            out=Path(t)
            with patch.object(p,'Supervisor',side_effect=OSError(self.SENTINEL)):
                self.assertFalse(p.capture(out,Path('/not-executed'),{}))
            row=p.strict((out/'receipt.json').read_bytes())
            self.assertEqual(row['diagnostic']['failure_stage'],'supervisor_init')
            self.assertEqual(row['diagnostic']['cleanup'],'not_attempted')
            self.assertEqual(row['diagnostic']['xvfb_startup'],'unknown')
            self.assertNotIn(self.SENTINEL,(out/'receipt.json').read_text())

    def test_full_mock_operation_checkpoint_faults(self):
        stages=('build_binding','scratch_setup','facilities','fixture_copy','auth_setup',
                'display_pipe','xvfb_start','display_ready','display_record','display_close',
                'facilities_validation','read_validation','display_ownership','studio_start','app_ready','app_ownership','listener_ownership',
                'frame_ownership','frame_worker','frame_validation','ledger_validation',
                'final_binding','final_ownership','campaign_budget','final_clock',
                'final_validation','final_receipt') + tuple('read_'+str(i) for i in range(7))
        for stage in stages:
            with self.subTest(stage=stage): Pure().mock_capture(False,fault=stage)

    def test_cleanup_raised_is_unknown_not_reaped_or_known_failed(self):
        Pure().mock_capture(False,cleanup_mode='unknown')

    def test_cleanup_false_is_known_failed(self):
        Pure().mock_capture(False,cleanup_mode='failed')

    def test_capture_failure_preserved_when_receipt_write_also_fails(self):
        Pure().mock_capture(False,fault='facilities',finalization_error=True)

    def test_success_body_but_receipt_write_failure_cannot_pass(self):
        Pure().mock_capture(False,finalization_error=True)

    def test_partial_startup_stage_and_unknown_cleanup_remain_honest(self):
        for fail,expected in [('spawn','child_spawn'),('mask','child_mask_restore'),
                              ('pidfd','child_pidfd'),('birth','child_birth'),('group','child_group')]:
            with self.subTest(fail=fail),self.temp() as t:
                sup=p.Supervisor(Path(t)); sup.diagnostic=p.new_diagnostic()
                sup.diagnostic['xvfb_startup']='not_started'
                fake=unittest.mock.Mock(pid=321,returncode=None)
                def mask(how,values):
                    if fail=='mask' and how==p.signal.SIG_SETMASK: raise RuntimeError(self.SENTINEL)
                    return set()
                with patch.object(p.signal,'pthread_sigmask',side_effect=mask), \
                     patch.object(p.subprocess,'Popen',side_effect=OSError(self.SENTINEL) if fail=='spawn' else None,return_value=fake), \
                     patch.object(p.os,'pidfd_open',side_effect=OSError(self.SENTINEL) if fail=='pidfd' else None,return_value=7), \
                     patch.object(p,'birth',side_effect=OSError(self.SENTINEL) if fail=='birth' else None,
                         return_value={'pid':321,'start_ticks':1,'pgrp':999 if fail=='group' else 321,'state':'S'}):
                    try: sup.start('xvfb',['NEVER EXECUTED'])
                    except Exception as error: p.diagnostic_failure(sup.diagnostic,error)
                    else: self.fail('mock failure did not fail')
                self.assertEqual(sup.diagnostic['failure_stage'],expected)
                self.assertEqual(sup.diagnostic['xvfb_startup'],'unknown' if fail=='spawn' else 'registered')
                self.assertEqual(len(sup.children),0 if fail=='spawn' else 1)
                self.assertNotIn(self.SENTINEL,json.dumps(sup.diagnostic))

    def test_pre_capture_source_failure_and_safe_marker_staging(self):
        import contextlib,io
        with self.temp() as t:
            out=Path(t); stdout=io.StringIO();stderr=io.StringIO()
            with patch.object(p.sys,'argv',['probe','capture',str(out),'not-executed']), \
                 patch.object(p.signal,'signal'),patch.object(p,'binding',side_effect=OSError(self.SENTINEL)), \
                 contextlib.redirect_stdout(stdout),contextlib.redirect_stderr(stderr):
                self.assertEqual(p.main(),1)
            self.assertEqual(stdout.getvalue()+stderr.getvalue(),'')
            marker=p.strict((out/'receipt-finalization-failure.json').read_bytes())
            self.assertEqual(marker['diagnostic']['failure_stage'],'source_binding')
            self.assertEqual(marker['diagnostic']['cleanup'],'not_attempted')
            p.stage_evidence(out)
            public=p.strict((out/'upload/public-receipt.json').read_bytes())
            self.assertEqual(public['diagnostic']['failure_stage'],'source_binding')
            self.assertNotIn(self.SENTINEL,json.dumps(public))

    def test_source_hash_exception_has_explicit_stage(self):
        value=p.new_diagnostic()
        with patch.dict(p.os.environ,{'REVIEWED_HEAD':'a'*40,'REVIEWED_TREE':'b'*40}), \
             patch.object(p.subprocess,'check_output',side_effect=[b'a'*40,b'b'*40,b'']), \
             patch.object(p,'digest',side_effect=OSError(self.SENTINEL)):
            try: p.binding(value)
            except Exception as error: p.diagnostic_failure(value,error)
        self.assertEqual(value['failure_stage'],'source_hash')

    def test_worker_wait_keeps_read_operation(self):
        with self.temp() as t:
            sup=p.Supervisor(Path(t));sup.diagnostic=p.new_diagnostic()
            p.checkpoint(sup.diagnostic,'read_5')
            with patch.object(sup,'start',return_value=(object(),{})), \
                 patch.object(sup,'wait',side_effect=p.subprocess.TimeoutExpired(self.SENTINEL,3)):
                try: sup.worker('http',{})
                except Exception as error: p.diagnostic_failure(sup.diagnostic,error)
            self.assertEqual(sup.diagnostic['failure_stage'],'child_worker_wait')
            self.assertEqual(sup.diagnostic['failure_operation'],'read_5')
            self.assertEqual(sup.diagnostic['failure_code'],'timeout')

    def test_malformed_failure_markers_never_publish_fields(self):
        for marker in ({'diagnostic':{'detail':self.SENTINEL}},
                       {'schema':p.SCHEMA,'status':'failed','cleanup':'reaped','semantic_pass':False,
                        'native_gestures':'UNRUN','diagnostic':{**p.new_diagnostic(),'path':self.SENTINEL}}):
            with self.temp() as t:
                out=Path(t);p.put(out/'receipt-finalization-failure.json',marker)
                p.stage_evidence(out)
                public=p.strict((out/'upload/public-receipt.json').read_bytes())
                p.checked_public(public)
                self.assertEqual(public['status'],'failed')
                self.assertNotIn(self.SENTINEL,json.dumps(public))


class DiagnosticEntry(unittest.TestCase):
    def test_pre_capture_checkpoint_boundaries_and_handler_restore(self):
        stages=('handler_install','capture_arguments','output_guard','source_binding',
                'source_record','binary_guard','campaign_directory','build_copy','handler_restore')
        for stage in stages:
            with self.subTest(stage=stage),Diagnostics().temp() as t:
                out=Path(t);p.put(out/'source.json',{});p.put(out/'build.json',{})
                binary=out/'mock';binary.write_bytes(b'NEVER EXECUTED')
                original=p.checkpoint;triggered=[]
                def checkpoint(value,where):
                    original(value,where)
                    if where==stage and not triggered:
                        triggered.append(where);raise OSError(Diagnostics.SENTINEL)
                with patch.object(p.sys,'argv',['probe','capture',str(out),str(binary)]), \
                     patch.object(p,'binding',return_value={}),patch.object(p.os,'access',return_value=True), \
                     patch.object(p.signal,'signal'),patch.object(p,'checkpoint',side_effect=checkpoint), \
                     patch.object(p,'capture',return_value=True):
                    self.assertEqual(p.main(),1)
                marker=p.strict((out/'receipt-finalization-failure.json').read_bytes())
                self.assertEqual(marker['diagnostic']['failure_stage'],stage)
                self.assertNotIn(Diagnostics.SENTINEL,json.dumps(marker))
                p.stage_evidence(out)
                public=p.strict((out/'upload/public-receipt.json').read_bytes())
                self.assertEqual(public['status'],'failed')
                self.assertEqual(public['diagnostic']['failure_stage'],stage)

    def test_primary_entry_failure_preserved_if_restore_also_fails(self):
        with Diagnostics().temp() as t:
            out=Path(t)
            def handler(sig,value):
                if value=='original': raise OSError(Diagnostics.SENTINEL)
                return 'original'
            with patch.object(p.sys,'argv',['probe','capture',str(out),'not-executed']), \
                 patch.object(p.signal,'signal',side_effect=handler) as restore, \
                 patch.object(p,'binding',side_effect=ValueError(Diagnostics.SENTINEL)):
                self.assertEqual(p.main(),1)
            self.assertEqual(restore.call_count,4)
            marker=p.strict((out/'receipt-finalization-failure.json').read_bytes())
            self.assertEqual(marker['diagnostic']['failure_stage'],'source_binding')
            self.assertEqual(marker['diagnostic']['failure_code'],'invalid_value')
            self.assertEqual(marker['diagnostic']['finalization'],'failed')

    def test_staging_itself_rejects_unrecognized_constructed_public_fields(self):
        with Diagnostics().temp() as t:
            out=Path(t); original=p.checked_public
            def validator(value):
                value['env']=Diagnostics.SENTINEL
                return original(value)
            with patch.object(p,'checked_public',side_effect=validator):
                with self.assertRaises(RuntimeError): p.stage_evidence(out)
            self.assertFalse((out/'upload/public-receipt.json').exists())
            self.assertFalse((out/'upload/manifest.json').exists())

class DiagnosticSuccessor(unittest.TestCase):
    # New tests only; the 77 rejected-candidate methods remain unchanged.
    def temp(self):
        return tempfile.TemporaryDirectory(dir=str(Path.cwd()/'.openclaw/tmp/beatbox-p0a-frame-diagnostic-fixed-proof/test-tmp'))

    def stage_copy(self, out, outcome):
        import shutil
        parent=out.parent/('stage-'+outcome); parent.mkdir()
        shutil.copytree(out,parent/'campaign')
        p.put(parent/'source.json',{'head':'a'*40,'tree':'b'*40,
                                  'runner_sha256':'c'*64,'workflow_sha256':'d'*64})
        with patch.dict(p.os.environ,{'CAPTURE_OUTCOME':outcome}): p.stage_evidence(parent)
        public=p.strict((parent/'upload/public-receipt.json').read_bytes())
        p.checked_public(public)
        return public

    def observe_full_capture(self, observer):
        original=p.capture
        class Observed(Exception): pass
        def capture(out,binary,binding,*a,**kw):
            observer(out,binary,binding,p.Supervisor.return_value,original)
            raise Observed()
        with patch.object(p,'capture',side_effect=capture):
            with self.assertRaises(Observed): Pure().mock_capture(False)

    def test_receipt_serialization_file_and_parent_fsync_failures_never_promise_written(self):
        # Actual scratch serialization/write/fsync, failing after bytes were
        # emitted or at either fsync; the failure marker cannot be written.
        for boundary in ('serialized','file_fsync','parent_fsync'):
            with self.subTest(boundary=boundary),self.temp() as t:
                out=Path(t)/'capture';out.mkdir()
                sup=unittest.mock.Mock();sup.cleanup.return_value=True
                diagnostic=p.new_diagnostic();real_put=p.put;real_open=Path.open
                real_fsync=p.os.fsync;calls=[]
                class FailedWrite:
                    def __init__(self, file): self.file=file
                    def __enter__(self): self.file.__enter__(); return self
                    def __exit__(self,*args): return self.file.__exit__(*args)
                    def write(self,data):
                        self.file.write(data)
                        raise OSError('SECRET-DIAGNOSTIC-SENTINEL')
                def opened(path,*a,**kw):
                    file=real_open(path,*a,**kw)
                    return FailedWrite(file) if path.name=='receipt.json' and a==('xb',) else file
                def fsync(fd):
                    calls.append(fd)
                    if len(calls)==(1 if boundary=='file_fsync' else 2):
                        raise OSError('SECRET-DIAGNOSTIC-SENTINEL')
                    return real_fsync(fd)
                def put(path,value,*args):
                    if path.name=='receipt-finalization-failure.json': raise PermissionError('SECRET-DIAGNOSTIC-SENTINEL')
                    self.assertEqual(value['diagnostic']['finalization'],'unknown')
                    if boundary=='serialized':
                        with patch.object(Path,'open',opened): return real_put(path,value,*args)
                    with patch.object(p.os,'fsync',side_effect=fsync): return real_put(path,value,*args)
                with patch.object(p,'Supervisor',return_value=sup),patch.object(p,'put',side_effect=put):
                    self.assertFalse(p.capture(out,Path('/never-executed'),{},diagnostic))
                self.assertEqual(diagnostic['finalization'],'failed')
                self.assertEqual(diagnostic['failure_stage'],'build_binding')
                row=p.strict((out/'receipt.json').read_bytes())
                self.assertEqual(row['diagnostic']['finalization'],'unknown')
                self.assertFalse((out/'receipt-finalization-failure.json').exists())
                public=self.stage_copy(out,'failure')
                self.assertEqual(public['status'],'failed')
                self.assertEqual(public['diagnostic']['finalization'],'unknown')
                self.assertNotIn('SECRET-DIAGNOSTIC-SENTINEL',json.dumps(public))
                if boundary!='serialized': self.assertEqual(len(calls),1 if boundary=='file_fsync' else 2)

    def test_failed_body_successfully_persisted_is_not_persist_failure(self):
        with self.temp() as t:
            out=Path(t)/'capture';out.mkdir();sup=unittest.mock.Mock();sup.cleanup.return_value=True
            diagnostic=p.new_diagnostic();seen=[];real_put=p.put
            def put(path,value,*args):
                seen.append(copy.deepcopy(value))
                return real_put(path,value,*args)
            with patch.object(p,'Supervisor',return_value=sup),patch.object(p,'put',side_effect=put):
                self.assertFalse(p.capture(out,Path('/never-executed'),{},diagnostic))
            self.assertEqual(seen[0]['diagnostic']['finalization'],'unknown')
            self.assertEqual(diagnostic['finalization'],'written')
            self.assertEqual(diagnostic['failure_stage'],'build_binding')
            self.assertFalse((out/'receipt-finalization-failure.json').exists())
            public=self.stage_copy(out,'failure')
            self.assertEqual(public['status'],'failed')
            self.assertEqual(public['diagnostic']['finalization'],'unknown')

    def test_stale_written_downgraded_without_trusted_completion(self):
        with self.temp() as t:
            out=Path(t)/'capture';out.mkdir();sup=unittest.mock.Mock();sup.cleanup.return_value=True
            with patch.object(p,'Supervisor',return_value=sup): p.capture(out,Path('/never-executed'),{})
            row=p.strict((out/'receipt.json').read_bytes());row['diagnostic']['finalization']='written'
            # A legacy positive in otherwise validated failure-body bytes is
            # not trusted completion, even with a contradictory success outcome.
            (out/'receipt.json').unlink();p.put(out/'receipt.json',row)
            for outcome in ('failure','success'):
                public=self.stage_copy(out,outcome)
                self.assertEqual(public['status'],'failed')
                self.assertEqual(public['diagnostic']['finalization'],'unknown')

    def test_explicit_malformed_and_unreadable_markers_downgrade_stale_positive(self):
        for location in ('campaign','outer'):
            for kind in ('explicit','malformed','unreadable','directory'):
                with self.subTest(location=location,kind=kind),self.temp() as t:
                    out=Path(t);campaign=out/'campaign';campaign.mkdir()
                    diagnostic=p.new_diagnostic();diagnostic['finalization']='written'
                    row={'diagnostic':diagnostic}
                    path=(campaign if location=='campaign' else out)/'receipt-finalization-failure.json'
                    if kind=='explicit': p.failure_receipt(path,diagnostic)
                    elif kind=='directory': path.mkdir()
                    else: path.write_bytes(b'{')
                    original=Path.read_bytes
                    def read(file):
                        if kind=='unreadable' and file==path: raise PermissionError('SECRET-DIAGNOSTIC-SENTINEL')
                        return original(file)
                    with patch.object(Path,'read_bytes',read):
                        staged=p.staged_diagnostic(out,campaign,row)
                    self.assertEqual(staged['finalization'],'failed')
                    self.assertNotIn('SECRET-DIAGNOSTIC-SENTINEL',json.dumps(staged))

    def test_normal_success_trusted_outcome_and_validated_receipt_still_stage(self):
        def observe(out,binary,binding,sup,capture):
            diagnostic=p.new_diagnostic()
            self.assertTrue(capture(out,binary,binding,diagnostic))
            self.assertEqual(diagnostic['finalization'],'written')
            self.assertEqual(p.strict((out/'receipt.json').read_bytes())['diagnostic']['finalization'],'unknown')
            public=self.stage_copy(out,'success')
            self.assertEqual(public['status'],'probe_complete_not_semantic_pass')
            self.assertEqual(public['diagnostic']['finalization'],'written')
            self.assertIsNotNone(public['linkage'])
            failed=self.stage_copy(out,'failure')
            self.assertEqual(failed['status'],'failed')
            self.assertEqual(failed['diagnostic']['finalization'],'unknown')
        self.observe_full_capture(observe)

    def test_success_shaped_stale_receipt_with_bad_markers_fails_finalization(self):
        def observe(out,binary,binding,sup,capture):
            self.assertTrue(capture(out,binary,binding))
            original=p.strict((out/'receipt.json').read_bytes())
            original['diagnostic']['finalization']='written'
            (out/'receipt.json').unlink();p.put(out/'receipt.json',original)
            for name in ('receipt-finalization-failure.json','cleanup-finalization-failure.json'):
                (out/name).write_bytes(b'{')
                # stage_copy needs a distinct root for each case.
                import shutil
                dest=out.parent/name;shutil.copytree(out,dest)
                public=self.stage_copy(dest,'success')
                self.assertEqual(public['status'],'failed')
                self.assertEqual(public['diagnostic']['finalization'],'failed')
                (out/name).unlink()
                (dest.parent/'stage-success').rename(dest.parent/('result-'+name))
        self.observe_full_capture(observe)

    def test_success_body_parent_fsync_failure_with_or_without_failure_marker(self):
        for marker_writable in (False,True):
            def observe(out,binary,binding,sup,capture):
                real_put=p.put;real_fsync=p.os.fsync;calls=[];diagnostic=p.new_diagnostic()
                def fsync(fd):
                    calls.append(fd)
                    if len(calls)==2: raise OSError('SECRET-DIAGNOSTIC-SENTINEL')
                    return real_fsync(fd)
                def put(path,value,*args):
                    if path.name=='receipt-finalization-failure.json' and not marker_writable:
                        raise PermissionError('SECRET-DIAGNOSTIC-SENTINEL')
                    if path.name=='receipt.json':
                        self.assertEqual(value['status'],'probe_complete_not_semantic_pass')
                        self.assertEqual(value['diagnostic']['finalization'],'unknown')
                        with patch.object(p.os,'fsync',side_effect=fsync): return real_put(path,value,*args)
                    return real_put(path,value,*args)
                with patch.object(p,'put',side_effect=put):
                    self.assertFalse(capture(out,binary,binding,diagnostic))
                self.assertEqual(len(calls),2)
                self.assertEqual(diagnostic['failure_stage'],'final_receipt')
                self.assertEqual(diagnostic['finalization'],'failed')
                row=p.strict((out/'receipt.json').read_bytes())
                self.assertEqual(row['status'],'probe_complete_not_semantic_pass')
                self.assertEqual(row['diagnostic']['finalization'],'unknown')
                public=self.stage_copy(out,'failure')
                self.assertIsNotNone(public['linkage'])
                self.assertEqual(public['status'],'failed')
                self.assertEqual(public['diagnostic']['finalization'],'failed' if marker_writable else 'unknown')
                self.assertEqual((out/'receipt-finalization-failure.json').exists(),marker_writable)
            with self.subTest(marker_writable=marker_writable): self.observe_full_capture(observe)

    def test_unreadable_marker_staging_downgrades_success_shaped_written(self):
        def observe(out,binary,binding,sup,capture):
            self.assertTrue(capture(out,binary,binding))
            row=p.strict((out/'receipt.json').read_bytes());row['diagnostic']['finalization']='written'
            (out/'receipt.json').unlink();p.put(out/'receipt.json',row)
            (out/'receipt-finalization-failure.json').write_bytes(b'{}')
            read=Path.read_bytes
            def unreadable(path):
                if path.name=='receipt-finalization-failure.json': raise PermissionError('SECRET-DIAGNOSTIC-SENTINEL')
                return read(path)
            with patch.object(Path,'read_bytes',unreadable): public=self.stage_copy(out,'success')
            self.assertEqual(public['status'],'failed')
            self.assertEqual(public['diagnostic']['finalization'],'failed')
            self.assertNotIn('SECRET-DIAGNOSTIC-SENTINEL',json.dumps(public))
        self.observe_full_capture(observe)

    def test_spawn_and_log_close_two_error_first_failure(self):
        class Unknown(Exception):
            def __str__(self): raise AssertionError('formatted')
            def __repr__(self): raise AssertionError('represented')
        for first,second,code in ((ValueError(),OSError(),'invalid_value'),
                                  (Unknown(),OSError(),'unknown_exception'),
                                  (ValueError(),Unknown(),'invalid_value'),
                                  (Unknown(),Unknown(),'unknown_exception')):
            with self.subTest(code=code),self.temp() as t:
                sup=p.Supervisor(Path(t));sup.diagnostic=p.new_diagnostic();p.checkpoint(sup.diagnostic,'xvfb_start')
                class BadClose:
                    def __enter__(self): return object()
                    def __exit__(self,*a): raise second
                with patch.object(p,'put'),patch.object(Path,'open',return_value=BadClose()), \
                     patch.object(p.signal,'pthread_sigmask',return_value=set()) as mask, \
                     patch.object(p.subprocess,'Popen',side_effect=first):
                    with self.assertRaises(type(second)): sup.start('xvfb',['NEVER EXECUTED'])
                self.assertEqual(sup.diagnostic['failure_stage'],'child_spawn')
                self.assertEqual(sup.diagnostic['failure_operation'],'xvfb')
                self.assertEqual(sup.diagnostic['failure_code'],code)
                self.assertEqual(sup.children,[])
                self.assertEqual([c.args[0] for c in mask.call_args_list],[p.signal.SIG_BLOCK,p.signal.SIG_SETMASK])

    def test_log_close_only_retains_registered_handle_and_close_stage(self):
        with self.temp() as t:
            sup=p.Supervisor(Path(t));sup.diagnostic=p.new_diagnostic();p.checkpoint(sup.diagnostic,'xvfb_start')
            fake=unittest.mock.Mock(pid=321)
            class BadClose:
                def __enter__(self): return object()
                def __exit__(self,*a): raise OSError()
            with patch.object(p,'put'),patch.object(Path,'open',return_value=BadClose()), \
                 patch.object(p.signal,'pthread_sigmask',return_value=set()), \
                 patch.object(p.subprocess,'Popen',return_value=fake):
                with self.assertRaises(OSError): sup.start('xvfb',['NEVER EXECUTED'])
            self.assertIs(sup.children[0]['p'],fake)
            self.assertEqual(sup.diagnostic['xvfb_startup'],'registered')
            self.assertEqual(sup.diagnostic['failure_stage'],'child_log_close')
            self.assertEqual(sup.diagnostic['failure_operation'],'xvfb')
            self.assertEqual(sup.diagnostic['failure_code'],'io_error')

    def test_actual_writer_close_after_child_group_checkpoint_is_display_close(self):
        def observe(out,binary,binding,sup,capture):
            start=sup.start.side_effect;real_close=p.os.close;target=[];failed=[]
            def started(label,argv,env=None,pass_fds=()):
                result=start(label,argv,env,pass_fds)
                if label=='xvfb': target.append(pass_fds[0])
                p.checkpoint(sup.diagnostic,'child_group')
                return result
            def close(fd):
                if target and fd==target[0] and not failed:
                    failed.append(fd);raise OSError('SECRET-DIAGNOSTIC-SENTINEL')
                return real_close(fd)
            with patch.object(sup.start,'side_effect',started),patch.object(p.os,'close',side_effect=close):
                self.assertFalse(capture(out,binary,binding))
            self.assertEqual(len(failed),1)
            diag=p.strict((out/'receipt.json').read_bytes())['diagnostic']
            self.assertEqual(diag['failure_stage'],'display_close')
            self.assertEqual(diag['failure_operation'],'xvfb')
            self.assertEqual(diag['failure_code'],'io_error')
            public=self.stage_copy(out,'failure')
            self.assertEqual(public['diagnostic']['failure_stage'],'display_close')
            self.assertNotIn('SECRET-DIAGNOSTIC-SENTINEL',json.dumps(public))
        self.observe_full_capture(observe)


class DisplayReadiness(unittest.TestCase):
    def branch(self, kind, stage, code):
        def observe(out,binary,binding,sup,capture):
            original_start=sup.start.side_effect
            def start(label,*args,**kwargs):
                result=original_start(label,*args,**kwargs)
                if label=='xvfb': sup.diagnostic['xvfb_startup']='identified'
                p.checkpoint(sup.diagnostic,'child_group')
                return result
            original_remaining=sup.remaining.side_effect
            def remaining(maximum=p.CHILD_SECONDS):
                if kind=='budget': raise RuntimeError('PRIVATE-SENTINEL')
                return original_remaining(maximum)
            def selected(*args):
                if kind=='io': raise OSError('PRIVATE-SENTINEL')
                if kind=='exception': raise RuntimeError('PRIVATE-SENTINEL')
                return ([],[],[]) if kind=='empty' else ([args[0][0]],[],[])
            with patch.object(sup.start,'side_effect',start), \
                 patch.object(sup.remaining,'side_effect',remaining), \
                 patch.object(p.select,'select',side_effect=selected) as select_call:
                self.assertFalse(capture(out,binary,binding))
            row=p.strict((out/'receipt.json').read_bytes())
            diag=row['diagnostic']
            self.assertEqual((diag['failure_stage'],diag['failure_operation'],diag['failure_code']),
                             (stage,'xvfb',code))
            self.assertEqual(diag['cleanup'],'reaped')
            self.assertEqual(diag['xvfb_startup'],'identified')
            self.assertEqual(diag['studio_startup'],'not_started')
            self.assertEqual(row['status'],'failed')
            self.assertIsNone(row['app']); self.assertEqual(row['reads'],[])
            self.assertIsNone(row['frame'])
            self.assertEqual(sup.start.call_count,1)
            self.assertEqual(sup.start.call_args.args[0],'xvfb')
            sup.cleanup.assert_called_once()
            self.assertEqual(select_call.call_count,0 if kind=='budget' else 1)
            if kind!='budget': self.assertEqual(select_call.call_args.args[3],3)
            public=DiagnosticSuccessor().stage_copy(out,'failure')
            self.assertEqual(public['diagnostic']['failure_stage'],stage)
            self.assertEqual(public['diagnostic']['failure_code'],code)
            self.assertIsNone(public['linkage'])
            self.assertEqual(public['status'],'failed')
            self.assertEqual(public['diagnostic']['finalization'],'unknown')
            self.assertNotIn('PRIVATE-SENTINEL',json.dumps(public))
        DiagnosticSuccessor().observe_full_capture(observe)

    def test_empty_wait_is_only_display_ready_timeout_not_process_exit(self):
        self.branch('empty','display_ready','display_ready_timeout')

    def test_expired_budget_never_enters_select(self):
        self.branch('budget','display_budget','operation_failed')

    def test_select_oserror_is_not_reported_as_timeout(self):
        self.branch('io','display_ready','io_error')

    def test_select_runtimeerror_is_not_reported_as_empty_result(self):
        self.branch('exception','display_ready','operation_failed')

    def test_remaining_caps_campaign_and_child_without_extension(self):
        with patch.object(p.time,'monotonic',return_value=100): sup=p.Supervisor(Path('/mock'))
        self.assertEqual(sup.deadline,190)
        for now,expected in ((100,3),(188.75,1.25)):
            with patch.object(p.time,'monotonic',return_value=now):
                self.assertEqual(sup.remaining(),expected)
        for now in (190,191):
            with patch.object(p.time,'monotonic',return_value=now):
                with self.assertRaises(RuntimeError): sup.remaining()

    def test_new_enums_remain_closed_and_first_failure_is_retained(self):
        diag=p.new_diagnostic(); p.checkpoint(diag,'display_ready')
        p.diagnostic_failure(diag,code='display_ready_timeout')
        for where in ('display_close','cleanup','final_receipt'):
            p.checkpoint(diag,where);p.diagnostic_failure(diag,OSError('PRIVATE-SENTINEL'))
        self.assertEqual(diag['failure_code'],'display_ready_timeout')
        self.assertEqual(diag['failure_stage'],'display_ready')
        for key,value in (('failure_code','process_exited'),('stage','arbitrary_detail')):
            bad=copy.deepcopy(diag);bad[key]=value
            with self.assertRaises(RuntimeError): p.checked_diagnostic(bad)

    def test_preexec_restores_only_saved_mask_then_limits_and_keeps_fd(self):
        with Diagnostics().temp() as t:
            sup=p.Supervisor(Path(t)); events=[]
            saved={p.signal.SIGUSR1}
            def mask(how,values):
                events.append(('mask',how,set(values)))
                return saved
            fake=unittest.mock.Mock(pid=321)
            ident={'pid':321,'start_ticks':99,'pgrp':321,'state':'S'}
            with patch.object(p.subprocess,'Popen',return_value=fake) as popen, \
                 patch.object(p.signal,'pthread_sigmask',side_effect=mask), \
                 patch.object(p.signal,'signal') as disposition, \
                 patch.object(p,'limits',side_effect=lambda:events.append(('limits',))), \
                 patch.object(p.os,'pidfd_open',return_value=7), \
                 patch.object(p,'birth',return_value=ident):
                sup.start('xvfb',['NEVER EXECUTED'],{'PATH':'/mock'},(42,))
                self.assertEqual(popen.call_args.kwargs['pass_fds'],(42,))
                self.assertTrue(popen.call_args.kwargs['start_new_session'])
                popen.call_args.kwargs['preexec_fn']()
            self.assertEqual(events,[('mask',p.signal.SIG_BLOCK,{p.signal.SIGINT,p.signal.SIGTERM}),
                                     ('mask',p.signal.SIG_SETMASK,saved),
                                     ('mask',p.signal.SIG_SETMASK,saved),('limits',)])
            disposition.assert_not_called()


class DisplayObservation(unittest.TestCase):
    def owned(self):
        sup=p.Supervisor(Path('/mock'))
        ident={'pid':322,'start_ticks':100,'pgrp':322,'state':'S'}
        child=unittest.mock.Mock(pid=322,returncode=None)
        row={'p':child,'birth':ident,'label':'xvfb','reaped':False,'pidfd':7}
        sup.children=[row]
        return sup,child,ident,row

    def samples(self, values, expected):
        sup,child,ident,row=self.owned()
        with patch.object(p,'birth',side_effect=values) as read:
            result=p.xvfb_liveness_after_timeout(sup,child,ident)
        self.assertEqual(result,expected)
        self.assertLessEqual(read.call_count,2)
        self.assertTrue(all(call.args==(322,) for call in read.call_args_list))
        child.poll.assert_not_called();child.wait.assert_not_called()
        self.assertIsNone(child.returncode);self.assertFalse(row['reaped'])
        return read.call_count

    def test_alive_samples_include_blocked_and_stopped_without_cause_claim(self):
        _,_,ident,_=self.owned()
        for state in ('R','S','D','T','t','I'):
            with self.subTest(state=state):
                self.assertEqual(self.samples([{**ident,'state':state},{**ident,'state':'S'}],'alive_owned'),2)

    def test_two_matching_zombies_are_exited_but_unreaped(self):
        _,_,ident,_=self.owned()
        self.assertEqual(self.samples([{**ident,'state':'Z'},{**ident,'state':'Z'}],'exited_owned'),2)

    def test_mixed_alive_zombie_dead_or_unknown_states_are_unknown(self):
        _,_,ident,_=self.owned()
        for states in (('R','Z'),('Z','R'),('X','X'),('x','x'),('W','W'),('P','P'),('?','S')):
            with self.subTest(states=states):
                self.samples([{**ident,'state':s} for s in states],'unknown')

    def test_missing_first_proc_record_is_identity_unavailable_not_exit(self):
        self.assertEqual(self.samples([FileNotFoundError('PRIVATE-SENTINEL')],'identity_unavailable'),1)

    def test_missing_second_proc_record_is_identity_unavailable_not_exit(self):
        _,_,ident,_=self.owned()
        self.assertEqual(self.samples([ident,FileNotFoundError('PRIVATE-SENTINEL')],'identity_unavailable'),2)

    def test_permission_parse_and_hostile_read_errors_return_unknown_without_formatting(self):
        class Hostile(Exception):
            def __str__(self): raise AssertionError('formatted')
            def __repr__(self): raise AssertionError('represented')
        _,_,ident,_=self.owned()
        for error in (PermissionError('PRIVATE-SENTINEL'),OSError('PRIVATE-SENTINEL'),
                      ValueError('PRIVATE-SENTINEL'),RuntimeError('PRIVATE-SENTINEL'),Hostile()):
            for second in (False,True):
                with self.subTest(kind=type(error).__name__,second=second):
                    self.samples(([ident] if second else [])+[error],'unknown')

    def test_pid_reuse_birth_and_group_mismatch_never_promoted(self):
        _,_,ident,_=self.owned()
        for key,value in (('pid',999),('start_ticks',101),('pgrp',999)):
            for second in (False,True):
                values=([ident] if second else [])+[{**ident,key:value}]
                with self.subTest(key=key,second=second):self.samples(values,'identity_unavailable')

    def test_unregistered_partial_reaped_known_return_and_missing_pidfd_skip_proc(self):
        for kind in ('unregistered','not_list','duplicate','reaped','returncode','pidfd','label','birth','pid'):
            sup,child,ident,row=self.owned()
            if kind=='unregistered':sup.children=[]
            if kind=='not_list':sup.children=object()
            if kind=='duplicate':sup.children=[row,row]
            if kind=='reaped':row['reaped']=True
            if kind=='returncode':child.returncode=0
            if kind=='pidfd':row['pidfd']=None
            if kind=='label':row['label']='studio'
            if kind=='birth':row['birth']={**ident,'start_ticks':101}
            if kind=='pid':child.pid=999
            with self.subTest(kind=kind),patch.object(p,'birth') as read:
                self.assertEqual(p.xvfb_liveness_after_timeout(sup,child,ident),'identity_unavailable')
                read.assert_not_called();child.poll.assert_not_called();child.wait.assert_not_called()

    def test_malformed_identity_and_samples_are_unknown_not_known_state(self):
        _,_,ident,_=self.owned()
        for bad in ({}, {**ident,'pid':True},{**ident,'start_ticks':100.0},
                    {**ident,'state':True},{**ident,'extra':'PRIVATE-SENTINEL'},[],None):
            with self.subTest(bad=bad):self.samples([bad,ident],'unknown')
        sup,child,ident,row=self.owned()
        with patch.object(p,'birth') as read:
            self.assertEqual(p.xvfb_liveness_after_timeout(sup,child,{}),'unknown')
            read.assert_not_called()

    def test_exhausted_campaign_before_first_read_is_unknown_without_extension(self):
        sup,child,ident,row=self.owned();deadline=sup.deadline
        with patch.object(p.time,'monotonic',return_value=deadline),patch.object(p,'birth') as read:
            self.assertEqual(p.xvfb_liveness_after_timeout(sup,child,ident),'unknown')
        read.assert_not_called();self.assertEqual(sup.deadline,deadline)

    def test_campaign_exhaustion_between_reads_or_after_second_withholds_known_state(self):
        for times,count in (([0,10],1),([0,0,10],2)):
            sup,child,ident,row=self.owned();sup.deadline=10
            with patch.object(p.time,'monotonic',side_effect=times),patch.object(p,'birth',return_value=ident) as read:
                self.assertEqual(p.xvfb_liveness_after_timeout(sup,child,ident),'unknown')
            self.assertEqual(read.call_count,count);self.assertEqual(sup.deadline,10)

    def test_versioned_field_and_all_closed_values_validate_only_with_timeout(self):
        self.assertEqual(p.PUBLIC_SCHEMA,'beatbox.native-frame-probe.public.v3')
        self.assertEqual(p.DIAGNOSTIC_SCHEMA,'beatbox.native-frame-probe.diagnostic.v2')
        value=p.new_diagnostic();self.assertEqual(value['xvfb_liveness'],'not_observed')
        for state in p.XVFB_LIVENESS:
            diag=p.new_diagnostic();diag['xvfb_liveness']=state
            if state!='not_observed':
                p.checkpoint(diag,'display_ready');diag['xvfb_startup']='identified'
                p.diagnostic_failure(diag,code='display_ready_timeout')
            p.checked_diagnostic(diag)
        for bad in (None,False,0,{},[], 'Z','PRIVATE-SENTINEL','reaped','process_exited'):
            diag=p.new_diagnostic();diag['xvfb_liveness']=bad
            with self.assertRaises(RuntimeError):p.checked_diagnostic(diag)

    def test_old_versions_missing_field_extras_and_contradictory_states_rejected(self):
        value=p.new_diagnostic()
        bads=[]
        old=copy.deepcopy(value);old['schema']='beatbox.native-frame-probe.diagnostic.v1';bads.append(old)
        missing=copy.deepcopy(value);del missing['xvfb_liveness'];bads.append(missing)
        for key in ('pid','start_ticks','state','path','argv','env','cookie','detail'):
            bad=copy.deepcopy(value);bad[key]='PRIVATE-SENTINEL';bads.append(bad)
        for state in p.XVFB_LIVENESS[1:]:
            bad=copy.deepcopy(value);bad['xvfb_liveness']=state;bads.append(bad)
        for bad in bads:
            with self.assertRaises(RuntimeError):p.checked_diagnostic(bad)
        row=Diagnostics().public();row['schema']='beatbox.native-frame-probe.public.v2'
        with self.assertRaises(RuntimeError):p.checked_public(row)
        for state in ('alive_owned','exited_owned'):
            diag=p.new_diagnostic();p.checkpoint(diag,'display_ready')
            p.diagnostic_failure(diag,code='display_ready_timeout');diag['xvfb_liveness']=state
            with self.assertRaises(RuntimeError):p.checked_diagnostic(diag)

    def timeout_capture(self, states=None, helper_error=None, helper_value=None, cleanup_error=False, receipt_error=False):
        def observe(out,binary,binding,sup,capture):
            original_start=sup.start.side_effect; original_put=p.put
            child=unittest.mock.Mock(pid=322,returncode=None);sup.children=[]
            sup.deadline=p.time.monotonic()+90
            def start(label,*a,**kw):
                result=original_start(label,*a,**kw)
                self.assertEqual(label,'xvfb')
                ident=result[1];sup.diagnostic['xvfb_startup']='identified'
                sup.children.append({'p':child,'birth':ident,'label':'xvfb','reaped':False,'pidfd':7})
                p.checkpoint(sup.diagnostic,'child_group')
                return child,ident
            def put(path,value,*a):
                if receipt_error and path.name=='receipt.json':raise OSError('PRIVATE-SENTINEL')
                return original_put(path,value,*a)
            if cleanup_error:sup.cleanup.side_effect=OSError('PRIVATE-SENTINEL')
            samples=[{'pid':322,'start_ticks':100,'pgrp':322,'state':s} for s in (states or ('S','S'))]
            helper=p.xvfb_liveness_after_timeout
            def observed(*a):
                if helper_error is not None:raise helper_error
                if helper_value is not None:return helper_value
                return helper(*a)
            with patch.object(sup.start,'side_effect',start),patch.object(p.select,'select',return_value=([],[],[])) as selected, \
                 patch.object(p,'birth',side_effect=samples) as reads,patch.object(p,'xvfb_liveness_after_timeout',side_effect=observed) as once, \
                 patch.object(p,'put',side_effect=put):
                self.assertFalse(capture(out,binary,binding))
            once.assert_called_once();selected.assert_called_once()
            self.assertEqual(reads.call_count,0 if helper_error is not None or helper_value is not None else 2)
            sup.cleanup.assert_called_once();self.assertEqual(sup.start.call_count,1)
            child.poll.assert_not_called();child.wait.assert_not_called();self.assertIsNone(child.returncode)
            self.assertFalse(sup.children[0]['reaped'])
            file=out/('receipt-finalization-failure.json' if receipt_error else 'receipt.json')
            row=p.strict(file.read_bytes());diag=row['diagnostic']
            self.assertEqual((diag['failure_stage'],diag['failure_operation'],diag['failure_code']),
                             ('display_ready','xvfb','display_ready_timeout'))
            expected='unknown' if helper_error is not None or helper_value is not None else ('exited_owned' if states==('Z','Z') else 'alive_owned')
            self.assertEqual(diag['xvfb_liveness'],expected)
            self.assertEqual(diag['cleanup'],'unknown' if cleanup_error else 'reaped')
            public=DiagnosticSuccessor().stage_copy(out,'failure')
            self.assertEqual(public['schema'],p.PUBLIC_SCHEMA)
            self.assertEqual(public['diagnostic']['xvfb_liveness'],expected)
            self.assertEqual(public['diagnostic']['failure_code'],'display_ready_timeout')
            self.assertEqual(public['status'],'failed');self.assertIsNone(public['linkage'])
            self.assertEqual(public['diagnostic']['finalization'],'failed' if receipt_error else 'unknown')
            self.assertNotIn('PRIVATE-SENTINEL',json.dumps(public))
            for key in ('pid','start_ticks','pgrp','state','path','detail','error','env','argv'):
                self.assertNotIn(key,public['diagnostic'])
            self.assertEqual(set(f.name for f in (out.parent/'stage-failure/upload').iterdir()),{'manifest.json','public-receipt.json'})
        DiagnosticSuccessor().observe_full_capture(observe)

    def test_actual_empty_path_observes_once_persists_owned_alive_without_public_identity(self):
        self.timeout_capture(('D','S'))

    def test_actual_empty_path_observes_zombie_without_reaping_or_success(self):
        self.timeout_capture(('Z','Z'))

    def test_helper_throw_cannot_replace_timeout_or_skip_cleanup(self):
        class Hostile(Exception):
            def __str__(self):raise AssertionError('formatted')
            def __repr__(self):raise AssertionError('represented')
        for error in (OSError('PRIVATE-SENTINEL'),RuntimeError('PRIVATE-SENTINEL'),Hostile()):
            with self.subTest(kind=type(error).__name__):self.timeout_capture(helper_error=error)

    def test_invalid_helper_return_is_unknown_not_arbitrary_public_data(self):
        for value in ('PRIVATE-SENTINEL',{'pid':322},False,'not_observed'):
            with self.subTest(value=value):self.timeout_capture(helper_value=value)

    def test_observation_and_timeout_survive_cleanup_and_receipt_failures(self):
        for cleanup,receipt in ((True,False),(False,True),(True,True)):
            with self.subTest(cleanup=cleanup,receipt=receipt):
                self.timeout_capture(('S','S'),cleanup_error=cleanup,receipt_error=receipt)

    def test_no_observation_on_ready_success_budget_error_or_select_exception(self):
        with patch.object(p,'xvfb_liveness_after_timeout') as observe:
            Pure().mock_capture(False)
            for kind,stage,code in (('budget','display_budget','operation_failed'),
                                    ('io','display_ready','io_error'),('exception','display_ready','operation_failed')):
                DisplayReadiness().branch(kind,stage,code)
        observe.assert_not_called()

    def test_known_liveness_never_erases_first_failure_during_staging(self):
        diag=p.new_diagnostic();diag['xvfb_startup']='identified';p.checkpoint(diag,'display_ready')
        p.diagnostic_failure(diag,code='display_ready_timeout');diag['xvfb_liveness']='exited_owned'
        for stage in ('display_close','cleanup','final_receipt','public_linkage','capture_outcome'):
            p.checkpoint(diag,stage);p.diagnostic_failure(diag,OSError('PRIVATE-SENTINEL'))
        self.assertEqual(diag['xvfb_liveness'],'exited_owned')
        self.assertEqual((diag['failure_stage'],diag['failure_code']),('display_ready','display_ready_timeout'))
        p.checked_diagnostic(diag)



class DisplayRecord(unittest.TestCase):
    # Deterministic pure schedules, not native Xvfb or hosted timing evidence.
    def parse(self, chunks, times=None, selections=None, deadline=3):
        with patch.object(p.time,'monotonic',side_effect=times,
                          return_value=0) as clock, \
             patch.object(p.os,'read',side_effect=chunks) as read, \
             patch.object(p.select,'select',side_effect=selections,
                          return_value=([42],[],[])) as selected:
            result=p.read_display_record(42,deadline)
        self.assertLessEqual(read.call_count,32)
        self.assertLessEqual(selected.call_count,31)
        return result,read,selected,clock

    def test_all_splits_and_all_compositions_of_valid_records(self):
        # Every ordered partition, including digits / newline and byte-at-a-time.
        for record in (b'0\n',b'77\n',b'65535\n',b'00000\n'):
            for mask in range(1 << (len(record)-1)):
                ends=[i+1 for i in range(len(record)-1) if mask & (1 << i)]+[len(record)]
                starts=[0]+ends[:-1]
                chunks=[record[a:b] for a,b in zip(starts,ends)]
                with self.subTest(record=record,mask=mask):
                    number,read,selected,_=self.parse(chunks)
                    self.assertEqual(number,int(record))
                    self.assertEqual(read.call_count,len(chunks))
                    self.assertEqual(selected.call_count,len(chunks)-1)
                    consumed=0
                    for call,chunk in zip(read.call_args_list,chunks):
                        self.assertEqual(call.args,(42,32-consumed))
                        consumed+=len(chunk)
                    self.assertTrue(all(0<c.args[3]<=3 for c in selected.call_args_list))

    def test_complete_one_read_has_no_extra_wait_or_eof_read(self):
        number,read,selected,_=self.parse([b'65535\n',AssertionError('extra EOF read')])
        self.assertEqual(number,65535);read.assert_called_once_with(42,32)
        selected.assert_not_called()

    def test_strict_rejections_eof_ascii_crlf_trailing_and_numeric_range(self):
        for chunks in ([b''],[b'7',b''],[b'\n'],[b' 7\n'],[b'+7\n'],[b'-1\n'],
                       [b'7\r\n'],[b'7',b'\r',b'\n'],[b'7\n8\n'],[b'7\nx'],
                       [b'\xff'],[b'\xc2\xb2\n'],[b'65536\n'],[b'123456\n'],
                       [b'12345',b'6'],[b'1'*32],[b'1'*33], [None],['7\n']):
            with self.subTest(chunks=chunks),self.assertRaises(RuntimeError):
                self.parse(chunks)

    def test_exact_cap_is_bounded_and_never_requests_more_bytes(self):
        for chunks in ([b'1'*32],[b'1',b'2'*31],[b'1'*33]):
            with patch.object(p.time,'monotonic',return_value=0), \
                 patch.object(p.os,'read',side_effect=chunks) as read, \
                 patch.object(p.select,'select',return_value=([42],[],[])) as selected:
                with self.assertRaises(RuntimeError):p.read_display_record(42,3)
            self.assertLessEqual(read.call_count,2)
            self.assertLessEqual(selected.call_count,1)
            self.assertEqual(read.call_args_list[0].args,(42,32))
            if read.call_count==2:self.assertEqual(read.call_args_list[1].args,(42,31))

    def test_read_errors_eintr_eagain_fail_closed_without_busyspin(self):
        for error in (OSError('PRIVATE-RECORD-SENTINEL'),InterruptedError(),BlockingIOError()):
            for after in (False,True):
                with self.subTest(error=type(error).__name__,after=after), \
                     patch.object(p.time,'monotonic',return_value=0), \
                     patch.object(p.os,'read',side_effect=([b'7'] if after else [])+[error,b'7\n']) as read, \
                     patch.object(p.select,'select',return_value=([42],[],[])) as selected:
                    with self.assertRaises(type(error)):p.read_display_record(42,3)
                self.assertEqual(read.call_count,2 if after else 1)
                self.assertEqual(selected.call_count,1 if after else 0)

    def test_select_error_or_empty_fragment_wait_is_not_initial_timeout(self):
        for error in (OSError('PRIVATE-RECORD-SENTINEL'),InterruptedError(),BlockingIOError()):
            with self.subTest(error=type(error).__name__), \
                 patch.object(p.time,'monotonic',return_value=0), \
                 patch.object(p.os,'read',return_value=b'7') as read, \
                 patch.object(p.select,'select',side_effect=error) as selected:
                with self.assertRaises(type(error)):p.read_display_record(42,3)
            read.assert_called_once();selected.assert_called_once()
        with self.assertRaises(TimeoutError):self.parse([b'7'],selections=[([],[],[])])

    def test_each_monotonic_boundary_exhaustion_prevents_next_operation(self):
        # [pre-read, post-read, pre-select, timeout-computation, post-select,
        #  pre-read, post-read, pre-return] for two chunks.
        counts=[(0,0),(1,0),(1,0),(1,0),(1,1),(1,1),(2,1),(2,1)]
        for boundary,(reads,selects) in enumerate(counts):
            for exhausted in (3,4):
                with self.subTest(boundary=boundary,exhausted=exhausted), \
                     patch.object(p.time,'monotonic',side_effect=[0]*boundary+[exhausted]) as clock, \
                     patch.object(p.os,'read',side_effect=[b'7',b'\n']) as read, \
                     patch.object(p.select,'select',return_value=([42],[],[])) as selected:
                    with self.assertRaises(TimeoutError):p.read_display_record(42,3)
                self.assertEqual((read.call_count,selected.call_count),(reads,selects))
                self.assertEqual(clock.call_count,boundary+1)

    def test_remaining_waits_shrink_and_never_reset_original_deadline(self):
        # A successful two-fragment wait consumes almost all the original 3s.
        times=[0,.1,.2,.3,.4,.5,.6,.7,2.8,2.9,2.95,2.96,2.97]
        number,read,selected,_=self.parse([b'6',b'5',b'\n'],times=times)
        self.assertEqual(number,65)
        self.assertEqual([c.args[3] for c in selected.call_args_list],[2.7,.20000000000000018])
        self.assertEqual([c.args[1] for c in read.call_args_list],[32,31,30])
        # A campaign-limited allowance is passed unchanged, not raised to 3s.
        _,_,selected,_=self.parse([b'7',b'\n'],times=[0,.1,.2,.25,.3,.4,.5,.6],deadline=1)
        self.assertEqual(selected.call_args.args[3],.75)

    def test_baseexception_not_swallowed_by_parser(self):
        for error in (KeyboardInterrupt(),SystemExit(7)):
            with patch.object(p.time,'monotonic',return_value=0), \
                 patch.object(p.os,'read',side_effect=error),patch.object(p.select,'select') as selected:
                with self.assertRaises(type(error)):p.read_display_record(42,3)
            selected.assert_not_called()

    def capture_record(self, chunks, selections=None, error=None, cleanup_error=False,
                       receipt_error=False, close_error=False, allowance=3):
        def observe(out,binary,binding,sup,capture):
            start=sup.start.side_effect;put=p.put;close=p.close_descriptors
            def started(label,*a,**kw):
                result=start(label,*a,**kw)
                if label=='xvfb':sup.diagnostic['xvfb_startup']='identified'
                return result
            def persisted(path,value,*a):
                if receipt_error and path.name=='receipt.json':raise OSError('PRIVATE-RECORD-SENTINEL')
                return put(path,value,*a)
            def closed(fds):
                close(fds)
                if close_error:raise OSError('PRIVATE-RECORD-SENTINEL')
            if cleanup_error:sup.cleanup.side_effect=OSError('PRIVATE-RECORD-SENTINEL')
            sup.remaining.side_effect=lambda maximum=3:min(allowance,maximum)
            with patch.object(sup.start,'side_effect',started), \
                 patch.object(p.os,'read',side_effect=chunks) as read, \
                 patch.object(p.select,'select',side_effect=selections,
                              return_value=([42],[],[])) as selected, \
                 patch.object(p,'xvfb_liveness_after_timeout') as liveness, \
                 patch.object(p,'put',side_effect=persisted), \
                 patch.object(p,'close_descriptors',side_effect=closed) as descriptors:
                if error is None:self.assertTrue(capture(out,binary,binding))
                else:self.assertFalse(capture(out,binary,binding))
            sup.cleanup.assert_called_once();descriptors.assert_called_once()
            liveness.assert_not_called()
            diag=p.strict((out/('receipt-finalization-failure.json' if receipt_error else 'receipt.json')).read_bytes())['diagnostic']
            if error is not None:
                self.assertEqual((diag['failure_stage'],diag['failure_operation'],diag['failure_code']),
                                 ('display_record','xvfb',error))
                self.assertEqual(sup.start.call_count,1)
                self.assertEqual(diag['studio_startup'],'not_started')
                public=DiagnosticSuccessor().stage_copy(out,'failure')
                self.assertEqual(public['status'],'failed');self.assertIsNone(public['linkage'])
                self.assertEqual(public['diagnostic']['failure_stage'],'display_record')
                self.assertEqual(public['diagnostic']['failure_code'],error)
                self.assertEqual(public['diagnostic']['xvfb_liveness'],'not_observed')
                self.assertEqual(public['diagnostic']['cleanup'],'unknown' if cleanup_error else 'reaped')
                self.assertEqual(public['diagnostic']['finalization'],'failed' if receipt_error else 'unknown')
                self.assertNotIn('PRIVATE-RECORD-SENTINEL',json.dumps(public))
                self.assertLessEqual(len(json.dumps(public).encode()),p.RECEIPT_CAP)
                for key in ('pid','path','env','argv','cookie','detail','error','state'):
                    self.assertNotIn(key,public['diagnostic'])
                upload=out.parent/'stage-failure/upload'
                self.assertEqual({f.name for f in upload.iterdir()},{'manifest.json','public-receipt.json'})
            self.assertEqual(selected.call_args_list[0].args[3],allowance)
            self.assertLessEqual(read.call_count,32);self.assertLessEqual(selected.call_count,32)
        DiagnosticSuccessor().observe_full_capture(observe)

    def test_fragment_success_runs_unchanged_owned_capture_and_seven_read_flow(self):
        self.capture_record([b'7',b'7',b'\n'])
        self.capture_record([b'77\n'],allowance=1)

    def test_fragment_failure_classification_and_first_error_cleanup_privacy(self):
        for chunks,selections,code in (
                ([b'7'],[([42],[],[]),([],[],[])],'timeout'),
                ([b'7',b''],None,'operation_failed'),
                ([b'7\nx'],None,'operation_failed'),
                ([b'7',OSError('PRIVATE-RECORD-SENTINEL')],None,'io_error'),
                ([b'7',InterruptedError('PRIVATE-RECORD-SENTINEL')],None,'unknown_exception'),
                ([b'7',BlockingIOError('PRIVATE-RECORD-SENTINEL')],None,'unknown_exception'),
                ([b'7'],[([42],[],[]),InterruptedError('PRIVATE-RECORD-SENTINEL')],'unknown_exception'),
                ([b'7'],[([42],[],[]),BlockingIOError('PRIVATE-RECORD-SENTINEL')],'unknown_exception'),
                ([b'7'],[([42],[],[]),OSError('PRIVATE-RECORD-SENTINEL')],'io_error')):
            for cleanup,receipt,closed in ((False,False,False),(True,True,True)):
                with self.subTest(code=code,cleanup=cleanup):
                    self.capture_record(chunks,selections,code,cleanup,receipt,closed)

    def test_initial_empty_wait_and_liveness_remain_exact_once(self):
        # Existing assertions exercise the actual first empty branch unchanged.
        DisplayObservation().timeout_capture(('S','S'),cleanup_error=True,receipt_error=True)
        with patch.object(p.os,'read') as read:
            DisplayReadiness().branch('empty','display_ready','display_ready_timeout')
        read.assert_not_called()

    def test_capture_uses_original_absolute_allowance_after_initial_wait(self):
        for end in (2.5,3):
            def observe(out,binary,binding,sup,capture):
                now=[0];timeouts=[];start=sup.start.side_effect
                def started(label,*a,**kw):
                    result=start(label,*a,**kw)
                    if label=='xvfb':sup.diagnostic['xvfb_startup']='identified'
                    return result
                def selected(*a):
                    timeouts.append(a[3]);now[0]=end if len(timeouts)==1 else 2.75
                    return ([a[0][0]],[],[])
                with patch.object(sup.start,'side_effect',started), \
                     patch.object(p.time,'monotonic',side_effect=lambda:now[0]), \
                     patch.object(p.select,'select',side_effect=selected), \
                     patch.object(p.os,'read',side_effect=[b'7',b'7\n']) as read, \
                     patch.object(p,'xvfb_liveness_after_timeout') as liveness:
                    self.assertEqual(capture(out,binary,binding),end<3)
                self.assertEqual(timeouts,[3,.5] if end<3 else [3])
                self.assertEqual(read.call_count,2 if end<3 else 0)
                liveness.assert_not_called();sup.cleanup.assert_called_once()
                diag=p.strict((out/'receipt.json').read_bytes())['diagnostic']
                if end==3:
                    self.assertEqual((diag['failure_stage'],diag['failure_code']),('display_record','timeout'))
            DiagnosticSuccessor().observe_full_capture(observe)

    def test_source_binding_schemas_caps_and_protocol_assumption_unchanged(self):
        self.assertEqual((p.CAMPAIGN_SECONDS,p.CHILD_SECONDS,p.CLEANUP_SECONDS),(90,3,5))
        self.assertEqual((p.RECEIPT_CAP,p.READ_CAP,p.FRAME_CAP),(65536,8*1024*1024,32*1024*1024))
        self.assertEqual((p.PUBLIC_SCHEMA,p.DIAGNOSTIC_SCHEMA),
                         ('beatbox.native-frame-probe.public.v3','beatbox.native-frame-probe.diagnostic.v2'))
        self.assertEqual(len(p.UPLOAD_FILES),9);self.assertEqual(len(p.READS),7)
        Pure().test_binding_requires_exact_new_head_tree_clean_checkout()
        # Trusted-owned protocol assumption: later writes are not read after a
        # newline. This is disclosed, NOT evidence that later trailing data is absent.
        number,read,selected,_=self.parse([b'7\n',b'bad later trailing bytes'])
        self.assertEqual(number,7);read.assert_called_once();selected.assert_not_called()

if __name__=='__main__':
    unittest.main(verbosity=2)
