import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

import type React from "react";

import { resetAppletsKitMocks, setAppletsKitUseHeaderHeight } from "./mocks/applets-kit";

const routeMocks = vi.hoisted(() => ({
  navigate: vi.fn(),
  params: {
    ticker: "ETHUSD",
  },
  redirect: vi.fn((options: unknown) => ({
    options,
    type: "redirect",
  })),
  search: {} as {
    action?: "buy" | "sell";
    order_type?: "limit" | "market";
  },
}));

vi.mock("@tanstack/react-router", () => ({
  createFileRoute: (routePath: string) => (options: unknown) => ({
    options,
    routePath,
  }),
  createLazyFileRoute: (routePath: string) => (options: unknown) => ({
    options,
    routePath,
    useParams: () => routeMocks.params,
    useSearch: () => routeMocks.search,
  }),
  redirect: routeMocks.redirect,
  useNavigate: () => routeMocks.navigate,
}));

vi.mock("@left-curve/store", () => ({
  useConfig: () => ({
    coins: {
      bySymbol: {
        BTC: {
          denom: "bridge/btc",
        },
        ETH: {
          denom: "bridge/eth",
        },
        USDC: {
          denom: "bridge/usdc",
        },
      },
    },
  }),
}));

vi.mock("~/components/dex/components/ProTrade", () => {
  type ProTradeProps = {
    action: "buy" | "sell";
    children: React.ReactNode;
    onChangeAction: (action: "buy" | "sell") => void;
    onChangeOrderType: (orderType: "limit" | "market") => void;
    onChangeTicker: (ticker: string) => void;
    orderType: "limit" | "market";
    pair: {
      id: string;
      ticker: string;
    };
  };

  const ProTrade = ({
    action,
    children,
    onChangeAction,
    onChangeOrderType,
    onChangeTicker,
    orderType,
    pair,
  }: ProTradeProps) => (
    <section
      data-action={action}
      data-order-type={orderType}
      data-perps-pair-id={pair.id}
      data-ticker={pair.ticker}
      data-testid="pro-trade"
    >
      <button onClick={() => onChangeTicker("BTCUSD")} type="button">
        change pair
      </button>
      <button onClick={() => onChangeAction(action === "buy" ? "sell" : "buy")} type="button">
        change action
      </button>
      <button
        onClick={() => onChangeOrderType(orderType === "market" ? "limit" : "market")}
        type="button"
      >
        change order type
      </button>
      {children}
    </section>
  );

  ProTrade.Header = () => <div data-testid="trade-header" />;
  ProTrade.Chart = () => <div data-testid="trade-chart" />;
  ProTrade.OrderBook = () => <div data-testid="trade-order-book" />;
  ProTrade.History = () => <div data-testid="trade-history" />;
  ProTrade.TradeMenu = () => <div data-testid="trade-menu" />;

  return {
    ProTrade,
  };
});

type TradeRoute = {
  options: {
    beforeLoad: () => Promise<void>;
    validateSearch: {
      parse: (search: unknown) => {
        action: "buy" | "sell";
        order_type: "limit" | "market";
      };
    };
  };
};

type TradeIndexRoute = {
  options: {
    beforeLoad: () => Promise<void>;
  };
};

type TradeLazyRoute = {
  options: {
    component: React.ComponentType;
  };
};

let tradeRoutePromise: Promise<TradeRoute> | undefined;
let tradeLazyRoutePromise: Promise<TradeLazyRoute> | undefined;
let tradeIndexRoutePromise: Promise<TradeIndexRoute> | undefined;

async function loadTradeRoute() {
  routeMocks.redirect.mockClear();

  tradeRoutePromise ??= import("../src/pages/(app)/_app.trade.$ticker").then(
    ({ Route }) => Route as unknown as TradeRoute,
  );
  return tradeRoutePromise;
}

async function loadTradeLazyRoute() {
  routeMocks.navigate.mockClear();

  tradeLazyRoutePromise ??= import("../src/pages/(app)/_app.trade.$ticker.lazy").then(
    ({ Route }) => Route as unknown as TradeLazyRoute,
  );
  return tradeLazyRoutePromise;
}

async function loadTradeIndexRoute() {
  routeMocks.redirect.mockClear();

  tradeIndexRoutePromise ??= import("../src/pages/(app)/_app.trade.index").then(
    ({ Route }) => Route as unknown as TradeIndexRoute,
  );
  return tradeIndexRoutePromise;
}

function setLazyRouteState({
  ticker = "ETHUSD",
  search = {},
}: {
  ticker?: string;
  search?: typeof routeMocks.search;
} = {}) {
  routeMocks.params = {
    ticker,
  };
  routeMocks.search = search;
}

async function expectRedirect(promise: Promise<unknown>) {
  await expect(promise).rejects.toMatchObject({
    type: "redirect",
  });
  return routeMocks.redirect.mock.results.at(-1)?.value;
}

describe("trade routes", () => {
  beforeAll(async () => {
    await Promise.all([loadTradeRoute(), loadTradeLazyRoute(), loadTradeIndexRoute()]);
  }, 20_000);

  beforeEach(() => {
    resetAppletsKitMocks();
    setAppletsKitUseHeaderHeight(88);
  });

  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
    setLazyRouteState();
  });

  it("redirects the trade pair route to the exchange shutdown notice", async () => {
    const Route = await loadTradeRoute();

    const redirect = await expectRedirect(Route.options.beforeLoad());

    expect(redirect.options).toEqual({
      replace: true,
      search: {
        notice: "exchange-shutdown",
      },
      to: "/",
    });
  });

  it("defaults missing trade search params to market buys", async () => {
    const Route = await loadTradeRoute();

    expect(Route.options.validateSearch.parse({})).toEqual({
      action: "buy",
      order_type: "market",
    });
    expect(
      Route.options.validateSearch.parse({
        action: "sell",
        order_type: "limit",
      }),
    ).toEqual({
      action: "sell",
      order_type: "limit",
    });
    expect(() => Route.options.validateSearch.parse({ action: "close" })).toThrow();
  });

  it("redirects the trade index route to the exchange shutdown notice", async () => {
    const Route = await loadTradeIndexRoute();

    const redirect = await expectRedirect(Route.options.beforeLoad());

    expect(redirect.options).toEqual({
      replace: true,
      search: {
        notice: "exchange-shutdown",
      },
      to: "/",
    });
  });

  it("maps route ticker and search params into the trade screen", async () => {
    setLazyRouteState({
      ticker: "ETHUSD",
      search: {
        action: "sell",
        order_type: "limit",
      },
    });
    const Route = await loadTradeLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    const proTrade = screen.getByTestId("pro-trade");
    expect(proTrade).toHaveAttribute("data-ticker", "ETHUSD");
    expect(proTrade).toHaveAttribute("data-perps-pair-id", "perp/ethusd");
    expect(proTrade).toHaveAttribute("data-action", "sell");
    expect(proTrade).toHaveAttribute("data-order-type", "limit");
    expect(screen.getByTestId("trade-header")).toBeInTheDocument();
    expect(screen.getByTestId("trade-chart")).toBeInTheDocument();
    expect(screen.getByTestId("trade-order-book")).toBeInTheDocument();
    expect(screen.getByTestId("trade-history")).toBeInTheDocument();
    expect(screen.getByTestId("trade-menu")).toBeInTheDocument();
  });

  it("keeps trade screen navigation callbacks on the active pair route", async () => {
    setLazyRouteState({
      ticker: "ETHUSD",
      search: {
        action: "sell",
        order_type: "limit",
      },
    });
    const Route = await loadTradeLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    fireEvent.click(screen.getByRole("button", { name: "change pair" }));
    expect(routeMocks.navigate).toHaveBeenLastCalledWith({
      params: {
        ticker: "BTCUSD",
      },
      replace: true,
      to: "/trade/$ticker",
    });

    fireEvent.click(screen.getByRole("button", { name: "change action" }));
    expect(routeMocks.navigate).toHaveBeenLastCalledWith({
      params: {
        ticker: "ETHUSD",
      },
      replace: true,
      search: {
        action: "buy",
        order_type: "limit",
      },
      to: "/trade/$ticker",
    });

    fireEvent.click(screen.getByRole("button", { name: "change order type" }));
    expect(routeMocks.navigate).toHaveBeenLastCalledWith({
      params: {
        ticker: "ETHUSD",
      },
      replace: true,
      search: {
        action: "sell",
        order_type: "market",
      },
      to: "/trade/$ticker",
    });
  });

  it("uses the default pair when rendering with invalid route ticker input", async () => {
    setLazyRouteState({
      ticker: "ETH",
    });
    const Route = await loadTradeLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    const proTrade = screen.getByTestId("pro-trade");
    expect(proTrade).toHaveAttribute("data-ticker", "BTCUSD");
    expect(proTrade).toHaveAttribute("data-perps-pair-id", "perp/btcusd");
    expect(proTrade).toHaveAttribute("data-action", "buy");
    expect(proTrade).toHaveAttribute("data-order-type", "market");
  });
});
