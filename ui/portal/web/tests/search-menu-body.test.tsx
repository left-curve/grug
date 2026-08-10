import { QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import type React from "react";
import type { SearchBarResult } from "@left-curve/store";

import {
  resetAppletsKitMocks,
  setAppletsKitUseApp,
  setAppletsKitUseClickAwayFactory,
  setAppletsKitUseMediaQuery,
} from "./mocks/applets-kit";

import { SearchMenu } from "../src/components/foundation/SearchMenu";
import { createTestQueryClient } from "./utils/query-client";

const searchMenuMocks = vi.hoisted(() => ({
  addFavApplet: vi.fn(),
  hideMenu: vi.fn(),
  navigate: vi.fn(),
  removeFavApplet: vi.fn(),
}));

type AppletFixture = {
  description: string;
  id: string;
  img: string;
  isDisabled?: boolean;
  path: string;
  title: string;
};

const tradeApplet: AppletFixture = {
  description: "Trade perpetuals",
  id: "trade",
  img: "/trade.svg",
  path: "/trade/BTCUSDC",
  title: "Trade",
};

const bridgeApplet: AppletFixture = {
  description: "Move funds",
  id: "bridge",
  img: "/bridge.svg",
  path: "/bridge",
  title: "Bridge",
};

const blockHash = "0x626c6f636b000000000000000000000000000000000000000000000000000000";
const txHash = "0x7478686173680000000000000000000000000000000000000000000000000000";
const accountAddress = "0x6163636f756e7400000000000000000000000000";
const contractAddress = "0x636f6e7472616374000000000000000000000000";

vi.mock("framer-motion", () => ({
  AnimatePresence: ({ children }: React.PropsWithChildren) => <>{children}</>,
  motion: {
    div: ({
      animate: _animate,
      children,
      exit: _exit,
      initial: _initial,
      transition: _transition,
      variants: _variants,
      ...props
    }: React.HTMLAttributes<HTMLDivElement> & {
      animate?: unknown;
      exit?: unknown;
      initial?: unknown;
      transition?: unknown;
      variants?: unknown;
    }) => <div {...props}>{children}</div>,
  },
}));

vi.mock("cmdk", () => ({
  Command: {
    Empty: ({ children }: React.PropsWithChildren) => <div>{children}</div>,
    Group: ({ children, heading }: React.PropsWithChildren<{ heading: string }>) => (
      <section aria-label={heading}>
        <h2>{heading}</h2>
        {children}
      </section>
    ),
    Item: ({
      children,
      disabled,
      onSelect,
      value,
    }: React.PropsWithChildren<{
      disabled?: boolean;
      onSelect?: () => void;
      value?: string;
    }>) => (
      <div
        aria-disabled={disabled || undefined}
        data-command-item="true"
        data-value={value}
        onClick={disabled ? undefined : onSelect}
      >
        {children}
      </div>
    ),
    List: ({ children }: React.PropsWithChildren) => <div>{children}</div>,
  },
}));

vi.mock("@tanstack/react-router", () => ({
  useLocation: () => ({ pathname: "/" }),
  useNavigate: () => searchMenuMocks.navigate,
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
        accountPage: "https://explorer.example/account/$" + "{address}",
        contractPage: "https://explorer.example/contract/$" + "{address}",
      },
    },
  }),
  useFavApplets: () => ({
    addFavApplet: searchMenuMocks.addFavApplet,
    favApplets: ["trade"],
    removeFavApplet: searchMenuMocks.removeFavApplet,
  }),
  usePublicClient: () => ({
    getAccountInfo: vi.fn(),
    getContractInfo: vi.fn().mockResolvedValue({
      label: "dex",
    }),
  }),
  useSearchBar: vi.fn(),
}));

function renderBody({
  allApplets = [bridgeApplet],
  isLoading = false,
  isSearching = true,
  searchResult,
}: {
  allApplets?: AppletFixture[];
  isLoading?: boolean;
  isSearching?: boolean;
  searchResult: SearchBarResult;
}) {
  return render(
    <QueryClientProvider client={createTestQueryClient()}>
      <SearchMenu.Body
        allApplets={allApplets as never}
        hideMenu={searchMenuMocks.hideMenu}
        isLoading={isLoading}
        isSearching={isSearching}
        isVisible
        searchResult={searchResult}
      />
    </QueryClientProvider>,
  );
}

function emptySearchResult(overrides: Partial<SearchBarResult> = {}): SearchBarResult {
  return {
    applets: [],
    contracts: [],
    txs: [],
    ...overrides,
  };
}

function clickResult(text: string) {
  const target =
    screen.queryAllByText(text)[0]?.closest('[data-command-item="true"]') ??
    document.querySelector(`[data-value="${text}"]`);
  if (!target) throw new Error(`Expected selectable result for ${text}`);
  fireEvent.click(target);
}

describe("search menu body", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    setAppletsKitUseApp({
      isSearchBarVisible: true,
      setSearchBarVisibility: vi.fn(),
    });
    setAppletsKitUseClickAwayFactory(() => undefined);
    setAppletsKitUseMediaQuery({
      isLg: true,
      isMd: true,
    });
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("renders all search result groups and routes selections to their detail pages", () => {
    const searchResult = emptySearchResult({
      account: {
        address: accountAddress,
        index: 8,
        owner: 3,
        username: "alice",
      },
      applets: [tradeApplet as never],
      block: {
        appHash: "app-hash",
        blockHeight: 42,
        createdAt: "2026-06-08T12:00:00Z",
        cronsOutcomes: "{}",
        hash: blockHash,
        transactions: [],
      },
      contracts: [
        {
          address: contractAddress,
          codeHash: "0x636f646500000000000000000000000000000000000000000000000000000000",
          label: "dex",
        },
      ],
      txs: [
        {
          blockHeight: 42,
          createdAt: "2026-06-08T12:01:00Z",
          errorMessage: "",
          gasUsed: 12,
          gasWanted: 20,
          hasSucceeded: true,
          hash: txHash,
          messages: [],
          nestedEvents: "[]",
          sender: accountAddress,
          transactionIdx: 0,
          transactionType: "TX",
        },
      ],
      user: {
        accounts: {
          8: accountAddress,
        },
        index: 3,
        keys: {},
        name: "alice",
      },
    });

    renderBody({ searchResult });

    expect(screen.getByRole("region", { name: "Applets" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Block" })).toBeInTheDocument();
    expect(
      screen.getByRole("region", { name: m["explorer.block.details.transactions"]() }),
    ).toBeInTheDocument();
    expect(screen.getByRole("region", { name: m["common.accounts"]() })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Users" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Contracts" })).toBeInTheDocument();

    clickResult("Trade");
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({ to: "/trade/BTCUSDC" });
    expect(searchMenuMocks.hideMenu).toHaveBeenCalledTimes(1);

    clickResult("#42 Block");
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({ to: "/block/42" });

    clickResult(txHash);
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({ to: `/tx/${txHash}` });

    clickResult("alice #8");
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({ to: `/account/${accountAddress}` });

    clickResult("alice");
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({ to: "/user/alice" });

    clickResult(contractAddress);
    expect(searchMenuMocks.navigate).toHaveBeenLastCalledWith({
      to: `/contract/${contractAddress}`,
    });
    expect(searchMenuMocks.hideMenu).toHaveBeenCalledTimes(6);
  });

  it("preserves backend block height zero when routing block search results", () => {
    const searchResult = emptySearchResult({
      block: {
        appHash: "genesis-app-hash",
        blockHeight: 0,
        createdAt: "2026-06-08T12:00:00Z",
        cronsOutcomes: "{}",
        hash: "0x67656e657369732d626c6f636b000000000000000000000000000000000000",
        transactions: [],
      },
    });

    renderBody({ searchResult });

    clickResult("#0 Block");

    expect(searchMenuMocks.navigate).toHaveBeenCalledWith({ to: "/block/0" });
    expect(searchMenuMocks.hideMenu).toHaveBeenCalledOnce();
  });

  it("preserves backend block height zero in transaction search rows", () => {
    const genesisTxHash = "0x67656e657369732d747800000000000000000000000000000000000000000000";
    const searchResult = emptySearchResult({
      txs: [
        {
          blockHeight: 0,
          createdAt: "2026-06-08T12:01:00Z",
          errorMessage: "",
          gasUsed: 0,
          gasWanted: 0,
          hasSucceeded: true,
          hash: genesisTxHash,
          messages: [],
          nestedEvents: "[]",
          sender: accountAddress,
          transactionIdx: 0,
          transactionType: "TX",
        },
      ],
    });

    renderBody({ searchResult });

    expect(screen.getByText("Block: #0")).toBeInTheDocument();

    clickResult(genesisTxHash);

    expect(searchMenuMocks.navigate).toHaveBeenCalledWith({ to: `/tx/${genesisTxHash}` });
    expect(searchMenuMocks.hideMenu).toHaveBeenCalledOnce();
  });

  it("preserves backend account index zero in account search rows", () => {
    const searchResult = emptySearchResult({
      account: {
        address: accountAddress,
        index: 0,
        owner: 3,
        username: "",
      },
    });

    renderBody({ searchResult });

    expect(screen.getByText("Account #0")).toBeInTheDocument();

    clickResult("Account #0");

    expect(searchMenuMocks.navigate).toHaveBeenCalledWith({ to: `/account/${accountAddress}` });
    expect(searchMenuMocks.hideMenu).toHaveBeenCalledOnce();
  });

  it("shows favorite and non-favorite applets when idle and lets users update favorites", () => {
    renderBody({
      isSearching: false,
      searchResult: emptySearchResult({
        applets: [tradeApplet as never],
      }),
    });

    expect(screen.getByRole("region", { name: "Favorite Applets" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Applets" })).toBeInTheDocument();
    expect(screen.getByText("Trade perpetuals")).toBeInTheDocument();
    expect(screen.getByText("Move funds")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: m["common.starToggle.remove"]() }));
    expect(searchMenuMocks.removeFavApplet).toHaveBeenCalledWith(tradeApplet);

    fireEvent.click(screen.getByRole("button", { name: m["common.starToggle.add"]() }));
    expect(searchMenuMocks.addFavApplet).toHaveBeenCalledWith(bridgeApplet);
  });

  it("keeps disabled applets visible without allowing selection or favorite changes", () => {
    const disabledTradeApplet = {
      ...tradeApplet,
      isDisabled: true,
    };

    renderBody({
      allApplets: [],
      searchResult: emptySearchResult({
        applets: [disabledTradeApplet as never],
      }),
    });

    const row = screen.getByText("Trade perpetuals").closest('[data-command-item="true"]');
    expect(row).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByAltText("Trade")).toHaveClass("grayscale");
    expect(screen.getByRole("button", { name: m["common.starToggle.remove"]() })).toBeDisabled();

    clickResult("Trade");

    expect(searchMenuMocks.navigate).not.toHaveBeenCalled();
    expect(searchMenuMocks.hideMenu).not.toHaveBeenCalled();
    expect(searchMenuMocks.removeFavApplet).not.toHaveBeenCalled();
  });

  it("renders loading and empty states with localized copy", () => {
    const { container } = renderBody({
      isLoading: true,
      searchResult: emptySearchResult(),
    });

    expect(container.querySelector(".animate-spinner-ease-spin")).toBeInTheDocument();

    cleanup();

    renderBody({
      isLoading: false,
      searchResult: emptySearchResult(),
    });

    expect(screen.getByText(m["searchBar.noResult"]())).toBeInTheDocument();
  });
});
