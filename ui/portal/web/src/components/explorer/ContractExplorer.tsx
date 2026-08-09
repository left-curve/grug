import {
  useExplorerContract,
  useExplorerTransactionsByAddress,
  usePrices,
} from "@left-curve/store";
import { createContext, useContext } from "react";

import { m } from "@left-curve/foundation/paraglide/messages.js";

import { Badge, TextCopy, useApp } from "@left-curve/applets-kit";
import { ContractCard } from "../foundation/ContractCard";
import { AssetsTable } from "./AssetsTable";
import { HeaderExplorer } from "./HeaderExplorer";
import { TransactionsTable } from "./TransactionsTable";

import type { Address, Coins, ContractInfo } from "@left-curve/types";
import type { UseQueryResult } from "@tanstack/react-query";
import type React from "react";
import type { PropsWithChildren } from "react";

const ContractExplorerContext = createContext<
  | (UseQueryResult<(ContractInfo & { balances: Coins }) | null, Error> & {
      address: Address;
    })
  | null
>(null);

const useContractExplorer = () => {
  const context = useContext(ContractExplorerContext);
  if (context === null) {
    throw new Error("useContractExplorer must be used within a ContractExplorerProvider");
  }
  return context;
};

type ContractExplorerProps = {
  address: Address;
};

const Root: React.FC<PropsWithChildren<ContractExplorerProps>> = ({ address, children }) => {
  const query = useExplorerContract(address);

  return (
    <ContractExplorerContext.Provider value={{ address, ...query }}>
      <div className="w-full md:max-w-[76rem] flex flex-col gap-6 p-4 pt-6 mb-16">{children}</div>
    </ContractExplorerContext.Provider>
  );
};

const Details: React.FC = () => {
  const { isLoading, data: contract, address } = useContractExplorer();
  const { calculateBalance } = usePrices();
  const { settings } = useApp();
  const { formatNumberOptions } = settings;

  if (!contract || isLoading) return null;

  const { codeHash, admin, balances } = contract;
  const totalCoins = Object.values(balances).length;
  const totalBalance = calculateBalance(balances, {
    format: true,
    formatOptions: { ...formatNumberOptions, currency: "usd" },
  });

  return (
    <div className="flex flex-col gap-4 lg:flex-row">
      <ContractCard address={address} balance={totalBalance} />
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
            <p className="diatype-sm-medium">{admin ? admin : "None"}</p>
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
  const { isLoading, data: contract, address } = useContractExplorer();
  if (isLoading || contract) return null;

  return (
    <div className="w-full md:max-w-[76rem] p-4">
      <HeaderExplorer>
        <div className="flex flex-col gap-2 items-center">
          <h3 className="exposure-m-italic text-ink-secondary-700">
            {m["explorer.contracts.notFound.title"]()}
          </h3>
          <p className="diatype-m-medium max-w-[42.5rem] text-center text-ink-tertiary-500 ">
            {m["explorer.contracts.notFound.pre"]()}
            <span className="break-all overflow-hidden underline"> {address}</span>{" "}
            {m["explorer.contracts.notFound.description"]()}
          </p>
        </div>
      </HeaderExplorer>
    </div>
  );
};

const Assets: React.FC = () => {
  const { isLoading, data: contract } = useContractExplorer();

  if (isLoading || !contract) return null;

  return <AssetsTable balances={contract.balances} />;
};

const Transactions: React.FC = () => {
  const { isLoading, data: contract, address } = useContractExplorer();
  const { data, pagination, ...transactions } = useExplorerTransactionsByAddress(
    address,
    !!contract,
  );

  if (isLoading || !contract) return null;

  return (
    <TransactionsTable
      transactions={data?.nodes || []}
      pagination={{ ...pagination, isLoading: transactions.isLoading }}
    />
  );
};

export const ContractExplorer = Object.assign(Root, {
  Details,
  NotFound,
  Assets,
  Transactions,
});
