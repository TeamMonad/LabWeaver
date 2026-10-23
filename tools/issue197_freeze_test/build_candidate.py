#!/usr/bin/env python3
"""Export a local candidate OCI archive. Never push or contact Kubernetes."""
import argparse, hashlib, json, pathlib, re, subprocess, tarfile, tempfile

ROOT = pathlib.Path(__file__).resolve().parent
PIN = json.loads((ROOT / 'candidate.json').read_text())

def git(repo, *args):
    return subprocess.check_output(['git', '-C', str(repo), *args], text=True).strip()

def main():
    p = argparse.ArgumentParser()
    p.add_argument('--repo', type=pathlib.Path, required=True)
    p.add_argument('--source-ref', default=PIN['candidateCommit'])
    p.add_argument('--output', type=pathlib.Path, required=True)
    p.add_argument('--build', action='store_true')
    a = p.parse_args()
    if not re.fullmatch(r'[0-9a-f]{40}', a.source_ref):
        raise SystemExit('STOP: source ref must be an exact commit SHA')
    tree = git(a.repo, 'rev-parse', a.source_ref + '^{tree}')
    if tree != PIN['candidateTree']:
        raise SystemExit('STOP: source tree does not equal the reviewed candidate')
    blob = git(a.repo, 'rev-parse', a.source_ref + ':' + PIN['containerfile'])
    if blob != PIN['containerfileBlob']:
        raise SystemExit('STOP: candidate Containerfile differs')
    if a.source_ref != PIN['candidateCommit']:
        print('Equivalent source tree used. Record its ref; the target revision remains the reviewed candidate.')
    cmd = ['docker', 'buildx', 'build', '--platform', PIN['platform'], '--file', PIN['containerfile'],
           '--target', 'runtime', '--build-arg', 'SERVICE=evaluation-service',
           '--build-arg', 'SOURCE_COMMIT=' + PIN['candidateCommit'],
           '--build-arg', 'SOURCE_DATE_EPOCH=' + str(PIN['sourceDateEpoch']),
           '--provenance=false', '--sbom=false', '--tag', PIN['localTag'],
           '--output', 'type=oci,dest=' + str(a.output.resolve()) + ',rewrite-timestamp=true,oci-mediatypes=true',
           '--metadata-file', str(a.output.resolve()) + '.metadata.json', '.']
    print(json.dumps({'verifiedTree': tree, 'sourceRef': a.source_ref,
                      'targetCommit': PIN['candidateCommit'], 'command': cmd}, indent=2))
    if not a.build:
        print('PLAN ONLY. No image build, push, import or cluster write occurred.')
        return
    if a.output.exists():
        raise SystemExit('STOP: output archive already exists')
    a.output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='lw197-candidate-') as context:
        with subprocess.Popen(['git', '-C', str(a.repo), 'archive', a.source_ref], stdout=subprocess.PIPE) as archive:
            subprocess.run(['tar', '-x', '-C', context], stdin=archive.stdout, check=True)
            if archive.wait():
                raise SystemExit('STOP: source archive failed')
        subprocess.run(cmd, cwd=context, check=True)
    with tarfile.open(a.output) as archive:
        index = json.load(archive.extractfile('index.json'))
        entries = [d for d in index['manifests'] if d.get('platform', {}).get('architecture', 'amd64') == 'amd64']
        if len(entries) != 1 or entries[0]['mediaType'] != 'application/vnd.oci.image.manifest.v1+json':
            raise SystemExit('STOP: inspect unexpected OCI descriptor before any import')
        digest = entries[0]['digest']
        raw = archive.extractfile('blobs/sha256/' + digest.split(':')[1]).read()
        if 'sha256:' + hashlib.sha256(raw).hexdigest() != digest:
            raise SystemExit('STOP: OCI manifest digest mismatch')
        manifest = json.loads(raw)
        config = json.load(archive.extractfile('blobs/sha256/' + manifest['config']['digest'].split(':')[1]))
        if config.get('architecture') != 'amd64' or config.get('os') != 'linux':
            raise SystemExit('STOP: OCI platform mismatch')
        if config['config']['Labels']['org.opencontainers.image.revision'] != PIN['candidateCommit']:
            raise SystemExit('STOP: OCI revision label mismatch')
    archive_hash = hashlib.sha256()
    with a.output.open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            archive_hash.update(chunk)
    receipt = {'candidateCommit': PIN['candidateCommit'], 'candidateTree': tree, 'sourceRef': a.source_ref,
               'ociManifestDigest': digest, 'archive': str(a.output.resolve()),
               'archiveSha256': archive_hash.hexdigest(),
               'localTag': PIN['localTag'], 'imageReference': PIN['localTag'].split(':')[0] + '@' + digest,
               'nodeImportVerified': False}
    pathlib.Path(str(a.output) + '.receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt, indent=2))

if __name__ == '__main__':
    main()
