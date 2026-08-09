import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import type React from "react";

import {
  resetAppletsKitMocks,
  setAppletsKitUseAppFactory,
  setAppletsKitUseCountdown,
  setAppletsKitUseMediaQueryFactory,
} from "./mocks/applets-kit";

import { Modals } from "@left-curve/applets-kit";

import { Header } from "../src/components/foundation/Header";

const headerMocks = vi.hoisted(() => ({
  account: {
    index: 12,
  } as { index: number } | undefined,
  chainName: "Mainnet",
  isConnected: true,
  isGeoblocked: false,
  isLg: true,
  isSearchBarVisible: false,
  isSidebarVisible: false,
  isUserActive: true,
  pathname: "/trade/BTCUSD",
  setSidebarVisibility: vi.fn(),
  showModal: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  Link: ({ children, to, ...props }: React.PropsWithChildren<{ to?: string }>) => (
    <a href={to} {...props}>
      {children}
    </a>
  ),
  useRouterState: () => ({
    location: {
      pathname: headerMocks.pathname,
    },
  }),
}));

vi.mock("../src/components/foundation/AccountMenu", () => ({
  AccountMenu: () => <div data-testid="account-menu" />,
}));

vi.mock("../src/components/foundation/SearchMenu", () => ({
  SearchMenu: () => <div data-testid="search-menu" />,
}));

vi.mock("../src/components/foundation/TxIndicator", () => ({
  TxIndicator: ({ icon }: { icon: React.ReactNode }) => (
    <div data-testid="tx-indicator">{icon}</div>
  ),
}));

vi.mock("../src/components/foundation/GeoblockBanner", () => ({
  GeoblockBanner: () => <div role="alert">geoblock banner</div>,
}));

vi.mock("../src/components/foundation/TestnetBanner", () => ({
  TestnetBanner: () => <div data-testid="testnet-banner" />,
}));

vi.mock("../src/components/foundation/hooks/useGeoblock", () => ({
  useGeoblock: () => headerMocks.isGeoblocked,
}));

vi.mock("@left-curve/store", () => ({
  useAccount: () => ({
    account: headerMocks.account,
    isConnected: headerMocks.isConnected,
    isUserActive: headerMocks.isUserActive,
  }),
  useConfig: () => ({
    chain: {
      name: headerMocks.chainName,
    },
  }),
}));

describe("Header shell", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    headerMocks.account = {
      index: 12,
    };
    headerMocks.chainName = "Mainnet";
    headerMocks.isConnected = true;
    headerMocks.isGeoblocked = false;
    headerMocks.isLg = true;
    headerMocks.isSearchBarVisible = false;
    headerMocks.isSidebarVisible = false;
    headerMocks.isUserActive = true;
    headerMocks.pathname = "/trade/BTCUSD";
    Object.defineProperty(window, "matchMedia", {
      configurable: true,
      value: vi.fn(() => ({
        matches: false,
      })),
    });
    setAppletsKitUseAppFactory(() => ({
      isSearchBarVisible: headerMocks.isSearchBarVisible,
      isSidebarVisible: headerMocks.isSidebarVisible,
      setSidebarVisibility: headerMocks.setSidebarVisibility,
      showModal: headerMocks.showModal,
    }));
    setAppletsKitUseCountdown({
      days: "1",
      hours: "2",
      minutes: "3",
      seconds: "4",
    });
    setAppletsKitUseMediaQueryFactory(() => ({
      isLg: headerMocks.isLg,
    }));
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    vi.useRealTimers();
  });

  it("renders desktop trade chrome with geoblock and testnet banners from route state", () => {
    headerMocks.isGeoblocked = true;

    render(<Header isScrolled />);

    expect(screen.getByRole("alert")).toHaveTextContent("geoblock banner");
    expect(screen.getByTestId("testnet-banner")).toBeInTheDocument();
    expect(screen.getByTestId("search-menu")).toBeInTheDocument();
    expect(document.querySelector("#trade-buttons")).not.toBeNull();
    expect(screen.getByRole("link", { name: "dango logo" })).toHaveAttribute("href", "/");
  });

  it("routes connected desktop account clicks through sidebar visibility", () => {
    headerMocks.isSidebarVisible = false;

    render(<Header isScrolled={false} />);

    expect(screen.getByText(`${m["common.account"]()} #12`)).toBeInTheDocument();

    fireEvent.click(
      screen.getByRole("button", {
        name: (name) => name.includes(`${m["common.account"]()} #12`),
      }),
    );

    expect(headerMocks.setSidebarVisibility).toHaveBeenCalledWith(true);
    expect(headerMocks.showModal).not.toHaveBeenCalled();
  });

  it("opens authentication for disconnected users and locks mainnet points before campaign start", () => {
    vi.useFakeTimers({
      now: new Date("2026-04-14T12:00:00Z"),
    });
    headerMocks.account = undefined;
    headerMocks.isConnected = false;
    headerMocks.pathname = "/portfolio";

    render(<Header isScrolled={false} />);

    expect(screen.getByRole("button", { name: m["points.campaign"]() })).toBeDisabled();
    expect(screen.queryByRole("link", { name: m["points.campaign"]() })).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: m["common.signin"]() }));

    expect(headerMocks.showModal).toHaveBeenCalledWith(Modals.Authenticate);
    expect(headerMocks.setSidebarVisibility).not.toHaveBeenCalled();
  });

  it("hides search on desktop landing and shows points after campaign start", () => {
    vi.useFakeTimers({
      now: new Date("2026-04-16T12:00:00Z"),
    });
    headerMocks.pathname = "/";

    render(<Header isScrolled={false} />);

    expect(screen.queryByTestId("search-menu")).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: m["points.campaign"]() })).toHaveAttribute(
      "href",
      "/points",
    );
    expect(screen.queryByRole("button", { name: m["points.campaign"]() })).not.toBeInTheDocument();
  });

  it("uses the mobile wallet action and hides desktop-only banners on mobile trade routes", () => {
    headerMocks.isGeoblocked = true;
    headerMocks.isLg = false;
    headerMocks.isUserActive = false;

    render(<Header isScrolled={false} />);

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.queryByTestId("testnet-banner")).not.toBeInTheDocument();
    expect(screen.queryByTestId("search-menu")).not.toBeInTheDocument();
    expect(document.querySelector("#trade-buttons")).not.toBeNull();

    const walletButtons = screen
      .getAllByTestId("tx-indicator")
      .map((indicator) => indicator.closest("button"))
      .filter((button): button is HTMLButtonElement => Boolean(button));
    const mobileWalletButton = walletButtons.find(
      (button) => !button.textContent?.includes(`${m["common.account"]()} #12`),
    );
    expect(mobileWalletButton).toBeDefined();
    expect(mobileWalletButton?.querySelectorAll("svg")).toHaveLength(2);

    fireEvent.click(mobileWalletButton!);

    expect(headerMocks.setSidebarVisibility).toHaveBeenCalledWith(true);
  });
});
