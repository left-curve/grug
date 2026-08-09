import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import type { IndexedMessage, IndexedTransaction } from "@left-curve/types";
import type React from "react";

import { resetAppletsKitMocks, setAppletsKitUseApp } from "./mocks/applets-kit";

import { AssetsTable } from "../src/components/explorer/AssetsTable";
import { TransactionsTable } from "../src/components/explorer/TransactionsTable";
import { createTestQueryClient } from "./utils/query-client";

const explorerTableMocks = vi.hoisted(() => ({
  getAccountInfo: vi.fn(),
  getContractInfo: vi.fn(),
  getPrice: vi.fn(),
  goNext: vi.fn(),
  goPrev: vi.fn(),
  navigate: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => explorerTableMocks.navigate,
}));

vi.mock("@left-curve/foundation", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@left-curve/foundation")>()),
  useApp: () => ({
    settings: {
      formatNumberOptions: {
        currency: "USD",
        language: "en-US",
        mask: 1,
      },
    },
  }),
}));

vi.mock("@left-curve/store", () => ({
  getExplorerTransactionKey: (transaction: IndexedTransaction) =>
    `${transaction.blockHeight}:${transaction.transactionType}:${transaction.transactionIdx}`,
  useAccount: () => ({
    accounts: [],
    username: undefined,
  }),
  useAppConfig: () => ({
    data: {
      addresses: {},
    },
  }),
  useConfig: () => ({
    chain: {
      blockExplorer: {
        accountPage: `https://explorer.example/account/${"$"}{address}`,
        contractPage: `https://explorer.example/contract/${"$"}{address}`,
      },
    },
    coins: {
      getCoinInfo: (denom: string) => {
        const coins = {
          "bridge/usdc": {
            decimals: 6,
            denom: "bridge/usdc",
            logoURI: "/images/usdc.svg",
            name: "USD Coin",
            symbol: "USDC",
          },
          uatom: {
            decimals: 6,
            denom: "uatom",
            logoURI: "/images/atom.svg",
            name: "Cosmos Hub Atom",
            symbol: "ATOM",
          },
        };
        const coin = coins[denom as keyof typeof coins];
        if (!coin) throw new Error(`missing coin fixture for ${denom}`);
        return coin;
      },
    },
  }),
  usePublicClient: () => ({
    getAccountInfo: explorerTableMocks.getAccountInfo,
    getContractInfo: explorerTableMocks.getContractInfo,
  }),
  usePrices: () => ({
    getPrice: explorerTableMocks.getPrice,
    prices: {
      "bridge/usdc": {
        humanizedPrice: 1,
      },
      uatom: {
        humanizedPrice: 2,
      },
    },
  }),
}));

function createIndexedMessage(
  methodName: string,
  overrides: Partial<IndexedMessage> = {},
): IndexedMessage {
  return {
    blockHeight: 1234,
    contractAddr: "0x636f6e7472616374000000000000000000000000",
    createdAt: "2026-06-08T12:00:00.000Z",
    data: {},
    methodName,
    orderIdx: 0,
    senderAddr: "0x73656e6465720000000000000000000000000000",
    ...overrides,
  };
}

function renderWithQueryClient(component: React.ReactNode) {
  const queryClient = createTestQueryClient();

  return render(<QueryClientProvider client={queryClient}>{component}</QueryClientProvider>);
}

function getBodyRows() {
  return screen.getAllByRole("row").slice(1);
}

function getHeaderLabels() {
  return rowCells(screen.getAllByRole("row")[0]);
}

function rowCells(row: HTMLElement) {
  return within(row)
    .getAllByRole("cell")
    .map((cell) => cell.textContent);
}

describe("explorer tables", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    setAppletsKitUseApp({
      settings: {
        formatNumberOptions: {
          currency: "USD",
          language: "en-US",
          mask: 1,
        },
      },
    });
    explorerTableMocks.getAccountInfo.mockResolvedValue(null);
    explorerTableMocks.getContractInfo.mockResolvedValue(null);
    explorerTableMocks.getPrice.mockImplementation((amount: string, denom: string) => {
      return `price:${denom}:${amount}`;
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders indexed transactions with navigation targets, result labels, and cursor pagination", async () => {
    const sender = "0x73656e6465720000000000000000000000000000";
    const failedSender = "0x6661696c73656e64657200000000000000000000";
    explorerTableMocks.getAccountInfo.mockImplementation(async ({ address }) => {
      if (address === sender) return { index: 1, username: "alice" };
      if (address === failedSender) return { index: 2, username: "bob" };
      return null;
    });

    const transactions: IndexedTransaction[] = [
      {
        blockHeight: 1234,
        createdAt: "2026-06-08T12:00:00.000Z",
        errorMessage: "",
        gasUsed: 80_000,
        gasWanted: 100_000,
        hash: "0x7478686173680000000000000000000000000000000000000000000000000000",
        hasSucceeded: true,
        messages: [
          createIndexedMessage("transfer"),
          createIndexedMessage("execute", { orderIdx: 1 }),
        ],
        nestedEvents: "[]",
        sender,
        transactionIdx: 0,
        transactionType: "TX",
      },
      {
        blockHeight: 1235,
        createdAt: "2026-06-08T12:01:00.000Z",
        errorMessage: "swap failed",
        gasUsed: 42_000,
        gasWanted: 100_000,
        hash: "0x6661696c65640000000000000000000000000000000000000000000000000000",
        hasSucceeded: false,
        messages: [createIndexedMessage("swap", { blockHeight: 1235 })],
        nestedEvents: "[]",
        sender: failedSender,
        transactionIdx: 1,
        transactionType: "TX",
      },
    ];

    renderWithQueryClient(
      <TransactionsTable
        transactions={transactions}
        pagination={{
          goNext: explorerTableMocks.goNext,
          goPrev: explorerTableMocks.goPrev,
          hasNextPage: true,
          hasPreviousPage: false,
          isLoading: false,
        }}
      />,
    );

    expect(getHeaderLabels()).toEqual(["Hash", "Block", "Age", "Sender", "Actions", "Result"]);
    await waitFor(() => expect(screen.getByText("alice #1")).toBeInTheDocument());

    const [firstRow, secondRow] = getBodyRows();
    const firstCells = rowCells(firstRow);
    const secondCells = rowCells(secondRow);

    expect(firstCells[0]).toBe(transactions[0].hash);
    expect(firstCells[1]).toBe("1234");
    expect(firstCells[2]).toMatch(/ago$/);
    expect(firstCells[3]).toBe("alice #1");
    expect(firstCells[4]).toBe("Transfer+1");
    expect(firstCells[5]).toBe(`${m["explorer.txs.result"]({ result: "true" })}+1`);
    expect(secondCells[3]).toBe("bob #2");
    expect(secondCells[4]).toBe("Swap");
    expect(secondCells[5]).toBe(m["explorer.txs.result"]({ result: "false" }));

    fireEvent.click(screen.getByText(transactions[0].hash).closest("a") as HTMLAnchorElement);
    expect(explorerTableMocks.navigate).toHaveBeenCalledWith({
      to: "/tx/0x7478686173680000000000000000000000000000000000000000000000000000",
    });

    fireEvent.click(screen.getByText("1234").closest("a") as HTMLAnchorElement);
    expect(explorerTableMocks.navigate).toHaveBeenCalledWith({ to: "/block/1234" });

    fireEvent.click(screen.getByRole("link", { name: /alice #1/ }));
    expect(explorerTableMocks.navigate).toHaveBeenCalledWith({
      to: `https://explorer.example/account/${sender}`,
    });

    expect(
      screen.queryByRole("button", { name: m["pagination.previous"]() }),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: m["pagination.next"]() }));
    expect(explorerTableMocks.goNext).toHaveBeenCalledOnce();
    expect(explorerTableMocks.goPrev).not.toHaveBeenCalled();
  });

  it("preserves backend block height zero in transaction table links", () => {
    const transaction: IndexedTransaction = {
      blockHeight: 0,
      createdAt: "2026-06-08T12:00:00.000Z",
      errorMessage: "",
      gasUsed: 21_000,
      gasWanted: 50_000,
      hash: "0x67656e657369732d747800000000000000000000000000000000000000000000",
      hasSucceeded: true,
      messages: [createIndexedMessage("transfer", { blockHeight: 0 })],
      nestedEvents: "[]",
      sender: "0x67656e657369732d73656e646572000000000000",
      transactionIdx: 0,
      transactionType: "TX",
    };

    renderWithQueryClient(<TransactionsTable transactions={[transaction]} />);

    const [row] = getBodyRows();

    expect(rowCells(row)[1]).toBe("0");

    fireEvent.click(within(row).getByRole("link", { name: "0" }));

    expect(explorerTableMocks.navigate).toHaveBeenCalledWith({ to: "/block/0" });
  });

  it("shows contextual roles and renders cron units without transaction links", async () => {
    const sender = "0x73656e6465720000000000000000000000000000";
    explorerTableMocks.getAccountInfo.mockResolvedValue({ index: 1, username: "alice" });
    const transaction = {
      blockHeight: 80,
      createdAt: "2026-06-08T12:01:00.000Z",
      errorMessage: "",
      gasUsed: 1,
      gasWanted: 1,
      hash: "0x7478000000000000000000000000000000000000000000000000000000000000",
      hasSucceeded: true,
      involvement: ["sender", "participant"] as ("sender" | "participant")[],
      messages: [createIndexedMessage("execute")],
      nestedEvents: "[]",
      sender,
      transactionIdx: 0,
      transactionType: "TX" as const,
    };
    const cron = {
      blockHeight: 79,
      createdAt: "2026-06-08T12:00:00.000Z",
      errorMessage: "",
      gasUsed: 1,
      gasWanted: 0,
      hash: "",
      hasSucceeded: true,
      involvement: ["participant"] as ("sender" | "participant")[],
      messages: [],
      nestedEvents: "[]",
      sender: "" as const,
      transactionIdx: 1,
      transactionType: "CRON" as const,
    };

    renderWithQueryClient(<TransactionsTable transactions={[transaction, cron]} />);

    expect(getHeaderLabels()).toEqual([
      "Hash",
      "Block",
      "Age",
      "Sender",
      m["explorer.txs.role"](),
      "Actions",
      "Result",
    ]);
    expect(screen.getAllByText(m["explorer.txs.roles.sender"]())).toHaveLength(2);
    expect(screen.getAllByText(m["explorer.txs.roles.participant"]())).toHaveLength(2);
    await waitFor(() => expect(screen.getByText("alice #1")).toBeInTheDocument());

    const [, cronRow] = getBodyRows();
    const cronCells = rowCells(cronRow);
    expect(cronCells[0]).toBe("—");
    expect(cronCells[3]).toBe("—");
    expect(cronCells[4]).toBe(m["explorer.txs.roles.participant"]());
    expect(cronCells[5]).toBe("—");
    expect(cronRow.querySelector('a[href="/tx/"]')).toBeNull();
    expect(explorerTableMocks.getAccountInfo).not.toHaveBeenCalledWith({ address: "" });
    expect(explorerTableMocks.getContractInfo).not.toHaveBeenCalledWith({ address: "" });
  });

  it("falls back to the raw 0x sender when backend lookups cannot identify it", async () => {
    const unknownSender = "0x756e6b6e6f776e00000000000000000000000000";
    const transactions: IndexedTransaction[] = [
      {
        blockHeight: 1236,
        createdAt: "2026-06-08T12:02:00.000Z",
        errorMessage: "",
        gasUsed: 21_000,
        gasWanted: 50_000,
        hash: "0x756e6b6e6f776e74780000000000000000000000000000000000000000000000",
        hasSucceeded: true,
        messages: [createIndexedMessage("transfer", { senderAddr: unknownSender })],
        nestedEvents: "[]",
        sender: unknownSender,
        transactionIdx: 2,
        transactionType: "TX",
      },
    ];

    renderWithQueryClient(<TransactionsTable transactions={transactions} />);

    await waitFor(() => {
      expect(explorerTableMocks.getAccountInfo).toHaveBeenCalledWith({
        address: unknownSender,
      });
    });
    await waitFor(() => {
      expect(explorerTableMocks.getContractInfo).toHaveBeenCalledWith({
        address: unknownSender,
      });
    });

    const [row] = getBodyRows();
    expect(rowCells(row)[3]).toBe(unknownSender);
    expect(within(row).queryByRole("link", { name: unknownSender })).not.toBeInTheDocument();
  });

  it("renders nothing for empty transaction results", () => {
    const { container } = render(<TransactionsTable transactions={[]} />);

    expect(container).toBeEmptyDOMElement();
  });

  it("maps balances into asset rows with normalized amounts, prices, and market price options", () => {
    render(
      <AssetsTable
        balances={{
          "bridge/usdc": "1250000",
          uatom: "2000000",
        }}
      />,
    );

    expect(getHeaderLabels()).toEqual(["Asset", "Market Price", m["common.available"](), "Total"]);

    const [usdcRow, atomRow] = getBodyRows();
    const usdcCells = rowCells(usdcRow);
    const atomCells = rowCells(atomRow);

    expect(within(usdcRow).getByAltText("USDC")).toHaveAttribute("src", "/images/usdc.svg");
    expect(within(atomRow).getByAltText("ATOM")).toHaveAttribute("src", "/images/atom.svg");
    expect(usdcCells[0]).toBe("USDC");
    expect(usdcCells[1]).toContain("$1");
    expect(usdcCells[2]).toBe("1.25price:bridge/usdc:1.25");
    expect(usdcCells[3]).toBe("1.25price:bridge/usdc:1.25");
    expect(atomCells[0]).toBe("ATOM");
    expect(atomCells[1]).toContain("$2");
    expect(atomCells[2]).toBe("2price:uatom:2");
    expect(atomCells[3]).toBe("2price:uatom:2");
    expect(explorerTableMocks.getPrice).toHaveBeenCalledWith("1.25", "bridge/usdc", {
      format: true,
      formatOptions: {
        currency: "USD",
        language: "en-US",
        mask: 1,
      },
    });
    expect(explorerTableMocks.getPrice).toHaveBeenCalledWith("2", "uatom", {
      format: true,
      formatOptions: {
        currency: "USD",
        language: "en-US",
        mask: 1,
      },
    });
  });

  it("keeps backend zero asset balances visible and priced as zero", () => {
    render(
      <AssetsTable
        balances={{
          "bridge/usdc": "0",
        }}
      />,
    );

    const [row] = getBodyRows();
    const cells = rowCells(row);

    expect(cells[0]).toBe("USDC");
    expect(cells[2]).toBe("0price:bridge/usdc:0");
    expect(cells[3]).toBe("0price:bridge/usdc:0");
    expect(explorerTableMocks.getPrice).toHaveBeenCalledWith("0", "bridge/usdc", {
      format: true,
      formatOptions: {
        currency: "USD",
        language: "en-US",
        mask: 1,
      },
    });
  });

  it("renders nothing when an account has no asset balances", () => {
    const { container } = render(<AssetsTable balances={{}} />);

    expect(container).toBeEmptyDOMElement();
    expect(explorerTableMocks.getPrice).not.toHaveBeenCalled();
  });
});
