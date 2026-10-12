#!/usr/bin/env python3
"""Check exported synthetic evidence. Never fetch objects or credentials."""
import argparse, base64, hashlib, json, pathlib
from jsonschema import Draft202012Validator, FormatChecker
from common import pinned_schema

ROOT = pathlib.Path(__file__).resolve().parent

def main():
    if not __debug__:
        raise SystemExit('STOP: optimized Python would disable verification checks')
    p = argparse.ArgumentParser()
    p.add_argument('--submission', type=pathlib.Path, required=True)
    p.add_argument('--archive', type=pathlib.Path, required=True)
    p.add_argument('--expected', type=pathlib.Path, required=True)
    p.add_argument('--phase', choices=['a', 'b'], required=True)
    p.add_argument('--compare', type=pathlib.Path)
    p.add_argument('--replay', action='store_true')
    a = p.parse_args()
    sub = json.loads(a.submission.read_text())
    schema = pinned_schema('frozen-submission.schema.json')
    Draft202012Validator(schema, format_checker=FormatChecker()).validate(sub)
    assert a.archive.stat().st_size <= 100663296
    raw = a.archive.read_bytes()
    assert hashlib.sha256(raw).hexdigest() == sub['contentSha256']
    assert len(raw) == sub['object']['sizeBytes']
    expected = json.loads(a.expected.read_text())[a.phase]
    actual = sorted([{k: f[k] for k in ('path', 'sizeBytes', 'sha256')} for f in sub['files']], key=lambda f: f['path'])
    assert actual == expected
    contents = json.loads(raw)
    assert contents['apiVersion'] == 'evaluation.labweaver.io/frozen-submission-archive/v1'
    data = {f['path']: base64.b64decode(f['contentBase64'], validate=True) for f in contents['files']}
    assert len(data) == len(contents['files']) == len(expected)
    assert sorted(data) == sorted(f['path'] for f in expected)
    for f in expected:
        assert len(data[f['path']]) == f['sizeBytes']
        assert hashlib.sha256(data[f['path']]).hexdigest() == f['sha256']
    if a.compare:
        before = json.loads(a.compare.read_text())
        if a.replay:
            assert sub == before
        else:
            assert sub['id'] != before['id']
            assert [f['sizeBytes'] for f in sub['files']] == [f['sizeBytes'] for f in before['files']]
            assert sub['contentSha256'] != before['contentSha256']
            assert [f['sha256'] for f in sub['files']] != [f['sha256'] for f in before['files']]
    print(json.dumps({'submission': sub['id'], 'phase': a.phase, 'files': len(actual),
                      'contentSha256': sub['contentSha256'], 'objectVersion': sub['object']['objectVersion'],
                      'byteHashesMatch': True, 'replay': a.replay}))

if __name__ == '__main__':
    main()
