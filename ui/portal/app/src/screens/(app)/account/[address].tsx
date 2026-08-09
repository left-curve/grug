import { m } from "@left-curve/foundation/paraglide/messages.js";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useApp } from "@left-curve/foundation";
import { useExplorerAccount, useExplorerTransactionsByAddress, usePrices } from "@left-curve/store";

import { AddressVisualizer } from "~/components/foundation/AddressVisualizer";
import { Badge, GlobalText, MobileTitle, Skeleton } from "~/components/foundation";
import {
  ExplorerAssetsList,
  ExplorerHashValue,
  ExplorerKeyValueRow,
  ExplorerNotFound,
  ExplorerScreen,
  ExplorerSectionCard,
  ExplorerTransactionsList,
} from "~/components/explorer/ExplorerCommon";

export default function AccountExplorerScreen() {
  const { address } = useLocalSearchParams<{ address: string }>();
  const router = useRouter();
  const { data: account, isLoading } = useExplorerAccount(address as `0x${string}`);
  const { settings } = useApp();
  const { calculateBalance } = usePrices();

  const {
    data: txData,
    pagination,
    isLoading: txsLoading,
  } = useExplorerTransactionsByAddress(account?.address as `0x${string}`, !!account);

  const totalCoins = account ? Object.values(account.balances).length : 0;
  const totalBalance = account
    ? calculateBalance(account.balances, {
        format: true,
        formatOptions: { ...settings.formatNumberOptions, currency: "usd" },
      })
    : "$0";

  return (
    <ExplorerScreen>
      <MobileTitle title={m["explorer.accounts.title"]()} />

      {isLoading ? (
        <ExplorerSectionCard title={m["explorer.contracts.details.contractDetails"]()}>
          <Skeleton className="w-full h-10" />
          <Skeleton className="w-full h-10" />
          <Skeleton className="w-full h-10" />
        </ExplorerSectionCard>
      ) : null}

      {!isLoading && !account ? (
        <ExplorerNotFound
          title={m["explorer.accounts.notFound.title"]()}
          description={
            <>
              {m["explorer.accounts.notFound.pre"]()}{" "}
              <GlobalText className="underline">{address}</GlobalText>{" "}
              {m["explorer.accounts.notFound.description"]()}
            </>
          }
        />
      ) : null}

      {account ? (
        <>
          <ExplorerSectionCard title={m["explorer.contracts.details.contractDetails"]()}>
            <ExplorerKeyValueRow label={m["explorer.txs.sender"]()}>
              <GlobalText>{`${account.username} #${account.index}`}</GlobalText>
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.accounts.title"]()}>
              <ExplorerHashValue value={account.address} />
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.codeHash"]()}>
              <ExplorerHashValue value={account.codeHash} />
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.admin"]()}>
              {account.admin ? (
                <AddressVisualizer
                  withIcon
                  address={account.admin}
                  onClick={(url) => router.push(url as never)}
                />
              ) : (
                <GlobalText>None</GlobalText>
              )}
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.balances"]()}>
              <Badge color="green" text={`${totalBalance} (${totalCoins} Assets)`} />
            </ExplorerKeyValueRow>
          </ExplorerSectionCard>

          <ExplorerAssetsList balances={account.balances} />

          <ExplorerTransactionsList
            transactions={txData?.nodes || []}
            pagination={{ ...pagination, isLoading: txsLoading }}
            onOpenTx={(hash) => router.push(`/tx/${hash}` as never)}
            onOpenBlock={(height) => router.push(`/block/${height}` as never)}
            onOpenAddress={(url) => router.push(url as never)}
          />
        </>
      ) : null}
    </ExplorerScreen>
  );
}
