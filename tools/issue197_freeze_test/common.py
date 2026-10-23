"""Read the existing candidate schemas without a second schema copy."""
import hashlib
import json
from pathlib import Path

SUPPORT_ROOT = Path(__file__).resolve().parent
REPOSITORY_ROOT = SUPPORT_ROOT.parents[1]


def pinned_schema(name):
    pin = json.loads((SUPPORT_ROOT / 'candidate.json').read_text())
    records = [item for item in pin['schemas'] if Path(item['source']).name == name]
    if len(records) != 1:
        raise ValueError('STOP: unknown candidate schema')
    record = records[0]
    payload = (REPOSITORY_ROOT / record['source']).read_bytes()
    if hashlib.sha256(payload).hexdigest() != record['sha256']:
        raise ValueError('STOP: repository schema differs from the frozen candidate')
    return json.loads(payload)
