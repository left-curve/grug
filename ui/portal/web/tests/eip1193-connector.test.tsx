import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { decodeBase64, decodeUtf8 } from "@left-curve/encoding";

import { eip1193 } from "../../../store/src/connectors/eip1193";

const connectorMocks = vi.hoisted(() => ({
  createKeyHash: vi.fn((value: string | Uint8Array) => {
    if (typeof value === "string" && value === "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd") {
      return "0x6574682d77616c6c65742d6b65790000000000000000000000000000000000";
    }

    return "0x66616c6c6261636b2d6b657900000000000000000000000000000000000000";
  }),
  createSignerClient: vi.fn(),
  getAccountStatus: vi.fn(),
  getUser: vi.fn(),
  toAccount: vi.fn(),
}));

vi.mock("@left-curve/sdk", () => ({
  createKeyHash: connectorMocks.createKeyHash,
  createSignerClient: connectorMocks.createSignerClient,
  toAccount: connectorMocks.toAccount,
}));

vi.mock("@left-curve/sdk/actions", () => ({
  getUser: connectorMocks.getUser,
}));

const walletAddress = "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD";
const walletAddressLower = "0xabcdefabcdefabcdefabcdefabcdefabcdefabcd";
const walletKeyHash = "0x6574682d77616c6c65742d6b65790000000000000000000000000000000000";
const userAccountAddress = "0x757365722d6163636f756e742d30000000000000";
const secondUserAccountAddress = "0x757365722d6163636f756e742d31000000000000";
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
        return [walletAddress];
      case "eth_signTypedData_v4":
        return signatureHex;
      case "wallet_switchEthereumChain":
        return null;
      default:
        throw new Error(`Unhandled wallet method: ${method}`);
    }
  });

  return {
    on: vi.fn(),
    removeListener: vi.fn(),
    request,
  };
}

function createEip1193Connector({
  getUserIndex = () => 7,
  provider = createProvider(),
}: {
  getUserIndex?: () => number | undefined;
  provider?: ReturnType<typeof createProvider>;
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
  const connector = eip1193({
    id: "metamask",
    name: "MetaMask",
    provider: () => provider,
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
    provider,
    transport,
  };
}

function decodeTypedData(encoded: string) {
  return JSON.parse(decodeUtf8(decodeBase64(encoded))) as Record<string, unknown>;
}

describe("eip1193 connector", () => {
  beforeEach(() => {
    connectorMocks.createSignerClient.mockReturnValue({
      getAccountStatus: connectorMocks.getAccountStatus,
    });
    connectorMocks.getAccountStatus.mockResolvedValue("active");
    connectorMocks.getUser.mockResolvedValue({
      accounts: {
        0: userAccountAddress,
        1: secondUserAccountAddress,
      },
      keys: {
        [walletKeyHash]: {
          ethereum: walletAddressLower,
        },
      },
      name: "wallet-user",
    });
    connectorMocks.toAccount.mockImplementation(({ accountIndex, address, user }) => ({
      accountIndex,
      address,
      username: user.name,
    }));
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("connects through the wallet, authorizes the backend key, and emits account status", async () => {
    const { chain, connector, emitter, provider, transport } = createEip1193Connector();

    await connector.connect({
      chainId: chain.id,
      userIndex: 7,
    });

    expect(connectorMocks.createSignerClient).toHaveBeenCalledWith({
      chain,
      signer: connector,
      transport,
      type: "eip1193",
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "wallet_switchEthereumChain",
      params: [{ chainId: "0x1" }],
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_requestAccounts",
    });
    expect(connectorMocks.createKeyHash).toHaveBeenCalledWith(walletAddressLower);
    expect(connectorMocks.getUser).toHaveBeenCalledWith(
      expect.objectContaining({
        getAccountStatus: connectorMocks.getAccountStatus,
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
          username: "wallet-user",
        },
        {
          accountIndex: 1,
          address: secondUserAccountAddress,
          username: "wallet-user",
        },
      ],
      chainId: chain.id,
      keyHash: walletKeyHash,
      userIndex: 7,
      userStatus: "active",
      username: "wallet-user",
    });
  });

  it("rejects unauthorized backend keys without emitting a connection", async () => {
    connectorMocks.getUser.mockResolvedValueOnce({
      accounts: {
        0: userAccountAddress,
      },
      keys: {},
      name: "wallet-user",
    });
    const { chain, connector, emitter, provider } = createEip1193Connector();

    await expect(
      connector.connect({
        chainId: chain.id,
        keyHash: walletKeyHash,
        userIndex: 7,
      }),
    ).rejects.toThrow("Not authorized");

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

  it("disconnects wallet sessions through the connector event without wallet or backend calls", async () => {
    const { connector, emitter, provider } = createEip1193Connector();

    await connector.disconnect();

    expect(emitter.emit).toHaveBeenCalledWith("disconnect");
    expect(provider.request).not.toHaveBeenCalled();
    expect(connectorMocks.createSignerClient).not.toHaveBeenCalled();
    expect(connectorMocks.getUser).not.toHaveBeenCalled();
  });

  it("creates and reads wallet key hashes from the lowercased controller address", async () => {
    const { connector, provider } = createEip1193Connector();

    await expect(connector.createNewKey()).resolves.toEqual({
      key: {
        ethereum: walletAddressLower,
      },
      keyHash: walletKeyHash,
    });
    await expect(connector.getKeyHash()).resolves.toBe(walletKeyHash);

    expect(provider.request).toHaveBeenCalledTimes(2);
    expect(provider.request).toHaveBeenNthCalledWith(1, {
      method: "eth_requestAccounts",
    });
    expect(provider.request).toHaveBeenNthCalledWith(2, {
      method: "eth_requestAccounts",
    });
    expect(connectorMocks.createKeyHash).toHaveBeenCalledWith(walletAddressLower);
  });

  it("reads backend accounts for the selected user index and checks wallet authorization", async () => {
    const { chain, connector, provider, transport } = createEip1193Connector();

    await expect(connector.getAccounts()).resolves.toEqual([
      {
        accountIndex: 0,
        address: userAccountAddress,
        username: "wallet-user",
      },
      {
        accountIndex: 1,
        address: secondUserAccountAddress,
        username: "wallet-user",
      },
    ]);

    expect(connectorMocks.createSignerClient).toHaveBeenCalledWith({
      chain,
      signer: connector,
      transport,
      type: "eip1193",
    });
    expect(connectorMocks.getUser).toHaveBeenCalledWith(
      expect.objectContaining({
        getAccountStatus: connectorMocks.getAccountStatus,
      }),
      {
        userIndexOrName: {
          index: 7,
        },
      },
    );
    expect(connectorMocks.toAccount).toHaveBeenNthCalledWith(1, {
      accountIndex: 0,
      address: userAccountAddress,
      user: expect.objectContaining({
        name: "wallet-user",
      }),
    });
    expect(connectorMocks.toAccount).toHaveBeenNthCalledWith(2, {
      accountIndex: 1,
      address: secondUserAccountAddress,
      user: expect.objectContaining({
        name: "wallet-user",
      }),
    });

    await expect(connector.isAuthorized()).resolves.toBe(true);

    expect(provider.request).toHaveBeenCalledWith({
      method: "wallet_switchEthereumChain",
      params: [{ chainId: "0x1" }],
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_accounts",
    });
    expect(connectorMocks.getUser).toHaveBeenCalledTimes(2);
  });

  it("signs arbitrary messages as composed EIP-712 typed data", async () => {
    const { connector, provider } = createEip1193Connector();

    const payload = {
      message: {
        accountAddress: userAccountAddress,
        memo: "confirm session",
      },
      primaryType: "Message" as const,
      types: {
        Message: [
          { name: "account_address", type: "address" },
          { name: "memo", type: "string" },
        ],
      },
    };

    const signed = await connector.signArbitrary(payload);
    const eip712 = signed.credential.standard.signature.eip712;
    const typedData = decodeTypedData(eip712.typed_data);

    expect(signed.signed).toEqual(payload);
    expect(eip712.sig).toBe("AQIDBAUG");
    expect(signed.credential.standard.keyHash).toBe(walletKeyHash);
    expect(typedData).toMatchObject({
      domain: {
        chainId: 1,
        name: "DangoArbitraryMessage",
        verifyingContract: "0x0000000000000000000000000000000000000000",
      },
      message: {
        account_address: userAccountAddress,
        memo: "confirm session",
      },
      primaryType: "Message",
    });
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_signTypedData_v4",
      params: [walletAddress, JSON.stringify(typedData)],
    });
  });

  it("signs transaction documents and rejects account reads before a user index is selected", async () => {
    const { connector, provider } = createEip1193Connector({
      getUserIndex: () => undefined,
    });
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
          username: "wallet-user",
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

    const signed = await connector.signTx(signDoc);

    // The connector binds each message as its canonical JSON string before
    // signing (EIP-712 can't express the `Message` enum as a struct). The
    // returned `signed` is still the original doc (message objects).
    const eip712SignData = {
      ...signDoc,
      message: {
        ...signDoc.message,
        messages: [`{"transfer":{"to":"${userAccountAddress}"}}`],
      },
    };

    expect(signed).toEqual({
      credential: {
        standard: {
          keyHash: walletKeyHash,
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
    expect(decodeTypedData(signed.credential.standard.signature.eip712.typed_data)).toEqual(
      eip712SignData,
    );
    expect(provider.request).toHaveBeenCalledWith({
      method: "eth_signTypedData_v4",
      params: [walletAddress, JSON.stringify(eip712SignData)],
    });

    await expect(connector.getAccounts()).rejects.toThrow("eip1193: user index not found");
  });
});
