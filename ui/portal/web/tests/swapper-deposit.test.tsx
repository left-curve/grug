import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resetAppletsKitMocks } from "./mocks/applets-kit";

import { SwapperDeposit } from "../src/components/bridge/SwapperDeposit";

import type { Connector } from "@left-curve/store/types";

type MockConnector = {
  getProvider?: () => Promise<unknown>;
  icon: string;
  id: string;
  name: string;
  type?: string;
  uid?: string;
};

type MockSignerClient = { id: string };

const swapperCssVariableColors = {
  "--color-ink-primary-900": "rgb(17, 17, 17)",
  "--color-surface-primary-rice": "rgb(255, 252, 246)",
  "--color-surface-secondary-rice": "rgb(255, 249, 240)",
  "--color-surface-tertiary-rice": "rgb(255, 243, 225)",
  "--color-surface-quaternary-rice": "rgb(243, 229, 191)",
};

const swapperDepositMocks = vi.hoisted(() => ({
  dangoConnector: undefined as MockConnector | undefined,
  isConnected: true,
  refreshBalances: vi.fn(),
  refreshUserStatus: vi.fn(),
  signerClients: new Map<string, MockSignerClient>(),
  signerError: null as Error | null,
  swapperDestroy: vi.fn(),
  swapperOptions: [] as Array<{
    depositWalletAddress?: string;
    dstChainId?: string;
    dstTokenAddr?: string;
    iframeAttributes?: Record<string, string | undefined>;
    integratorId?: string;
    onEvent?: (event: { type: string }) => void;
    wallet?: { signer: MockSignerClient };
  }>,
}));

vi.mock("@left-curve/store", () => ({
  isEvmProviderConnector: (connector?: MockConnector | null) =>
    !!connector &&
    (connector.type === "eip1193" || connector.type === "privy") &&
    typeof connector.getProvider === "function",
  useAccount: () => ({
    account: swapperDepositMocks.isConnected
      ? {
          address: "0x6272696467657573657200000000000000000000",
        }
      : undefined,
    connector: swapperDepositMocks.dangoConnector,
    isConnected: swapperDepositMocks.isConnected,
    refreshUserStatus: swapperDepositMocks.refreshUserStatus,
  }),
  useBalances: () => ({
    refetch: swapperDepositMocks.refreshBalances,
  }),
  useConnectorWalletClient: ({ connector }: { connector?: MockConnector }) => ({
    data: connector?.uid ? swapperDepositMocks.signerClients.get(connector.uid) : undefined,
    error: swapperDepositMocks.signerError,
  }),
}));

vi.mock("@swapper-finance/deposit-sdk", () => {
  class SwapperIframe {
    private readonly iframe: HTMLIFrameElement;

    constructor(options: {
      container: HTMLElement;
      iframeAttributes?: Record<string, string | undefined>;
      onEvent?: (event: { type: string }) => void;
      wallet?: { signer: MockSignerClient };
    }) {
      swapperDepositMocks.swapperOptions.push(options);
      const wrapper = document.createElement("div");
      wrapper.style.height = options.iframeAttributes?.height ?? "560px";
      this.iframe = document.createElement("iframe");
      this.iframe.title = "Swapper deposit";
      wrapper.appendChild(this.iframe);
      options.container.appendChild(wrapper);
    }

    getIframe() {
      return this.iframe;
    }

    destroy() {
      swapperDepositMocks.swapperDestroy();
      this.iframe.remove();
    }
  }

  return {
    SwapperIframe,
    WidgetEventName: {
      TRANSACTION_SUCCESS: "transaction_success",
    },
  };
});

const createEvmConnector = (id: string, uid = id): MockConnector => ({
  getProvider: vi.fn(),
  icon: "/wallet.svg",
  id,
  name: id,
  type: "eip1193",
  uid,
});

function renderSwapperDeposit(signerConnector?: MockConnector) {
  return render(
    <SwapperDeposit onBack={vi.fn()} signerConnector={signerConnector as Connector | undefined} />,
  );
}

describe("swapper deposit signer", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    swapperDepositMocks.dangoConnector = undefined;
    swapperDepositMocks.isConnected = true;
    swapperDepositMocks.signerClients = new Map();
    swapperDepositMocks.signerError = null;
    swapperDepositMocks.swapperOptions = [];
    for (const [variable, color] of Object.entries(swapperCssVariableColors)) {
      document.documentElement.style.setProperty(variable, color);
    }
    vi.stubEnv("PUBLIC_SWAPPER_INTEGRATOR_ID", "test-integrator");
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    for (const variable of Object.keys(swapperCssVariableColors)) {
      document.documentElement.style.removeProperty(variable);
    }
    vi.unstubAllEnvs();
  });

  it("passes the selected external EVM wallet signer before the Dango connector", async () => {
    const externalConnector = createEvmConnector("browser-wallet", "external-wallet-uid");
    const dangoConnector = createEvmConnector("dango-wallet", "dango-wallet-uid");
    const externalSigner = { id: "external-signer" };
    const dangoSigner = { id: "dango-signer" };

    swapperDepositMocks.dangoConnector = dangoConnector;
    swapperDepositMocks.signerClients.set(externalConnector.uid!, externalSigner);
    swapperDepositMocks.signerClients.set(dangoConnector.uid!, dangoSigner);

    renderSwapperDeposit(externalConnector);

    await waitFor(() => expect(swapperDepositMocks.swapperOptions).toHaveLength(1));
    expect(swapperDepositMocks.swapperOptions[0].wallet).toEqual({ signer: externalSigner });
  });

  it("falls back to the connected Dango EVM wallet signer when no external signer is selected", async () => {
    const dangoConnector = createEvmConnector("dango-wallet", "dango-wallet-uid");
    const dangoSigner = { id: "dango-signer" };

    swapperDepositMocks.dangoConnector = dangoConnector;
    swapperDepositMocks.signerClients.set(dangoConnector.uid!, dangoSigner);

    renderSwapperDeposit();

    await waitFor(() => expect(swapperDepositMocks.swapperOptions).toHaveLength(1));
    expect(swapperDepositMocks.swapperOptions[0].wallet).toEqual({ signer: dangoSigner });
  });
});
