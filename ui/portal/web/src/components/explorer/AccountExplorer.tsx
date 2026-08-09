import { AddressVisualizer, useApp } from "@left-curve/applets-kit";
import {
  type ExplorerAccount,
  useExplorerAccount,
  useExplorerTransactionsByAddress,
  usePrices,
} from "@left-curve/store";
import type { UseQueryResult } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { createContext, useContext } from "react";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import { Badge, Cell, FormattedNumber, Table, TextCopy } from "@left-curve/applets-kit";
import { Decimal, sharesToUsd } from "@left-curve/utils";
import { AccountCard } from "../foundation/AccountCard";
import { MarketPair } from "@left-curve/foundation/market-pair";
import { AssetsTable } from "./AssetsTable";
import { HeaderExplorer } from "./HeaderExplorer";

import type { Address, PerpsOrderByUserItem, PerpsPositionExtended } from "@left-curve/types";
import type { TableColumn } from "@left-curve/applets-kit";
import type React from "react";
import type { PropsWithChildren } from "react";
import { TransactionsTable } from "./TransactionsTable";

const AccountExplorerContext = createContext<
  (UseQueryResult<ExplorerAccount, Error> & { address: string }) | null
>(null);

const useAccountExplorer = () => {
  const context = useContext(AccountExplorerContext);
  if (context === null) {
    throw new Error("useAccountExplorer must be used within a AccountExplorerContext");
  }
  return context;
};

type AccountExplorerProps = {
  address: Address;
};

const Root: React.FC<PropsWithChildren<AccountExplorerProps>> = ({ address, children }) => {
  const query = useExplorerAccount(address);

  return (
    <AccountExplorerContext.Provider value={{ address, ...query }}>
      <div className="w-full md:max-w-[76rem] flex flex-col gap-6 p-4 pt-6 mb-16">{children}</div>
    </AccountExplorerContext.Provider>
  );
};

const Details: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();
  const navigate = useNavigate();
  const { calculateBalance } = usePrices();
  const { settings } = useApp();
  const { formatNumberOptions } = settings;

  if (!account || isLoading) return null;

  const { codeHash, admin, balances } = account;
  const totalCoins = Object.values(balances).length;
  const totalBalance = calculateBalance(balances, {
    format: true,
    formatOptions: { ...formatNumberOptions, currency: "usd" },
  });

  return (
    <div className="flex flex-col gap-4 lg:flex-row">
      <AccountCard account={account} balance={totalBalance} isUserActive />
      <div className="flex flex-col gap-4 rounded-xl p-4 bg-surface-secondary-rice shadow-account-card relative overflow-hidden w-full min-h-[10rem]">
        <h4 className="h4-bold text-ink-primary-900">
          {m["explorer.contracts.details.contractDetails"]()}
        </h4>
        <div className="flex flex-col gap-2">
          <div className="flex md:items-center gap-1 flex-col md:flex-row">
            <p className="diatype-sm-medium text-ink-tertiary-500 md:min-w-[8rem]">
              {m["explorer.contracts.details.codeHash"]()}
            </p>

            <p className="diatype-sm-medium break-all whitespace-normal">
              {codeHash}
              <TextCopy className="w-4 h-4 text-ink-tertiary-500 ml-1" copyText={codeHash} />
            </p>
          </div>
          <div className="flex md:items-center gap-1 flex-col md:flex-row">
            <p className="diatype-sm-medium text-ink-tertiary-500 md:min-w-[8rem]">
              {m["explorer.contracts.details.admin"]()}
            </p>
            {admin ? (
              <AddressVisualizer
                classNames={{ text: "diatype-sm-medium" }}
                address={admin}
                withIcon
                onClick={(url) => navigate({ to: url })}
              />
            ) : (
              <p className="diatype-sm-medium">None</p>
            )}
          </div>
          <div className="flex md:items-center gap-1 flex-col md:flex-row">
            <p className="diatype-sm-medium text-ink-tertiary-500 md:min-w-[8rem]">
              {m["explorer.contracts.details.balances"]()}
            </p>
            <Badge color="green" size="m" text={`${totalBalance} (${totalCoins} Assets)`} />
          </div>
        </div>
      </div>
    </div>
  );
};

const NotFound: React.FC = () => {
  const { isLoading, data: account, address } = useAccountExplorer();
  if (isLoading || account) return null;

  return (
    <div className="w-full md:max-w-[76rem] p-4">
      <HeaderExplorer>
        <div className="flex flex-col gap-2 items-center">
          <h3 className="exposure-m-italic text-ink-secondary-700">
            {m["explorer.accounts.notFound.title"]()}
          </h3>
          <p className="diatype-m-medium max-w-[42.5rem] text-center text-ink-tertiary-500 ">
            {m["explorer.accounts.notFound.pre"]()}
            <span className="break-all overflow-hidden underline">{address}</span>{" "}
            {m["explorer.accounts.notFound.description"]()}
          </p>
        </div>
      </HeaderExplorer>
    </div>
  );
};

const Assets: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();

  if (isLoading || !account) return null;

  return <AssetsTable balances={account.balances} />;
};

const Transactions: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();

  const { data, pagination, ...transactions } = useExplorerTransactionsByAddress(
    account?.address as Address,
    !!account,
  );

  if (isLoading || !account) return null;

  return (
    <TransactionsTable
      transactions={data?.nodes || []}
      pagination={{ ...pagination, isLoading: transactions.isLoading }}
    />
  );
};

type PerpsBalanceItem = {
  label: string;
  value: string;
};

const PerpsBalance: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();

  if (isLoading || !account) return null;

  const { userState, vaultState } = account.perps;
  if (!userState) return null;

  const vaultSharesValue = vaultState
    ? sharesToUsd(userState.vaultShares, vaultState.equity, vaultState.shareSupply)
    : "0";

  const items: PerpsBalanceItem[] = [
    { label: m["explorer.accounts.perps.balance.margin"](), value: userState.margin },
    { label: m["explorer.accounts.perps.balance.equity"](), value: userState.equity ?? "0" },
    {
      label: m["explorer.accounts.perps.balance.availableMargin"](),
      value: userState.availableMargin ?? "0",
    },
    {
      label: m["explorer.accounts.perps.balance.reservedMargin"](),
      value: userState.reservedMargin,
    },
    { label: m["explorer.accounts.perps.balance.vaultShares"](), value: vaultSharesValue },
  ];

  return (
    <div className="rounded-xl p-4 bg-surface-secondary-rice shadow-account-card">
      <div className="grid grid-cols-2 md:grid-cols-5 gap-4">
        {items.map((item) => (
          <div key={item.label} className="flex flex-col gap-1">
            <p className="diatype-sm-medium text-ink-tertiary-500">{item.label}</p>
            <p className="diatype-sm-medium text-ink-primary-900">
              <FormattedNumber number={item.value} formatOptions={{ currency: "USD" }} as="span" />
            </p>
          </div>
        ))}
      </div>
    </div>
  );
};

type PerpsPositionRow = {
  pairId: string;
  size: string;
  entryPrice: string;
  unrealizedPnl: string | null;
  liquidationPrice: string | null;
};

const PerpsPositions: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();

  if (isLoading || !account) return null;

  const { userState } = account.perps;
  if (!userState) return null;

  const rows = (Object.entries(userState.positions) as [string, PerpsPositionExtended][]).map(
    ([pairId, pos]) => ({
      pairId,
      size: pos.size,
      entryPrice: pos.entryPrice,
      unrealizedPnl: pos.unrealizedPnl,
      liquidationPrice: pos.liquidationPrice,
    }),
  );

  const columns: TableColumn<PerpsPositionRow> = [
    {
      header: m["explorer.accounts.perps.positions.pair"](),
      cell: ({ row }) => (
        <Cell.Text
          text={MarketPair.fromPairId(row.original.pairId).ticker}
          className="diatype-xs-medium"
        />
      ),
    },
    {
      header: m["explorer.accounts.perps.positions.side"](),
      cell: ({ row }) => {
        const isLong = Decimal(row.original.size).gt(0);
        return (
          <Cell.Text
            text={
              isLong
                ? m["explorer.accounts.perps.positions.long"]()
                : m["explorer.accounts.perps.positions.short"]()
            }
            className={isLong ? "text-utility-success-600" : "text-utility-error-600"}
          />
        );
      },
    },
    {
      header: m["explorer.accounts.perps.positions.size"](),
      cell: ({ row }) => {
        const absSize = Decimal(row.original.size).abs().toFixed();
        return <Cell.Text text={<FormattedNumber number={absSize} as="span" />} />;
      },
    },
    {
      header: m["explorer.accounts.perps.positions.entryPrice"](),
      cell: ({ row }) => (
        <Cell.Text
          text={
            <FormattedNumber
              number={row.original.entryPrice}
              formatOptions={{ currency: "USD" }}
              as="span"
            />
          }
        />
      ),
    },
    {
      header: m["explorer.accounts.perps.positions.unrealizedPnl"](),
      cell: ({ row }) => {
        const { unrealizedPnl } = row.original;
        if (unrealizedPnl == null) return <Cell.Text text="N/A" />;
        const isPositive = Decimal(unrealizedPnl).gte(0);
        return (
          <Cell.Text
            text={
              <span className="tabular-nums">
                {isPositive ? "+" : ""}
                <FormattedNumber
                  number={unrealizedPnl}
                  formatOptions={{ currency: "USD" }}
                  as="span"
                />
              </span>
            }
            className={isPositive ? "text-utility-success-600" : "text-utility-error-600"}
          />
        );
      },
    },
    {
      header: m["explorer.accounts.perps.positions.liqPrice"](),
      cell: ({ row }) => {
        const { liquidationPrice } = row.original;
        if (liquidationPrice == null) return <Cell.Text text="N/A" />;
        return (
          <Cell.Text
            text={
              <FormattedNumber
                number={liquidationPrice}
                formatOptions={{ currency: "USD" }}
                as="span"
              />
            }
          />
        );
      },
    },
  ];

  return <Table data={rows} columns={columns} />;
};

type PerpsOrderRow = {
  orderId: string;
  pairId: string;
  size: string;
  limitPrice: string;
  reduceOnly: boolean;
  reservedMargin: string;
  createdAt: string;
};

const PerpsOrders: React.FC = () => {
  const { isLoading, data: account } = useAccountExplorer();

  if (isLoading || !account) return null;

  const { orders } = account.perps;
  if (!orders || Object.keys(orders).length === 0) return null;

  const rows = (Object.entries(orders) as [string, PerpsOrderByUserItem][]).map(
    ([orderId, order]) => ({
      orderId,
      pairId: order.pairId,
      size: order.size,
      limitPrice: order.limitPrice,
      reduceOnly: order.reduceOnly,
      reservedMargin: order.reservedMargin,
      createdAt: order.createdAt,
    }),
  );

  const columns: TableColumn<PerpsOrderRow> = [
    {
      header: m["explorer.accounts.perps.orders.pair"](),
      cell: ({ row }) => (
        <Cell.Text
          text={MarketPair.fromPairId(row.original.pairId).ticker}
          className="diatype-xs-medium"
        />
      ),
    },
    {
      header: m["explorer.accounts.perps.orders.side"](),
      cell: ({ row }) => {
        const isLong = Decimal(row.original.size).gt(0);
        return (
          <Cell.Text
            text={
              isLong
                ? m["explorer.accounts.perps.orders.long"]()
                : m["explorer.accounts.perps.orders.short"]()
            }
            className={isLong ? "text-utility-success-600" : "text-utility-error-600"}
          />
        );
      },
    },
    {
      header: m["explorer.accounts.perps.orders.size"](),
      cell: ({ row }) => {
        const absSize = Decimal(row.original.size).abs().toFixed();
        return <Cell.Text text={<FormattedNumber number={absSize} as="span" />} />;
      },
    },
    {
      header: m["explorer.accounts.perps.orders.limitPrice"](),
      cell: ({ row }) => (
        <Cell.Text
          text={
            <FormattedNumber
              number={row.original.limitPrice}
              formatOptions={{ currency: "USD" }}
              as="span"
            />
          }
        />
      ),
    },
    {
      header: m["explorer.accounts.perps.orders.reduceOnly"](),
      cell: ({ row }) => <Cell.Text text={row.original.reduceOnly ? "Yes" : "No"} />,
    },
    {
      header: m["explorer.accounts.perps.orders.reservedMargin"](),
      cell: ({ row }) => (
        <Cell.Text
          text={
            <FormattedNumber
              number={row.original.reservedMargin}
              formatOptions={{ currency: "USD" }}
              as="span"
            />
          }
        />
      ),
    },
    {
      header: m["explorer.accounts.perps.orders.createdAt"](),
      cell: ({ row }) => {
        const date = new Date(Number(row.original.createdAt) * 1000);
        return <Cell.Text text={date.toLocaleString()} />;
      },
    },
  ];

  return <Table data={rows} columns={columns} />;
};

export const AccountExplorer = Object.assign(Root, {
  Details,
  NotFound,
  Assets,
  Transactions,
  PerpsBalance,
  PerpsPositions,
  PerpsOrders,
});
