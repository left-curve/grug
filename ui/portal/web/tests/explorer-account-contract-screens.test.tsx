import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import type React from "react";

import { resetAppletsKitMocks, setAppletsKitUseApp } from "./mocks/applets-kit";

import { AccountExplorer } from "../src/components/explorer/AccountExplorer";
import { ContractExplorer } from "../src/components/explorer/ContractExplorer";
import { createTestQueryClient } from "./utils/query-client";

const explorerScreenMocks = vi.hoisted(() => ({
  calculateBalance: vi.fn(),
  getAccountInfo: vi.fn(),
  getContractInfo: vi.fn(),
  navigate: vi.fn(),
  useExplorerAccount: vi.fn(),
  useExplorerContract: vi.fn(),
  useExplorerTransactionsByAddress: vi.fn(),
}));

vi.mock("@left-curve/store", () => ({
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
  }),
  useExplorerAccount: explorerScreenMocks.useExplorerAccount,
  useExplorerContract: explorerScreenMocks.useExplorerContract,
  useExplorerTransactionsByAddress: explorerScreenMocks.useExplorerTransactionsByAddress,
  usePrices: () => ({
    calculateBalance: explorerScreenMocks.calculateBalance,
  }),
  usePublicClient: () => ({
    getAccountInfo: explorerScreenMocks.getAccountInfo,
    getContractInfo: explorerScreenMocks.getContractInfo,
  }),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => explorerScreenMocks.navigate,
}));

vi.mock("../src/components/foundation/AccountCard", () => ({
  AccountCard: ({
    account,
    balance,
    isUserActive,
  }: {
    account: { address: string };
    balance: string;
    isUserActive?: boolean;
  }) => (
    <div>
      account-card:{account.address}:{balance}:{String(isUserActive)}
    </div>
  ),
}));

vi.mock("../src/components/foundation/ContractCard", () => ({
  ContractCard: ({ address, balance }: { address: string; balance: string }) => (
    <div>
      contract-card:{address}:{balance}
    </div>
  ),
}));

vi.mock("../src/components/explorer/AssetsTable", () => ({
  AssetsTable: ({
    balances,
    classNames,
  }: {
    balances: Record<string, string>;
    classNames?: { base?: string };
  }) => (
    <div>
      assets-table:
      {Object.entries(balances)
        .map(([denom, amount]) => `${denom}:${amount}`)
        .join("|")}
      :{classNames?.base ?? ""}
    </div>
  ),
}));

vi.mock("../src/components/explorer/TransactionsTable", () => ({
  TransactionsTable: ({
    pagination,
    transactions,
  }: {
    pagination?: { hasNextPage: boolean; hasPreviousPage: boolean; isLoading: boolean };
    transactions: unknown[];
  }) => (
    <div>
      transactions-table:{transactions.length}:{String(pagination?.hasNextPage)}:
      {String(pagination?.hasPreviousPage)}:{String(pagination?.isLoading)}
    </div>
  ),
}));

vi.mock("@left-curve/foundation", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@left-curve/foundation")>()),
  useApp: () => ({
    settings: {
      formatNumberOptions: {
        language: "en-US",
        mask: 1,
      },
    },
  }),
}));

const accountAddress = "0x6163636f756e7400000000000000000000000000";
const adminAddress = "0x61646d696e000000000000000000000000000000";
const contractAddress = "0x636f6e7472616374000000000000000000000000";
const codeHash = "0x636f646568617368000000000000000000000000000000000000000000000000";

const balances = {
  "bridge/usdc": "1250000",
  uatom: "2000000",
};

const accountData = {
  address: accountAddress,
  admin: adminAddress,
  balances,
  codeHash,
  index: 7,
  perps: {
    orders: {
      "order-1": {
        createdAt: "1760000000",
        limitPrice: "31000",
        pairId: "perp/btcusd",
        reduceOnly: true,
        reservedMargin: "100",
        size: "-0.5",
      },
    },
    userState: {
      availableMargin: "800",
      equity: "1100",
      margin: "1000",
      positions: {
        "perp/btcusd": {
          entryPrice: "30000",
          liquidationPrice: "25000",
          size: "0.5",
          unrealizedPnl: "125",
        },
      },
      reservedMargin: "200",
      vaultShares: "10",
    },
    vaultState: {
      equity: "1000",
      shareSupply: "100",
    },
  },
};

const transactionsResponse = {
  data: {
    nodes: [{ hash: "0x7478680000000000000000000000000000000000000000000000000000000000" }],
  },
  isLoading: false,
  pagination: {
    goNext: vi.fn(),
    goPrev: vi.fn(),
    hasNextPage: true,
    hasPreviousPage: false,
  },
};

function renderAccountExplorer(
  children: React.ReactNode,
  data: typeof accountData | null = accountData,
) {
  explorerScreenMocks.useExplorerAccount.mockReturnValue({
    data,
    isLoading: false,
  });

  renderWithQueryClient(<AccountExplorer address={accountAddress}>{children}</AccountExplorer>);
}

function renderContractExplorer(
  children: React.ReactNode,
  data: { admin: string | null; balances: Record<string, string>; codeHash: string } | null = {
    admin: null,
    balances,
    codeHash,
  },
) {
  explorerScreenMocks.useExplorerContract.mockReturnValue({
    data,
    isLoading: false,
  });

  renderWithQueryClient(<ContractExplorer address={contractAddress}>{children}</ContractExplorer>);
}

function renderWithQueryClient(component: React.ReactNode) {
  const queryClient = createTestQueryClient();

  return render(<QueryClientProvider client={queryClient}>{component}</QueryClientProvider>);
}

function rowText(row: HTMLElement) {
  return within(row)
    .getAllByRole("cell")
    .map((cell) => cell.textContent);
}

function getTableRows() {
  return screen.getAllByRole("row").filter((row) => within(row).queryAllByRole("cell").length > 0);
}

function copyButtonBeside(text: string) {
  const textContainer = screen.getByText(text).closest("p");

  expect(textContainer).not.toBeNull();

  return within(textContainer as HTMLElement).getByRole("button");
}

function expectLabeledValue(label: string, value: string) {
  const field = screen.getByText(label).closest("div");

  expect(field).not.toBeNull();
  expect(field).toHaveTextContent(value);
}

describe("account and contract explorer screens", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    setAppletsKitUseApp({
      settings: {
        formatNumberOptions: {
          language: "en-US",
          mask: 1,
        },
      },
    });
    explorerScreenMocks.calculateBalance.mockReturnValue("$45.00");
    explorerScreenMocks.getAccountInfo.mockImplementation(async ({ address }) => {
      if (address === adminAddress) return { index: 8, username: "admin" };
      return null;
    });
    explorerScreenMocks.getContractInfo.mockResolvedValue(null);
    explorerScreenMocks.useExplorerTransactionsByAddress.mockReturnValue(transactionsResponse);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: {
        writeText: vi.fn(),
      },
    });
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders account details from backend account, contract, and balance data", async () => {
    renderAccountExplorer(<AccountExplorer.Details />);

    expect(screen.getByText(`account-card:${accountAddress}:$45.00:true`)).toBeInTheDocument();
    expect(explorerScreenMocks.calculateBalance).toHaveBeenCalledWith(balances, {
      format: true,
      formatOptions: {
        currency: "usd",
        language: "en-US",
        mask: 1,
      },
    });
    expect(screen.getByText(m["explorer.contracts.details.contractDetails"]())).toBeInTheDocument();
    expect(screen.getByText(m["explorer.contracts.details.codeHash"]())).toBeInTheDocument();
    expect(screen.getByText(codeHash)).toBeInTheDocument();
    fireEvent.pointerUp(copyButtonBeside(codeHash));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(codeHash);

    expect(screen.getByText("$45.00 (2 Assets)")).toHaveClass("bg-surface-tertiary-green");

    const adminLink = await screen.findByRole("link", { name: /admin #8/ });

    fireEvent.click(adminLink);
    expect(explorerScreenMocks.navigate).toHaveBeenCalledWith({
      to: `https://explorer.example/account/${adminAddress}`,
    });
  });

  it("hands account assets and transactions to the shared explorer tables", () => {
    renderAccountExplorer(
      <>
        <AccountExplorer.Assets />
        <AccountExplorer.Transactions />
      </>,
    );

    expect(screen.getByText("assets-table:bridge/usdc:1250000|uatom:2000000:")).toBeInTheDocument();
    expect(explorerScreenMocks.useExplorerTransactionsByAddress).toHaveBeenCalledWith(
      accountAddress,
      true,
    );
    expect(screen.getByText("transactions-table:1:true:false:false")).toBeInTheDocument();
  });

  it("maps account perps balances, positions, and orders into visible tables", () => {
    renderAccountExplorer(
      <>
        <AccountExplorer.PerpsBalance />
        <AccountExplorer.PerpsPositions />
        <AccountExplorer.PerpsOrders />
      </>,
    );

    expectLabeledValue(m["explorer.accounts.perps.balance.margin"](), "$1,000.00");
    expectLabeledValue(m["explorer.accounts.perps.balance.equity"](), "$1,100.00");
    expectLabeledValue(m["explorer.accounts.perps.balance.vaultShares"](), "$0.01");

    const rows = getTableRows();

    expect(rowText(rows[0])).toEqual([
      m["explorer.accounts.perps.positions.pair"](),
      m["explorer.accounts.perps.positions.side"](),
      m["explorer.accounts.perps.positions.size"](),
      m["explorer.accounts.perps.positions.entryPrice"](),
      m["explorer.accounts.perps.positions.unrealizedPnl"](),
      m["explorer.accounts.perps.positions.liqPrice"](),
    ]);
    expect(rowText(rows[2])).toEqual([
      m["explorer.accounts.perps.orders.pair"](),
      m["explorer.accounts.perps.orders.side"](),
      m["explorer.accounts.perps.orders.size"](),
      m["explorer.accounts.perps.orders.limitPrice"](),
      m["explorer.accounts.perps.orders.reduceOnly"](),
      m["explorer.accounts.perps.orders.reservedMargin"](),
      m["explorer.accounts.perps.orders.createdAt"](),
    ]);
    expect(rowText(rows[1])).toEqual([
      "BTCUSD",
      m["explorer.accounts.perps.positions.long"](),
      "0.5",
      "$30,000",
      "+$125.00",
      "$25,000",
    ]);
    expect(rowText(rows[3])).toEqual([
      "BTCUSD",
      m["explorer.accounts.perps.orders.short"](),
      "0.5",
      "$31,000",
      "Yes",
      "$100.00",
      new Date(1760000000 * 1000).toLocaleString(),
    ]);
  });

  it("renders backend perps edge fields without inventing missing values", () => {
    renderAccountExplorer(
      <>
        <AccountExplorer.PerpsBalance />
        <AccountExplorer.PerpsPositions />
        <AccountExplorer.PerpsOrders />
      </>,
      {
        ...accountData,
        perps: {
          orders: {},
          userState: {
            ...accountData.perps.userState,
            availableMargin: null,
            equity: null,
            positions: {
              "perp/ethusd": {
                entryPrice: "1800",
                liquidationPrice: null,
                size: "-2",
                unrealizedPnl: "-12.5",
              },
              "perp/solusd": {
                entryPrice: "150",
                liquidationPrice: "10",
                size: "3",
                unrealizedPnl: null,
              },
            },
          },
          vaultState: null,
        },
      } as typeof accountData,
    );

    expectLabeledValue(m["explorer.accounts.perps.balance.equity"](), "$0.00");
    expectLabeledValue(m["explorer.accounts.perps.balance.availableMargin"](), "$0.00");
    expectLabeledValue(m["explorer.accounts.perps.balance.vaultShares"](), "$0.00");

    const rows = getTableRows();

    expect(rows).toHaveLength(3);
    expect(rowText(rows[0])).toEqual([
      m["explorer.accounts.perps.positions.pair"](),
      m["explorer.accounts.perps.positions.side"](),
      m["explorer.accounts.perps.positions.size"](),
      m["explorer.accounts.perps.positions.entryPrice"](),
      m["explorer.accounts.perps.positions.unrealizedPnl"](),
      m["explorer.accounts.perps.positions.liqPrice"](),
    ]);

    const shortPosition = rowText(rows[1]).join("|");
    expect(shortPosition).toContain("ETHUSD");
    expect(shortPosition).toContain(m["explorer.accounts.perps.positions.short"]());
    expect(shortPosition).toContain("2");
    expect(shortPosition).toMatch(/\$1,800(?:\.00)?/);
    expect(shortPosition).toContain("-$12.50");
    expect(shortPosition).toContain("N/A");

    const longPosition = rowText(rows[2]).join("|");
    expect(longPosition).toContain("SOLUSD");
    expect(longPosition).toContain(m["explorer.accounts.perps.positions.long"]());
    expect(longPosition).toContain("3");
    expect(longPosition).toContain("N/A");
    expect(longPosition).toMatch(/\$10(?:\.00)?/);
    expect(
      screen.queryByText(m["explorer.accounts.perps.orders.reduceOnly"]()),
    ).not.toBeInTheDocument();
  });

  it("renders backend zero perps position fields as values instead of missing data", () => {
    renderAccountExplorer(<AccountExplorer.PerpsPositions />, {
      ...accountData,
      perps: {
        ...accountData.perps,
        userState: {
          ...accountData.perps.userState,
          positions: {
            "perp/btcusd": {
              entryPrice: "0",
              liquidationPrice: "0",
              size: "0.25",
              unrealizedPnl: "0",
            },
          },
        },
      },
    } as typeof accountData);

    const rows = getTableRows();

    expect(rowText(rows[1])).toEqual([
      "BTCUSD",
      m["explorer.accounts.perps.positions.long"](),
      "0.25",
      "$0.00",
      "+$0.00",
      "$0.00",
    ]);
  });

  it("renders backend zero perps order fields as values", () => {
    renderAccountExplorer(<AccountExplorer.PerpsOrders />, {
      ...accountData,
      perps: {
        ...accountData.perps,
        orders: {
          "genesis-order": {
            createdAt: "0",
            limitPrice: "0",
            pairId: "perp/btcusd",
            reduceOnly: false,
            reservedMargin: "0",
            size: "0.25",
          },
        },
      },
    } as typeof accountData);

    const rows = getTableRows();

    expect(rowText(rows[1])).toEqual([
      "BTCUSD",
      m["explorer.accounts.perps.orders.long"](),
      "0.25",
      "$0.00",
      "No",
      "$0.00",
      new Date(0).toLocaleString(),
    ]);
  });

  it("renders the account not-found branch with the searched 0x address", () => {
    renderAccountExplorer(<AccountExplorer.NotFound />, null);

    expect(screen.getByText(m["explorer.accounts.notFound.title"]())).toBeInTheDocument();
    expect(screen.getByText((content) => content.includes(accountAddress))).toBeInTheDocument();
  });

  it("renders contract details, assets, transactions, and not-found states", () => {
    renderContractExplorer(
      <>
        <ContractExplorer.Details />
        <ContractExplorer.Assets />
        <ContractExplorer.Transactions />
      </>,
    );

    expect(screen.getByText(`contract-card:${contractAddress}:$45.00`)).toBeInTheDocument();
    expect(screen.getByText(codeHash)).toBeInTheDocument();
    expect(screen.getByText("None")).toBeInTheDocument();
    expect(screen.getByText("$45.00 (2 Assets)")).toHaveClass("bg-surface-tertiary-green");
    expect(screen.getByText("assets-table:bridge/usdc:1250000|uatom:2000000:")).toBeInTheDocument();
    expect(explorerScreenMocks.useExplorerTransactionsByAddress).toHaveBeenCalledWith(
      contractAddress,
      true,
    );
    expect(screen.getByText("transactions-table:1:true:false:false")).toBeInTheDocument();

    cleanup();
    vi.clearAllMocks();
    explorerScreenMocks.useExplorerTransactionsByAddress.mockReturnValue(transactionsResponse);
    renderContractExplorer(<ContractExplorer.NotFound />, null);

    expect(screen.getByText(m["explorer.contracts.notFound.title"]())).toBeInTheDocument();
    expect(screen.getByText((content) => content.includes(contractAddress))).toBeInTheDocument();
  });

  it("keeps backend zero contract balances visible in details and tables", () => {
    const zeroBalances = {
      "bridge/usdc": "0",
      uatom: "0",
    };

    explorerScreenMocks.calculateBalance.mockReturnValueOnce("$0.00");
    explorerScreenMocks.useExplorerTransactionsByAddress.mockReturnValue({
      ...transactionsResponse,
      data: {
        nodes: [],
      },
      pagination: {
        ...transactionsResponse.pagination,
        hasNextPage: false,
      },
    });

    renderContractExplorer(
      <>
        <ContractExplorer.Details />
        <ContractExplorer.Assets />
        <ContractExplorer.Transactions />
      </>,
      {
        admin: null,
        balances: zeroBalances,
        codeHash,
      },
    );

    expect(screen.getByText(`contract-card:${contractAddress}:$0.00`)).toBeInTheDocument();
    expect(explorerScreenMocks.calculateBalance).toHaveBeenCalledWith(zeroBalances, {
      format: true,
      formatOptions: {
        currency: "usd",
        language: "en-US",
        mask: 1,
      },
    });
    expect(screen.getByText("$0.00 (2 Assets)")).toHaveClass("bg-surface-tertiary-green");
    expect(screen.getByText("assets-table:bridge/usdc:0|uatom:0:")).toBeInTheDocument();
    expect(explorerScreenMocks.useExplorerTransactionsByAddress).toHaveBeenCalledWith(
      contractAddress,
      true,
    );
    expect(screen.getByText("transactions-table:0:false:false:false")).toBeInTheDocument();
  });

  it("renders backend contract admin and balance details when an admin is present", () => {
    renderContractExplorer(<ContractExplorer.Details />, {
      admin: adminAddress,
      balances,
      codeHash,
    });

    expect(screen.getByText(`contract-card:${contractAddress}:$45.00`)).toBeInTheDocument();
    expect(explorerScreenMocks.calculateBalance).toHaveBeenCalledWith(balances, {
      format: true,
      formatOptions: {
        currency: "usd",
        language: "en-US",
        mask: 1,
      },
    });
    expect(screen.getByText(m["explorer.contracts.details.admin"]())).toBeInTheDocument();
    expect(screen.getByText(adminAddress)).toBeInTheDocument();
    expect(screen.getByText("$45.00 (2 Assets)")).toHaveClass("bg-surface-tertiary-green");

    fireEvent.pointerUp(copyButtonBeside(codeHash));
    expect(navigator.clipboard.writeText).toHaveBeenCalledWith(codeHash);
  });
});
