import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useAuthState } from "../../../store/src/hooks/useAuthState";
import { useSigninState } from "../../../store/src/hooks/useSigninState";
import { useSignupState } from "../../../store/src/hooks/useSignupState";
import { createQueryClientWrapper } from "./utils/query-client";

const hookMocks = vi.hoisted(() => ({
  createKeyHash: vi.fn((key: unknown) => `hash:${String(key)}`),
  createSessionKey: vi.fn(),
  registerUser: vi.fn(),
  setSession: vi.fn(),
  useChainId: vi.fn(),
  useConfig: vi.fn(),
  useConnectors: vi.fn(),
  usePublicClient: vi.fn(),
}));

vi.mock("@left-curve/sdk", async (importOriginal) => {
  const actual = await importOriginal<object>();

  return {
    ...actual,
    createKeyHash: hookMocks.createKeyHash,
  };
});

vi.mock("@left-curve/sdk/actions", async (importOriginal) => {
  const actual = await importOriginal<object>();

  return {
    ...actual,
    registerUser: hookMocks.registerUser,
  };
});

vi.mock("../../../store/src/hooks/useSessionKey.js", () => ({
  useSessionKey: () => ({
    createSessionKey: hookMocks.createSessionKey,
    setSession: hookMocks.setSession,
  }),
}));

vi.mock("../../../store/src/hooks/useChainId.js", () => ({
  useChainId: hookMocks.useChainId,
}));

vi.mock("../../../store/src/hooks/useConfig.js", () => ({
  useConfig: hookMocks.useConfig,
}));

vi.mock("../../../store/src/hooks/useConnectors.js", () => ({
  useConnectors: hookMocks.useConnectors,
}));

vi.mock("../../../store/src/hooks/usePublicClient.js", () => ({
  usePublicClient: hookMocks.usePublicClient,
}));

type TestConnector = {
  connect: ReturnType<typeof vi.fn>;
  createNewKey: ReturnType<typeof vi.fn>;
  getKeyHash: ReturnType<typeof vi.fn>;
  getProvider: ReturnType<typeof vi.fn>;
  id: string;
  signArbitrary: ReturnType<typeof vi.fn>;
};

function createConnector(overrides: Partial<TestConnector> = {}): TestConnector {
  const provider = {
    request: vi.fn().mockResolvedValue(["0xABCDEF0000000000000000000000000000000001"]),
  };

  return {
    connect: vi.fn().mockResolvedValue(undefined),
    createNewKey: vi.fn().mockResolvedValue({
      key: { secp256r1: "passkey-public-key" },
      keyHash: "passkey-key-hash",
    }),
    getKeyHash: vi.fn().mockResolvedValue("wallet-key-hash"),
    getProvider: vi.fn().mockResolvedValue(provider),
    id: "wallet",
    signArbitrary: vi.fn().mockResolvedValue({
      credential: {
        standard: {
          keyHash: "signed-key-hash",
          signature: "signature",
        },
      },
    }),
    ...overrides,
  };
}

describe("auth and signin hooks", () => {
  const publicClient = {
    forgotUsername: vi.fn(),
  };
  const signingSession = {
    keyHash: "session-key-hash",
    sessionInfo: {
      chainId: "dango-dev-1",
      expireAt: "1700003600",
      sessionKey: "session-public-key",
    },
  };

  beforeEach(() => {
    hookMocks.useChainId.mockReturnValue("dango-dev-1");
    hookMocks.useConfig.mockReturnValue({
      chain: {
        id: "dango-dev-1",
      },
    });
    hookMocks.usePublicClient.mockReturnValue(publicClient);
    hookMocks.createSessionKey.mockResolvedValue(signingSession);
    publicClient.forgotUsername.mockResolvedValue([{ index: 7, name: "alice" }]);
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  it("signs in with an existing wallet key hash and connects the selected account", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);

    const { result } = renderHook(
      () =>
        useSigninState({
          expiration: 3_600_000,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.connect.mutateAsync("wallet");
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(result.current.screen).toBe("usernames");
    expect(result.current.users).toEqual([{ index: 7, name: "alice" }]);

    await act(async () => {
      await result.current.login.mutateAsync(7);
    });

    expect(hookMocks.setSession).not.toHaveBeenCalled();
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 7,
    });
  });

  it("signs in with backend user index zero as a selectable account", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([{ index: 0, name: "genesis" }]);

    const { result } = renderHook(
      () =>
        useSigninState({
          expiration: 3_600_000,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.connect.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("usernames");
    expect(result.current.users).toEqual([{ index: 0, name: "genesis" }]);

    await act(async () => {
      await result.current.login.mutateAsync(0);
    });

    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 0,
    });
  });

  it("signs in with a temporary session key and stores it before connector login", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);

    const { result } = renderHook(
      () =>
        useSigninState({
          expiration: 3_600_000,
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.connect.mutateAsync("wallet");
    });

    expect(hookMocks.createSessionKey).toHaveBeenCalledWith(
      {
        connector,
        expireAt: 1_700_003_600_000,
      },
      { setSession: false },
    );
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "session-key-hash" });

    await act(async () => {
      await result.current.login.mutateAsync(7);
    });

    expect(hookMocks.setSession).toHaveBeenCalledWith(signingSession);
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "session-key-hash",
      userIndex: 7,
    });
  });

  it("signs in with a temporary session key for backend user index zero", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([{ index: 0, name: "genesis" }]);
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);

    const { result } = renderHook(
      () =>
        useSigninState({
          expiration: 3_600_000,
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.connect.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("usernames");
    expect(result.current.users).toEqual([{ index: 0, name: "genesis" }]);

    await act(async () => {
      await result.current.login.mutateAsync(0);
    });

    expect(hookMocks.setSession).toHaveBeenCalledWith(signingSession);
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "session-key-hash",
      userIndex: 0,
    });
  });

  it("does not look up usernames when the selected signin connector is missing", async () => {
    hookMocks.useConnectors.mockReturnValue([]);
    const connectError = vi.fn();
    const connectSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSigninState({
          connect: {
            error: connectError,
            success: connectSuccess,
          },
          expiration: 3_600_000,
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.connect.mutateAsync("wallet")).rejects.toThrow(
        "error: missing connector",
      );
    });

    expect(hookMocks.createSessionKey).not.toHaveBeenCalled();
    expect(publicClient.forgotUsername).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("options");
    expect(result.current.users).toEqual([]);
    expect(connectSuccess).not.toHaveBeenCalled();
    expect(connectError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((connectError.mock.calls[0]?.[0] as Error).message).toBe("error: missing connector");
  });

  it("keeps signin on the options screen when username lookup fails", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockRejectedValueOnce(new Error("indexer unavailable"));
    const connectError = vi.fn();
    const connectSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSigninState({
          connect: {
            error: connectError,
            success: connectSuccess,
          },
          expiration: 3_600_000,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.connect.mutateAsync("wallet")).rejects.toThrow(
        "indexer unavailable",
      );
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(result.current.screen).toBe("options");
    expect(result.current.users).toEqual([]);
    expect(connectSuccess).not.toHaveBeenCalled();
    expect(connectError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((connectError.mock.calls[0]?.[0] as Error).message).toBe("indexer unavailable");
  });

  it("does not log in when the signin connector disappears after username lookup", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    const loginError = vi.fn();
    const loginSuccess = vi.fn();

    const { rerender, result } = renderHook(
      () =>
        useSigninState({
          expiration: 3_600_000,
          login: {
            error: loginError,
            success: loginSuccess,
          },
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.connect.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("usernames");
    expect(result.current.users).toEqual([{ index: 7, name: "alice" }]);

    hookMocks.useConnectors.mockReturnValue([]);
    rerender();

    await act(async () => {
      await expect(result.current.login.mutateAsync(7)).rejects.toThrow("error: missing connector");
    });

    expect(hookMocks.setSession).not.toHaveBeenCalled();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(loginSuccess).not.toHaveBeenCalled();
    expect(loginError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((loginError.mock.calls[0]?.[0] as Error).message).toBe("error: missing connector");
  });

  it("authenticates an existing wallet account and selects it with the backend key hash", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    const provider = await connector.getProvider.mock.results[0].value;
    expect(provider.request).toHaveBeenCalledWith({ method: "eth_requestAccounts" });
    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(result.current.screen).toBe("account-picker");
    expect(result.current.identifier).toBe("0xABCDEF0000000000000000000000000000000001");

    await act(async () => {
      await result.current.selectAccount.mutateAsync(7);
    });

    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 7,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("authenticates and selects backend user index zero from the account picker", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([{ index: 0, name: "genesis" }]);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("account-picker");
    expect(result.current.users).toEqual([{ index: 0, name: "genesis" }]);

    await act(async () => {
      await result.current.selectAccount.mutateAsync(0);
    });

    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 0,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("keeps auth on the options screen when existing-account lookup fails", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockRejectedValueOnce(new Error("lookup failed"));
    const onError = vi.fn();
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onError,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.authenticate.mutateAsync("wallet")).rejects.toThrow(
        "lookup failed",
      );
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(result.current.screen).toBe("options");
    expect(result.current.users).toEqual([]);
    expect(result.current.identifier).toBeUndefined();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(onSuccess).not.toHaveBeenCalled();
    expect(onError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((onError.mock.calls[0]?.[0] as Error).message).toBe("lookup failed");
  });

  it("registers a new wallet account with key, seed, signature, and optional referrer", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ index: 42, name: "new-account" }]);
    vi.spyOn(Math, "random").mockReturnValue(0.25);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          referrer: 99,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    const key = { ethereum: "0xabcdef0000000000000000000000000000000001" };
    const keyHash = "hash:0xabcdef0000000000000000000000000000000001";

    expect(result.current.screen).toBe("create-account");
    expect(result.current.identifier).toBe("0xABCDEF0000000000000000000000000000000001");
    expect(hookMocks.createKeyHash).toHaveBeenCalledWith(key.ethereum);

    await act(async () => {
      await result.current.createAccount.mutateAsync();
    });

    expect(connector.signArbitrary).toHaveBeenCalledWith({
      primaryType: "Message",
      message: {
        chainId: "dango-dev-1",
        key,
        keyHash,
        referrer: 99,
        seed: 1_073_741_824,
      },
      types: {
        Message: [
          { name: "chain_id", type: "string" },
          { name: "key", type: "string" },
          { name: "key_hash", type: "string" },
          { name: "referrer", type: "uint32" },
          { name: "seed", type: "uint32" },
        ],
      },
    });
    expect(hookMocks.registerUser).toHaveBeenCalledWith(publicClient, {
      key,
      keyHash,
      referrer: 99,
      seed: 1_073_741_824,
      signature: "signature",
    });
    expect(publicClient.forgotUsername).toHaveBeenLastCalledWith({ keyHash: "wallet-key-hash" });
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 42,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("preserves backend referrer index zero in auth registration payloads", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ index: 42, name: "new-account" }]);
    vi.spyOn(Math, "random").mockReturnValue(0.125);

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          referrer: 0,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    const key = { ethereum: "0xabcdef0000000000000000000000000000000001" };
    const keyHash = "hash:0xabcdef0000000000000000000000000000000001";

    await act(async () => {
      await result.current.createAccount.mutateAsync();
    });

    expect(connector.signArbitrary).toHaveBeenCalledWith(
      expect.objectContaining({
        message: expect.objectContaining({
          referrer: 0,
          seed: 536_870_912,
        }),
        types: expect.objectContaining({
          Message: expect.arrayContaining([{ name: "referrer", type: "uint32" }]),
        }),
      }),
    );
    expect(hookMocks.registerUser).toHaveBeenCalledWith(
      publicClient,
      expect.objectContaining({
        key,
        keyHash,
        referrer: 0,
        seed: 536_870_912,
      }),
    );
  });

  it("registers a new wallet account and connects with a fresh session key when sessions are enabled", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    const preRegistrationSession = {
      keyHash: "pre-registration-session-key-hash",
      sessionInfo: {
        chainId: "dango-dev-1",
        expireAt: "1700003600",
        sessionKey: "pre-registration-session-public-key",
      },
    };
    const postRegistrationSession = {
      keyHash: "post-registration-session-key-hash",
      sessionInfo: {
        chainId: "dango-dev-1",
        expireAt: "1700003600",
        sessionKey: "post-registration-session-public-key",
      },
    };
    hookMocks.createSessionKey
      .mockResolvedValueOnce(preRegistrationSession)
      .mockResolvedValueOnce(postRegistrationSession);
    publicClient.forgotUsername
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([{ index: 84, name: "session-account" }]);
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    vi.spyOn(Math, "random").mockReturnValue(0.75);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    const key = { ethereum: "0xabcdef0000000000000000000000000000000001" };
    const keyHash = "hash:0xabcdef0000000000000000000000000000000001";

    expect(hookMocks.createSessionKey).toHaveBeenNthCalledWith(
      1,
      {
        connector,
        expireAt: 1_700_003_600_000,
      },
      { setSession: false },
    );
    expect(publicClient.forgotUsername).toHaveBeenNthCalledWith(1, {
      keyHash: "pre-registration-session-key-hash",
    });
    expect(result.current.screen).toBe("create-account");
    expect(result.current.identifier).toBe("0xABCDEF0000000000000000000000000000000001");

    await act(async () => {
      await result.current.createAccount.mutateAsync();
    });

    expect(connector.signArbitrary).toHaveBeenCalledWith({
      primaryType: "Message",
      message: {
        chainId: "dango-dev-1",
        key,
        keyHash,
        seed: 3_221_225_472,
      },
      types: {
        Message: [
          { name: "chain_id", type: "string" },
          { name: "key", type: "string" },
          { name: "key_hash", type: "string" },
          { name: "seed", type: "uint32" },
        ],
      },
    });
    expect(hookMocks.registerUser).toHaveBeenCalledWith(publicClient, {
      key,
      keyHash,
      referrer: undefined,
      seed: 3_221_225_472,
      signature: "signature",
    });
    expect(hookMocks.createSessionKey).toHaveBeenNthCalledWith(
      2,
      {
        connector,
        expireAt: 1_700_003_600_000,
      },
      { setSession: false },
    );
    expect(publicClient.forgotUsername).toHaveBeenNthCalledWith(2, {
      keyHash: "post-registration-session-key-hash",
    });
    expect(hookMocks.setSession).toHaveBeenCalledWith(postRegistrationSession);
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "post-registration-session-key-hash",
      userIndex: 84,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("rejects malformed registration signatures before calling backend registration", async () => {
    const connector = createConnector({
      signArbitrary: vi.fn().mockResolvedValue({
        credential: {
          passkey: {
            signature: "wrong-kind",
          },
        },
      }),
    });
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValueOnce([]);
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const onError = vi.fn();
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onError,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("create-account");

    await act(async () => {
      await expect(result.current.createAccount.mutateAsync()).rejects.toThrow(
        "Signed with wrong credential",
      );
    });

    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(onSuccess).not.toHaveBeenCalled();
    expect(onError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((onError.mock.calls[0]?.[0] as Error).message).toBe("Signed with wrong credential");
  });

  it("keeps new auth accounts unconnected when backend registration fails", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValueOnce([]);
    hookMocks.registerUser.mockRejectedValueOnce(new Error("registration failed"));
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const onError = vi.fn();
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onError,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("create-account");

    await act(async () => {
      await expect(result.current.createAccount.mutateAsync()).rejects.toThrow(
        "registration failed",
      );
    });

    expect(connector.signArbitrary).toHaveBeenCalledOnce();
    expect(hookMocks.registerUser).toHaveBeenCalledOnce();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(hookMocks.setSession).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("create-account");
    expect(onSuccess).not.toHaveBeenCalled();
    expect(onError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((onError.mock.calls[0]?.[0] as Error).message).toBe("registration failed");
  });

  it("routes passkey authentication through choice, creation, and missing-account states", async () => {
    const connector = createConnector({ id: "passkey" });
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([]);

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("passkey");
    });

    expect(result.current.screen).toBe("passkey-choice");

    await act(async () => {
      await result.current.passkeyCreate.mutateAsync();
    });

    expect(connector.createNewKey).toHaveBeenCalledWith(expect.any(String));
    expect(result.current.screen).toBe("create-account");
    expect(result.current.identifier).toBe("Passkey");

    await act(async () => {
      await result.current.authenticate.mutateAsync("passkey");
    });
    await act(async () => {
      await result.current.passkeyLogin.mutateAsync();
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(result.current.screen).toBe("passkey-error");
  });

  it("logs existing passkey accounts in through a stored session key", async () => {
    const connector = createConnector({ id: "passkey" });
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([{ index: 31, name: "passkey-account" }]);
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: true,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("passkey");
    });

    expect(result.current.screen).toBe("passkey-choice");

    await act(async () => {
      await result.current.passkeyLogin.mutateAsync();
    });

    expect(hookMocks.createSessionKey).toHaveBeenCalledWith(
      {
        connector,
        expireAt: 1_700_003_600_000,
      },
      { setSession: false },
    );
    expect(connector.getKeyHash).not.toHaveBeenCalled();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "session-key-hash" });
    expect(result.current.screen).toBe("account-picker");
    expect(result.current.users).toEqual([{ index: 31, name: "passkey-account" }]);

    await act(async () => {
      await result.current.selectAccount.mutateAsync(31);
    });

    expect(hookMocks.setSession).toHaveBeenCalledWith(signingSession);
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "session-key-hash",
      userIndex: 31,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("registers a new passkey account using the existing on-chain key material", async () => {
    const connector = createConnector({ id: "passkey" });
    hookMocks.useConnectors.mockReturnValue([connector]);
    const existingKey = { secp256r1: "existing-passkey-public-key" };
    publicClient.forgotUsername
      .mockResolvedValueOnce([
        {
          index: 7,
          keys: {
            "wallet-key-hash": existingKey,
          },
          name: "alice",
        },
      ])
      .mockResolvedValueOnce([
        {
          index: 7,
          name: "alice",
        },
        {
          index: 55,
          name: "new-passkey-account",
        },
      ]);
    vi.spyOn(Math, "random").mockReturnValue(0.125);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("passkey");
    });
    await act(async () => {
      await result.current.passkeyLogin.mutateAsync();
    });

    expect(result.current.screen).toBe("account-picker");

    await act(async () => {
      await result.current.createNewWithExistingKey.mutateAsync();
    });

    expect(result.current.screen).toBe("create-account");
    expect(result.current.identifier).toBe("New account");

    await act(async () => {
      await result.current.createAccount.mutateAsync();
    });

    expect(connector.createNewKey).not.toHaveBeenCalled();
    expect(connector.signArbitrary).toHaveBeenCalledWith({
      primaryType: "Message",
      message: {
        chainId: "dango-dev-1",
        key: existingKey,
        keyHash: "wallet-key-hash",
        seed: 536_870_912,
      },
      types: {
        Message: [
          { name: "chain_id", type: "string" },
          { name: "key", type: "string" },
          { name: "key_hash", type: "string" },
          { name: "seed", type: "uint32" },
        ],
      },
    });
    expect(hookMocks.registerUser).toHaveBeenCalledWith(publicClient, {
      key: existingKey,
      keyHash: "wallet-key-hash",
      referrer: undefined,
      seed: 536_870_912,
      signature: "signature",
    });
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 55,
    });
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("rejects passkey account creation when the selected key is not on the user", async () => {
    const connector = createConnector({ id: "passkey" });
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValueOnce([
      {
        index: 7,
        keys: {
          "other-key-hash": { secp256r1: "other-passkey-public-key" },
        },
        name: "alice",
      },
    ]);
    const onError = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onError,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticate.mutateAsync("passkey");
    });
    await act(async () => {
      await result.current.passkeyLogin.mutateAsync();
    });

    expect(result.current.screen).toBe("account-picker");

    await act(async () => {
      await expect(result.current.createNewWithExistingKey.mutateAsync()).rejects.toThrow(
        "error: key not found on chain",
      );
    });

    expect(result.current.screen).toBe("account-picker");
    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(onError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((onError.mock.calls[0]?.[0] as Error).message).toBe("error: key not found on chain");
  });

  it("authenticates debug users directly without backend username lookup or registration", async () => {
    const debugConnector = createConnector({ id: "debug" });
    hookMocks.useConnectors.mockReturnValue([debugConnector]);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticateDebug.mutateAsync(77);
    });

    expect(debugConnector.connect).toHaveBeenCalledWith({
      challenge: "debug",
      chainId: "dango-dev-1",
      userIndex: 77,
    });
    expect(publicClient.forgotUsername).not.toHaveBeenCalled();
    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("authenticates backend debug user index zero without username lookup", async () => {
    const debugConnector = createConnector({ id: "debug" });
    hookMocks.useConnectors.mockReturnValue([debugConnector]);
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.authenticateDebug.mutateAsync(0);
    });

    expect(debugConnector.connect).toHaveBeenCalledWith({
      challenge: "debug",
      chainId: "dango-dev-1",
      userIndex: 0,
    });
    expect(publicClient.forgotUsername).not.toHaveBeenCalled();
    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(onSuccess).toHaveBeenCalledOnce();
  });

  it("fails debug authentication before side effects when the debug connector is unavailable", async () => {
    hookMocks.useConnectors.mockReturnValue([]);
    const onError = vi.fn();
    const onSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useAuthState({
          expiration: 3_600_000,
          onError,
          onSuccess,
          session: false,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.authenticateDebug.mutateAsync(77)).rejects.toThrow(
        "debug connector not registered",
      );
    });

    expect(publicClient.forgotUsername).not.toHaveBeenCalled();
    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(onSuccess).not.toHaveBeenCalled();
    expect(onError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((onError.mock.calls[0]?.[0] as Error).message).toBe("debug connector not registered");
  });

  it("registers through signup with a wallet key and moves to login", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const registerSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          register: {
            onSuccess: registerSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.register.mutateAsync("wallet");
    });

    const key = { ethereum: "0xabcdef0000000000000000000000000000000001" };
    const keyHash = "hash:0xabcdef0000000000000000000000000000000001";
    const provider = await connector.getProvider.mock.results[0].value;

    expect(provider.request).toHaveBeenCalledWith({ method: "eth_requestAccounts" });
    expect(hookMocks.createKeyHash).toHaveBeenCalledWith(key.ethereum);
    expect(connector.signArbitrary).toHaveBeenCalledWith({
      primaryType: "Message",
      message: {
        chainId: "dango-dev-1",
        key,
        keyHash,
        seed: 2_147_483_648,
      },
      types: {
        Message: [
          { name: "chain_id", type: "string" },
          { name: "key", type: "string" },
          { name: "key_hash", type: "string" },
          { name: "seed", type: "uint32" },
        ],
      },
    });
    expect(hookMocks.registerUser).toHaveBeenCalledWith(publicClient, {
      key,
      keyHash,
      seed: 2_147_483_648,
      signature: "signature",
    });
    expect(result.current.screen).toBe("login");
    expect(registerSuccess).toHaveBeenCalledOnce();
  });

  it("registers through signup with a passkey-created key and backend registration payload", async () => {
    const connector = createConnector({ id: "passkey" });
    hookMocks.useConnectors.mockReturnValue([connector]);
    vi.spyOn(Math, "random").mockReturnValue(0.375);
    const registerSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          register: {
            onSuccess: registerSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.register.mutateAsync("passkey");
    });

    const key = { secp256r1: "passkey-public-key" };
    const keyHash = "passkey-key-hash";

    expect(connector.createNewKey).toHaveBeenCalledWith(expect.any(String));
    expect(connector.getProvider).not.toHaveBeenCalled();
    expect(hookMocks.createKeyHash).not.toHaveBeenCalled();
    expect(connector.signArbitrary).toHaveBeenCalledWith({
      primaryType: "Message",
      message: {
        chainId: "dango-dev-1",
        key,
        keyHash,
        seed: 1_610_612_736,
      },
      types: {
        Message: [
          { name: "chain_id", type: "string" },
          { name: "key", type: "string" },
          { name: "key_hash", type: "string" },
          { name: "seed", type: "uint32" },
        ],
      },
    });
    expect(hookMocks.registerUser).toHaveBeenCalledWith(publicClient, {
      key,
      keyHash,
      seed: 1_610_612_736,
      signature: "signature",
    });
    expect(result.current.screen).toBe("login");
    expect(registerSuccess).toHaveBeenCalledOnce();
  });

  it("does not sign or call backend registration when the signup connector is missing", async () => {
    hookMocks.useConnectors.mockReturnValue([]);
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const registerError = vi.fn();
    const registerSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          register: {
            onError: registerError,
            onSuccess: registerSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.register.mutateAsync("wallet")).rejects.toThrow(
        "error: missing connector",
      );
    });

    expect(hookMocks.createKeyHash).not.toHaveBeenCalled();
    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(hookMocks.createSessionKey).not.toHaveBeenCalled();
    expect(publicClient.forgotUsername).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("options");
    expect(registerSuccess).not.toHaveBeenCalled();
    expect(registerError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((registerError.mock.calls[0]?.[0] as Error).message).toBe("error: missing connector");
  });

  it("rejects malformed signup signatures before calling backend registration", async () => {
    const connector = createConnector({
      signArbitrary: vi.fn().mockResolvedValue({
        credential: {
          passkey: {
            signature: "wrong-kind",
          },
        },
      }),
    });
    hookMocks.useConnectors.mockReturnValue([connector]);
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const registerError = vi.fn();
    const registerSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          register: {
            onError: registerError,
            onSuccess: registerSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.register.mutateAsync("wallet")).rejects.toThrow(
        "Signed with wrong credential",
      );
    });

    expect(hookMocks.registerUser).not.toHaveBeenCalled();
    expect(connector.connect).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("options");
    expect(registerSuccess).not.toHaveBeenCalled();
    expect(registerError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((registerError.mock.calls[0]?.[0] as Error).message).toBe(
      "Signed with wrong credential",
    );
  });

  it("keeps signup on options when backend registration fails", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    hookMocks.registerUser.mockRejectedValueOnce(new Error("registration failed"));
    vi.spyOn(Math, "random").mockReturnValue(0.5);
    const registerError = vi.fn();
    const registerSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          register: {
            onError: registerError,
            onSuccess: registerSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await expect(result.current.register.mutateAsync("wallet")).rejects.toThrow(
        "registration failed",
      );
    });

    expect(connector.signArbitrary).toHaveBeenCalledOnce();
    expect(hookMocks.registerUser).toHaveBeenCalledOnce();
    expect(result.current.screen).toBe("options");
    expect(connector.connect).not.toHaveBeenCalled();
    expect(registerSuccess).not.toHaveBeenCalled();
    expect(registerError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((registerError.mock.calls[0]?.[0] as Error).message).toBe("registration failed");
  });

  it("logs in after signup with the controller key hash when session keys are disabled", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    const loginSuccess = vi.fn();
    publicClient.forgotUsername.mockResolvedValue([{ index: 13, name: "new-account" }]);

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          login: {
            onSuccess: loginSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.register.mutateAsync("wallet");
    });

    await act(async () => {
      await result.current.login.mutateAsync({ useSessionKey: false });
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "wallet-key-hash",
      userIndex: 13,
    });
    expect(hookMocks.setSession).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("login");
    expect(loginSuccess).toHaveBeenCalledOnce();
  });

  it("keeps signup login unconnected when username lookup fails", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockRejectedValueOnce(new Error("lookup failed"));
    const loginError = vi.fn();
    const loginSuccess = vi.fn();

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 3_600_000,
          login: {
            onError: loginError,
            onSuccess: loginSuccess,
          },
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.register.mutateAsync("wallet");
    });

    expect(result.current.screen).toBe("login");

    await act(async () => {
      await expect(result.current.login.mutateAsync({ useSessionKey: false })).rejects.toThrow(
        "lookup failed",
      );
    });

    expect(connector.getKeyHash).toHaveBeenCalledOnce();
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "wallet-key-hash" });
    expect(connector.connect).not.toHaveBeenCalled();
    expect(hookMocks.setSession).not.toHaveBeenCalled();
    expect(result.current.screen).toBe("login");
    expect(loginSuccess).not.toHaveBeenCalled();
    expect(loginError.mock.calls[0]?.[0]).toEqual(expect.any(Error));
    expect((loginError.mock.calls[0]?.[0] as Error).message).toBe("lookup failed");
  });

  it("logs in after signup with a stored session key when session keys are enabled", async () => {
    const connector = createConnector();
    hookMocks.useConnectors.mockReturnValue([connector]);
    publicClient.forgotUsername.mockResolvedValue([{ index: 21, name: "session-account" }]);
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);

    const { result } = renderHook(
      () =>
        useSignupState({
          expiration: 7_200_000,
        }),
      { wrapper: createQueryClientWrapper() },
    );

    await act(async () => {
      await result.current.register.mutateAsync("wallet");
    });

    await act(async () => {
      await result.current.login.mutateAsync({ useSessionKey: true });
    });

    expect(hookMocks.createSessionKey).toHaveBeenCalledWith(
      {
        connector,
        expireAt: 1_700_007_200_000,
      },
      { setSession: false },
    );
    expect(publicClient.forgotUsername).toHaveBeenCalledWith({ keyHash: "session-key-hash" });
    expect(hookMocks.setSession).toHaveBeenCalledWith(signingSession);
    expect(connector.connect).toHaveBeenCalledWith({
      chainId: "dango-dev-1",
      keyHash: "session-key-hash",
      userIndex: 21,
    });
    expect(result.current.screen).toBe("deposit");
  });
});
