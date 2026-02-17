declare module "@siteed/expo-audio-studio" {
  export type AudioStreamEvent = { data?: string };
  export type AudioAnalysis = { dataPoints?: Array<{ amplitude?: number }> };

  export type StartRecordingOptions = {
    sampleRate: number;
    channels: number;
    encoding: string;
    interval: number;
    enableProcessing: boolean;
    onAudioStream?: (event: AudioStreamEvent) => Promise<void> | void;
    onAudioAnalysis?: (analysis: AudioAnalysis) => Promise<void> | void;
  };

  export type AudioRecorder = {
    startRecording: (opts: StartRecordingOptions) => Promise<void>;
    stopRecording: () => Promise<void>;
  };

  export function useAudioRecorder(): AudioRecorder;
}

// Keep this file for any ad-hoc module shims.
