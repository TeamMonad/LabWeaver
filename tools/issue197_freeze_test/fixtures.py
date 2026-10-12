#!/usr/bin/env python3
"""Make only synthetic files and validate the published manifest schema."""
import argparse, hashlib, json, pathlib, re
from jsonschema import Draft202012Validator
from common import pinned_schema

ROOT = pathlib.Path(__file__).resolve().parent

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--run-id', required=True)
    p.add_argument('--output', type=pathlib.Path, required=True)
    a = p.parse_args()
    if not re.fullmatch(r'lw197-[a-z0-9][a-z0-9-]{0,23}', a.run_id):
        raise SystemExit('STOP: use a new lw197- lowercase run ID')
    if a.output.exists():
        raise SystemExit('STOP: fixture output already exists')
    fixture_prefix = a.run_id
    values = {'README.md': b'# Issue197 synthetic freeze fixture\n', 'src/main.c': b'int main(void){return 1;}\n', 'empty.txt': b''}
    manifest = {'apiVersion': 'evaluation.labweaver.io/v1', 'kind': 'SubmissionManifest',
                'name': a.run_id + '-1', 'source': 'workspace',
                'include': [{'kind': 'exactFile', 'path': fixture_prefix + '/' + path} for path in sorted(values)],
                'exclude': [], 'required': [],
                'llmReadable': [], 'maxTotalBytes': 4096, 'maxFiles': 10000, 'followSymlinks': False}
    schema = pinned_schema('submission-manifest.schema.json')
    Draft202012Validator(schema).validate(manifest)
    expected = {}
    for phase in ('a', 'b'):
        files = []
        for name, original in values.items():
            content = original.replace(b'return 1;', b'return 2;') if phase == 'b' else original
            path = a.output / ('workspace-' + phase) / fixture_prefix / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            path.chmod(0o644)
            files.append({'path': fixture_prefix + '/' + name, 'sizeBytes': len(content),
                          'sha256': hashlib.sha256(content).hexdigest()})
        expected[phase] = sorted(files, key=lambda f: f['path'])
    assert [f['sizeBytes'] for f in expected['a']] == [f['sizeBytes'] for f in expected['b']]
    assert expected['a'] != expected['b']
    (a.output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    (a.output / 'expected-files.json').write_text(json.dumps(expected, indent=2) + '\n')
    print(json.dumps({'manifestSchema': 'valid', 'runId': a.run_id, 'files': len(values),
                      'sourceBytes': sum(len(v) for v in values.values()), 'sameSizeRewrite': True,
                      'fixturePrefix': fixture_prefix, 'clusterExecuted': False}))

if __name__ == '__main__':
    main()
