import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";

import type React from "react";

const appletRouteMocks = vi.hoisted(() => ({
  isConnected: true,
  navigate: vi.fn(),
  params: {} as Record<string, unknown>,
  redirect: vi.fn((options: unknown) => ({
    options,
    type: "redirect",
  })),
  search: {} as Record<string, unknown>,
}));

function createInspectableRoute(routePath: string) {
  return (options: unknown) => ({
    options,
    routePath,
    useNavigate: () => appletRouteMocks.navigate,
    useParams: () => appletRouteMocks.params,
    useSearch: () => appletRouteMocks.search,
  });
}

vi.mock("@tanstack/react-router", () => ({
  createFileRoute: createInspectableRoute,
  createLazyFileRoute: createInspectableRoute,
  redirect: appletRouteMocks.redirect,
  useNavigate: () => appletRouteMocks.navigate,
}));

vi.mock("~/components/foundation/MobileTitle", () => ({
  MobileTitle: ({ title }: { title: string }) => <h1>{title}</h1>,
}));

vi.mock("@left-curve/store", () => ({
  useAccount: () => ({
    isConnected: appletRouteMocks.isConnected,
  }),
}));

vi.mock("~/components/account/AccountCreation", () => {
  const AccountCreation = ({ children }: React.PropsWithChildren) => (
    <section data-testid="account-creation-wrapper">{children}</section>
  );

  AccountCreation.Deposit = () => <div data-testid="account-creation-deposit" />;

  return {
    AccountCreation,
  };
});

vi.mock("~/components/bridge/Bridge", () => {
  type BridgeAction = "deposit" | "withdraw";

  const Bridge = ({
    action,
    changeAction,
    children,
  }: React.PropsWithChildren<{
    action: BridgeAction;
    changeAction: (action: BridgeAction) => void;
  }>) => (
    <section data-action={action} data-testid="bridge-route-applet">
      <button
        onClick={() => changeAction(action === "deposit" ? "withdraw" : "deposit")}
        type="button"
      >
        change bridge action
      </button>
      {children}
    </section>
  );

  Bridge.Deposit = () => <div data-testid="bridge-deposit" />;
  Bridge.Withdraw = () => <div data-testid="bridge-withdraw" />;

  return {
    Bridge,
  };
});

vi.mock("~/components/earn/VaultLiquidity", () => {
  type EarnAction = "deposit" | "withdraw";

  const VaultLiquidity = ({
    action,
    children,
    onChangeAction,
  }: React.PropsWithChildren<{
    action: EarnAction;
    onChangeAction: (action: EarnAction) => void;
  }>) => (
    <section data-action={action} data-testid="earn-route-applet">
      <button
        onClick={() => onChangeAction(action === "deposit" ? "withdraw" : "deposit")}
        type="button"
      >
        change earn action
      </button>
      {children}
    </section>
  );

  VaultLiquidity.Content = () => <div data-testid="earn-content" />;

  return {
    VaultLiquidity,
  };
});

vi.mock("~/components/transfer/Transfer", () => {
  type TransferAction = "send" | "spot-perp";

  const Transfer = ({
    action,
    changeAction,
    children,
  }: React.PropsWithChildren<{
    action: TransferAction;
    changeAction: (action: TransferAction) => void;
  }>) => (
    <section data-action={action} data-testid="transfer-route-applet">
      <button onClick={() => changeAction(action === "send" ? "spot-perp" : "send")} type="button">
        change transfer action
      </button>
      {children}
    </section>
  );

  Transfer.Send = () => <div data-testid="transfer-send" />;
  Transfer.SpotPerp = () => <div data-testid="transfer-spot-perp" />;

  return {
    Transfer,
  };
});

vi.mock("~/components/points/PointsCampaign", () => {
  type PointsTab = "profile" | "rewards" | "leaderboard";

  const PointsCampaign = ({
    activeTab,
    children,
    onTabChange,
  }: React.PropsWithChildren<{
    activeTab: PointsTab;
    onTabChange: (tab: PointsTab) => void;
  }>) => (
    <section data-tab={activeTab} data-testid="points-route-applet">
      <button onClick={() => onTabChange("rewards")} type="button">
        change points tab
      </button>
      {children}
    </section>
  );

  PointsCampaign.Header = () => <div data-testid="points-header" />;
  PointsCampaign.Tabs = () => <div data-testid="points-tabs" />;

  return {
    PointsCampaign,
  };
});

vi.mock("~/components/referral/ReferralCampaign", () => {
  type ReferralTab = "affiliate" | "trader";

  const ReferralCampaign = ({
    activeTab,
    children,
    onTabChange,
  }: React.PropsWithChildren<{
    activeTab: ReferralTab;
    onTabChange: (tab: ReferralTab) => void;
  }>) => (
    <section data-tab={activeTab} data-testid="referral-route-applet">
      <button onClick={() => onTabChange("trader")} type="button">
        change referral tab
      </button>
      {children}
    </section>
  );

  ReferralCampaign.Header = () => <div data-testid="referral-header" />;
  ReferralCampaign.Content = () => <div data-testid="referral-content" />;

  return {
    ReferralCampaign,
  };
});

vi.mock("~/components/devtools/MsgBuilder", () => {
  const MsgBuilder = ({ children }: React.PropsWithChildren) => (
    <section data-testid="devtool-msg-builder">{children}</section>
  );

  MsgBuilder.QueryMsg = () => <div data-testid="devtool-query-msg" />;
  MsgBuilder.ExecuteMsg = () => <div data-testid="devtool-execute-msg" />;

  return {
    MsgBuilder,
  };
});

vi.mock("~/components/settings/DisplaySection", () => {
  const DisplaySection = ({ children }: React.PropsWithChildren) => (
    <section data-testid="settings-display-section">{children}</section>
  );

  DisplaySection.Theme = () => <div data-testid="settings-display-theme" />;
  DisplaySection.Language = () => <div data-testid="settings-display-language" />;
  DisplaySection.FormatNumber = () => <div data-testid="settings-display-format-number" />;
  DisplaySection.DateFormat = () => <div data-testid="settings-display-date-format" />;
  DisplaySection.TimeFormat = () => <div data-testid="settings-display-time-format" />;
  DisplaySection.TimeZone = () => <div data-testid="settings-display-time-zone" />;
  DisplaySection.ChartEngine = () => <div data-testid="settings-display-chart-engine" />;

  return {
    DisplaySection,
  };
});

vi.mock("~/components/settings/KeyManagementSection", () => ({
  KeyManagementSection: () => <section data-testid="settings-key-management" />,
}));

vi.mock("~/components/settings/SessionSection", () => {
  const SessionSection = ({ children }: React.PropsWithChildren) => (
    <section data-testid="settings-session-section">{children}</section>
  );

  SessionSection.Username = () => <div data-testid="settings-session-username" />;
  SessionSection.UserStatus = () => <div data-testid="settings-session-user-status" />;
  SessionSection.ConnectMobile = () => <div data-testid="settings-session-connect-mobile" />;
  SessionSection.RemainingTime = () => <div data-testid="settings-session-remaining-time" />;
  SessionSection.Network = () => <div data-testid="settings-session-network" />;
  SessionSection.Status = () => <div data-testid="settings-session-status" />;

  return {
    SessionSection,
  };
});

vi.mock("~/components/explorer/AccountExplorer", () => {
  const AccountExplorer = ({ address, children }: React.PropsWithChildren<{ address: string }>) => (
    <section data-address={address} data-testid="account-explorer-route">
      {children}
    </section>
  );

  AccountExplorer.NotFound = () => <div data-testid="account-not-found" />;
  AccountExplorer.Details = () => <div data-testid="account-details" />;
  AccountExplorer.PerpsBalance = () => <div data-testid="account-perps-balance" />;
  AccountExplorer.Assets = () => <div data-testid="account-assets" />;
  AccountExplorer.PerpsPositions = () => <div data-testid="account-perps-positions" />;
  AccountExplorer.PerpsOrders = () => <div data-testid="account-perps-orders" />;
  AccountExplorer.Transactions = () => <div data-testid="account-transactions" />;

  return {
    AccountExplorer,
  };
});

vi.mock("~/components/explorer/BlockExplorer", () => {
  const BlockExplorer = ({ children, height }: React.PropsWithChildren<{ height: string }>) => (
    <section data-height={height} data-testid="block-explorer-route">
      {children}
    </section>
  );

  BlockExplorer.Skeleton = () => <div data-testid="block-skeleton" />;
  BlockExplorer.NotFound = () => <div data-testid="block-not-found" />;
  BlockExplorer.FutureBlock = () => <div data-testid="block-future" />;
  BlockExplorer.Details = () => <div data-testid="block-details" />;
  BlockExplorer.CronsOutcomes = () => <div data-testid="block-crons-outcomes" />;
  BlockExplorer.TxTable = () => <div data-testid="block-tx-table" />;

  return {
    BlockExplorer,
  };
});

vi.mock("~/components/explorer/ContractExplorer", () => {
  const ContractExplorer = ({
    address,
    children,
  }: React.PropsWithChildren<{ address: string }>) => (
    <section data-address={address} data-testid="contract-explorer-route">
      {children}
    </section>
  );

  ContractExplorer.NotFound = () => <div data-testid="contract-not-found" />;
  ContractExplorer.Details = () => <div data-testid="contract-details" />;
  ContractExplorer.Transactions = () => <div data-testid="contract-transactions" />;
  ContractExplorer.Assets = () => <div data-testid="contract-assets" />;

  return {
    ContractExplorer,
  };
});

vi.mock("~/components/explorer/TransactionExplorer", () => {
  const TransactionExplorer = ({
    children,
    txHash,
  }: React.PropsWithChildren<{ txHash: string }>) => (
    <section data-tx-hash={txHash} data-testid="transaction-explorer-route">
      {children}
    </section>
  );

  TransactionExplorer.NotFound = () => <div data-testid="transaction-not-found" />;
  TransactionExplorer.Details = () => <div data-testid="transaction-details" />;
  TransactionExplorer.Messages = () => <div data-testid="transaction-messages" />;

  return {
    TransactionExplorer,
  };
});

vi.mock("~/components/explorer/UserExplorer", () => {
  const UserExplorer = ({ children, username }: React.PropsWithChildren<{ username: string }>) => (
    <section data-username={username} data-testid="user-explorer-route">
      {children}
    </section>
  );

  UserExplorer.NotFound = () => <div data-testid="user-not-found" />;
  UserExplorer.Header = () => <div data-testid="user-header" />;
  UserExplorer.Content = () => <div data-testid="user-content" />;

  return {
    UserExplorer,
  };
});

type RouteWithValidate<TSearch> = {
  options: {
    component?: React.ComponentType;
    validateSearch: {
      parse: (search: unknown) => TSearch;
    };
  };
};

type RouteWithComponent = {
  options: {
    component: React.ComponentType;
  };
};

type RouteWithBeforeLoad = {
  options: {
    beforeLoad: () => Promise<void>;
  };
};

function setSearch(search: Record<string, unknown>) {
  appletRouteMocks.search = search;
}

function setParams(params: Record<string, unknown>) {
  appletRouteMocks.params = params;
}

async function loadBridgeRoute() {
  return import("../src/pages/(app)/_app.bridge").then(
    ({ Route }) => Route as unknown as RouteWithValidate<{ action: "deposit" | "withdraw" }>,
  );
}

async function loadBridgeLazyRoute() {
  return import("../src/pages/(app)/_app.bridge.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadEarnRoute() {
  return import("../src/pages/(app)/_app.earn.index").then(
    ({ Route }) =>
      Route as unknown as RouteWithValidate<{ action: "deposit" | "withdraw" }> &
        RouteWithBeforeLoad,
  );
}

async function loadEarnLazyRoute() {
  return import("../src/pages/(app)/_app.earn.index.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadTransferRoute() {
  return import("../src/pages/(app)/_app.transfer").then(
    ({ Route }) => Route as unknown as RouteWithValidate<{ action: "send" | "spot-perp" }>,
  );
}

async function loadTransferLazyRoute() {
  return import("../src/pages/(app)/_app.transfer.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadPointsRoute() {
  return import("../src/pages/(app)/_app.points").then(
    ({ Route }) =>
      Route as unknown as RouteWithValidate<{ tab: "profile" | "rewards" | "leaderboard" }> &
        RouteWithComponent,
  );
}

async function loadReferralRoute() {
  return import("../src/pages/(app)/_app.referral").then(
    ({ Route }) =>
      Route as unknown as RouteWithValidate<{ tab: "affiliate" | "trader" }> & RouteWithComponent,
  );
}

async function loadAccountCreateLazyRoute() {
  return import("../src/pages/(app)/_app.account.create.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadDevtoolLazyRoute() {
  return import("../src/pages/(app)/_app.devtool.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadSettingsLazyRoute() {
  return import("../src/pages/(app)/_app.settings.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadAccountExplorerLazyRoute() {
  return import("../src/pages/(app)/_app.account.$address.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadBlockExplorerLazyRoute() {
  return import("../src/pages/(app)/_app.block.$block.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadContractExplorerLazyRoute() {
  return import("../src/pages/(app)/_app.contract.$address.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadTransactionExplorerLazyRoute() {
  return import("../src/pages/(app)/_app.tx.$txHash.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

async function loadUserExplorerLazyRoute() {
  return import("../src/pages/(app)/_app.user.$username.lazy").then(
    ({ Route }) => Route as unknown as RouteWithComponent,
  );
}

describe("applet routes", () => {
  beforeAll(async () => {
    await Promise.all([
      loadBridgeRoute(),
      loadBridgeLazyRoute(),
      loadEarnRoute(),
      loadEarnLazyRoute(),
      loadTransferRoute(),
      loadTransferLazyRoute(),
      loadPointsRoute(),
      loadReferralRoute(),
      loadAccountCreateLazyRoute(),
      loadDevtoolLazyRoute(),
      loadSettingsLazyRoute(),
      loadAccountExplorerLazyRoute(),
      loadBlockExplorerLazyRoute(),
      loadContractExplorerLazyRoute(),
      loadTransactionExplorerLazyRoute(),
      loadUserExplorerLazyRoute(),
    ]);
  }, 30_000);

  afterEach(() => {
    cleanup();
    appletRouteMocks.isConnected = true;
    vi.clearAllMocks();
    setParams({});
    setSearch({});
  });

  it("defaults bridge and earn search params to deposits", async () => {
    const BridgeRoute = await loadBridgeRoute();
    const EarnRoute = await loadEarnRoute();

    expect(BridgeRoute.options.validateSearch.parse({})).toEqual({ action: "deposit" });
    expect(BridgeRoute.options.validateSearch.parse({ action: "withdraw" })).toEqual({
      action: "withdraw",
    });
    expect(BridgeRoute.options.validateSearch.parse({ action: "send" })).toEqual({
      action: "deposit",
    });

    expect(EarnRoute.options.validateSearch.parse({})).toEqual({ action: "deposit" });
    expect(EarnRoute.options.validateSearch.parse({ action: "withdraw" })).toEqual({
      action: "withdraw",
    });
    expect(EarnRoute.options.validateSearch.parse({ action: "redeem" })).toEqual({
      action: "deposit",
    });
  }, 10_000);

  it("redirects the earn route to the exchange shutdown notice", async () => {
    const EarnRoute = await loadEarnRoute();

    await expect(EarnRoute.options.beforeLoad()).rejects.toMatchObject({
      type: "redirect",
    });
    expect(appletRouteMocks.redirect).toHaveBeenLastCalledWith({
      replace: true,
      search: {
        notice: "exchange-shutdown",
      },
      to: "/",
    });
  });

  it("defaults transfer search params to sends", async () => {
    const Route = await loadTransferRoute();

    expect(Route.options.validateSearch.parse({})).toEqual({ action: "send" });
    expect(Route.options.validateSearch.parse({ action: "spot-perp" })).toEqual({
      action: "spot-perp",
    });
    expect(Route.options.validateSearch.parse({ action: "withdraw" })).toEqual({
      action: "send",
    });
  });

  it("passes the bridge action into the bridge applet and routes action changes through search", async () => {
    setSearch({ action: "withdraw" });
    const Route = await loadBridgeLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    expect(screen.getByTestId("bridge-route-applet")).toHaveAttribute("data-action", "withdraw");
    expect(screen.getByTestId("bridge-deposit")).toBeInTheDocument();
    expect(screen.getByTestId("bridge-withdraw")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "change bridge action" }));

    expect(appletRouteMocks.navigate).toHaveBeenCalledWith({
      search: {
        action: "deposit",
      },
      to: ".",
    });
  });

  it("passes the earn action into vault liquidity and routes action changes through search", async () => {
    setSearch({ action: "deposit" });
    const Route = await loadEarnLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    expect(screen.getByTestId("earn-route-applet")).toHaveAttribute("data-action", "deposit");
    expect(screen.getByTestId("earn-content")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "change earn action" }));

    expect(appletRouteMocks.navigate).toHaveBeenCalledWith({
      replace: true,
      search: {
        action: "withdraw",
      },
      to: "/earn",
    });
  });

  it("passes the transfer action into the transfer applet and routes action changes through search", async () => {
    setSearch({ action: "spot-perp" });
    const Route = await loadTransferLazyRoute();
    const Component = Route.options.component;

    render(<Component />);

    expect(screen.getByTestId("transfer-route-applet")).toHaveAttribute("data-action", "spot-perp");
    expect(screen.getByTestId("transfer-send")).toBeInTheDocument();
    expect(screen.getByTestId("transfer-spot-perp")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "change transfer action" }));

    expect(appletRouteMocks.navigate).toHaveBeenCalledWith({
      replace: true,
      search: {
        action: "send",
      },
    });
  });

  it("defaults points search to profile and preserves tab changes in the URL", async () => {
    const Route = await loadPointsRoute();

    expect(Route.options.validateSearch.parse({})).toEqual({ tab: "profile" });
    expect(Route.options.validateSearch.parse({ tab: "leaderboard" })).toEqual({
      tab: "leaderboard",
    });
    expect(() => Route.options.validateSearch.parse({ tab: "affiliate" })).toThrow();

    setSearch({ tab: "profile" });
    const Component = Route.options.component;
    render(<Component />);

    expect(screen.getByTestId("points-route-applet")).toHaveAttribute("data-tab", "profile");
    expect(screen.getByTestId("points-header")).toBeInTheDocument();
    expect(screen.getByTestId("points-tabs")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "change points tab" }));

    expect(appletRouteMocks.navigate).toHaveBeenCalledWith({
      replace: true,
      resetScroll: false,
      search: {
        tab: "rewards",
      },
    });
  });

  it("defaults referral search to affiliate and preserves tab changes in the URL", async () => {
    const Route = await loadReferralRoute();

    expect(Route.options.validateSearch.parse({})).toEqual({ tab: "affiliate" });
    expect(Route.options.validateSearch.parse({ tab: "trader" })).toEqual({ tab: "trader" });
    expect(() => Route.options.validateSearch.parse({ tab: "rewards" })).toThrow();

    setSearch({ tab: "affiliate" });
    const Component = Route.options.component;
    render(<Component />);

    expect(screen.getByTestId("referral-route-applet")).toHaveAttribute("data-tab", "affiliate");
    expect(screen.getByTestId("referral-header")).toBeInTheDocument();
    expect(screen.getByTestId("referral-content")).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "change referral tab" }));

    expect(appletRouteMocks.navigate).toHaveBeenCalledWith({
      replace: true,
      resetScroll: false,
      search: {
        tab: "trader",
      },
    });
  });

  it("keeps settings route key management scoped to connected accounts", async () => {
    appletRouteMocks.isConnected = false;
    const Route = await loadSettingsLazyRoute();
    const Component = Route.options.component;

    const { rerender } = render(<Component />);

    expect(screen.getByTestId("settings-session-section")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-username")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-user-status")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-connect-mobile")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-remaining-time")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-network")).toBeInTheDocument();
    expect(screen.getByTestId("settings-session-status")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-section")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-theme")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-language")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-format-number")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-date-format")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-time-format")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-time-zone")).toBeInTheDocument();
    expect(screen.getByTestId("settings-display-chart-engine")).toBeInTheDocument();
    expect(screen.queryByTestId("settings-key-management")).not.toBeInTheDocument();

    appletRouteMocks.isConnected = true;
    rerender(<Component />);

    expect(screen.getByTestId("settings-key-management")).toBeInTheDocument();
  });

  it("mounts account-creation lazy route with its required child regions", async () => {
    const AccountCreateRoute = await loadAccountCreateLazyRoute();
    render(<AccountCreateRoute.options.component />);

    expect(screen.getByTestId("account-creation-wrapper")).toBeInTheDocument();
    expect(screen.getByTestId("account-creation-deposit")).toBeInTheDocument();
    expect(screen.getByTestId("account-creation-wrapper")).toContainElement(
      screen.getByTestId("account-creation-deposit"),
    );
  }, 20_000);

  it("mounts devtool lazy route with its required child regions", async () => {
    const DevtoolRoute = await loadDevtoolLazyRoute();
    render(<DevtoolRoute.options.component />);

    expect(screen.getByTestId("devtool-msg-builder")).toBeInTheDocument();
    expect(screen.getByTestId("devtool-query-msg")).toBeInTheDocument();
    expect(screen.getByTestId("devtool-execute-msg")).toBeInTheDocument();
  });

  it("passes URL params through explorer lazy routes and mounts their data regions", async () => {
    const accountAddress = "0x6163636f756e7400000000000000000000000000";
    setParams({ address: accountAddress });
    const AccountRoute = await loadAccountExplorerLazyRoute();
    render(<AccountRoute.options.component />);

    expect(screen.getByTestId("account-explorer-route")).toHaveAttribute(
      "data-address",
      accountAddress,
    );
    expect(screen.getByTestId("account-not-found")).toBeInTheDocument();
    expect(screen.getByTestId("account-details")).toBeInTheDocument();
    expect(screen.getByTestId("account-perps-balance")).toBeInTheDocument();
    expect(screen.getByTestId("account-assets")).toBeInTheDocument();
    expect(screen.getByTestId("account-perps-positions")).toBeInTheDocument();
    expect(screen.getByTestId("account-perps-orders")).toBeInTheDocument();
    expect(screen.getByTestId("account-transactions")).toBeInTheDocument();

    cleanup();
    const blockHeight = "12345";
    setParams({ block: blockHeight });
    const BlockRoute = await loadBlockExplorerLazyRoute();
    render(<BlockRoute.options.component />);

    expect(screen.getByTestId("block-explorer-route")).toHaveAttribute("data-height", blockHeight);
    expect(screen.getByTestId("block-skeleton")).toBeInTheDocument();
    expect(screen.getByTestId("block-not-found")).toBeInTheDocument();
    expect(screen.getByTestId("block-future")).toBeInTheDocument();
    expect(screen.getByTestId("block-details")).toBeInTheDocument();
    expect(screen.getByTestId("block-crons-outcomes")).toBeInTheDocument();
    expect(screen.getByTestId("block-tx-table")).toBeInTheDocument();

    cleanup();
    const contractAddress = "0x636f6e7472616374000000000000000000000000";
    setParams({ address: contractAddress });
    const ContractRoute = await loadContractExplorerLazyRoute();
    render(<ContractRoute.options.component />);

    expect(screen.getByTestId("contract-explorer-route")).toHaveAttribute(
      "data-address",
      contractAddress,
    );
    expect(screen.getByTestId("contract-not-found")).toBeInTheDocument();
    expect(screen.getByTestId("contract-details")).toBeInTheDocument();
    expect(screen.getByTestId("contract-transactions")).toBeInTheDocument();
    expect(screen.getByTestId("contract-assets")).toBeInTheDocument();

    cleanup();
    const txHash = "0x7478686173680000000000000000000000000000000000000000000000000000";
    setParams({ txHash });
    const TransactionRoute = await loadTransactionExplorerLazyRoute();
    render(<TransactionRoute.options.component />);

    expect(screen.getByTestId("transaction-explorer-route")).toHaveAttribute(
      "data-tx-hash",
      txHash,
    );
    expect(screen.getByTestId("transaction-not-found")).toBeInTheDocument();
    expect(screen.getByTestId("transaction-details")).toBeInTheDocument();
    expect(screen.getByTestId("transaction-messages")).toBeInTheDocument();

    cleanup();
    setParams({ username: "alice" });
    const UserRoute = await loadUserExplorerLazyRoute();
    render(<UserRoute.options.component />);

    expect(screen.getByTestId("user-explorer-route")).toHaveAttribute("data-username", "alice");
    expect(screen.getByTestId("user-not-found")).toBeInTheDocument();
    expect(screen.getByTestId("user-header")).toBeInTheDocument();
    expect(screen.getByTestId("user-content")).toBeInTheDocument();
  });
});
