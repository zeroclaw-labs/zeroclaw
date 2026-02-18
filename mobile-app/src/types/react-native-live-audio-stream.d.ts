declare module "react-native-live-audio-stream" {
  type StreamOptions = {
    sampleRate?: number;
    channels?: number;
    bitsPerSample?: number;
    audioSource?: number;
    bufferSize?: number;
    wavFile?: string;
  };

  type Subscription = {
    remove?: () => void;
  };

  const LiveAudioStream: {
    init: (options: StreamOptions) => void;
    start: () => void;
    stop: () => void;
    on: (event: "data", callback: (data: string) => void) => Subscription;
  };

  export default LiveAudioStream;
}
