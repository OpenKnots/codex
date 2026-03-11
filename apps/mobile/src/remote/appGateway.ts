import {
  readNativeCapabilities,
  readRemoteConnectorSnapshot,
} from "../native/bridge";
import { createLocalPreviewGateway } from "./localPreviewGateway";
import { createMockGateway } from "./mockGateway";
import {
  createGatewayBackedRelayConnector,
} from "./relayConnector";
import { createRelayBackedGateway } from "./relayGateway";

export async function createAppGateway() {
  const [nativeCapabilities, snapshot] = await Promise.all([
    readNativeCapabilities(),
    readRemoteConnectorSnapshot(),
  ]);
  if (snapshot) {
    return createLocalPreviewGateway(snapshot, nativeCapabilities);
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
