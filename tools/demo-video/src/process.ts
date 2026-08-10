import {spawn} from 'node:child_process';
import {DemoVideoError} from './errors.js';

type RunOptions = {cwd?: string; env?: NodeJS.ProcessEnv};

export async function run(command: string, args: string[], code: string, options: RunOptions = {}): Promise<string> {
  return await new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true,
      ...(options.cwd ? {cwd: options.cwd} : {}),
      ...(options.env ? {env: options.env} : {}),
    });
    let stdout = '';
    let stderr = '';
    child.stdout.setEncoding('utf8').on('data', (chunk: string) => { stdout += chunk; });
    child.stderr.setEncoding('utf8').on('data', (chunk: string) => { stderr += chunk; });
    child.once('error', (error) => reject(new DemoVideoError(code, `${command} failed to start: ${error.message}`)));
    child.once('close', (exitCode) => exitCode === 0
      ? resolve(stdout.trim())
      : reject(new DemoVideoError(code, `${command} exited ${exitCode}: ${stderr.trim()}`)));
  });
}

export async function probeVideo(absolute: string): Promise<{codec: string; width: number; height: number; fps: number; durationSeconds: number; audioStreams: number}> {
  const output = await run('ffprobe', ['-v', 'error', '-show_entries', 'stream=codec_type,codec_name,width,height,r_frame_rate:format=duration', '-of', 'json', absolute], 'LW_DEMO_VIDEO_FFPROBE_FAILED');
  const value = JSON.parse(output) as {streams?: Array<Record<string, unknown>>; format?: {duration?: string}};
  const video = value.streams?.find((stream) => stream.codec_type === 'video');
  if (!video) throw new DemoVideoError('LW_DEMO_VIDEO_STREAM_MISSING', 'video stream is missing');
  const rate = String(video.r_frame_rate ?? '0/1').split('/').map(Number);
  return {
    codec: String(video.codec_name ?? ''),
    width: Number(video.width),
    height: Number(video.height),
    fps: (rate[0] ?? 0) / (rate[1] || 1),
    durationSeconds: Number(value.format?.duration ?? 0),
    audioStreams: value.streams?.filter((stream) => stream.codec_type === 'audio').length ?? 0,
  };
}
