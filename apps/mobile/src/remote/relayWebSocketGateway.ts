import { createRelaySocketClient } from "./relaySocketClient";
import { createRelaySocketGateway } from "./relaySocketGateway";
import type { NativeCapabilities, RemoteGateway } from "./types";

export function createRelayWebSocketGateway({
  nativeCapabilities,
  url,
}: {
  url: string;
  nativeCapabilities: NativeCapabilities;
}): RemoteGateway {
  return createRelaySocketGateway({
    client: createRelaySocketClient({
      url,
    }),
    nativeCapabilities,
  });
}
