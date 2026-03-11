import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { App } from "./App";
import { createMockGateway } from "../remote/mockGateway";
import type { RemoteBootstrap, RemoteGateway } from "../remote/types";

describe("Codex remote mobile shell", () => {
  it("shows the sign-in and pairing flow before a session exists", async () => {
    render(
      <App
        gateway={createMockGateway({
          signedIn: false,
        })}
        initialEntries={["/"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /sign in to codex remote/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /continue with openai/i }),
    ).toBeVisible();
    expect(screen.getByText(/scan a host pairing code/i)).toBeVisible();
  });

  it("navigates from hosts to threads and shows the live approval sheet", async () => {
    const user = userEvent.setup();

    render(<App gateway={createMockGateway()} initialEntries={["/hosts"]} />);

    await user.click(
      await screen.findByRole("link", { name: /studio macbook/i }),
    );
    await user.click(
      await screen.findByRole("link", { name: /remote ios daemon/i }),
    );

    expect(
      await screen.findByRole("heading", { name: /remote ios daemon/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("button", { name: /interrupt turn/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: /approval required/i }),
    ).toBeVisible();
    expect(
      screen.getByRole("heading", { name: /waiting on approval/i }),
    ).toBeVisible();
    expect(screen.getByText(/1 approval/i)).toBeVisible();
    expect(screen.getAllByText(/pnpm tauri ios dev/i)).toHaveLength(3);
  });

  it("routes approval and interrupt actions through the gateway", async () => {
    const user = userEvent.setup();
    const gateway = createMockGateway();

    render(
      <App
        gateway={gateway}
        initialEntries={["/hosts/host-studio/threads/thread-ios"]}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: /remote ios daemon/i }),
      ).toBeVisible(),
    );

    await user.click(screen.getByRole("button", { name: /approve for turn/i }));
    await user.click(screen.getByRole("button", { name: /interrupt turn/i }));

    expect(gateway.inspect().approvalsResolved).toEqual([
      {
        decision: "accept",
        requestId: "approval-turn-shell",
      },
    ]);
    expect(gateway.inspect().interrupts).toEqual(["thread-ios"]);
  });

  it("disables steering when the thread is ready for a new turn", async () => {
    render(
      <App
        gateway={createMockGateway()}
        initialEntries={["/hosts/host-studio/threads/thread-review"]}
      />,
    );

    expect(
      await screen.findByRole("heading", { name: /ready for input/i }),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: /steer in-flight/i })).toBeDisabled();
  });

  it("refreshes host bootstrap surfaces when the gateway pushes connector updates", async () => {
    const gateway = await createBootstrapGateway();

    render(<App gateway={gateway.gateway} initialEntries={["/hosts"]} />);

    expect(await screen.findByText(/relay protected/i)).toBeVisible();

    await act(async () => {
      gateway.pushBootstrap((bootstrap) => ({
        ...bootstrap,
        deviceGroups: bootstrap.deviceGroups.map((group) => ({
          ...group,
          host:
            group.host.id === "host-studio"
              ? {
                  ...group.host,
                  relayStatus: "Relay reconnecting",
                }
              : group.host,
        })),
        hosts: bootstrap.hosts.map((host) => ({
          ...host,
          relayStatus:
            host.id === "host-studio" ? "Relay reconnecting" : host.relayStatus,
        })),
      }));
    });

    expect(await screen.findByText(/relay reconnecting/i)).toBeVisible();
  });
});

async function createBootstrapGateway(): Promise<{
  gateway: RemoteGateway;
  pushBootstrap(
    updater: (bootstrap: RemoteBootstrap) => RemoteBootstrap,
  ): void;
}> {
  const base = createMockGateway();
  let currentBootstrap: RemoteBootstrap = {
    deviceGroups: await base.listDeviceGroups(),
    hosts: await base.listHosts(),
    session: await base.getSession(),
  };
  const listeners = new Set<(bootstrap: RemoteBootstrap) => void>();

  return {
    gateway: {
      ...base,
      async getSession() {
        return clone(currentBootstrap.session);
      },
      async listHosts() {
        return clone(currentBootstrap.hosts);
      },
      async listDeviceGroups() {
        return clone(currentBootstrap.deviceGroups);
      },
      subscribeToBootstrap(listener) {
        listeners.add(listener);
        listener(clone(currentBootstrap));
        return () => {
          listeners.delete(listener);
        };
      },
    },
    pushBootstrap(updater) {
      currentBootstrap = updater(currentBootstrap);
      for (const listener of listeners) {
        listener(clone(currentBootstrap));
      }
    },
  };
}

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}
