export type RemoteAppConfig = {
  relayUrl?: string;
};

type GlobalConfig = {
  __CODEX_REMOTE_CONFIG__?: RemoteAppConfig;
};

export function readRemoteAppConfig(): RemoteAppConfig {
  const relayUrl =
    (globalThis as typeof globalThis & GlobalConfig).__CODEX_REMOTE_CONFIG__
      ?.relayUrl ?? import.meta.env.VITE_CODEX_RELAY_URL;

  return relayUrl ? { relayUrl } : {};
}
