"""Synthetic-only, bounded CLI/MCP recovery admission; no provider calls."""
import collections
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import time

HERE=Path(__file__).resolve().parent
COMMANDS=0
TIMINGS=[]
DEADLINE=float('inf')

class Failure(Exception):pass
def require(ok,reason):
    if not ok:raise Failure(reason)
def sha(data):return hashlib.sha256(data).hexdigest()
def digest(path):return sha(path.read_bytes())
def save(path,value):
    path.write_text(json.dumps(value,ensure_ascii=False,indent=2)+'\n');path.chmod(0o600)
def env():
    value={k:v for k,v in os.environ.items() if not k.startswith(('GOBSTOPPER_','TYPESAFE_','AI_GATEWAY_','OPENAI_','ANTHROPIC_','VERCEL_'))}
    value.update(XDG_DATA_HOME=str(HERE/'environment/data'),XDG_CONFIG_HOME=str(HERE/'environment/config'))
    return value
def command(argv,stdin=None,environment=None):
    global COMMANDS
    COMMANDS+=1;require(COMMANDS<=2200,'command_cap_exceeded')
    began=time.monotonic();child=None
    require(began<DEADLINE,'study_timeout')
    with tempfile.TemporaryFile(dir=HERE) as stdout,tempfile.TemporaryFile(dir=HERE) as stderr:
        try:
            child=subprocess.Popen(argv,stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                stdout=stdout,stderr=stderr,env=environment or env(),start_new_session=True)
            if stdin is not None:child.stdin.write(stdin);child.stdin.close()
            while True:
                code=child.poll()
                require(os.fstat(stdout.fileno()).st_size+os.fstat(stderr.fileno()).st_size<=4194304,'output_limit')
                if code is not None:break
                require(time.monotonic()-began<20,'command_timeout')
                require(time.monotonic()<DEADLINE,'study_timeout')
                time.sleep(.005)
            stdout.seek(0);result=stdout.read();stderr.seek(0);diagnostic=stderr.read()
            TIMINGS.append((time.monotonic()-began)*1000)
            return code,result,diagnostic
        finally:
            if child is not None and child.poll() is None:
                try:os.killpg(child.pid,signal.SIGKILL)
                except ProcessLookupError:pass
                child.wait()
def json_command(binary,args):
    code,raw,_=command([str(binary),*args]);require(code==0,'unexpected_command_failure')
    try:return json.loads(raw)
    except (ValueError,UnicodeError):raise Failure('invalid_json') from None
def search(binary,sample,query,limit=20):
    result=json_command(binary,['search-snapshot',sample['snapshot_sha256'],'--query',query,'--limit',str(limit),'--json'])
    require(result['snapshot_sha256']==sample['snapshot_sha256'] and result['source_sha256']==sample['source_sha256'],'search_snapshot_binding')
    allowed={'record_index','record_sha256','record_bytes','match_count'}
    require(all(set(row)==allowed for row in result['matches']),'search_metadata_only')
    require(set(result)<= {'snapshot_sha256','source_sha256','matches','matched_records','unsearchable_records','truncated'},'search_no_content_fields')
    require(len(result['matches'])<=limit and result['truncated']==(result['matched_records']>limit),'search_bound_or_truncation')
    return result
def read(binary,sample,index,offset=0,limit=4096):
    value=json_command(binary,['read-snapshot',sample['snapshot_sha256'],'--record',str(index),'--offset',str(offset),'--max-bytes',str(limit),'--json'])
    require(value['snapshot_sha256']==sample['snapshot_sha256'] and value['source_sha256']==sample['source_sha256'],'read_snapshot_binding')
    require(value['offset']==offset and isinstance(value['content'],str),'read_offset_or_content')
    data=value['content'].encode()
    require(len(data)<=limit and (value['next_offset'] is None or value['next_offset']==offset+len(data)),'read_bound_or_continuation')
    return value,data
def exact_read(binary,sample,query):
    source=(HERE/sample['fixture']).read_bytes().splitlines()[query['record_index']]
    offset=0;pieces=[]
    for _ in range(64):
        result,data=read(binary,sample,query['record_index'],offset)
        require(result['record_sha256']==sha(source) and result['record_bytes']==len(source),'record_binding')
        require(data==source[offset:offset+len(data)],'record_exact_page')
        pieces.append(data)
        if result['next_offset'] is None:break
        require(len(data)>0,'pagination_no_progress');offset=result['next_offset']
    else:raise Failure('page_cap_exceeded')
    require(b''.join(pieces)==source,'record_exact_reconstruction')
    # Check the predeclared fact against decoded JSON values, since half the
    # fixtures deliberately encode non-ASCII characters as JSON escapes.
    require(query['expected_fact'] in json.dumps(json.loads(source),ensure_ascii=False),'fixture_expected_fact')
def recall(binary,sample,query):
    return json_command(binary,['recall','--sha',sample['snapshot_sha256'],'--query',query,'--json'])
def rejected(binary,args,environment=None):
    code,stdout,_=command([str(binary),*args],environment=environment)
    require(code!=0 and not stdout.strip(),'invalid_input_not_rejected_cleanly')
def mcp(binary,enabled,requests):
    messages=[{'jsonrpc':'2.0','id':i+1,**r} for i,r in enumerate(requests)]
    code,raw,_=command([str(binary),'mcp',*(['--allow-transcript-content'] if enabled else [])],
                       stdin=b''.join(json.dumps(m).encode()+b'\n' for m in messages))
    require(code==0,'mcp_process_failed')
    values=[json.loads(line) for line in raw.splitlines() if line.strip()]
    byid={v['id']:v for v in values};require(len(byid)==len(messages),'mcp_response_count')
    return [byid[m['id']] for m in messages]
def result_payload(response):
    require('error' not in response and not response.get('result',{}).get('isError'),'mcp_tool_failed')
    return json.loads(response['result']['content'][0]['text'])

def main():
    global DEADLINE
    os.umask(0o077)
    require(len(sys.argv)==2,'candidate_path_required')
    require(not (HERE/'started.json').exists(),'study_already_started')
    registration_bytes=(HERE/'registration.json').read_bytes();registration=json.loads(registration_bytes)
    require(digest(Path(__file__))==registration['runner_sha256'],'runner_changed')
    require(digest(HERE/'prepare.py')==registration['prepare_sha256'],'generator_changed')
    require(digest(HERE/'manifest.json')==registration['manifest_sha256'],'manifest_changed')
    samples=json.loads((HERE/'manifest.json').read_text())
    baseline=HERE/registration['baseline_binary'];require(digest(baseline)==registration['baseline_binary_sha256'],'baseline_changed')
    source=Path(sys.argv[1]);before=source.stat();candidate_bytes=source.read_bytes();after=source.stat()
    require((before.st_size,before.st_mtime_ns)==(after.st_size,after.st_mtime_ns),'candidate_changed_during_pin')
    candidate_sha=sha(candidate_bytes);candidate=HERE/'bin'/f'candidate-{candidate_sha}'
    if not candidate.exists():candidate.write_bytes(candidate_bytes);candidate.chmod(0o700)
    require(digest(candidate)==candidate_sha,'candidate_pin_mismatch')
    vault=HERE/'environment/data/gobstopper/vault'
    vault_files={str(p.relative_to(vault)):digest(p) for p in vault.rglob('*') if p.is_file()}
    provenance={'started_unix_seconds':time.time(),'baseline_binary_sha256':digest(baseline),
                'candidate_binary_sha256':candidate_sha,'runner_sha256':digest(Path(__file__)),
                'manifest_sha256':digest(HERE/'manifest.json'),'registration_sha256':sha(registration_bytes),
                'remote_model_calls':0,'local_model_calls':0}
    save(HERE/'started.json',provenance)
    outcomes=[];start=time.monotonic();DEADLINE=start+600
    def check(name,fn,sample=None):
        row={'check':name,'sample':sample,'passed':False}
        try:
            fn();row['passed']=True
        except Failure as e:row['failure_kind']=str(e)
        except (KeyError,TypeError,ValueError,UnicodeError):row['failure_kind']='invalid_result_schema'
        outcomes.append(row)
    capabilities={}
    for command_name,args in [('search-snapshot',['--query','PUBLIC_TOOL_TAG','--json']),('read-snapshot',['--record','0','--json'])]:
        code,raw,stderr=command([str(baseline),command_name,samples[0]['snapshot_sha256'],*args])
        capabilities[command_name]='unsupported' if code!=0 and (b'unrecognized subcommand' in stderr or b'unknown subcommand' in stderr) else 'supported' if code==0 else 'unavailable'
    recall_counts=collections.Counter()
    for sample in samples:
        require(digest(HERE/sample['fixture'])==sample['source_sha256'],'fixture_changed')
        sid=sample['sample']
        def baseline_show():
            value=json_command(baseline,['show',sample['snapshot_sha256'],'--json'])
            require(value['bytes']==sample['bytes'] and value['record_count']==sample['record_count'],'seeded_vault_incompatible')
        check('baseline_v3_read_compatibility',baseline_show,sid)
        for query_name,query in sample['queries'].items():
            def positive(q=query):
                result=search(candidate,sample,q['query'])
                require(result['matched_records']==1 and result['unsearchable_records']==0,'positive_search_count')
                require(result['matches']==[{k:q[k] for k in ('record_index','record_sha256','record_bytes','match_count')}],'positive_search_record')
            check('positive_search_'+query_name,positive,sid)
            check('exact_read_'+query_name,lambda q=query:exact_read(candidate,sample,q),sid)
        for negative_name,query in [('absent',sample['negative_query']),('other_version',sample['other_version_query']),
                                    ('case_sensitive',sample['queries']['fact']['query'].lower()),('keys_not_values','synthetic_key_only_marker')]:
            def negative(q=query):
                result=search(candidate,sample,q);require(result['matched_records']==0 and result['matches']==[] and result['unsearchable_records']==0,'negative_search_match')
            check('negative_search_'+negative_name,negative,sid)
        def limited():
            result=search(candidate,sample,'PUBLIC_TOOL_TAG',2)
            require(result['matched_records']==24 and len(result['matches'])==2 and result['truncated'],'broad_search_accounting')
        check('limited_search',limited,sid)
        for backend,binary in [('baseline',baseline),('candidate',candidate)]:
            for field,query in sample['card_queries'].items():
                try:
                    found=recall(binary,sample,query)
                    found_correct=bool(len(found)==1 and found[0]['snapshot_sha']==sample['snapshot_sha256'])
                    if found_correct:recall_counts[f'{backend}/{sample["provider"]}/{field}/found']+=1
                    else:recall_counts[f'{backend}/{sample["provider"]}/{field}/absent']+=1
                    if backend=='candidate':
                        require(found_correct,'candidate_card_not_found')
                        expected_key={'goal':'goal','error':'errors','current':'current_work'}[field]
                        require(query in found[0][expected_key] if field=='error' else found[0][expected_key]==query,'candidate_card_field_missing')
                        outcomes.append({'check':'recall_'+field,'sample':sid,'passed':True})
                except (Failure,KeyError,TypeError,ValueError):
                    outcomes.append({'check':backend+'_recall_'+field,'sample':sid,'passed':False,'failure_kind':'recall_unavailable_or_incorrect'})
        if len(outcomes)%50<16:save(HERE/'checkpoint.json',{'outcomes':outcomes,'recall':dict(recall_counts)})
    malformed=registration['malformed_fixture']
    def malformed_search():
        value=json_command(candidate,['search-snapshot',malformed['snapshot_sha256'],'--query','VALID_PUBLIC_VALUE','--json'])
        require(value['matched_records']==1 and value['unsearchable_records']==malformed['expected_unsearchable_records'],'malformed_record_accounting')
    check('malformed_physical_records',malformed_search)
    sample=next(s for s in samples if s['encoding']=='utf8');query=sample['queries']['unicode']
    line=(HERE/sample['fixture']).read_bytes().splitlines()[query['record_index']]
    unicode_offset=line.index('東京'.encode())
    def unicode_page():
        value,data=read(candidate,sample,query['record_index'],unicode_offset,7)
        require(data==line[unicode_offset:unicode_offset+len(data)] and value['next_offset']==unicode_offset+len(data),'unicode_page_exact')
    check('unicode_boundary_pagination',unicode_page)
    invalid=[['read-snapshot',sample['snapshot_sha256'],'--record',str(query['record_index']),'--offset',str(unicode_offset+1),'--json'],
        ['read-snapshot',sample['snapshot_sha256'],'--record',str(sample['record_count']),'--json'],
        ['read-snapshot',sample['snapshot_sha256'],'--record','0','--offset','999999999','--json'],
        ['read-snapshot',sample['snapshot_sha256'],'--record','0','--max-bytes','3','--json'],
        ['read-snapshot',sample['snapshot_sha256'],'--record','0','--max-bytes','16385','--json'],
        ['read-snapshot',sample['snapshot_sha256'][:16],'--record','0','--json'],
        ['search-snapshot',sample['snapshot_sha256'],'--query','PUBLIC','--limit','51','--json'],
        ['search-snapshot',sample['snapshot_sha256'],'--query','PUBLIC','--limit','0','--json'],
        ['search-snapshot','../not-a-digest','--query','PUBLIC','--json']]
    for i,args in enumerate(invalid):check(f'invalid_input_{i}',lambda a=args:rejected(candidate,a))
    def corrupt():
        with tempfile.TemporaryDirectory(dir=HERE,prefix='corrupt-') as tmp:
            data_root=Path(tmp);copy=data_root/'gobstopper/vault';shutil.copytree(vault,copy)
            manifest=json.loads((copy/'manifests'/sample['snapshot_sha256']).read_text())
            (copy/'chunks'/manifest['chunks'][0]).write_bytes(b'PUBLIC_CORRUPTION')
            isolated=env();isolated['XDG_DATA_HOME']=str(data_root)
            rejected(candidate,['read-snapshot',sample['snapshot_sha256'],'--record','0','--json'],isolated)
            rejected(candidate,['search-snapshot',sample['snapshot_sha256'],'--query','PUBLIC','--json'],isolated)
    check('integrity_fail_closed',corrupt)
    def mcp_gate():
        args={'sha':sample['snapshot_sha256'],'record':query['record_index'],'offset':0,'max_bytes':4096}
        requests=[{'method':'tools/list'},{'method':'tools/call','params':{'name':'read_snapshot','arguments':args}}]
        responses=mcp(candidate,False,requests)
        names={t['name'] for t in responses[0]['result']['tools']}
        require(not {'read_snapshot','search_snapshot'}&names,'mcp_default_tools_exposed')
        require('error' in responses[1] or responses[1].get('result',{}).get('isError') is True,'mcp_default_direct_call_not_denied')
        enabled=mcp(candidate,True,requests+[{'method':'tools/call','params':{'name':'search_snapshot','arguments':{'sha':sample['snapshot_sha256'],'query':query['query'],'limit':20}}}])
        names={t['name'] for t in enabled[0]['result']['tools']}
        require({'read_snapshot','search_snapshot'}<=names,'mcp_enabled_tools_missing')
        require(not {'apply','undo','watch','snapshot','fork'}&names,'mcp_mutating_surface')
        result=result_payload(enabled[1]);require(result['content'].encode()==line,'mcp_exact_read')
        result=result_payload(enabled[2]);require(result['matched_records']==1 and 'content' not in result,'mcp_search')
    check('mcp_default_denial_and_opt_in',mcp_gate)
    require(digest(candidate)==candidate_sha and digest(baseline)==registration['baseline_binary_sha256'],'pinned_executable_changed')
    require((HERE/'registration.json').read_bytes()==registration_bytes and digest(Path(__file__))==registration['runner_sha256'],'protocol_changed')
    require(digest(HERE/'manifest.json')==registration['manifest_sha256'],'manifest_changed')
    require(vault_files=={str(p.relative_to(vault)):digest(p) for p in vault.rglob('*') if p.is_file()},'original_vault_changed')
    require(all(digest(HERE/s['fixture'])==s['source_sha256'] for s in samples),'fixture_changed')
    totals=collections.Counter(o['check'] for o in outcomes);passed=collections.Counter(o['check'] for o in outcomes if o['passed'])
    report={'schema':'gobstopper-public-synthetic-recovery-result-v1','provenance':provenance,
        'sample_count':len(samples),'baseline_recovery_capabilities':capabilities,'recall_counts':dict(recall_counts),
        'checks':[{'check':k,'total':v,'passed':passed[k],'failed':v-passed[k]} for k,v in sorted(totals.items())],
        'total_checks':len(outcomes),'failed_checks':sum(not o['passed'] for o in outcomes),
        'commands':COMMANDS,'wall_seconds':round(time.monotonic()-start,3),
        'command_wall_ms_median':statistics.median(TIMINGS),'command_wall_ms_max':max(TIMINGS),
        'original_vault_and_fixtures_unchanged':True,'binaries_unchanged':True,'no_provider_calls':True,
        'limitations':registration['limitations']}
    save(HERE/'results.json',report);save(HERE/'case-results.json',outcomes)
    print(json.dumps({k:report[k] for k in ['sample_count','total_checks','failed_checks','commands','wall_seconds']}),flush=True)
    return 0 if report['failed_checks']==0 else 1

if __name__=='__main__':
    signal.signal(signal.SIGTERM,lambda *_:(_ for _ in ()).throw(KeyboardInterrupt()))
    try:raise SystemExit(main())
    except Failure as error:print(json.dumps({'study_failed':str(error)}));raise SystemExit(1)
    except (OSError,ValueError,KeyError,TypeError,UnicodeError):print(json.dumps({'study_failed':'unavailable_or_invalid_input'}));raise SystemExit(1)
    except KeyboardInterrupt:print(json.dumps({'study_failed':'interrupted'}));raise SystemExit(130)
