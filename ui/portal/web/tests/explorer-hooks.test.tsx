import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useExplorerAccount } from "../../../store/src/hooks/explorer/useExplorerAccount";
import { useExplorerBlock } from "../../../store/src/hooks/explorer/useExplorerBlock";
import { useExplorerContract } from "../../../store/src/hooks/explorer/useExplorerContract";
import { useExplorerTransaction } from "../../../store/src/hooks/explorer/useExplorerTransaction";
import { useExplorerTransactionsByAddress } from "../../../store/src/hooks/explorer/useExplorerTransactionsByAddress";
import { useExplorerTransactionsBySender } from "../../../store/src/hooks/explorer/useExplorerTransactionsBySender";
import { useExplorerUser } from "../../../store/src/hooks/explorer/useExplorerUser";
import { useExplorerUserTransactions } from "../../../store/src/hooks/explorer/useExplorerUserTransactions";
import { parseExplorerErrorMessage } from "../../../store/src/hooks/explorer/parseExplorerErrorMessage";
import { createQueryClientWrapper } from "./utils/query-client";

const storeHookMocks = vi.hoisted(() => ({
  useAppConfig: vi.fn(),
  usePrices: vi.fn(),
  usePublicClient: vi.fn(),
}));

vi.mock("../../../store/src/hooks/useAppConfig.js", () => ({
  useAppConfig: storeHookMocks.useAppConfig,
}));

vi.mock("../../../store/src/hooks/usePrices.js", () => ({
  usePrices: storeHookMocks.usePrices,
}));

vi.mock("../../../store/src/hooks/usePublicClient.js", () => ({
  usePublicClient: storeHookMocks.usePublicClient,
}));

const publicClient = {
  chain: { id: "dev-9" },
  getAccountInfo: vi.fn(),
  getAccountStatus: vi.fn(),
  getBalances: vi.fn(),
  getContractInfo: vi.fn(),
  getPerpsOrdersByUser: vi.fn(),
  getPerpsUserStateExtended: vi.fn(),
  getPerpsVaultState: vi.fn(),
  getUser: vi.fn(),
  getUserKeys: vi.fn(),
  queryBlock: vi.fn(),
  searchTxs: vi.fn(),
};

const calculateBalance = vi.fn(
  (balances: Record<string, string>) =>
    `usd:${Object.entries(balances)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([denom, amount]) => `${denom}:${amount}`)
      .join("|")}`,
);

describe("explorer hooks", () => {
  beforeEach(() => {
    publicClient.chain = { id: "dev-9" };
    storeHookMocks.usePublicClient.mockReturnValue(publicClient);
    storeHookMocks.usePrices.mockReturnValue({ calculateBalance });
    storeHookMocks.useAppConfig.mockReturnValue({
      data: {
        accountFactory: {
          codeHash: "account-code-hash",
        },
      },
    });
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

  it("marks invalid block searches without querying a specific backend height", async () => {
    const currentBlock = { blockHeight: 100, hash: "current-block" };
    publicClient.queryBlock.mockResolvedValue(currentBlock);

    const { result } = renderHook(() => useExplorerBlock("not-a-height"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
    expect(publicClient.queryBlock).toHaveBeenCalledWith();
    expect(result.current.data).toMatchObject({
      searchBlock: null,
      currentBlock,
      isFutureBlock: false,
      isInvalidBlock: true,
    });
    expect(Number.isNaN(result.current.data?.height)).toBe(true);
  });

  it("detects future block heights using the current backend block", async () => {
    const currentBlock = { blockHeight: 100, hash: "current-block" };
    const searchedBlock = { blockHeight: 125, hash: "future-block" };
    publicClient.queryBlock.mockImplementation((parameters?: { height?: number }) =>
      Promise.resolve(parameters?.height ? searchedBlock : currentBlock),
    );

    const { result } = renderHook(() => useExplorerBlock("125"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.isFutureBlock).toBe(true));

    expect(publicClient.queryBlock).toHaveBeenCalledWith({ height: 125 });
    expect(publicClient.queryBlock).toHaveBeenCalledWith();
    expect(result.current.data).toMatchObject({
      searchBlock: searchedBlock,
      currentBlock,
      height: 125,
      isInvalidBlock: false,
    });
  });

  it("treats the latest block route as the backend current block", async () => {
    const currentBlock = { blockHeight: 100, hash: "current-block" };
    publicClient.queryBlock.mockResolvedValue(currentBlock);

    const { result } = renderHook(() => useExplorerBlock("latest"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.searchBlock).toEqual(currentBlock));

    expect(publicClient.queryBlock).toHaveBeenCalledTimes(2);
    expect(publicClient.queryBlock).toHaveBeenNthCalledWith(1, undefined);
    expect(publicClient.queryBlock).toHaveBeenNthCalledWith(2);
    expect(result.current.data).toMatchObject({
      searchBlock: currentBlock,
      currentBlock,
      isFutureBlock: false,
      isInvalidBlock: false,
    });
    expect(Number.isNaN(result.current.data?.height)).toBe(true);
  });

  it("preserves backend block height zero in explorer block searches", async () => {
    const currentBlock = { blockHeight: 100, hash: "current-block" };
    const genesisBlock = { blockHeight: 0, hash: "genesis-block" };
    publicClient.queryBlock.mockImplementation((parameters?: { height?: number }) =>
      Promise.resolve(parameters && "height" in parameters ? genesisBlock : currentBlock),
    );

    const { result } = renderHook(() => useExplorerBlock("0"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.searchBlock).toEqual(genesisBlock));

    expect(publicClient.queryBlock).toHaveBeenNthCalledWith(1, { height: 0 });
    expect(publicClient.queryBlock).toHaveBeenNthCalledWith(2);
    expect(result.current.data).toMatchObject({
      searchBlock: genesisBlock,
      currentBlock,
      height: 0,
      isFutureBlock: false,
      isInvalidBlock: false,
    });
  });

  it("uses the mainnet archive for concrete block explorer lookups", async () => {
    publicClient.chain = { id: "dango-1" };
    const currentBlock = { blockHeight: 200, hash: "current-block" };
    publicClient.queryBlock.mockResolvedValue(currentBlock);
    const sender = "0x73656e6465720000000000000000000000000000";
    const txHash = "archive-tx-hash";
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        block: {
          info: {
            height: 125,
            timestamp: "31536000.123456789",
            hash: "archive-block",
          },
          txs: [
            [
              {
                sender,
                gas_limit: 100,
                msgs: [{ transfer: { [sender]: { usdc: "1" } } }],
              },
              txHash,
            ],
          ],
        },
        outcome: {
          app_hash: "archive-app-hash",
          cron_outcomes: [{ ok: true }],
          tx_outcomes: [
            {
              gas_limit: 100,
              gas_used: 75,
              result: { ok: null },
              events: { msgs_and_backrun: { msgs: [] } },
            },
          ],
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerBlock("125"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.searchBlock?.hash).toBe("archive-block"));

    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
    expect(publicClient.queryBlock).toHaveBeenCalledWith();
    expect((fetchMock.mock.calls[0]?.[0] as URL).toString()).toBe(
      "https://api-archive-mainnet.dango.zone/blocks/125",
    );
    expect(result.current.data?.searchBlock).toMatchObject({
      appHash: "archive-app-hash",
      blockHeight: 125,
      createdAt: "1971-01-01T00:00:00.123Z",
      transactions: [
        {
          createdAt: "1971-01-01T00:00:00.123Z",
          hash: txHash,
          sender,
          gasWanted: 100,
          gasUsed: 75,
          transactionIdx: 0,
          transactionType: "TX",
          hasSucceeded: true,
        },
      ],
    });
  });

  it("uses the testnet archive for concrete block explorer lookups", async () => {
    publicClient.chain = { id: "dango-testnet-1" };
    publicClient.queryBlock.mockResolvedValue({ blockHeight: 200, hash: "current-block" });
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        block: {
          info: {
            height: 125,
            timestamp: "31536000",
            hash: "archive-testnet-block",
          },
          txs: [],
        },
        outcome: {
          app_hash: "archive-testnet-app-hash",
          cron_outcomes: [],
          tx_outcomes: [],
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerBlock("125"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() =>
      expect(result.current.data?.searchBlock?.hash).toBe("archive-testnet-block"),
    );

    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
    expect((fetchMock.mock.calls[0]?.[0] as URL).toString()).toBe(
      "https://api-archive-testnet.dango.zone/blocks/125",
    );
  });

  it("uses the mainnet archive for latest block explorer lookups", async () => {
    publicClient.chain = { id: "dango-1" };
    publicClient.queryBlock.mockResolvedValue({ blockHeight: 200, hash: "live-current-block" });
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        block: {
          info: {
            height: 199,
            timestamp: "31535999",
            hash: "archive-latest-block",
          },
          txs: [],
        },
        outcome: {
          app_hash: "archive-latest-app-hash",
          cron_outcomes: [],
          tx_outcomes: [],
        },
      }),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerBlock("latest"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() =>
      expect(result.current.data?.searchBlock?.hash).toBe("archive-latest-block"),
    );

    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
    expect((fetchMock.mock.calls[0]?.[0] as URL).toString()).toBe(
      "https://api-archive-mainnet.dango.zone/blocks/latest",
    );
    expect(result.current.data?.currentBlock.hash).toBe("live-current-block");
  });

  it("keeps future mainnet block detection on the live client without querying archive", async () => {
    publicClient.chain = { id: "dango-1" };
    publicClient.queryBlock.mockResolvedValue({ blockHeight: 100, hash: "current-block" });
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerBlock("125"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.isFutureBlock).toBe(true));

    expect(publicClient.queryBlock).toHaveBeenCalledOnce();
    expect(fetchMock).not.toHaveBeenCalled();
    expect(result.current.data?.searchBlock).toBeNull();
  });

  it("surfaces backend failures for block explorer lookups", async () => {
    const queryError = new Error("block query unavailable");
    publicClient.queryBlock.mockRejectedValueOnce(queryError);

    const { result } = renderHook(() => useExplorerBlock("42"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.isError).toBe(true));

    expect(publicClient.queryBlock).toHaveBeenCalledWith({ height: 42 });
    expect(result.current.error).toBe(queryError);
    expect(result.current.data).toBeUndefined();
  });

  it("keeps block lookups idle without a route height", () => {
    const { result } = renderHook(() => useExplorerBlock(""), {
      wrapper: createQueryClientWrapper(),
    });

    expect(result.current.data).toBeUndefined();
    expect(result.current.fetchStatus).toBe("idle");
    expect(publicClient.queryBlock).not.toHaveBeenCalled();
  });

  it("returns the first indexed transaction for a hash and null when the backend has no match", async () => {
    const firstTx = { hash: "tx-hash", blockHeight: 12 };
    publicClient.searchTxs.mockResolvedValueOnce({ nodes: [firstTx, { hash: "second" }] });

    const firstLookup = renderHook(() => useExplorerTransaction("tx-hash"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(firstLookup.result.current.data).toEqual(firstTx));
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash: "tx-hash" });

    publicClient.searchTxs.mockResolvedValueOnce({ nodes: [] });
    const missingLookup = renderHook(() => useExplorerTransaction("missing-hash"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(missingLookup.result.current.data).toBeNull());
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash: "missing-hash" });
  });

  it("preserves zero-valued indexed transaction fields from backend lookups", async () => {
    const txHash = "0x67656e657369732d747800000000000000000000000000000000000000000000";
    const indexedTransaction = {
      blockHeight: 0,
      createdAt: "2026-06-08T12:00:00Z",
      errorMessage: "",
      gasUsed: 0,
      gasWanted: 0,
      hasSucceeded: true,
      hash: txHash,
      messages: [],
      nestedEvents: "[]",
      sender: "0x73656e6465720000000000000000000000000000",
      transactionIdx: 0,
      transactionType: "TX",
    };
    publicClient.searchTxs.mockResolvedValueOnce({ nodes: [indexedTransaction] });

    const { result } = renderHook(() => useExplorerTransaction(txHash), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data).toEqual(indexedTransaction));
    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash: txHash });
  });

  it("uses the mainnet archive for direct transaction hash lookups", async () => {
    publicClient.chain = { id: "dango-1" };
    const txHash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const sender = "0x73656e6465720000000000000000000000000000";
    const contract = "0x636f6e7472616374000000000000000000000000";
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse([
        {
          blockHeight: 77,
          idx: 3,
          kind: "transaction",
          hash: txHash,
          sender,
          success: true,
          timestamp: "2026-06-08T12:00:00Z",
          tx: {
            sender,
            gas_limit: 120,
            msgs: [{ execute: { contract, msg: { ping: {} }, funds: {} } }],
          },
          outcome: {
            transaction: {
              gas_limit: 120,
              gas_used: 80,
              result: { ok: null },
              events: { msgs_and_backrun: { msgs: [] } },
            },
          },
        },
      ]),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerTransaction(txHash), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.hash).toBe(txHash));

    expect(publicClient.searchTxs).not.toHaveBeenCalled();
    expect((fetchMock.mock.calls[0]?.[0] as URL).toString()).toBe(
      `https://api-archive-mainnet.dango.zone/transactions/${txHash.toUpperCase()}`,
    );
    expect(result.current.data).toMatchObject({
      blockHeight: 77,
      createdAt: "2026-06-08T12:00:00Z",
      gasUsed: 80,
      gasWanted: 120,
      hasSucceeded: true,
      messages: [
        {
          contractAddr: contract,
          methodName: "execute",
          orderIdx: 0,
          senderAddr: sender,
        },
      ],
      nestedEvents: JSON.stringify({ msgs_and_backrun: { msgs: [] } }),
      sender,
      transactionIdx: 3,
      transactionType: "TX",
    });
  });

  it("surfaces backend failures for direct transaction hash lookups", async () => {
    const txHash = "0x6661696c65642d74782d6c6f6f6b7570000000000000000000000000000000";
    const queryError = new Error("transaction search unavailable");
    publicClient.searchTxs.mockRejectedValueOnce(queryError);

    const { result } = renderHook(() => useExplorerTransaction(txHash), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.isError).toBe(true));

    expect(publicClient.searchTxs).toHaveBeenCalledWith({ hash: txHash });
    expect(result.current.error).toBe(queryError);
    expect(result.current.data).toBeUndefined();
  });

  it("keeps transaction lookups idle without a hash", () => {
    const { result } = renderHook(() => useExplorerTransaction(""), {
      wrapper: createQueryClientWrapper(),
    });

    expect(result.current.data).toBeUndefined();
    expect(result.current.fetchStatus).toBe("idle");
    expect(publicClient.searchTxs).not.toHaveBeenCalled();
  });

  it("queries sender transactions with backend cursor pagination", async () => {
    const senderAddress = "0x73656e6465720000000000000000000000000000";
    publicClient.searchTxs.mockImplementation(
      ({ after, before }: { after?: string; before?: string; senderAddress: string }) => {
        if (after) {
          return Promise.resolve({
            nodes: [{ hash: "second-page" }],
            pageInfo: {
              endCursor: "second-end",
              hasNextPage: false,
              hasPreviousPage: true,
              startCursor: "second-start",
            },
          });
        }

        if (before) {
          return Promise.resolve({
            nodes: [{ hash: "previous-page" }],
            pageInfo: {
              endCursor: "previous-end",
              hasNextPage: true,
              hasPreviousPage: false,
              startCursor: "previous-start",
            },
          });
        }

        return Promise.resolve({
          nodes: [{ hash: "first-page" }],
          pageInfo: {
            endCursor: "first-end",
            hasNextPage: true,
            hasPreviousPage: false,
            startCursor: "first-start",
          },
        });
      },
    );

    const { result } = renderHook(() => useExplorerTransactionsBySender(senderAddress), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("first-page"));

    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      after: undefined,
      before: undefined,
      first: 10,
      last: undefined,
      senderAddress,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(result.current.pagination.hasNextPage).toBe(true);
    expect(result.current.pagination.hasPreviousPage).toBe(false);

    act(() => {
      result.current.pagination.goNext();
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("second-page"));

    expect(publicClient.searchTxs).toHaveBeenLastCalledWith({
      after: "first-end",
      before: undefined,
      first: 10,
      last: undefined,
      senderAddress,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(true);

    act(() => {
      result.current.pagination.goPrev();
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("previous-page"));

    expect(publicClient.searchTxs).toHaveBeenLastCalledWith({
      after: undefined,
      before: "second-start",
      first: undefined,
      last: 10,
      senderAddress,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(result.current.pagination.hasNextPage).toBe(true);
    expect(result.current.pagination.hasPreviousPage).toBe(false);
  });

  it("uses archive sender pagination with a local previous-page cursor stack on mainnet", async () => {
    publicClient.chain = { id: "dango-1" };
    const senderAddress = "0x73656e6465720000000000000000000000000000";
    const firstPage = {
      items: [
        {
          blockHeight: 10,
          idx: 0,
          kind: "transaction",
          hash: "first-page",
          sender: senderAddress,
          success: true,
          timestamp: "2026-06-08T12:00:00Z",
          tx: { sender: senderAddress, gas_limit: 1, msgs: [] },
          outcome: { transaction: { gas_limit: 1, gas_used: 1, result: { ok: null }, events: [] } },
        },
      ],
      pageInfo: { hasNextPage: true, endCursor: "first-end" },
    };
    const secondPage = {
      items: [
        {
          blockHeight: 9,
          idx: 0,
          kind: "transaction",
          hash: "second-page",
          sender: senderAddress,
          success: true,
          timestamp: "2026-06-08T12:00:01Z",
          tx: { sender: senderAddress, gas_limit: 1, msgs: [] },
          outcome: { transaction: { gas_limit: 1, gas_used: 1, result: { ok: null }, events: [] } },
        },
      ],
      pageInfo: { hasNextPage: false, endCursor: "second-end" },
    };
    const fetchMock = vi.fn((input: URL) => {
      const after = input.searchParams.get("after");
      return Promise.resolve(jsonResponse(after === "first-end" ? secondPage : firstPage));
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerTransactionsBySender(senderAddress), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("first-page"));
    expect(result.current.pagination.hasNextPage).toBe(true);
    expect(result.current.pagination.hasPreviousPage).toBe(false);

    act(() => {
      result.current.pagination.goNext();
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("second-page"));
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(true);

    act(() => {
      result.current.pagination.goPrev();
    });

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("first-page"));
    expect(result.current.pagination.hasPreviousPage).toBe(false);
    expect(publicClient.searchTxs).not.toHaveBeenCalled();
    expect(fetchMock.mock.calls.map(([url]) => (url as URL).searchParams.get("after"))).toEqual([
      null,
      "first-end",
      null,
    ]);
    expect(fetchMock.mock.calls.some(([url]) => (url as URL).searchParams.has("before"))).toBe(
      false,
    );
    expect(
      fetchMock.mock.calls.every(([url]) => (url as URL).searchParams.get("role") === "sender"),
    ).toBe(true);
  });

  it("queries archive transactions involving an address and infers sender and participant roles", async () => {
    publicClient.chain = { id: "dango-1" };
    const address = "0x6164647265737300000000000000000000000000";
    const otherSender = "0x6f74686572000000000000000000000000000000";
    const page = {
      items: [
        {
          blockHeight: 12,
          idx: 0,
          kind: "transaction",
          hash: "sender-transaction",
          sender: address,
          success: true,
          timestamp: "2026-06-08T12:00:02Z",
          tx: { sender: address, gas_limit: 2, msgs: [] },
          outcome: { transaction: { gas_limit: 2, gas_used: 1, result: { ok: null }, events: [] } },
        },
        {
          blockHeight: 11,
          idx: 0,
          kind: "transaction",
          hash: "participant-transaction",
          sender: otherSender,
          success: true,
          timestamp: "2026-06-08T12:00:01Z",
          tx: { sender: otherSender, gas_limit: 2, msgs: [] },
          outcome: { transaction: { gas_limit: 2, gas_used: 1, result: { ok: null }, events: [] } },
        },
        {
          blockHeight: 10,
          idx: 1,
          kind: "cron",
          hash: null,
          sender: null,
          success: true,
          timestamp: "2026-06-08T12:00:00Z",
          tx: null,
          outcome: { cron: { gas_used: 1, result: { ok: null }, events: [] } },
        },
      ],
      pageInfo: { hasNextPage: true, endCursor: "page-end" },
    };
    const nextPage = {
      items: [
        {
          blockHeight: 9,
          idx: 0,
          kind: "transaction",
          hash: "next-page-transaction",
          sender: otherSender,
          success: true,
          timestamp: "2026-06-08T11:59:59Z",
          tx: { sender: otherSender, gas_limit: 2, msgs: [] },
          outcome: { transaction: { gas_limit: 2, gas_used: 1, result: { ok: null }, events: [] } },
        },
      ],
      pageInfo: { hasNextPage: false, endCursor: "next-page-end" },
    };
    const fetchMock = vi.fn((input: URL) =>
      Promise.resolve(
        jsonResponse(input.searchParams.get("after") === "page-end" ? nextPage : page),
      ),
    );
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(() => useExplorerTransactionsByAddress(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.nodes).toHaveLength(3));

    expect(result.current.data?.nodes.map((transaction) => transaction.involvement)).toEqual([
      ["sender"],
      ["participant"],
      ["participant"],
    ]);
    expect(result.current.data?.nodes[2]).toMatchObject({
      hash: "",
      sender: "",
      transactionType: "CRON",
    });

    const [requestUrl] = fetchMock.mock.calls[0] as unknown as [URL];
    expect(requestUrl.pathname).toBe(`/transactions/involving/${address}`);
    expect(requestUrl.searchParams.get("first")).toBe("10");
    expect(requestUrl.searchParams.has("role")).toBe(false);
    expect(requestUrl.searchParams.has("kind")).toBe(false);
    expect(publicClient.searchTxs).not.toHaveBeenCalled();

    act(() => result.current.pagination.goNext());

    await waitFor(() => expect(result.current.data?.nodes[0]?.hash).toBe("next-page-transaction"));
    expect((fetchMock.mock.calls[1]?.[0] as URL).searchParams.get("after")).toBe("page-end");
    expect(
      fetchMock.mock.calls.every(
        ([url]) => !(url as URL).searchParams.has("role") && !(url as URL).searchParams.has("kind"),
      ),
    ).toBe(true);
  });

  it("labels non-archive address history as sender-only", async () => {
    const address = "0x6164647265737300000000000000000000000000";
    publicClient.searchTxs.mockResolvedValue({
      nodes: [
        {
          blockHeight: 1,
          createdAt: "2026-06-08T12:00:00Z",
          hash: "public-transaction",
          sender: address,
          transactionIdx: 0,
          transactionType: "TX",
        },
      ],
      edge: [],
      pageInfo: { hasNextPage: false, hasPreviousPage: false },
    });

    const { result } = renderHook(() => useExplorerTransactionsByAddress(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.nodes).toHaveLength(1));

    expect(result.current.data?.nodes[0]?.involvement).toEqual(["sender"]);
    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      after: undefined,
      before: undefined,
      first: 10,
      last: undefined,
      senderAddress: address,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
  });

  it("does not query sender transactions when disabled", () => {
    const { result } = renderHook(
      () => useExplorerTransactionsBySender("0x73656e6465720000000000000000000000000000", false),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    expect(publicClient.searchTxs).not.toHaveBeenCalled();
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(false);
  });

  it("merges account, contract, balance, and optional perps data for account explorer", async () => {
    const account = { address: "0x6163636f756e7400000000000000000000000000", accountType: "spot" };
    const contractInfo = {
      codeHash: "account-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    };
    const balances = { "bridge/usdc": "1000000" };
    const orders = { orders: [{ id: "order-1" }] };

    publicClient.getAccountInfo.mockResolvedValue(account);
    publicClient.getContractInfo.mockResolvedValue(contractInfo);
    publicClient.getBalances.mockResolvedValue(balances);
    publicClient.getPerpsUserStateExtended.mockRejectedValue(new Error("no perps state"));
    publicClient.getPerpsOrdersByUser.mockResolvedValue(orders);
    publicClient.getPerpsVaultState.mockRejectedValue(new Error("vault unavailable"));

    const { result } = renderHook(
      () => useExplorerAccount("0x6163636f756e7400000000000000000000000000"),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(publicClient.getAccountInfo).toHaveBeenCalledWith({
      address: "0x6163636f756e7400000000000000000000000000",
    });
    expect(publicClient.getContractInfo).toHaveBeenCalledWith({
      address: "0x6163636f756e7400000000000000000000000000",
    });
    expect(publicClient.getBalances).toHaveBeenCalledWith({
      address: "0x6163636f756e7400000000000000000000000000",
    });
    expect(publicClient.getPerpsUserStateExtended).toHaveBeenCalledWith({
      user: "0x6163636f756e7400000000000000000000000000",
      includeAll: true,
    });
    expect(result.current.data).toEqual({
      ...account,
      ...contractInfo,
      balances,
      perps: {
        userState: null,
        orders,
        vaultState: null,
      },
    });
  });

  it("preserves successful perps state, orders, and vault lookups for account explorer", async () => {
    const address = "0x70657270732d6163636f756e7400000000000000";
    const account = { address, accountType: "spot" };
    const contractInfo = {
      codeHash: "account-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    };
    const balances = { "bridge/usdc": "2500000" };
    const perpsUserState = {
      equity: "1200",
      margin: "900",
      positions: {
        "perp/btcusd": {
          size: "2",
          unrealizedPnl: "15",
        },
      },
    };
    const perpsOrders = {
      orders: [
        {
          id: "order-1",
          pairId: "perp/btcusd",
        },
      ],
    };
    const perpsVaultState = {
      equity: "2000",
      shareSupply: "1000",
    };

    publicClient.getAccountInfo.mockResolvedValue(account);
    publicClient.getContractInfo.mockResolvedValue(contractInfo);
    publicClient.getBalances.mockResolvedValue(balances);
    publicClient.getPerpsUserStateExtended.mockResolvedValue(perpsUserState);
    publicClient.getPerpsOrdersByUser.mockResolvedValue(perpsOrders);
    publicClient.getPerpsVaultState.mockResolvedValue(perpsVaultState);

    const { result } = renderHook(() => useExplorerAccount(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.perps.userState).toEqual(perpsUserState));

    expect(publicClient.getPerpsUserStateExtended).toHaveBeenCalledWith({
      user: address,
      includeAll: true,
    });
    expect(publicClient.getPerpsOrdersByUser).toHaveBeenCalledWith({
      user: address,
    });
    expect(publicClient.getPerpsVaultState).toHaveBeenCalledWith();
    expect(result.current.data).toEqual({
      ...account,
      ...contractInfo,
      balances,
      perps: {
        userState: perpsUserState,
        orders: perpsOrders,
        vaultState: perpsVaultState,
      },
    });
  });

  it("preserves zero-valued account balances and perps payloads from backend lookups", async () => {
    const address = "0x7a65726f2d6163636f756e7400000000000000";
    const account = { address, accountType: "spot" };
    const contractInfo = {
      codeHash: "account-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    };
    const balances = { "bridge/usdc": "0", "native/dango": "0" };
    const perpsUserState = {
      equity: "0",
      margin: "0",
      positions: {},
      availableMargin: "0",
      reservedMargin: "0",
      vaultShares: "0",
    };
    const perpsOrders = { orders: [] };
    const perpsVaultState = {
      equity: "0",
      shareSupply: "0",
    };

    publicClient.getAccountInfo.mockResolvedValue(account);
    publicClient.getContractInfo.mockResolvedValue(contractInfo);
    publicClient.getBalances.mockResolvedValue(balances);
    publicClient.getPerpsUserStateExtended.mockResolvedValue(perpsUserState);
    publicClient.getPerpsOrdersByUser.mockResolvedValue(perpsOrders);
    publicClient.getPerpsVaultState.mockResolvedValue(perpsVaultState);

    const { result } = renderHook(() => useExplorerAccount(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.balances).toEqual(balances));

    expect(publicClient.getPerpsUserStateExtended).toHaveBeenCalledWith({
      user: address,
      includeAll: true,
    });
    expect(publicClient.getPerpsOrdersByUser).toHaveBeenCalledWith({
      user: address,
    });
    expect(result.current.data).toEqual({
      ...account,
      ...contractInfo,
      balances,
      perps: {
        userState: perpsUserState,
        orders: perpsOrders,
        vaultState: perpsVaultState,
      },
    });
  });

  it("keeps account explorer details available when optional perps lookups fail", async () => {
    const account = { address: "0x6163636f756e7400000000000000000000000000", accountType: "spot" };
    const contractInfo = {
      codeHash: "account-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    };
    const balances = { "bridge/usdc": "1000000" };

    publicClient.getAccountInfo.mockResolvedValue(account);
    publicClient.getContractInfo.mockResolvedValue(contractInfo);
    publicClient.getBalances.mockResolvedValue(balances);
    publicClient.getPerpsUserStateExtended.mockRejectedValue(new Error("no perps state"));
    publicClient.getPerpsOrdersByUser.mockRejectedValue(new Error("orders unavailable"));
    publicClient.getPerpsVaultState.mockRejectedValue(new Error("vault unavailable"));

    const { result } = renderHook(
      () => useExplorerAccount("0x6163636f756e7400000000000000000000000000"),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(result.current.data).toEqual({
      ...account,
      ...contractInfo,
      balances,
      perps: {
        userState: null,
        orders: null,
        vaultState: null,
      },
    });
  });

  it("surfaces mandatory account balance lookup failures in account explorer", async () => {
    const address = "0x62616c616e63652d6572726f7200000000000000";
    const queryError = new Error("account balances unavailable");
    publicClient.getAccountInfo.mockResolvedValue({ address, accountType: "spot" });
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "account-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    });
    publicClient.getBalances.mockRejectedValueOnce(queryError);
    publicClient.getPerpsUserStateExtended.mockResolvedValue(null);
    publicClient.getPerpsOrdersByUser.mockResolvedValue({ orders: [] });
    publicClient.getPerpsVaultState.mockResolvedValue(null);

    const { result } = renderHook(() => useExplorerAccount(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.isError).toBe(true));

    expect(publicClient.getAccountInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getBalances).toHaveBeenCalledWith({ address });
    expect(result.current.error).toBe(queryError);
    expect(result.current.data).toBeUndefined();
  });

  it("treats account-factory contracts as accounts in contract explorer", async () => {
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "account-code-hash",
      admin: null,
    });
    publicClient.getBalances.mockResolvedValue({ "bridge/usdc": "25" });

    const { result } = renderHook(
      () => useExplorerContract("0x6163636f756e7400000000000000000000000000"),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    await waitFor(() => expect(result.current.data).toBeNull());

    expect(publicClient.getContractInfo).toHaveBeenCalledWith({
      address: "0x6163636f756e7400000000000000000000000000",
    });
    expect(publicClient.getBalances).toHaveBeenCalledWith({
      address: "0x6163636f756e7400000000000000000000000000",
    });
  });

  it("returns non-account contract details with balances from backend lookups", async () => {
    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "vault-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    });
    publicClient.getBalances.mockResolvedValue({
      "bridge/usdc": "7500000",
      "native/dango": "12",
    });

    const { result } = renderHook(
      () => useExplorerContract("0x7661756c74000000000000000000000000000000"),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    await waitFor(() => expect(result.current.data).toBeDefined());

    expect(publicClient.getContractInfo).toHaveBeenCalledWith({
      address: "0x7661756c74000000000000000000000000000000",
    });
    expect(publicClient.getBalances).toHaveBeenCalledWith({
      address: "0x7661756c74000000000000000000000000000000",
    });
    expect(result.current.data).toEqual({
      address: "0x7661756c74000000000000000000000000000000",
      admin: "0x61646d696e000000000000000000000000000000",
      balances: {
        "bridge/usdc": "7500000",
        "native/dango": "12",
      },
      codeHash: "vault-code-hash",
    });
  });

  it("preserves zero-valued balances for non-account contract backend lookups", async () => {
    const address = "0x7a65726f2d636f6e747261637400000000000000";

    publicClient.getContractInfo.mockResolvedValue({
      codeHash: "vault-code-hash",
      admin: "0x61646d696e000000000000000000000000000000",
    });
    publicClient.getBalances.mockResolvedValue({
      "bridge/usdc": "0",
      "native/dango": "0",
    });

    const { result } = renderHook(() => useExplorerContract(address), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() =>
      expect(result.current.data?.balances).toEqual({ "bridge/usdc": "0", "native/dango": "0" }),
    );

    expect(publicClient.getContractInfo).toHaveBeenCalledWith({ address });
    expect(publicClient.getBalances).toHaveBeenCalledWith({ address });
    expect(result.current.data).toEqual({
      address,
      admin: "0x61646d696e000000000000000000000000000000",
      balances: {
        "bridge/usdc": "0",
        "native/dango": "0",
      },
      codeHash: "vault-code-hash",
    });
  });

  it("aggregates user accounts, balances, statuses, and keys from backend responses", async () => {
    publicClient.getUser.mockResolvedValue({
      index: 7,
      name: "alice",
      accounts: {
        0: "0x6669727374000000000000000000000000000000",
        2: "0x7365636f6e640000000000000000000000000000",
      },
    });
    publicClient.getUserKeys.mockResolvedValue([{ keyHash: "key-1", keyType: "ETHEREUM" }]);
    publicClient.getBalances.mockImplementation(({ address }: { address: string }) =>
      Promise.resolve(
        address === "0x6669727374000000000000000000000000000000"
          ? { "bridge/usdc": "100", "native/dango": "9" }
          : { "bridge/usdc": "50" },
      ),
    );
    publicClient.getAccountStatus.mockImplementation(({ address }: { address: string }) =>
      address === "0x6669727374000000000000000000000000000000"
        ? Promise.resolve("active")
        : Promise.reject(new Error("missing")),
    );

    const { result } = renderHook(() => useExplorerUser("alice"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.accounts).toHaveLength(2));

    expect(publicClient.getUser).toHaveBeenCalledWith({ userIndexOrName: { name: "alice" } });
    expect(publicClient.getUserKeys).toHaveBeenCalledWith({ userIndex: 7 });
    expect(result.current).toMatchObject({
      isLoading: false,
      isNotFound: false,
      data: {
        totalAccounts: 2,
        aggregatedBalances: {
          "bridge/usdc": "150",
          "native/dango": "9",
        },
        totalValue: "usd:bridge/usdc:150|native/dango:9",
        keys: [{ keyHash: "key-1", keyType: "ETHEREUM" }],
      },
    });
    expect(result.current.data?.accounts).toEqual([
      {
        address: "0x6669727374000000000000000000000000000000",
        index: 0,
        balance: { "bridge/usdc": "100", "native/dango": "9" },
        balanceUSD: "usd:bridge/usdc:100|native/dango:9",
        isActive: true,
      },
      {
        address: "0x7365636f6e640000000000000000000000000000",
        index: 2,
        balance: { "bridge/usdc": "50" },
        balanceUSD: "usd:bridge/usdc:50",
        isActive: false,
      },
    ]);
  });

  it("preserves zero-valued backend balances in user account aggregation", async () => {
    const zeroBalanceAddress = "0x7a65726f62616c616e6365000000000000000000";
    publicClient.getUser.mockResolvedValue({
      accounts: {
        0: zeroBalanceAddress,
      },
      index: 10,
      name: "zero-balance",
    });
    publicClient.getUserKeys.mockResolvedValue([]);
    publicClient.getBalances.mockResolvedValue({
      "bridge/usdc": "0",
    });
    publicClient.getAccountStatus.mockResolvedValue("active");

    const { result } = renderHook(() => useExplorerUser("zero-balance"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.accounts).toHaveLength(1));

    expect(result.current).toMatchObject({
      isLoading: false,
      isNotFound: false,
      data: {
        accounts: [
          {
            address: zeroBalanceAddress,
            balance: {
              "bridge/usdc": "0",
            },
            balanceUSD: "usd:bridge/usdc:0",
            index: 0,
            isActive: true,
          },
        ],
        aggregatedBalances: {
          "bridge/usdc": "0",
        },
        totalAccounts: 1,
        totalValue: "usd:bridge/usdc:0",
      },
    });
    expect(calculateBalance).toHaveBeenCalledWith({ "bridge/usdc": "0" }, { format: true });
  });

  it("keeps user explorer details available when one account balance lookup fails", async () => {
    publicClient.getUser.mockResolvedValue({
      index: 9,
      name: "partial",
      accounts: {
        0: "0x7061727469616c31000000000000000000000000",
        1: "0x7061727469616c32000000000000000000000000",
      },
    });
    publicClient.getUserKeys.mockResolvedValue([{ keyHash: "key-partial", keyType: "ED25519" }]);
    publicClient.getBalances.mockImplementation(({ address }: { address: string }) => {
      if (address === "0x7061727469616c31000000000000000000000000") {
        return Promise.resolve({ "bridge/usdc": "125", "native/dango": "3" });
      }

      return Promise.reject(new Error("account balances unavailable"));
    });
    publicClient.getAccountStatus.mockResolvedValue("active");

    const { result } = renderHook(() => useExplorerUser("partial"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.accounts).toHaveLength(1));

    expect(publicClient.getUser).toHaveBeenCalledWith({ userIndexOrName: { name: "partial" } });
    expect(publicClient.getUserKeys).toHaveBeenCalledWith({ userIndex: 9 });
    expect(publicClient.getBalances).toHaveBeenCalledWith({
      address: "0x7061727469616c31000000000000000000000000",
    });
    expect(publicClient.getBalances).toHaveBeenCalledWith({
      address: "0x7061727469616c32000000000000000000000000",
    });
    expect(result.current).toMatchObject({
      isLoading: false,
      isNotFound: false,
      data: {
        accounts: [
          {
            address: "0x7061727469616c31000000000000000000000000",
            balance: { "bridge/usdc": "125", "native/dango": "3" },
            balanceUSD: "usd:bridge/usdc:125|native/dango:3",
            index: 0,
            isActive: true,
          },
        ],
        aggregatedBalances: {
          "bridge/usdc": "125",
          "native/dango": "3",
        },
        keys: [{ keyHash: "key-partial", keyType: "ED25519" }],
        totalAccounts: 2,
        totalValue: "usd:bridge/usdc:125|native/dango:3",
      },
    });
  });

  it("keeps existing users with no accounts distinct from not-found users", async () => {
    publicClient.getUser.mockResolvedValue({
      index: 8,
      name: "empty",
      accounts: {},
    });
    publicClient.getUserKeys.mockResolvedValue([{ keyHash: "key-empty", keyType: "SECP256K1" }]);

    const { result } = renderHook(() => useExplorerUser("empty"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.data?.keys).toHaveLength(1));

    expect(publicClient.getUser).toHaveBeenCalledWith({ userIndexOrName: { name: "empty" } });
    expect(publicClient.getUserKeys).toHaveBeenCalledWith({ userIndex: 8 });
    expect(publicClient.getBalances).not.toHaveBeenCalled();
    expect(publicClient.getAccountStatus).not.toHaveBeenCalled();
    expect(result.current).toMatchObject({
      isLoading: false,
      isNotFound: false,
      data: {
        accounts: [],
        aggregatedBalances: {},
        keys: [{ keyHash: "key-empty", keyType: "SECP256K1" }],
        totalAccounts: 0,
        totalValue: "usd:",
        user: {
          index: 8,
          name: "empty",
          accounts: {},
        },
      },
    });
  });

  it("loads backend keys for explorer users with index zero", async () => {
    publicClient.getUser.mockResolvedValue({
      index: 0,
      name: "genesis",
      accounts: {},
    });
    publicClient.getUserKeys.mockResolvedValue([{ keyHash: "key-zero", keyType: "SECP256R1" }]);

    const { result } = renderHook(() => useExplorerUser("genesis"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() =>
      expect(result.current.data?.keys).toEqual([{ keyHash: "key-zero", keyType: "SECP256R1" }]),
    );

    expect(publicClient.getUser).toHaveBeenCalledWith({ userIndexOrName: { name: "genesis" } });
    expect(publicClient.getUserKeys).toHaveBeenCalledWith({ userIndex: 0 });
    expect(result.current).toMatchObject({
      isLoading: false,
      isNotFound: false,
      data: {
        totalAccounts: 0,
        aggregatedBalances: {},
        totalValue: "usd:",
      },
    });
    expect(publicClient.getBalances).not.toHaveBeenCalled();
    expect(publicClient.getAccountStatus).not.toHaveBeenCalled();
  });

  it("short-circuits user explorer details when the backend user lookup misses", async () => {
    publicClient.getUser.mockRejectedValue(new Error("missing user"));

    const { result } = renderHook(() => useExplorerUser("missing"), {
      wrapper: createQueryClientWrapper(),
    });

    await waitFor(() => expect(result.current.isNotFound).toBe(true));

    expect(result.current).toMatchObject({
      data: null,
      isLoading: false,
      isNotFound: true,
    });
    expect(publicClient.getUser).toHaveBeenCalledWith({
      userIndexOrName: { name: "missing" },
    });
    expect(publicClient.getUserKeys).not.toHaveBeenCalled();
    expect(publicClient.getBalances).not.toHaveBeenCalled();
    expect(publicClient.getAccountStatus).not.toHaveBeenCalled();
  });

  it("sorts and paginates user transactions across all account addresses", async () => {
    const firstAddress = "0x6669727374000000000000000000000000000000";
    const newestTxs = Array.from({ length: 11 }, (_, index) => ({
      blockHeight: index + 10,
      hash: `new-${index}`,
      createdAt: new Date(Date.UTC(2026, 0, 1, 0, index)).toISOString(),
      sender: firstAddress,
      transactionIdx: 0,
      transactionType: "TX",
    }));
    const oldestTx = {
      blockHeight: 1,
      hash: "oldest",
      createdAt: new Date(Date.UTC(2025, 0, 1)).toISOString(),
      sender: "0x7365636f6e640000000000000000000000000000",
      transactionIdx: 0,
      transactionType: "TX",
    };
    publicClient.searchTxs.mockImplementation(({ senderAddress }: { senderAddress: string }) =>
      Promise.resolve({
        nodes: senderAddress === firstAddress ? newestTxs : [oldestTx],
      }),
    );

    const { result } = renderHook(
      () =>
        useExplorerUserTransactions([firstAddress, "0x7365636f6e640000000000000000000000000000"]),
      { wrapper: createQueryClientWrapper() },
    );

    await waitFor(() => expect(result.current.allTransactions).toHaveLength(12));

    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      senderAddress: "0x6669727374000000000000000000000000000000",
      first: 50,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      senderAddress: "0x7365636f6e640000000000000000000000000000",
      first: 50,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(result.current.data.map((tx) => tx.hash)).toEqual([
      "new-10",
      "new-9",
      "new-8",
      "new-7",
      "new-6",
      "new-5",
      "new-4",
      "new-3",
      "new-2",
      "new-1",
    ]);
    expect(result.current.pagination.hasNextPage).toBe(true);
    expect(result.current.pagination.hasPreviousPage).toBe(false);

    act(() => result.current.pagination.goNext());

    expect(result.current.data.map((tx) => tx.hash)).toEqual(["new-0", "oldest"]);
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(true);
  });

  it("deduplicates archive user activity by unit position and merges roles across addresses", async () => {
    publicClient.chain = { id: "dango-testnet-1" };
    const firstAddress = "0x6669727374000000000000000000000000000000";
    const secondAddress = "0x7365636f6e640000000000000000000000000000";
    const repeatedHash = "ABCD";
    const sharedTransaction = {
      blockHeight: 5,
      idx: 0,
      kind: "transaction",
      hash: repeatedHash,
      sender: firstAddress,
      success: true,
      timestamp: "2026-01-03T00:00:00.000Z",
      tx: { sender: firstAddress, gas_limit: 1, msgs: [] },
      outcome: { transaction: { gas_limit: 1, gas_used: 1, result: { ok: null }, events: [] } },
    };
    const repeatedSubmission = {
      ...sharedTransaction,
      blockHeight: 4,
      timestamp: "2026-01-02T00:00:00.000Z",
    };
    const cron = {
      blockHeight: 3,
      idx: 0,
      kind: "cron",
      hash: null,
      sender: null,
      success: true,
      timestamp: "2026-01-01T00:00:00.000Z",
      tx: null,
      outcome: { cron: { result: { ok: null }, events: [] } },
    };
    const fetchMock = vi.fn((input: URL) => {
      const items = input.pathname.endsWith(firstAddress)
        ? [sharedTransaction, repeatedSubmission]
        : [sharedTransaction, cron];
      return Promise.resolve(
        jsonResponse({ items, pageInfo: { hasNextPage: false, endCursor: null } }),
      );
    });
    vi.stubGlobal("fetch", fetchMock);

    const { result } = renderHook(
      () => useExplorerUserTransactions([firstAddress, secondAddress]),
      { wrapper: createQueryClientWrapper() },
    );

    await waitFor(() => expect(result.current.allTransactions).toHaveLength(3));

    expect(result.current.allTransactions.map((transaction) => transaction.hash)).toEqual([
      repeatedHash,
      repeatedHash,
      "",
    ]);
    expect(result.current.allTransactions.map((transaction) => transaction.involvement)).toEqual([
      ["sender", "participant"],
      ["sender"],
      ["participant"],
    ]);
    expect(
      fetchMock.mock.calls.every(
        ([url]) => !(url as URL).searchParams.has("role") && !(url as URL).searchParams.has("kind"),
      ),
    ).toBe(true);
  });

  it("preserves zero-valued backend fields while aggregating user transactions", async () => {
    const firstAddress = "0x6669727374000000000000000000000000000000";
    const secondAddress = "0x7365636f6e640000000000000000000000000000";
    const genesisTx = {
      blockHeight: 0,
      createdAt: "2026-01-01T00:00:00.000Z",
      errorMessage: "",
      gasUsed: 0,
      gasWanted: 0,
      hasSucceeded: true,
      hash: "0x67656e657369732d747800000000000000000000000000000000000000000000",
      messages: [],
      nestedEvents: "[]",
      sender: firstAddress,
      transactionIdx: 0,
      transactionType: "TX",
    };
    const laterTx = {
      blockHeight: 1,
      hash: "0x6c617465722d7478000000000000000000000000000000000000000000000000",
      createdAt: "2026-01-02T00:00:00.000Z",
      sender: secondAddress,
      transactionIdx: 1,
      transactionType: "TX",
    };
    publicClient.searchTxs.mockImplementation(({ senderAddress }: { senderAddress: string }) =>
      Promise.resolve({
        nodes: senderAddress === firstAddress ? [genesisTx] : [laterTx],
      }),
    );

    const { result } = renderHook(
      () => useExplorerUserTransactions([firstAddress, secondAddress]),
      {
        wrapper: createQueryClientWrapper(),
      },
    );

    await waitFor(() => expect(result.current.allTransactions).toHaveLength(2));

    expect(result.current.allTransactions).toEqual([
      { ...laterTx, involvement: ["sender"] },
      { ...genesisTx, involvement: ["sender"] },
    ]);
    expect(result.current.data).toEqual(result.current.allTransactions);
  });

  it("keeps user transaction history available when one account query fails", async () => {
    publicClient.searchTxs.mockImplementation(({ senderAddress }: { senderAddress: string }) => {
      if (senderAddress === "0x6669727374000000000000000000000000000000") {
        return Promise.resolve({
          nodes: [
            {
              blockHeight: 1,
              hash: "first-account-tx",
              createdAt: "2026-01-01T00:00:00.000Z",
              sender: "0x6669727374000000000000000000000000000000",
              transactionIdx: 0,
              transactionType: "TX",
            },
          ],
        });
      }

      return Promise.reject(new Error("account transactions unavailable"));
    });

    const { result } = renderHook(
      () =>
        useExplorerUserTransactions([
          "0x6669727374000000000000000000000000000000",
          "0x7365636f6e640000000000000000000000000000",
        ]),
      { wrapper: createQueryClientWrapper() },
    );

    await waitFor(() => expect(result.current.allTransactions).toHaveLength(1));

    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      senderAddress: "0x6669727374000000000000000000000000000000",
      first: 50,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(publicClient.searchTxs).toHaveBeenCalledWith({
      senderAddress: "0x7365636f6e640000000000000000000000000000",
      first: 50,
      sortBy: "BLOCK_HEIGHT_DESC",
    });
    expect(result.current.data).toEqual([
      {
        blockHeight: 1,
        hash: "first-account-tx",
        createdAt: "2026-01-01T00:00:00.000Z",
        involvement: ["sender"],
        sender: "0x6669727374000000000000000000000000000000",
        transactionIdx: 0,
        transactionType: "TX",
      },
    ]);
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(false);
  });

  it("does not query user transactions when the user has no account addresses", () => {
    const { result } = renderHook(() => useExplorerUserTransactions([]), {
      wrapper: createQueryClientWrapper(),
    });

    expect(publicClient.searchTxs).not.toHaveBeenCalled();
    expect(result.current).toMatchObject({
      allTransactions: [],
      data: [],
      isLoading: false,
    });
    expect(result.current.pagination.hasNextPage).toBe(false);
    expect(result.current.pagination.hasPreviousPage).toBe(false);
  });
});

describe("parseExplorerErrorMessage", () => {
  it("extracts structured error and backtrace payloads from backend error messages", () => {
    expect(
      parseExplorerErrorMessage(
        JSON.stringify({
          error: { kind: "contract", message: "insufficient funds" },
          backtrace: "0: execute\n1: transfer",
        }),
      ),
    ).toEqual({
      error: { kind: "contract", message: "insufficient funds" },
      backtrace: "0: execute\n1: transfer",
    });
  });

  it("preserves backend error messages that include only one optional field", () => {
    expect(
      parseExplorerErrorMessage(JSON.stringify({ backtrace: "0: verify\n1: execute" })),
    ).toEqual({
      error: undefined,
      backtrace: "0: verify\n1: execute",
    });

    expect(parseExplorerErrorMessage(JSON.stringify({ error: { kind: "signature" } }))).toEqual({
      error: { kind: "signature" },
      backtrace: undefined,
    });
  });

  it("returns an empty object for missing or non-JSON backend error messages", () => {
    expect(parseExplorerErrorMessage()).toEqual({});
    expect(parseExplorerErrorMessage("plain text error")).toEqual({});
  });
});
