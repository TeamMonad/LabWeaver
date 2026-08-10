import React from 'react';
import {Video} from '@remotion/media';
import {AbsoluteFill, Composition, Img, Sequence, interpolate, staticFile, useCurrentFrame} from 'remotion';
import {SCENES, TOTAL_SECONDS, VIDEO} from './model.ts';

export type ExplanationBeat = {atSeconds: number; title: string; body: string};
export type RenderScene = {
  sceneId: string;
  label: string;
  clip: string;
  screenshot: string;
  clipDurationInFrames: number;
  durationInFrames: number;
  trimBeforeFrames: number;
  beats: ExplanationBeat[];
};
export type DemoVideoProps = {cut: 'preview' | 'final'; scenes: RenderScene[]};

const fontFamily = 'Inter, "Noto Sans SC", "Microsoft YaHei", sans-serif';

const SceneView: React.FC<{scene: RenderScene; cut: DemoVideoProps['cut']}> = ({scene, cut}) => {
  const frame = useCurrentFrame();
  const scale = interpolate(frame, [0, scene.durationInFrames], [1, 1.025], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  const beatIndex = scene.beats.reduce((selected, beat, index) => beat.atSeconds * VIDEO.fps <= frame ? index : selected, 0);
  const beat = scene.beats[beatIndex]!;
  const beatFrame = frame - beat.atSeconds * VIDEO.fps;
  const cardOpacity = interpolate(beatFrame, [0, 12], [0, 1], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  const cardOffset = interpolate(beatFrame, [0, 16], [28, 0], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  const progress = Math.min(1, Math.max(0, frame / scene.durationInFrames));
  return <AbsoluteFill style={{backgroundColor: '#07111f', overflow: 'hidden', fontFamily}}>
    <div style={{position: 'absolute', inset: 0, transform: `scale(${scale})`}}>
      <Sequence durationInFrames={scene.clipDurationInFrames}>
        <Video src={staticFile(scene.clip)} trimBefore={scene.trimBeforeFrames} muted onError={() => 'fail'} />
      </Sequence>
      <Sequence from={scene.clipDurationInFrames} durationInFrames={scene.durationInFrames - scene.clipDurationInFrames}>
        <Img src={staticFile(scene.screenshot)} style={{width: '100%', height: '100%', objectFit: 'cover'}} />
      </Sequence>
    </div>
    <AbsoluteFill style={{background: 'linear-gradient(180deg, rgba(4,10,20,.12) 35%, rgba(4,10,20,.88) 100%)'}} />
    <div style={{position: 'absolute', top: 44, left: 58, display: 'flex', gap: 12, alignItems: 'center'}}>
      <div style={{padding: '9px 15px', borderRadius: 999, background: '#2563eb', color: '#fff', fontSize: 21, fontWeight: 700}}>{scene.label}</div>
      <div style={{color: '#dbeafe', fontSize: 19, fontWeight: 600}}>LabWeaver 云原生实验平台</div>
    </div>
    {cut === 'preview' && <div style={{position: 'absolute', top: 48, right: 58, color: '#fee2e2', background: 'rgba(127,29,29,.86)', border: '1px solid rgba(254,202,202,.45)', borderRadius: 9, padding: '8px 13px', fontSize: 17, fontWeight: 700}}>
      Fixture preview · not release evidence
    </div>}
    <div key={`${scene.sceneId}-${beatIndex}`} style={{position: 'absolute', left: 58, right: 58, bottom: 54, opacity: cardOpacity, transform: `translateY(${cardOffset}px)`}}>
      <div style={{maxWidth: 1360, borderLeft: '6px solid #60a5fa', borderRadius: 16, background: 'rgba(7,17,31,.92)', padding: '22px 28px 24px', boxShadow: '0 18px 52px rgba(0,0,0,.34)'}}>
        <div style={{color: '#93c5fd', fontSize: 24, fontWeight: 800, marginBottom: 8}}>{beat.title}</div>
        <div style={{color: '#f8fafc', fontSize: 31, lineHeight: 1.45, fontWeight: 600}}>{beat.body}</div>
      </div>
    </div>
    <div style={{position: 'absolute', left: 0, right: 0, bottom: 0, height: 7, background: 'rgba(148,163,184,.25)'}}>
      <div style={{height: '100%', width: `${progress * 100}%`, background: 'linear-gradient(90deg,#2563eb,#22d3ee)'}} />
    </div>
  </AbsoluteFill>;
};

export const DemoVideo: React.FC<DemoVideoProps> = ({cut, scenes}) => {
  let from = 0;
  return <AbsoluteFill>
    {scenes.map((scene) => {
      const start = from;
      from += scene.durationInFrames;
      return <Sequence key={scene.sceneId} from={start} durationInFrames={scene.durationInFrames} premountFor={VIDEO.fps}>
        <SceneView scene={scene} cut={cut} />
      </Sequence>;
    })}
  </AbsoluteFill>;
};

export const RemotionRoot: React.FC = () => <Composition
  id="LabWeaverDemoVideo"
  component={DemoVideo}
  durationInFrames={TOTAL_SECONDS * VIDEO.fps}
  fps={VIDEO.fps}
  width={VIDEO.width}
  height={VIDEO.height}
  defaultProps={{cut: 'preview', scenes: SCENES.map((scene) => ({
    sceneId: scene.id, label: scene.label, clip: '', screenshot: '',
    clipDurationInFrames: VIDEO.fps, durationInFrames: scene.seconds * VIDEO.fps,
    trimBeforeFrames: VIDEO.fps, beats: [...scene.beats],
  }))}}
/>;
