import React from 'react';
import {AbsoluteFill, Composition, Loop, OffthreadVideo, Sequence, interpolate, staticFile, useCurrentFrame} from 'remotion';
import {SCENES, TOTAL_SECONDS, VIDEO} from './model.ts';

export type RenderScene = {sceneId: string; label: string; clip: string; durationInFrames: number; sourceFrames: number};
export type DemoVideoProps = {cut: 'preview' | 'final'; scenes: RenderScene[]};

const labelStyle: React.CSSProperties = {
  position: 'absolute', left: 64, bottom: 48, padding: '12px 20px', borderRadius: 12,
  background: 'rgba(10,18,32,.82)', color: '#fff', fontFamily: 'Inter, Noto Sans SC, sans-serif',
  fontSize: 28, letterSpacing: '.02em', boxShadow: '0 8px 32px rgba(0,0,0,.22)',
};

const SceneView: React.FC<{scene: RenderScene; cut: DemoVideoProps['cut']}> = ({scene, cut}) => {
  const frame = useCurrentFrame();
  const scale = interpolate(frame, [0, scene.durationInFrames], [1, 1.018], {extrapolateLeft: 'clamp', extrapolateRight: 'clamp'});
  return <AbsoluteFill style={{backgroundColor: '#07111f', overflow: 'hidden'}}>
    <div style={{width: '100%', height: '100%', transform: `scale(${scale})`}}>
      <Loop durationInFrames={Math.max(1, scene.sourceFrames)}>
        <OffthreadVideo src={staticFile(scene.clip)} muted />
      </Loop>
    </div>
    <div style={labelStyle}>{scene.label}{cut === 'preview' ? ' · Fixture preview / not release evidence' : ''}</div>
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
  defaultProps={{cut: 'preview', scenes: SCENES.map((scene) => ({sceneId: scene.id, label: scene.label, clip: '', durationInFrames: scene.seconds * VIDEO.fps, sourceFrames: VIDEO.fps}))}}
/>;
