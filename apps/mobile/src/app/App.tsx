import { QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";
import type { ReactNode } from "react";
import { BrowserRouter, MemoryRouter } from "react-router-dom";
import { GatewayProvider, createGatewayQueryClient } from "../remote/query";
import type { RemoteGateway } from "../remote/types";
import { AppRoutes } from "./routes";

type AppProps = {
  gateway: RemoteGateway;
  initialEntries?: string[];
};

export function App({ gateway, initialEntries }: AppProps) {
  const [queryClient] = useState(() => createGatewayQueryClient());

  return (
    <QueryClientProvider client={queryClient}>
      <GatewayProvider gateway={gateway}>
        <RouterContainer initialEntries={initialEntries}>
          <AppRoutes />
        </RouterContainer>
      </GatewayProvider>
    </QueryClientProvider>
  );
}

function RouterContainer({
  children,
  initialEntries,
}: {
  children: ReactNode;
  initialEntries?: string[];
}) {
  if (initialEntries) {
    return (
      <MemoryRouter initialEntries={initialEntries}>{children}</MemoryRouter>
    );
  }

  return <BrowserRouter>{children}</BrowserRouter>;
}
