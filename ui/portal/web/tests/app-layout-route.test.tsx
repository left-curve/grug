import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type React from "react";

import {
  resetAppletsKitMocks,
  setAppletsKitUseAppFactory,
  setAppletsKitUseMediaQueryFactory,
  setAppletsKitUseThemeFactory,
} from "./mocks/applets-kit";

import { Modals } from "@left-curve/applets-kit";

import { Route } from "../src/pages/(app)/_app";

const layoutRouteMocks = vi.hoisted(() => ({
  account: {
    address: "0x616c696365000000000000000000000000000000",
  },
  balances: {
    uatom: "1",
  } as Record<string, string> | undefined,
  chainId: "dango-1",
  connector: {
    id: "wallet",
    name: "Wallet",
    type: "wallet",
  },
  isConnected: true,
  isGeoblocked: false,
  isLg: true,
  isSidebarVisible: false,
  modal: {
    modal: null as string | null,
  },
  pathname: "/",
  search: {} as { notice?: "exchange-shutdown"; socketId?: string },
  showModal: vi.fn(),
  theme: "light" as "dark" | "light",
  useBalances: vi.fn(),
  userStatus: "active" as "active" | "pending" | undefined,
}));

vi.mock("@sentry/react", () => ({
  captureException: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  createFileRoute: () => (options: unknown) => ({ options }),
  Outlet: () => <div data-testid="route-outlet" />,
  useRouter: () => ({
    state: {
      location: {
        pathname: layoutRouteMocks.pathname,
      },
    },
  }),
  useSearch: () => layoutRouteMocks.search,
}));

vi.mock("@left-curve/store", () => ({
  useAccount: () => ({
    account: layoutRouteMocks.account,
    connector: layoutRouteMocks.connector,
    isConnected: layoutRouteMocks.isConnected,
    userStatus: layoutRouteMocks.userStatus,
  }),
  useBalances: (params: unknown) => {
    layoutRouteMocks.useBalances(params);
    return {
      data: layoutRouteMocks.balances,
    };
  },
  useConfig: () => ({
    chain: {
      id: layoutRouteMocks.chainId,
    },
  }),
}));

vi.mock("~/components/foundation/Footer", () => ({
  Footer: () => <footer>footer</footer>,
}));

vi.mock("~/components/foundation/Header", () => ({
  Header: ({ isScrolled }: { isScrolled: boolean }) => (
    <header data-scrolled={String(isScrolled)}>header</header>
  ),
}));

vi.mock("~/components/foundation/GeoblockBanner", () => ({
  GeoblockBanner: () => <div role="alert">geoblock banner</div>,
}));

vi.mock("~/components/foundation/NotFound", () => ({
  NotFound: () => <div data-testid="not-found">not found</div>,
}));

vi.mock("~/components/foundation/StatusBadge", () => ({
  StatusBadge: () => <div>status badge</div>,
}));

vi.mock("~/components/foundation/TestnetBanner", () => ({
  TestnetBanner: () => <div>testnet banner</div>,
}));

vi.mock("~/components/foundation/hooks/useGeoblock", () => ({
  useGeoblock: () => layoutRouteMocks.isGeoblocked,
}));

type LayoutRoute = {
  options: {
    component: React.ComponentType;
    validateSearch: {
      parse: (value: unknown) => { notice?: "exchange-shutdown"; socketId?: string };
    };
  };
};

function LayoutComponent() {
  return (Route as unknown as LayoutRoute).options.component;
}

describe("app layout route", () => {
  beforeEach(() => {
    resetAppletsKitMocks();
    layoutRouteMocks.account = {
      address: "0x616c696365000000000000000000000000000000",
    };
    layoutRouteMocks.balances = {
      uatom: "1",
    };
    layoutRouteMocks.chainId = "dango-1";
    layoutRouteMocks.connector = {
      id: "wallet",
      name: "Wallet",
      type: "wallet",
    };
    layoutRouteMocks.isConnected = true;
    layoutRouteMocks.isGeoblocked = false;
    layoutRouteMocks.isLg = true;
    layoutRouteMocks.isSidebarVisible = false;
    layoutRouteMocks.modal = {
      modal: null,
    };
    layoutRouteMocks.pathname = "/";
    layoutRouteMocks.search = {};
    layoutRouteMocks.theme = "light";
    layoutRouteMocks.userStatus = "active";
    document.body.dataset.scrollLockY = "";
    window.history.pushState({}, "", "/");
    Object.defineProperty(window, "scrollY", {
      configurable: true,
      value: 0,
    });
    setAppletsKitUseAppFactory(() => ({
      isSidebarVisible: layoutRouteMocks.isSidebarVisible,
      modal: layoutRouteMocks.modal,
      settings: {
        useSessionKey: false,
      },
      showModal: layoutRouteMocks.showModal,
    }));
    setAppletsKitUseMediaQueryFactory(() => ({
      isLg: layoutRouteMocks.isLg,
    }));
    setAppletsKitUseThemeFactory(() => ({
      theme: layoutRouteMocks.theme,
    }));
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("validates supported route search state", () => {
    const route = Route as unknown as LayoutRoute;

    expect(route.options.validateSearch.parse({})).toEqual({});
    expect(route.options.validateSearch.parse({ notice: "exchange-shutdown" })).toEqual({
      notice: "exchange-shutdown",
    });
    expect(route.options.validateSearch.parse({ socketId: "native-camera-socket" })).toEqual({
      socketId: "native-camera-socket",
    });
    expect(() => route.options.validateSearch.parse({ notice: "maintenance" })).toThrow();
    expect(() => route.options.validateSearch.parse({ socketId: 123 })).toThrow();
  });

  it("opens the winding-down modal from shutdown route state", async () => {
    layoutRouteMocks.search = {
      notice: "exchange-shutdown",
    };
    const Component = LayoutComponent();

    render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.WindingDown);
    });
    expect(layoutRouteMocks.showModal).toHaveBeenCalledTimes(1);
  });

  it("shows account activation on mainnet when the connected user is not active", async () => {
    layoutRouteMocks.userStatus = "pending";
    const Component = LayoutComponent();
    const { rerender } = render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.ActivateAccount);
    });
    expect(layoutRouteMocks.useBalances).toHaveBeenCalledWith({
      address: "0x616c696365000000000000000000000000000000",
    });

    rerender(<Component />);

    expect(layoutRouteMocks.showModal).toHaveBeenCalledTimes(1);
  });

  it("resets the activation modal gate after disconnecting", async () => {
    layoutRouteMocks.userStatus = "pending";
    const Component = LayoutComponent();
    const { rerender } = render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.ActivateAccount);
    });

    layoutRouteMocks.showModal.mockClear();
    layoutRouteMocks.isConnected = false;
    rerender(<Component />);

    await waitFor(() => {
      expect(screen.getByTestId("route-outlet")).toBeInTheDocument();
    });
    expect(layoutRouteMocks.showModal).not.toHaveBeenCalled();

    layoutRouteMocks.isConnected = true;
    rerender(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.ActivateAccount);
    });
  });

  it("uses empty devnet balances as the activation signal", async () => {
    layoutRouteMocks.chainId = "dango-dev-1";
    layoutRouteMocks.balances = {};
    const Component = LayoutComponent();

    const { unmount } = render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.ActivateAccount);
    });

    unmount();
    vi.clearAllMocks();
    layoutRouteMocks.balances = {
      uatom: "1",
    };

    render(<Component />);

    await waitFor(() => {
      expect(screen.getByTestId("route-outlet")).toBeInTheDocument();
    });
    expect(layoutRouteMocks.showModal).not.toHaveBeenCalled();
  });

  it("waits for devnet balances before showing account activation", async () => {
    layoutRouteMocks.chainId = "dango-dev-1";
    layoutRouteMocks.balances = undefined;
    const Component = LayoutComponent();

    const { rerender } = render(<Component />);

    await waitFor(() => {
      expect(screen.getByTestId("route-outlet")).toBeInTheDocument();
    });
    expect(layoutRouteMocks.showModal).not.toHaveBeenCalled();

    layoutRouteMocks.balances = {};
    rerender(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.ActivateAccount);
    });
  });

  it("opens desktop-camera signing from route search state", async () => {
    layoutRouteMocks.search = {
      socketId: "native-camera-socket",
    };
    const Component = LayoutComponent();

    render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(
        Modals.SignWithDesktopFromNativeCamera,
        {
          socketId: "native-camera-socket",
        },
      );
    });
  });

  it("converts auth callbacks into authenticate modals and cleans callback params", async () => {
    window.history.pushState({}, "", "/?auth_callback=auth&ref=42&keep=yes");
    const Component = LayoutComponent();

    render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.Authenticate, {
        referrer: 42,
      });
    });
    expect(window.location.search).toBe("?keep=yes");
  });

  it("preserves backend referrer index zero when converting auth callbacks", async () => {
    window.history.pushState({}, "", "/?auth_callback=auth&ref=0");
    const Component = LayoutComponent();

    render(<Component />);

    await waitFor(() => {
      expect(layoutRouteMocks.showModal).toHaveBeenCalledWith(Modals.Authenticate, {
        referrer: 0,
      });
    });
    expect(window.location.search).toBe("");
  });

  it("drives header scroll state from locked sidebar scroll on trade routes", () => {
    layoutRouteMocks.pathname = "/trade/ETHUSD";
    layoutRouteMocks.isSidebarVisible = true;
    document.body.dataset.scrollLockY = "2";
    const Component = LayoutComponent();

    render(<Component />);

    expect(screen.getByRole("banner")).toHaveAttribute("data-scrolled", "true");
  });

  it("uses the trade route scroll threshold when the window scrolls", () => {
    layoutRouteMocks.pathname = "/trade/ETHUSD";
    const Component = LayoutComponent();

    render(<Component />);

    expect(screen.getByRole("banner")).toHaveAttribute("data-scrolled", "false");

    Object.defineProperty(window, "scrollY", {
      configurable: true,
      value: 2,
    });
    act(() => {
      window.dispatchEvent(new Event("scroll"));
    });

    expect(screen.getByRole("banner")).toHaveAttribute("data-scrolled", "true");
  });

  it("shows the geoblock banner only for geoblocked mobile trade routes", () => {
    layoutRouteMocks.isGeoblocked = true;
    layoutRouteMocks.isLg = false;
    layoutRouteMocks.pathname = "/trade/ETHUSD";
    const Component = LayoutComponent();

    const { rerender } = render(<Component />);

    expect(screen.getByRole("alert")).toHaveTextContent("geoblock banner");

    layoutRouteMocks.pathname = "/";
    rerender(<Component />);

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();

    layoutRouteMocks.pathname = "/trade/ETHUSD";
    layoutRouteMocks.isLg = true;
    rerender(<Component />);

    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });
});
