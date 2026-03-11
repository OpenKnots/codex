import type {
  DeviceGroup,
  NativeCapabilities,
  RelayConnectorSnapshot,
  RemoteGateway,
  RemoteSession,
} from "./types";

export type RelayBootstrap = {
  deviceGroups: DeviceGroup[];
  hosts: DeviceGroup["host"][];
  session: RemoteSession;
};

export interface RelayConnector {
  getBootstrap(): Promise<RelayBootstrap>;
  revokeDevice(hostId: string, deviceId: string): Promise<void>;
  signIn(): Promise<void>;
}

export function createGatewayBackedRelayConnector(
  gateway: Pick<
    RemoteGateway,
    "getSession" | "listDeviceGroups" | "listHosts" | "revokeDevice" | "signIn"
  >,
): RelayConnector {
  return {
    async getBootstrap() {
      const [session, hosts, deviceGroups] = await Promise.all([
        gateway.getSession(),
        gateway.listHosts(),
        gateway.listDeviceGroups(),
      ]);
      return {
        deviceGroups,
        hosts,
        session,
      };
    },
    revokeDevice(hostId, deviceId) {
      return gateway.revokeDevice(hostId, deviceId);
    },
    signIn() {
      return gateway.signIn();
    },
  };
}

export function createLocalPreviewRelayConnector(
  snapshot: RelayConnectorSnapshot,
  nativeCapabilities: NativeCapabilities,
): RelayConnector {
  let currentSnapshot = snapshot;

  return {
    async getBootstrap() {
      return {
        deviceGroups: currentSnapshot.deviceGroups,
        hosts: currentSnapshot.hosts,
        session: {
          ...currentSnapshot.session,
          nativeCapabilities,
        },
      };
    },
    async revokeDevice(hostId, deviceId) {
      currentSnapshot = {
        ...currentSnapshot,
        deviceGroups: currentSnapshot.deviceGroups.map((group) => {
          if (group.host.id !== hostId) {
            return group;
          }
          return {
            ...group,
            devices: group.devices.map((device) =>
              device.id === deviceId
                ? {
                    ...device,
                    trust: "revoked",
                  }
                : device,
            ),
          };
        }),
      };
    },
    async signIn() {
      currentSnapshot = {
        ...currentSnapshot,
        session: {
          ...currentSnapshot.session,
          signedIn: true,
        },
      };
    },
  };
}
