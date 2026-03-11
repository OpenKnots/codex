import { QueryClient, useQuery } from "@tanstack/react-query";
import { createContext, useContext } from "react";
import type { PropsWithChildren } from "react";
import type { RemoteGateway } from "./types";

const GatewayContext = createContext<RemoteGateway | null>(null);

export function GatewayProvider({
  children,
  gateway,
}: PropsWithChildren<{ gateway: RemoteGateway }>) {
  return (
    <GatewayContext.Provider value={gateway}>
      {children}
    </GatewayContext.Provider>
  );
}

export function useGateway(): RemoteGateway {
  const gateway = useContext(GatewayContext);
  if (!gateway) {
    throw new Error("Remote gateway is unavailable.");
  }
  return gateway;
}

export function createGatewayQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 15_000,
        gcTime: 5 * 60_000,
        retry: 1,
      },
    },
  });
}

export const gatewayQueryKeys = {
  session: ["remote-session"] as const,
  hosts: ["remote-hosts"] as const,
  threads: (hostId: string) => ["remote-threads", hostId] as const,
  thread: (hostId: string, threadId: string) =>
    ["remote-thread", hostId, threadId] as const,
  devices: ["remote-devices"] as const,
};

export function useSessionQuery() {
  const gateway = useGateway();
  return useQuery({
    queryKey: gatewayQueryKeys.session,
    queryFn: () => gateway.getSession(),
  });
}

export function useHostsQuery() {
  const gateway = useGateway();
  return useQuery({
    queryKey: gatewayQueryKeys.hosts,
    queryFn: () => gateway.listHosts(),
  });
}

export function useThreadsQuery(hostId: string) {
  const gateway = useGateway();
  return useQuery({
    enabled: hostId.length > 0,
    queryKey: gatewayQueryKeys.threads(hostId),
    queryFn: () => gateway.listThreads(hostId),
  });
}

export function useThreadQuery(hostId: string, threadId: string) {
  const gateway = useGateway();
  return useQuery({
    enabled: hostId.length > 0 && threadId.length > 0,
    queryKey: gatewayQueryKeys.thread(hostId, threadId),
    queryFn: () => gateway.getThread(hostId, threadId),
  });
}

export function useDeviceGroupsQuery() {
  const gateway = useGateway();
  return useQuery({
    queryKey: gatewayQueryKeys.devices,
    queryFn: () => gateway.listDeviceGroups(),
  });
}
