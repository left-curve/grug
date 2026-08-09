import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useSearchBar } from "../../../store/src/hooks/useSearchBar";
import { createQueryClientWrapper, createTestQueryClient } from "./utils/query-client";

import type { AppletMetadata } from "../../../store/src/types/applets";

const storeHookMocks = vi.hoisted(() => ({
  useAppConfig: vi.fn(),
  usePublicClient: vi.fn(),
}));

vi.mock("../../../store/src/hooks/useAppConfig.js", () => ({
  useAppConfig: storeHookMocks.useAppConfig,
}));

vi.mock("../../../store/src/hooks/usePublicClient.js", () => ({
  usePublicClient: storeHookMocks.usePublicClient,
}));

const applets: Record<string, AppletMetadata> = {
  trade: {
    id: "trade",
    img: "/trade.png",
    title: "Trade",
    keywords: ["perps", "swap"],
    description: "Trade spot and perpetual markets",
    path: "/trade",
  },
  transfer: {
    id: "transfer",
    img: "/transfer.png",
    title: "Transfer",
    keywords: ["send"],
    description: "Send funds",
    path: "/transfer",
  },
};

function renderUseSearchBar() {
  const queryClient = createTestQueryClient();

  function Consumer() {
    const search = useSearchBar({
      applets,
      debounceMs: 0,
      favApplets: ["transfer"],
    });

    return (
      <>
        <input
          aria-label="Search"
          value={search.searchText}
          onChange={(event) => search.setSearchText(event.target.value)}
        />
        <pre data-testid="result">{JSON.stringify(search.searchResult)}</pre>
        <pre data-testid="all-not-fav">{JSON.stringify(search.allNotFavApplets)}</pre>
        <pre data-testid="query-state">
          {JSON.stringify({
            isError: search.isError,
            isFetching: search.isFetching,
            isSuccess: search.isSuccess,
          })}
        </pre>
      </>
    );
  }

  render(<Consumer />, { wrapper: createQueryClientWrapper(queryClient) });

  return queryClient;
}

function readJson(testId: string) {
  return JSON.parse(screen.getByTestId(testId).textContent ?? "null") as Record<string, unknown>;
}

describe("useSearchBar", () => {
  const publicClient = {
    chain: { id: "dev-9" },
    getContractInfo: vi.fn(),
    getAccountInfo: vi.fn(),
    getUser: vi.fn(),
    queryBlock: vi.fn(),
    searchTxs: vi.fn(),
  };

  beforeEach(() => {
    publicClient.chain = { id: "dev-9" };
    storeHookMocks.useAppConfig.mockReturnValue({
      data: {
        accountFactory: {
          codeHash: "account-code-hash",
        },
        addresses: {
          accountFactory: "0x6163636f756e74666163746f7279000000000000",
          perps: "0x7065727073000000000000000000000000000000",
          "0xignored": "0xignored",
        },
      },
    });
    storeHookMocks.usePublicClient.mockReturnValue(publicClient);
    publicClient.getUser.mockRejectedValue(new Error("not found"));
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  function jsonResponse(body: unknown, status = 200) {
    return new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });
  }

  it("starts with favorite applets, non-favorite applets, and known contracts", async () => {
    renderUseSearchBar();

    await waitFor(() => {
      const result = readJson("result");
      expect(result.applets).toEqual([applets.transfer]);
      expect(result.contracts).toEqual([
        { label: "accountFactory", address: "0x6163636f756e74666163746f7279000000000000" },
        { label: "perps", address: "0x7065727073000000000000000000000000000000" },
      ]);
    });

    expect(readJson("all-not-fav")).toEqual([applets.trade]);
  });

  it("filters applets immediately while backend username lookup is debounced", async () => {
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "trade" },
    });

    await waitFor(() => {
      const result = readJson("result");
      expect(result.applets).toEqual([applets.trade]);
    });
    await waitFor(() => {
      expect(publicClient.getUser).toHaveBeenCalledWith({
        userIndexOrName: { name: "trade" },
      });
    });
  });

  it("queries blocks and stores the result in React Query cache", async () => {
    const queryClient = renderUseSearchBar();
    const block = { height: 123, hash: "block-hash" };
    publicClient.queryBlock.mockResolvedValue(block);

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "123" },
    });

    await waitFor(() => {
      expect(readJson("result").block).toEqual(block);
    });
    expect(publicClient.queryBlock).toHaveBeenCalledWith({ height: 123 });
    expect(queryClient.getQueryData(["block", "123"])).toEqual(block);
  });

  it("preserves backend block height zero in search results and cache", async () => {
    const queryClient = renderUseSearchBar();
    const block = { height: 0, hash: "genesis-block" };
    publicClient.queryBlock.mockResolvedValue(block);

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "0" },
    });

    await waitFor(() => {
      expect(readJson("result").block).toEqual(block);
    });
    expect(publicClient.queryBlock).toHaveBeenCalledWith({ height: 0 });
    expect(queryClient.getQueryData(["block", "0"])).toEqual(block);
  });

  it.each([
    ["mainnet", "dango-1", "https://api-archive-mainnet.dango.zone"],
    ["testnet", "dango-testnet-1", "https://api-archive-testnet.dango.zone"],
  ])(
    "uses archive lookups for %s block and transaction searches",
    async (_, chainId, archiveUrl) => {
      publicClient.chain = { id: chainId };
      const queryClient = renderUseSearchBar();
      const hash = "a".repeat(64);
      const sender = "0x73656e6465720000000000000000000000000000";
      const fetchMock = vi.fn((input: URL) => {
        if (input.pathname === "/blocks/123") {
          return Promise.resolve(
            jsonResponse({
              block: {
                info: {
                  height: 123,
                  timestamp: "2026-06-08T12:00:00Z",
                  hash: "archive-block",
                },
                txs: [],
              },
              outcome: {
                app_hash: "archive-app-hash",
                cron_outcomes: [],
                tx_outcomes: [],
              },
            }),
          );
        }

        return Promise.resolve(
          jsonResponse([
            {
              blockHeight: 123,
              idx: 0,
              kind: "transaction",
              hash,
              sender,
              success: true,
              timestamp: "2026-06-08T12:00:00Z",
              tx: { sender, gas_limit: 1, msgs: [] },
              outcome: {
                transaction: { gas_limit: 1, gas_used: 1, result: { ok: null }, events: [] },
              },
            },
          ]),
        );
      });
      vi.stubGlobal("fetch", fetchMock);

      fireEvent.change(screen.getByLabelText("Search"), {
        target: { value: "123" },
      });

      await waitFor(() => {
        expect(readJson("result").block).toMatchObject({
          blockHeight: 123,
          hash: "archive-block",
        });
      });
      expect(publicClient.queryBlock).not.toHaveBeenCalled();
      expect(queryClient.getQueryData(["block", "123"])).toMatchObject({
        blockHeight: 123,
        hash: "archive-block",
      });

      fireEvent.change(screen.getByLabelText("Search"), {
        target: { value: hash },
      });

      await waitFor(() => {
        expect(readJson("result").txs).toMatchObject([{ hash, sender }]);
      });
      expect(publicClient.searchTxs).not.toHaveBeenCalled();
      expect(queryClient.getQueryData(["tx", hash])).toMatchObject({ hash, sender });
      expect(fetchMock.mock.calls.map(([url]) => (url as URL).toString())).toEqual([
        `${archiveUrl}/blocks/123`,
        `${archiveUrl}/transactions/${hash.toUpperCase()}`,
      ]);
    },
  );

  it("clears backend-derived search results when the search text is cleared", async () => {
    const block = { height: 123, hash: "block-hash" };
    publicClient.queryBlock.mockResolvedValue(block);
    renderUseSearchBar();

    const searchInput = screen.getByLabelText("Search");
    fireEvent.change(searchInput, {
      target: { value: "123" },
    });

    await waitFor(() => {
      expect(readJson("result").block).toEqual(block);
    });

    fireEvent.change(searchInput, {
      target: { value: "" },
    });

    await waitFor(() => {
      const result = readJson("result");
      expect(result).toMatchObject({
        applets: [applets.transfer],
        contracts: [
          { address: "0x6163636f756e74666163746f7279000000000000", label: "accountFactory" },
          { address: "0x7065727073000000000000000000000000000000", label: "perps" },
        ],
        txs: [],
      });
      expect(result.block).toBeUndefined();
      expect(result.account).toBeUndefined();
      expect(result.user).toBeUndefined();
    });
    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
  });

  it("queries transaction hashes and stores the first transaction in React Query cache", async () => {
    const queryClient = renderUseSearchBar();
    const hash = "a".repeat(64);
    const tx = { hash, sender: "0x73656e6465720000000000000000000000000000" };
    publicClient.searchTxs.mockResolvedValue({ nodes: [tx] });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: hash },
    });

    await waitFor(() => {
      expect(readJson("result").txs).toEqual([tx]);
    });
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash });
    expect(queryClient.getQueryData(["tx", hash])).toEqual(tx);
  });

  it("normalizes 0x-prefixed transaction hashes before searching", async () => {
    const queryClient = renderUseSearchBar();
    const hash = "a".repeat(64);
    const prefixedHash = `0x${hash}`;
    const tx = { hash, sender: "0x73656e6465720000000000000000000000000000" };
    publicClient.searchTxs.mockResolvedValue({ nodes: [tx] });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: prefixedHash },
    });

    await waitFor(() => {
      expect(readJson("result").txs).toEqual([tx]);
    });
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash });
    expect(queryClient.getQueryData(["tx", prefixedHash])).toEqual(tx);
  });

  it("keeps transaction search empty when the backend returns no matching hash", async () => {
    const queryClient = renderUseSearchBar();
    const hash = "c".repeat(64);
    publicClient.searchTxs.mockResolvedValue({ nodes: [] });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: hash },
    });

    await waitFor(() => {
      expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash });
    });
    await waitFor(() => {
      expect(readJson("result").txs).toEqual([]);
    });
    expect(queryClient.getQueryData(["tx", hash])).toBeUndefined();
  });

  it("preserves zero-valued backend transaction fields in search results and cache", async () => {
    const queryClient = renderUseSearchBar();
    const hash = "0".repeat(64);
    const tx = {
      blockHeight: 0,
      createdAt: "2026-06-08T12:00:00Z",
      errorMessage: "",
      gasUsed: 0,
      gasWanted: 0,
      hasSucceeded: true,
      hash,
      messages: [],
      nestedEvents: "[]",
      sender: "0x73656e6465720000000000000000000000000000",
      transactionIdx: 0,
      transactionType: "TX",
    };
    publicClient.searchTxs.mockResolvedValueOnce({ nodes: [tx] });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: hash },
    });

    await waitFor(() => {
      expect(readJson("result").txs).toEqual([tx]);
    });
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash });
    expect(queryClient.getQueryData(["tx", hash])).toEqual(tx);
  });

  it("resolves account addresses through contract info before loading account details", async () => {
    const address = "0x6163636f756e7400000000000000000000000000";
    const account = {
      address,
      params: { spot: { owner: "0x6f776e6572000000000000000000000000000000" } },
    };
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "account-code-hash",
      label: "account",
    });
    publicClient.getAccountInfo.mockResolvedValue(account);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: address },
    });

    await waitFor(() => {
      expect(readJson("result").account).toEqual(account);
    });
    expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getAccountInfo).toHaveBeenCalledWith({ address });
  });

  it("preserves backend account index zero when resolving account address searches", async () => {
    const address = "0x67656e657369732d6163636f756e740000000000";
    const account = {
      address,
      index: 0,
      owner: 0,
      params: { spot: { owner: "0x6f776e6572000000000000000000000000000000" } },
      username: undefined,
    };
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "account-code-hash",
      label: "account",
    });
    publicClient.getAccountInfo.mockResolvedValue(account);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: address },
    });

    await waitFor(() => {
      expect(readJson("result").account).toEqual(account);
    });
    expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getAccountInfo).toHaveBeenCalledWith({ address });
  });

  it("clears stale account details when the next account address has no backend account", async () => {
    const firstAddress = "0x6163636f756e7431000000000000000000000000";
    const secondAddress = "0x6163636f756e7432000000000000000000000000";
    const account = {
      address: firstAddress,
      params: { spot: { owner: "0x6f776e6572000000000000000000000000000000" } },
    };
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "account-code-hash",
      label: "account",
    });
    publicClient.getAccountInfo.mockResolvedValueOnce(account).mockResolvedValueOnce(null);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: firstAddress },
    });

    await waitFor(() => {
      expect(readJson("result").account).toEqual(account);
    });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: secondAddress },
    });

    await waitFor(() => {
      expect(readJson("result").account).toBeUndefined();
    });
    expect(publicClient.getContractInfo).toHaveBeenLastCalledWith({ address: secondAddress });
    expect(publicClient.getAccountInfo).toHaveBeenLastCalledWith({ address: secondAddress });
  });

  it("resolves non-account addresses as contracts without loading account details", async () => {
    const address = "0x636f6e7472616374000000000000000000000000";
    const contractInfo = {
      codeHash: "vault-code-hash",
      label: "vault",
    };
    publicClient.getContractInfo.mockResolvedValue(contractInfo);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: address },
    });

    await waitFor(() => {
      expect(readJson("result").contracts).toEqual([{ ...contractInfo, address }]);
    });
    expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getAccountInfo).not.toHaveBeenCalled();
  });

  it("queries usernames and exposes the matching backend user", async () => {
    const user = { index: 7, username: "alice" };
    publicClient.getUser.mockResolvedValue(user);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "alice" },
    });

    await waitFor(() => {
      expect(readJson("result").user).toEqual(user);
    });
    expect(publicClient.getUser).toHaveBeenCalledWith({
      userIndexOrName: { name: "alice" },
    });
  });

  it("preserves backend user index zero and empty account maps in search results", async () => {
    const user = {
      accounts: {},
      index: 0,
      keys: {},
      name: "genesis",
    };
    publicClient.getUser.mockResolvedValue(user);
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "genesis" },
    });

    await waitFor(() => {
      expect(readJson("result").user).toEqual(user);
    });
    expect(publicClient.getUser).toHaveBeenCalledWith({
      userIndexOrName: { name: "genesis" },
    });
  });

  it("clears a stale backend user when the next username lookup misses", async () => {
    const user = { index: 7, username: "alice" };
    publicClient.getUser.mockResolvedValueOnce(user).mockRejectedValueOnce(new Error("not found"));
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "alice" },
    });

    await waitFor(() => {
      expect(readJson("result").user).toEqual(user);
    });

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "missing" },
    });

    await waitFor(() => {
      expect(readJson("result").user).toBeUndefined();
    });
    expect(publicClient.getUser).toHaveBeenLastCalledWith({
      userIndexOrName: { name: "missing" },
    });
  });

  it("settles rejected backend address lookups without surfacing a query error", async () => {
    const address = "0x626164636f6e7472616374000000000000000000";
    publicClient.getContractInfo.mockRejectedValue(new Error("contract unavailable"));
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: address },
    });

    await waitFor(() => {
      expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    });
    await waitFor(() => {
      expect(readJson("query-state")).toMatchObject({
        isError: false,
        isFetching: false,
        isSuccess: true,
      });
    });
    expect(readJson("result").account).toBeUndefined();
    expect(publicClient.getAccountInfo).not.toHaveBeenCalled();
  });

  it("settles rejected backend block and transaction lookups without stale results", async () => {
    publicClient.queryBlock.mockRejectedValueOnce(new Error("block unavailable"));
    renderUseSearchBar();

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: "123" },
    });

    await waitFor(() => {
      expect(publicClient.queryBlock).toHaveBeenCalledWith({ height: 123 });
    });
    await waitFor(() => {
      expect(readJson("query-state")).toMatchObject({
        isError: false,
        isFetching: false,
        isSuccess: true,
      });
    });
    expect(readJson("result").block).toBeUndefined();

    const hash = "b".repeat(64);
    publicClient.searchTxs.mockRejectedValueOnce(new Error("transaction unavailable"));

    fireEvent.change(screen.getByLabelText("Search"), {
      target: { value: hash },
    });

    await waitFor(() => {
      expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash });
    });
    await waitFor(() => {
      expect(readJson("query-state")).toMatchObject({
        isError: false,
        isFetching: false,
        isSuccess: true,
      });
    });
    expect(readJson("result").txs).toEqual([]);
  });
});
