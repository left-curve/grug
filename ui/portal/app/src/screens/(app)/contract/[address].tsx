import { m } from "@left-curve/foundation/paraglide/messages.js";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useApp } from "@left-curve/foundation";
import {
  useExplorerContract,
  useExplorerTransactionsByAddress,
  usePrices,
} from "@left-curve/store";

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

export default function ContractExplorerScreen() {
  const { address } = useLocalSearchParams<{ address: string }>();
  const router = useRouter();

  const { data: contract, isLoading } = useExplorerContract(address as `0x${string}`);
  const { settings } = useApp();
  const { calculateBalance } = usePrices();

  const {
    data: txData,
    pagination,
    isLoading: txsLoading,
  } = useExplorerTransactionsByAddress(address as `0x${string}`, !!contract);

  const totalCoins = contract ? Object.values(contract.balances).length : 0;
  const totalBalance = contract
    ? calculateBalance(contract.balances, {
        format: true,
        formatOptions: { ...settings.formatNumberOptions, currency: "usd" },
      })
    : "$0";

  return (
    <ExplorerScreen>
      <MobileTitle title={m["explorer.contracts.title"]()} />

      {isLoading ? (
        <ExplorerSectionCard title={m["explorer.contracts.details.contractDetails"]()}>
          <Skeleton className="w-full h-10" />
          <Skeleton className="w-full h-10" />
          <Skeleton className="w-full h-10" />
        </ExplorerSectionCard>
      ) : null}

      {!isLoading && !contract ? (
        <ExplorerNotFound
          title={m["explorer.contracts.notFound.title"]()}
          description={
            <>
              {m["explorer.contracts.notFound.pre"]()}{" "}
              <GlobalText className="underline">{address}</GlobalText>{" "}
              {m["explorer.contracts.notFound.description"]()}
            </>
          }
        />
      ) : null}

      {contract ? (
        <>
          <ExplorerSectionCard title={m["explorer.contracts.details.contractDetails"]()}>
            <ExplorerKeyValueRow label={m["explorer.accounts.title"]()}>
              <ExplorerHashValue value={contract.address} />
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.codeHash"]()}>
              <ExplorerHashValue value={contract.codeHash} />
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.admin"]()}>
              <GlobalText>{contract.admin || "None"}</GlobalText>
            </ExplorerKeyValueRow>
            <ExplorerKeyValueRow label={m["explorer.contracts.details.balances"]()}>
              <Badge color="green" text={`${totalBalance} (${totalCoins} Assets)`} />
            </ExplorerKeyValueRow>
          </ExplorerSectionCard>

          <ExplorerAssetsList balances={contract.balances} />

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
