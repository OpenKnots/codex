import type { RelayConnector } from "./relayConnector";
import type {
  ApprovalResolution,
  RemoteGateway,
  RemoteThreadRecord,
  SendTurnInput,
} from "./types";

export function createRelayBackedGateway({
  connector,
  threadGateway,
}: {
  connector: RelayConnector;
  threadGateway: RemoteGateway;
}): RemoteGateway {
  async function resolveThreadHostId(hostId: string): Promise<string> {
    const [{ hosts }, threadHosts] = await Promise.all([
      connector.getBootstrap(),
      threadGateway.listHosts(),
    ]);
    const connectorIndex = hosts.findIndex((host) => host.id === hostId);
    if (connectorIndex === -1) {
      return hostId;
    }
    return threadHosts[connectorIndex]?.id ?? threadHosts[0]?.id ?? hostId;
  }

  async function rehostThreadRecord(
    hostId: string,
    loader: () => Promise<RemoteThreadRecord>,
  ): Promise<RemoteThreadRecord> {
    const record = await loader();
    return {
      ...record,
      hostId,
    };
  }

  return {
    async getSession() {
      const { session } = await connector.getBootstrap();
      return session;
    },
    async signIn() {
      await Promise.all([connector.signIn(), threadGateway.signIn()]);
    },
    async listHosts() {
      const { hosts } = await connector.getBootstrap();
      return hosts;
    },
    async listThreads(hostId: string) {
      return threadGateway.listThreads(await resolveThreadHostId(hostId));
    },
    async getThread(hostId: string, threadId: string) {
      const resolvedHostId = await resolveThreadHostId(hostId);
      return rehostThreadRecord(hostId, () =>
        threadGateway.getThread(resolvedHostId, threadId),
      );
    },
    async listDeviceGroups() {
      const { deviceGroups } = await connector.getBootstrap();
      return deviceGroups;
    },
    async resolveApproval(
      hostId: string,
      threadId: string,
      resolution: ApprovalResolution,
    ) {
      return threadGateway.resolveApproval(
        await resolveThreadHostId(hostId),
        threadId,
        resolution,
      );
    },
    async interruptTurn(hostId: string, threadId: string) {
      return threadGateway.interruptTurn(
        await resolveThreadHostId(hostId),
        threadId,
      );
    },
    async sendPrompt(hostId: string, threadId: string, input: SendTurnInput) {
      return threadGateway.sendPrompt(
        await resolveThreadHostId(hostId),
        threadId,
        input,
      );
    },
    async revokeDevice(hostId: string, deviceId: string) {
      await connector.revokeDevice(hostId, deviceId);
    },
    subscribeToThread(hostId: string, threadId: string, listener) {
      let unsubscribe: (() => void) | undefined;
      void resolveThreadHostId(hostId).then((resolvedHostId) => {
        unsubscribe = threadGateway.subscribeToThread(
          resolvedHostId,
          threadId,
          (record) => {
            listener({
              ...record,
              hostId,
            });
          },
        );
      });

      return () => {
        unsubscribe?.();
      };
    },
    inspect() {
      return threadGateway.inspect();
    },
  };
}
