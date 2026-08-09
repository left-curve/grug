import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { decodeBase64, decodeUtf8 } from "@left-curve/encoding";

import { privy } from "../../../store/src/connectors/privy";

const connectorMocks = vi.hoisted(() => ({
  createKeyHash: vi.fn((value: string | Uint8Array) => {
    if (typeof value === "string" && value === "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd") {
      return "0x70726976792d77616c6c65742d6b65790000000000000000000000000000";
    }

    return "0x66616c6c6261636b2d7072697679000000000000000000000000000000";
  }),
  createSignerClient: vi.fn(),
  embeddedWalletGetEthereumProvider: vi.fn(),
  embeddedWalletGetURL: vi.fn(),
  embeddedWalletOnMessage: vi.fn(),
  getAccountStatus: vi.fn(),
  getEntropyDetailsFromUser: vi.fn(),
  getUser: vi.fn(),
  getUserEmbeddedEthereumWallet: vi.fn(),
  initialize: vi.fn(),
  localStorage: vi.fn(),
  privyConstructor: vi.fn(),
  setMessagePoster: vi.fn(),
  toAccount: vi.fn(),
  userGet: vi.fn(),
}));

vi.mock("@privy-io/js-sdk-core", () => ({
  default: connectorMocks.privyConstructor.mockImplementation((options: unknown) => ({
    embeddedWallet: {
      getEthereumProvider: connectorMocks.embeddedWalletGetEthereumProvider,
      getURL: connectorMocks.embeddedWalletGetURL,
      onMessage: connectorMocks.embeddedWalletOnMessage,
    },
    initialize: connectorMocks.initialize,
    options,
    setMessagePoster: connectorMocks.setMessagePoster,
    user: {
      get: connectorMocks.userGet,
    },
  })),
  getEntropyDetailsFromUser: connectorMocks.getEntropyDetailsFromUser,
  getUserEmbeddedEthereumWallet: connectorMocks.getUserEmbeddedEthereumWallet,
  LocalStorage: connectorMocks.localStorage.mockImplementation(() => ({
    kind: "privy-local-storage",
  })),
}));

vi.mock("@left-curve/sdk", () => ({
  createKeyHash: connectorMocks.createKeyHash,
  createSignerClient: connectorMocks.createSignerClient,
  toAccount: connectorMocks.toAccount,
}));

vi.mock("@left-curve/sdk/actions", () => ({
  getUser: connectorMocks.getUser,
}));

const controllerAddress = "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
const controllerAddressLower = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const privyKeyHash = "0x70726976792d77616c6c65742d6b65790000000000000000000000000000";
const userAccountAddress = "0x70726976792d6163636f756e7400000000000000";
const secondaryAccountAddress = "0x7365636f6e642d70726976790000000000000000";
const signatureHex = "0x010203040506";

type ProviderRequest = {
  method: string;
  params?: unknown;
};

function createProvider() {
  const request = vi.fn(async ({ method }: ProviderRequest) => {
    switch (method) {
      case "eth_accounts":
      case "eth_requestAccounts":
        return [controllerAddress];
      case "eth_signTypedData_v4":
        return signatureHex;
      case "wallet_switchEthereumChain":
        return null;
      default:
        throw new Error(`Unhandled Privy provider method: ${method}`);
    }
  });

  return {
    on: vi.fn(),
    removeListener: vi.fn(),
    request,
  };
}

function createPrivyConnector({
  getUserIndex = () => 7,
}: {
  getUserIndex?: () => number | undefined;
} = {}) {
  const emitter = {
    emit: vi.fn(),
  };
  const chain = {
    id: "dango-dev-1",
    name: "Devnet",
  };
  const transport = {
    type: "http",
  };
  const posterTarget = {
    postMessage: vi.fn(),
    reload: vi.fn(),
  };
  let listenerCallback: ((data: unknown) => void) | undefined;
  const listener = vi.fn((callback: (data: unknown) => void) => {
    listenerCallback = callback;
  });
  const poster = vi.fn(() => posterTarget);
  const connector = privy({
    appId: "privy-app-id",
    clientId: "privy-client-id",
    icon: "/privy.svg",
    listener,
    poster,
  })({
    chain,
    emitter,
    getUserIndex,
    transport,
  } as never);

  return {
    chain,
    connector,
    emitter,
    listener,
    listenerCallback: () => listenerCallback,
    poster,
    posterTarget,
    transport,
  };
}

function decodeTypedData(encoded: string) {
  return JSON.parse(decodeUtf8(decodeBase64(encoded))) as Record<string, unknown>;
}

describe("privy connector", () => {
  beforeEach(() => {
    const provider = createProvider();
    connectorMocks.createSignerClient.mockReturnValue({
      getAccountStatus: connectorMocks.getAccountStatus,
      uid: "privy-client",
    });
    connectorMocks.embeddedWalletGetEthereumProvider.mockResolvedValue(provider);
    connectorMocks.embeddedWalletGetURL.mockReturnValue("https://privy.example/embedded-wallet");
    connectorMocks.getAccountStatus.mockResolvedValue("active");
    connectorMocks.getEntropyDetailsFromUser.mockReturnValue({
      entropyId: "entropy-id",
      entropyIdVerifier: "entropy-verifier",
    });
    connectorMocks.getUser.mockResolvedValue({
      accounts: {
        0: userAccountAddress,
        1: secondaryAccountAddress,
      },
      keys: {
        [privyKeyHash]: {
          ethereum: controllerAddressLower,
        },
      },
      name: "privy-user",
    });
    connectorMocks.getUserEmbeddedEthereumWallet.mockReturnValue({
      address: controllerAddress,
      id: "embedded-wallet",
    });
    connectorMocks.initialize.mockResolvedValue(undefined);
    connectorMocks.toAccount.mockImplementation(({ accountIndex, address, user }) => ({
      accountIndex,
      address,
      username: user.name,
    }));
    connectorMocks.userGet.mockResolvedValue({
      user: {
        id: "privy-user-id",
      },
    });
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("sets up the Privy iframe poster, message listener, and SDK storage", async () => {
    const { connector, listener, listenerCallback, poster, posterTarget } = createPrivyConnector();

    await connector.setup?.();
    listenerCallback()?.({ type: "privy-ready" });

    expect(connectorMocks.localStorage).toHaveBeenCalledOnce();
    expect(connectorMocks.privyConstructor).toHaveBeenCalledWith({
      appId: "privy-app-id",
      clientId: "privy-client-id",
      storage: {
        kind: "privy-local-storage",
      },
    });
    expect(poster).toHaveBeenCalledWith("https://privy.example/embedded-wallet");
    expect(connectorMocks.setMessagePoster).toHaveBeenCalledWith(posterTarget);
    expect(listener).toHaveBeenCalledOnce();
    expect(connectorMocks.embeddedWalletOnMessage).toHaveBeenCalledWith({ type: "privy-ready" });
    expect(connectorMocks.initialize).toHaveBeenCalledOnce();
  });

  it("recovers the embedded Ethereum provider from the Privy user session", async () => {
    const { connector } = createPrivyConnector();

    const recoveredProvider = await connector.getProvider();
    const provider = await connectorMocks.embeddedWalletGetEthereumProvider.mock.results[0].value;

    expect(recoveredProvider).toBe(provider);

    expect(connectorMocks.userGet).toHaveBeenCalledOnce();
    expect(connectorMocks.getUserEmbeddedEthereumWallet).toHaveBeenCalledWith({
      id: "privy-user-id",
    });
    expect(connectorMocks.getEntropyDetailsFromUser).toHaveBeenCalledWith({
      id: "privy-user-id",
    });
    expect(connectorMocks.embeddedWalletGetEthereumProvider).toHaveBeenCalledWith({
      entropyId: "entropy-id",
      entropyIdVerifier: "entropy-verifier",
      wallet: {
        address: controllerAddress,
        id: "embedded-wallet",
      },
    });

    connectorMocks.userGet.mockResolvedValueOnce({ user: null });
    const next = createPrivyConnector();

    await expect(next.connector.getProvider()).rejects.toThrow("we couldn't recover the session");
  });

  it("connects through the Privy provider, authorizes the backend key, and emits account status", async () => {
    const { chain, connector, emitter, transport } = createPrivyConnector();

    await connector.connect({
      chainId: chain.id,
      userIndex: 7,
    });

    const provider = await connectorMocks.embeddedWalletGetEthereumProvider.mock.results[0].value;

    expect(connectorMocks.createSignerClient).toHaveBeenCalledWith({
      chain,
      signer: connector,
      transport,
      type: "privy",
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "wallet_switchEthereumChain",
      params: [{ chainId: "0x1" }],
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_requestAccounts",
    });
    expect(connectorMocks.createKeyHash).toHaveBeenCalledWith(controllerAddressLower);
    expect(connectorMocks.getUser).toHaveBeenCalledWith(
      expect.objectContaining({
        getAccountStatus: connectorMocks.getAccountStatus,
        uid: "privy-client",
      }),
      {
        userIndexOrName: {
          index: 7,
        },
      },
    );
    expect(connectorMocks.getAccountStatus).toHaveBeenCalledWith({
      address: userAccountAddress,
    });
    expect(emitter.emit).toHaveBeenCalledWith("connect", {
      accounts: [
        {
          accountIndex: 0,
          address: userAccountAddress,
          username: "privy-user",
        },
        {
          accountIndex: 1,
          address: secondaryAccountAddress,
          username: "privy-user",
        },
      ],
      chainId: chain.id,
      keyHash: privyKeyHash,
      userIndex: 7,
      userStatus: "active",
      username: "privy-user",
    });
  });

  it("rejects unauthorized backend keys before account status or connection emission", async () => {
    connectorMocks.getUser.mockResolvedValueOnce({
      accounts: {
        0: userAccountAddress,
      },
      keys: {},
      name: "privy-user",
    });
    const { chain, connector, emitter } = createPrivyConnector();

    await expect(
      connector.connect({
        chainId: chain.id,
        keyHash: privyKeyHash,
        userIndex: 7,
      }),
    ).rejects.toThrow("Not authorized");

    const provider = await connectorMocks.embeddedWalletGetEthereumProvider.mock.results[0].value;
    expect(provider.request).toHaveBeenCalledWith({
      method: "wallet_switchEthereumChain",
      params: [{ chainId: "0x1" }],
    });
    expect(provider.request).not.toHaveBeenCalledWith({
      method: "eth_requestAccounts",
    });
    expect(connectorMocks.getAccountStatus).not.toHaveBeenCalled();
    expect(emitter.emit).not.toHaveBeenCalled();
  });

  it("disconnects Privy sessions through the connector event without provider or backend calls", async () => {
    const { connector, emitter } = createPrivyConnector();

    await connector.disconnect();

    expect(emitter.emit).toHaveBeenCalledWith("disconnect");
    expect(connectorMocks.userGet).not.toHaveBeenCalled();
    expect(connectorMocks.embeddedWalletGetEthereumProvider).not.toHaveBeenCalled();
    expect(connectorMocks.createSignerClient).not.toHaveBeenCalled();
    expect(connectorMocks.getUser).not.toHaveBeenCalled();
  });

  it("reads key hashes, accounts, and authorization through the embedded wallet provider", async () => {
    const { connector } = createPrivyConnector();

    await expect(connector.getKeyHash()).resolves.toBe(privyKeyHash);
    await expect(connector.getAccounts()).resolves.toEqual([
      {
        accountIndex: 0,
        address: userAccountAddress,
        username: "privy-user",
      },
      {
        accountIndex: 1,
        address: secondaryAccountAddress,
        username: "privy-user",
      },
    ]);
    await expect(connector.isAuthorized()).resolves.toBe(true);

    const provider = await connectorMocks.embeddedWalletGetEthereumProvider.mock.results[0].value;
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_accounts",
    });
  });

  it("requires a selected user index for account reads before querying the backend", async () => {
    const { connector } = createPrivyConnector({
      getUserIndex: () => undefined,
    });

    await expect(connector.getAccounts()).rejects.toThrow("privy: user index not found");
    expect(connectorMocks.getUser).not.toHaveBeenCalled();
  });

  it("signs arbitrary messages and transactions as EIP-712 credentials", async () => {
    const { connector } = createPrivyConnector();
    const arbitraryPayload = {
      message: {
        accountAddress: userAccountAddress,
        memo: "confirm Privy session",
      },
      primaryType: "Message" as const,
      types: {
        Message: [
          { name: "account_address", type: "address" },
          { name: "memo", type: "string" },
        ],
      },
    };
    const signDoc = {
      domain: {
        chainId: 1,
        name: "Dango",
        verifyingContract: "0x0000000000000000000000000000000000000000",
      },
      message: {
        data: {
          chainId: "dango-dev-1",
          nonce: 7,
          username: "privy-user",
        },
        gas_limit: 500000,
        messages: [
          {
            transfer: {
              to: userAccountAddress,
            },
          },
        ],
        sender: userAccountAddress,
      },
      primaryType: "Message" as const,
      types: {
        EIP712Domain: [
          { name: "name", type: "string" },
          { name: "chainId", type: "uint256" },
          { name: "verifyingContract", type: "address" },
        ],
        Message: [
          { name: "sender", type: "address" },
          { name: "data", type: "Metadata" },
          { name: "gas_limit", type: "uint32" },
          { name: "messages", type: "string[]" },
        ],
        Metadata: [
          { name: "username", type: "string" },
          { name: "chainId", type: "string" },
          { name: "nonce", type: "uint32" },
        ],
      },
    };

    const arbitrarySigned = await connector.signArbitrary(arbitraryPayload);
    const arbitraryEip712 = arbitrarySigned.credential.standard.signature.eip712;
    const arbitraryTypedData = decodeTypedData(arbitraryEip712.typed_data);

    expect(arbitrarySigned.signed).toEqual(arbitraryPayload);
    expect(arbitrarySigned.credential.standard.keyHash).toBe(privyKeyHash);
    expect(arbitraryEip712.sig).toBe("AQIDBAUG");
    expect(arbitraryTypedData).toMatchObject({
      domain: {
        chainId: 1,
        name: "DangoArbitraryMessage",
        verifyingContract: "0x0000000000000000000000000000000000000000",
      },
      message: {
        account_address: userAccountAddress,
        memo: "confirm Privy session",
      },
      primaryType: "Message",
    });

    const txSigned = await connector.signTx(signDoc);
    const txEip712 = txSigned.credential.standard.signature.eip712;

    // The connector binds each message as its canonical JSON string before
    // signing; the returned `signed` is still the original doc.
    const eip712SignData = {
      ...signDoc,
      message: {
        ...signDoc.message,
        messages: [`{"transfer":{"to":"${userAccountAddress}"}}`],
      },
    };

    expect(txSigned).toEqual({
      credential: {
        standard: {
          keyHash: privyKeyHash,
          signature: {
            eip712: {
              sig: "AQIDBAUG",
              typed_data: expect.any(String),
            },
          },
        },
      },
      signed: signDoc,
    });
    expect(decodeTypedData(txEip712.typed_data)).toEqual(eip712SignData);

    const provider = await connectorMocks.embeddedWalletGetEthereumProvider.mock.results[0].value;
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_signTypedData_v4",
      params: [controllerAddress, JSON.stringify(arbitraryTypedData)],
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_signTypedData_v4",
      params: [controllerAddress, JSON.stringify(eip712SignData)],
    });
  });
});
