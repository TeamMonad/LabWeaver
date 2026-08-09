import {DemoVideoError} from './errors.js';

const FORBIDDEN_TEXT = [
  /(?:token|password|secret|authorization)\s*[:=]\s*[^\s,}]+/i,
  /(?:https?:\/\/)?(?:[a-z0-9-]+\.)+(?:internal|localdomain)(?::\d+)?/i,
  /(?:[A-Za-z]:\\|\/(?:Users|home)\/)[^\s"']+/,
  /-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/,
];

export function assertNoForbiddenText(values: string[]): void {
  for (const value of values) {
    for (const forbidden of FORBIDDEN_TEXT) {
      if (forbidden.test(value)) {
        throw new DemoVideoError('LW_DEMO_VIDEO_PRIVACY_SCAN_FAILED', `forbidden content matched ${forbidden.source}`);
      }
    }
  }
}
