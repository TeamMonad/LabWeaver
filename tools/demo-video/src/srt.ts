import {readFile} from 'node:fs/promises';
import {DemoVideoError, invariant} from './errors.js';

export type Cue = {index: number; startSeconds: number; endSeconds: number; text: string};

function timestamp(value: string): number {
  const match = /^(\d{2}):(\d{2}):(\d{2}),(\d{3})$/.exec(value);
  if (!match) throw new DemoVideoError('LW_DEMO_VIDEO_SRT_TIMESTAMP_INVALID', `invalid SRT timestamp: ${value}`);
  const [, hours = '0', minutes = '0', seconds = '0', millis = '0'] = match;
  return Number(hours) * 3600 + Number(minutes) * 60 + Number(seconds) + Number(millis) / 1000;
}

export function parseSrt(value: string): Cue[] {
  const blocks = value.replaceAll('\r\n', '\n').trim().split(/\n{2,}/);
  return blocks.map((block, position) => {
    const [indexLine, rangeLine, ...textLines] = block.split('\n');
    invariant(indexLine !== undefined && rangeLine !== undefined && textLines.length > 0, 'LW_DEMO_VIDEO_SRT_INVALID', `incomplete cue at position ${position + 1}`);
    const range = /^(\S+) --> (\S+)$/.exec(rangeLine);
    invariant(range, 'LW_DEMO_VIDEO_SRT_INVALID', `invalid cue range at position ${position + 1}`);
    return {index: Number(indexLine), startSeconds: timestamp(range[1]!), endSeconds: timestamp(range[2]!), text: textLines.join('\n')};
  });
}

export async function validateSrt(absolute: string, videoDuration: number): Promise<Cue[]> {
  const cues = parseSrt(await readFile(absolute, 'utf8'));
  invariant(cues.length > 0, 'LW_DEMO_VIDEO_SRT_EMPTY', 'subtitle file has no cues');
  let previousEnd = 0;
  for (const [position, cue] of cues.entries()) {
    invariant(cue.index === position + 1, 'LW_DEMO_VIDEO_SRT_ORDER', `cue index ${cue.index} is out of order`);
    invariant(cue.startSeconds >= previousEnd && cue.endSeconds > cue.startSeconds, 'LW_DEMO_VIDEO_SRT_OVERLAP', `cue ${cue.index} overlaps or has invalid duration`);
    invariant(cue.endSeconds <= videoDuration + 0.05, 'LW_DEMO_VIDEO_SRT_OUT_OF_RANGE', `cue ${cue.index} exceeds video duration`);
    previousEnd = cue.endSeconds;
  }
  return cues;
}
