import {
  readNativeCapabilities,
  readRemoteConnectorSnapshot,
} from "../native/bridge";
import { readRemoteAppConfig } from "./config";
import { createLocalPreviewGateway } from "./localPreviewGateway";
import { createMockGateway } from "./mockGateway";
import {
  createGatewayBackedRelayConnector,
} from "./relayConnector";
import { createRelayBackedGateway } from "./relayGateway";
import { createRelayWebSocketGateway } from "./relayWebSocketGateway";

export async function createAppGateway() {
  const [nativeCapabilities, snapshot] = await Promise.all([
    readNativeCapabilities(),
    readRemoteConnectorSnapshot(),
  ]);
  if (snapshot) {
    return createLocalPreviewGateway(snapshot, nativeCapabilities);
  }
  const { relayUrl } = readRemoteAppConfig();
  if (relayUrl) {
    return createRelayWebSocketGateway({
      nativeCapabilities,
      url: relayUrl,
    });
  }

  const threadGateway = createMockGateway({
    nativeCapabilities,
  });
  const connector = createGatewayBackedRelayConnector(threadGateway);

  return createRelayBackedGateway({
    connector,
    threadGateway,
  });
}
