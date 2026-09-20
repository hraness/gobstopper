"""Generate only public, deterministic synthetic recovery fixtures; no CLI runs."""
import argparse
import hashlib
import json
import os
from pathlib import Path

HERE = Path(__file__).resolve().parent
CHUNK_BYTES = 1024 * 1024

def sha(data): return hashlib.sha256(data).hexdigest()
def dump(path, value):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2)+'\n')
    path.chmod(0o600)
def encode(value, escaped=False):
    return json.dumps(value, ensure_ascii=escaped, separators=(',', ':')).encode()

def records(provider, family, version):
    sid=f'public-synthetic-{provider}-{family}'
    rows=[]
    if provider=='codex':
        rows.append({'type':'session_meta','payload':{'id':sid,'source':'cli','synthetic_key_only_marker':'PUBLIC_SYNTHETIC_FIXTURE'}})
        rows.append({'type':'response_item','payload':{'type':'message','role':'user','content':[{'type':'input_text','text':'Public synthetic task: recover the exact test result when requested.'}]}})
    else:
        rows.append({'type':'user','uuid':'u-start','parentUuid':None,'sessionId':sid,'synthetic_key_only_marker':'PUBLIC_SYNTHETIC_FIXTURE','message':{'role':'user','content':'Public synthetic task: recover the exact test result when requested.'}})
    refs={}
    for tool in range(24):
        call=f'call-{tool}'
        if provider=='codex':
            rows.append({'type':'response_item','payload':{'type':'function_call','call_id':call,'name':'synthetic_read','arguments':json.dumps({'path':f'/public/project/file-{tool}.txt'})}})
        else:
            rows.append({'type':'assistant','uuid':f'a-{tool}','parentUuid':'u-start' if tool==0 else f'u-{tool-1}','sessionId':sid,'message':{'role':'assistant','content':[{'type':'tool_use','id':call,'name':'synthetic_read','input':{'path':f'/public/project/file-{tool}.txt'}}]}})
        text=f'PUBLIC_TOOL_TAG public synthetic result {tool}; family {family}; version {version}.'
        if tool==5:
            query=f'ERR_RECOVER_{provider}_{family}_V{version}'
            text+=f' {query}: expected checksum FACT_{family}_{version}_73421; actual checksum 73420. {query}: unresolved.'
            refs['fact']={'record_index':len(rows),'query':query,'expected_fact':f'FACT_{family}_{version}_73421','match_count':2}
        if tool==10:
            query=f'café 東京 🧭 family-{family} version-{version}'
            text+=f' Unicode result: {query}; combining e\u0301; safe suffix.'
            refs['unicode']={'record_index':len(rows),'query':query,'expected_fact':query,'match_count':1}
        if tool==13:
            query=f'LONG_RECORD_{provider}_{family}_{version}'
            text+=f' {query} '+('abcdefghij 東京 🧭 ' * 1300)+' EXACT_LONG_TAIL'
            refs['long']={'record_index':len(rows),'query':query,'expected_fact':'EXACT_LONG_TAIL','match_count':1}
        if provider=='codex':
            rows.append({'type':'response_item','payload':{'type':'function_call_output','call_id':call,'output':text}})
        else:
            rows.append({'type':'user','uuid':f'u-{tool}','parentUuid':f'a-{tool}','sessionId':sid,'message':{'role':'user','content':[{'type':'tool_result','tool_use_id':call,'content':text}]}})
    goal=f'CARD_GOAL_{provider}_{family}_{version}'
    error=f'CARD_ERROR_{provider}_{family}_{version}'
    current=f'CARD_CURRENT_{provider}_{family}_{version}'
    card=f'[gobstopper state card]\ngoal: {goal}\nerror: {error}\ncurrent: {current}\n(covers 24 earlier records)\n'
    if provider=='codex':
        rows.append({'type':'response_item','payload':{'type':'message','role':'user','content':[{'type':'input_text','text':card}]}})
    else:
        rows.append({'type':'user','uuid':'state-card','parentUuid':'u-23','sessionId':sid,'message':{'role':'user','content':card}})
    return rows,refs,{'goal':goal,'error':error,'current':current},sid

def install_snapshot(data, provider, sid, label, vault, index):
    chunks=[]
    for data_chunk in [data[n:n+CHUNK_BYTES] for n in range(0,len(data),CHUNK_BYTES)]:
        digest=sha(data_chunk);path=vault/'chunks'/digest
        path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
        if not path.exists():path.write_bytes(data_chunk);path.chmod(0o600)
        chunks.append(digest)
    manifest={'schema_version':3,'source_sha256':sha(data),'bytes':len(data),'chunks':chunks}
    raw=encode(manifest);digest=sha(raw)
    target=vault/'manifests'/digest;target.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
    target.write_bytes(raw);target.chmod(0o600)
    entry={'ts':1900000000+len(index),'sha256':digest,'source_sha256':sha(data),
           'path':f'synthetic/{label}.jsonl','session_id':sid,'provider':provider,
           'bytes':len(data),'record_count':len(data.splitlines()),'strategy':'public-synthetic-fixture'}
    index.append(entry)
    return digest

def main():
    global HERE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline', required=True, type=Path,
                        help='existing baseline Gobstopper executable to copy and pin; never executed during preparation')
    parser.add_argument('--output', required=True, type=Path,
                        help='new output directory; an existing file, directory, or symlink is refused')
    args = parser.parse_args()
    os.umask(0o077)
    baseline_path = args.baseline.expanduser().resolve(strict=True)
    if not baseline_path.is_file() or not os.access(baseline_path, os.X_OK):
        parser.error('--baseline must name an executable regular file')
    before = baseline_path.stat()
    baseline = baseline_path.read_bytes()
    after = baseline_path.stat()
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        parser.error('baseline changed while being copied')
    sources = {name: (HERE/name).read_bytes() for name in ('prepare.py', 'run.py')}
    output = args.output.expanduser().absolute()
    try:
        output.mkdir(mode=0o700, parents=True, exist_ok=False)
    except FileExistsError:
        parser.error('--output already exists; select a new directory')
    HERE = output
    for name, data in sources.items():
        (HERE/name).write_bytes(data)
        (HERE/name).chmod(0o600)
    vault=HERE/'environment/data/gobstopper/vault'
    index=[];manifest=[]
    for provider in ['codex','claude_code']:
        for family in range(6):
            for version in range(3):
                label=f'{provider}-family-{family}-v{version}'
                rows,queries,cards,sid=records(provider,family,version)
                escaped=family%2==0
                lines=[encode(v,escaped) for v in rows]
                # Six fixtures put the fact's JSONL record across a v3 chunk boundary.
                if family==5:
                    target=queries['fact']['record_index']
                    padding=b' ' * (CHUNK_BYTES-80-sum(len(line)+1 for line in lines[:target]))
                    lines[target-1]+=padding  # JSON trailing whitespace is valid.
                data=b'\n'.join(lines)+b'\n'
                path=HERE/'fixtures'/f'{label}.jsonl';path.parent.mkdir(parents=True,exist_ok=True,mode=0o700)
                path.write_bytes(data);path.chmod(0o600)
                digest=install_snapshot(data,provider,sid,label,vault,index)
                for q in queries.values():
                    line=lines[q['record_index']]
                    q.update(record_sha256=sha(line),record_bytes=len(line))
                manifest.append({'sample':label,'provider':provider,'family':family,'version':version,
                    'fixture':str(path.relative_to(HERE)),'snapshot_sha256':digest,'source_sha256':sha(data),
                    'bytes':len(data),'record_count':len(lines),'encoding':'escaped' if escaped else 'utf8',
                    'cross_chunk_fact':family==5,'queries':queries,'card_queries':cards,
                    'negative_query':f'ABSENT_PUBLIC_FACT_{provider}_{family}_{version}',
                    'other_version_query':f'ERR_RECOVER_{provider}_{family}_V{(version+1)%3}'})
    # Deliberately malformed input is a separate API robustness fixture, not a
    # provider-resume or compaction-quality sample.
    malformed=b'{"value":"VALID_PUBLIC_VALUE"}\nnot-json\n42\n\n'
    malformed_sha=install_snapshot(malformed,'codex','public-malformed','malformed',vault,index)
    (vault/'index.jsonl').write_bytes(b''.join(encode(v)+b'\n' for v in index))
    (vault/'index.jsonl').chmod(0o600)
    (HERE/'environment/config/gobstopper').mkdir(parents=True,exist_ok=True,mode=0o700)
    (HERE/'environment/config/gobstopper/config.toml').write_text('[policy]\nadaptive=false\n')
    baseline_sha=sha(baseline);target=HERE/'bin'/f'baseline-{baseline_sha}'
    target.parent.mkdir(parents=True,exist_ok=True,mode=0o700);target.write_bytes(baseline);target.chmod(0o700)
    dump(HERE/'manifest.json',manifest)
    registration={'schema':'gobstopper-public-synthetic-recovery-registration-v1',
        'registered_before_outcomes':True,'selection':'All36 deterministic public fixtures; no outcome-dependent exclusion or replacement.',
        'sample_count':36,'provider_counts':{'codex':18,'claude_code':18},'families_per_provider':6,'versions_per_family':3,
        'fixture_seed':'public-recovery-v1','cross_chunk_fact_samples':6,'escaped_unicode_samples':18,
        'baseline_binary_sha256':baseline_sha,'baseline_binary':str(target.relative_to(HERE)),
        'candidate_rule':'Copy and SHA-pin the exact provided release executable before any candidate commands; record provenance before outcomes.',
        'manifest_sha256':sha((HERE/'manifest.json').read_bytes()),
        'prepare_sha256':sha((HERE/'prepare.py').read_bytes()),'runner_sha256':sha((HERE/'run.py').read_bytes()),
        'malformed_fixture':{'snapshot_sha256':malformed_sha,'expected_unsearchable_records':2},
        'search_contract':'Case-sensitive literal substring over decoded JSON string values only; no object-key matches and no raw JSON escape matching. Full64 snapshot digest, limit1..50, default20. Count all matching records, return at most limit metadata rows; expose unsearchable_records, matched_records and truncated.',
        'read_contract':'Exact raw UTF8 JSONL record bytes without newline; full64 snapshot digest; record index zero-based; offset valid UTF8 boundary; max_bytes4..16384. content must not exceed max_bytes UTF8 bytes; next_offset is exact byte continuation or null at end.',
        'evaluation':'For every fixture: baseline show validates seeded v3 compatibility; candidate positive search/read3facts; bounded4KiB paging reconstructs long record; negatives reject other-version, absent, differently-cased and object-key-only queries. Broad query limit2 checks total/truncation. Baseline/candidate recall goal,error,current fields separately. One fixed fixture covers invalid digest, bounds, UTF8 continuation and integrity rejection. MCP tools/list and tools/call check default-off versus explicit enablement; no content is emitted by default.',
        'bounds':{'study_timeout_seconds':600,'command_timeout_seconds':20,'command_output_bytes':4194304,'page_bytes':4096,'max_pages_per_record':64,'max_commands':2200},
        'unsupported_baseline_rule':'Record new recovery commands as unsupported on old binary; do not count unsupported as retrieval failure or compare their speed.',
        'metrics':['exact search targets and match counts','byte-exact recovered record and paged reconstruction','source and pinned binary unchanged','bounded output and malformed-record accounting','version isolation','recall shape/field coverage','default MCP gate and opt-in success','closed failure counts and descriptive latency'],
        'limitations':['Entirely synthetic public data; no private provider transcript or model invocation.','Recovery API correctness is separate from in-context retention, useful search selection, task success, or provider resume.','Known target queries and deterministic facts are an oracle; this is not an agent reasoning or semantic retrieval benchmark.','Directly seeded v3 vault exercises the public read path; it does not prove snapshot creation or compaction-to-recovery pointer wiring.','Multiple versions share families; counts are cases, not independent tasks.','No generalized speed or backend superiority claim; unsupported baseline commands are capability observations.']}
    dump(HERE/'registration.json',registration)
    (HERE/'PROTOCOL.md').write_text('# Public synthetic recovery admission study\n\n'+
        'The frozen registration is `registration.json`; exact expected queries and bytes are in `manifest.json`. There are 36 valid public synthetic snapshots (18 per provider), plus one malformed-record robustness fixture. Candidate execution has not run.\n\n'+
        'From this generated directory, run the copied harness with the candidate executable:\n\n'+
        '`python3 run.py /path/to/candidate/gobstopper`\n\n'+
        'The runner pins the candidate before any outcomes and never modifies original sessions or calls a model. It reports baseline unsupported recovery commands separately. Public synthetic facts are intentionally known; passing proves API mechanics, not agent task quality.\n\n'+
        'Execution has a 600-second global deadline, 20-second command deadline, 4 MiB captured-output bound, and a 2,200-command cap. Empty physical JSONL lines and malformed JSON are unsearchable; a final newline terminator does not add a record. The robustness fixture therefore has two unsearchable records. Its valid numeric JSON line has no string matches.\n')
    print(json.dumps({'prepared_snapshots':36,'robustness_snapshots':1,'frozen_bytes':sum(s['bytes'] for s in manifest),'baseline_binary_sha256':baseline_sha,'outcomes_run':False}))

if __name__=='__main__':main()
