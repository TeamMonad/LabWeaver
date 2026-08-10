import {readFile} from 'node:fs/promises';
import path from 'node:path';
import Ajv2020Module, {type ErrorObject, type ValidateFunction} from 'ajv/dist/2020.js';
import addFormatsModule from 'ajv-formats';
import {DemoVideoError} from './errors.js';

let validators: Promise<{receipt: ValidateFunction; manifest: ValidateFunction; releaseGate: ValidateFunction; localCluster: ValidateFunction}> | undefined;

function formatErrors(errors: ErrorObject[] | null | undefined): string {
  return (errors ?? []).map((error) => `${error.instancePath || '/'} ${error.message ?? 'invalid'}`).join('; ');
}

export async function getValidators(root: string) {
  validators ??= (async () => {
    const Ajv2020 = Ajv2020Module.default;
    const addFormats = addFormatsModule.default;
    const ajv = new Ajv2020({allErrors: true, strict: true});
    addFormats(ajv);
    const [receiptSchema, manifestSchema, releaseGateSchema, localClusterSchema] = await Promise.all([
      readFile(path.join(root, 'schemas/results/demo-video-capture-receipt.v1.schema.json'), 'utf8').then(JSON.parse),
      readFile(path.join(root, 'schemas/results/demo-video-manifest.v1.schema.json'), 'utf8').then(JSON.parse),
      readFile(path.join(root, 'schemas/results/release-gate-report.v3.schema.json'), 'utf8').then(JSON.parse),
      readFile(path.join(root, 'schemas/results/demo-video-local-cluster-report.v1.schema.json'), 'utf8').then(JSON.parse),
    ]);
    return {
      receipt: ajv.compile(receiptSchema), manifest: ajv.compile(manifestSchema),
      releaseGate: ajv.compile(releaseGateSchema), localCluster: ajv.compile(localClusterSchema),
    };
  })();
  return validators;
}

export async function validateReceipt(root: string, value: unknown): Promise<void> {
  const {receipt} = await getValidators(root);
  if (!receipt(value)) throw new DemoVideoError('LW_DEMO_VIDEO_RECEIPT_INVALID', formatErrors(receipt.errors));
}

export async function validateManifest(root: string, value: unknown): Promise<void> {
  const {manifest} = await getValidators(root);
  if (!manifest(value)) throw new DemoVideoError('LW_DEMO_VIDEO_MANIFEST_INVALID', formatErrors(manifest.errors));
  const record = value as {scenes: Array<{sceneId: string}>; checksums: Array<{path: string}>};
  if (new Set(record.scenes.map(({sceneId}) => sceneId)).size !== record.scenes.length) {
    throw new DemoVideoError('LW_DEMO_VIDEO_MANIFEST_INVALID', 'scene IDs must be unique');
  }
  if (new Set(record.checksums.map(({path: locator}) => locator)).size !== record.checksums.length) {
    throw new DemoVideoError('LW_DEMO_VIDEO_MANIFEST_INVALID', 'checksum locators must be unique');
  }
}

export async function validateReleaseGate(root: string, value: unknown): Promise<void> {
  const {releaseGate} = await getValidators(root);
  if (!releaseGate(value)) throw new DemoVideoError('LW_DEMO_VIDEO_RELEASE_GATE_INVALID', formatErrors(releaseGate.errors));
}

export async function validateLocalClusterReport(root: string, value: unknown): Promise<void> {
  const {localCluster} = await getValidators(root);
  if (!localCluster(value)) throw new DemoVideoError('LW_DEMO_VIDEO_LOCAL_CLUSTER_REPORT_INVALID', formatErrors(localCluster.errors));
}
