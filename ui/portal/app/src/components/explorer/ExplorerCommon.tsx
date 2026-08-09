import { m } from "@left-curve/foundation/paraglide/messages.js";
import { formatUnits } from "@left-curve/utils";
import { useApp } from "@left-curve/foundation";
import { getExplorerTransactionKey, useConfig, usePrices } from "@left-curve/store";
import { useMemo, useState } from "react";
import { Pressable, ScrollView, View } from "react-native";
import { AddressVisualizer } from "~/components/foundation/AddressVisualizer";
import {
  Badge,
  Button,
  GlobalText,
  IconChevronDown,
  IconLink,
  TextCopy,
} from "~/components/foundation";

import type { Address, Coins, IndexedTransaction } from "@left-curve/types";
import type { ExplorerTransactionRole } from "@left-curve/store";
import type React from "react";

type ExplorerTransactionRow = IndexedTransaction & {
  involvement?: ExplorerTransactionRole[];
};

function getRoleLabel(role: ExplorerTransactionRole): string {
  return role === "sender"
    ? m["explorer.txs.roles.sender"]()
    : m["explorer.txs.roles.participant"]();
}

export const ExplorerScreen: React.FC<React.PropsWithChildren> = ({ children }) => {
  return (
    <ScrollView
      className="flex-1 w-full bg-surface-primary-rice"
      contentContainerClassName="p-4 pb-20 gap-4"
      showsVerticalScrollIndicator={false}
    >
      {children}
    </ScrollView>
  );
};

export const ExplorerSectionCard: React.FC<
  React.PropsWithChildren<{ title?: string; className?: string }>
> = ({ title, children, className }) => {
  return (
    <View className={`w-full rounded-xl bg-surface-secondary-rice p-4 gap-3 ${className || ""}`}>
      {title ? <GlobalText className="h4-bold text-ink-primary-900">{title}</GlobalText> : null}
      {children}
    </View>
  );
};

export const ExplorerKeyValueRow: React.FC<
  React.PropsWithChildren<{ label: string; valueClassName?: string }>
> = ({ label, children, valueClassName }) => {
  return (
    <View className="flex flex-col gap-1">
      <GlobalText className="diatype-sm-medium text-ink-tertiary-500">{label}</GlobalText>
      <View className={valueClassName}>{children}</View>
    </View>
  );
};

export const ExplorerNotFound: React.FC<{ title: string; description: React.ReactNode }> = ({
  title,
  description,
}) => {
  return (
    <ExplorerSectionCard className="items-center justify-center min-h-[300px]">
      <GlobalText className="exposure-m-italic text-ink-secondary-700 text-center">
        {title}
      </GlobalText>
      <GlobalText className="diatype-m-medium text-ink-tertiary-500 text-center">
        {description}
      </GlobalText>
    </ExplorerSectionCard>
  );
};

export const ExplorerJsonBlock: React.FC<{ data: unknown }> = ({ data }) => {
  return (
    <ScrollView
      horizontal
      className="w-full rounded-md bg-primitives-gray-light-700 p-3"
      contentContainerClassName="min-w-full"
    >
      <GlobalText className="diatype-sm-regular text-primitives-white-light-100">
        {JSON.stringify(data, null, 2)}
      </GlobalText>
    </ScrollView>
  );
};

export const ExplorerAccordion: React.FC<
  React.PropsWithChildren<{ title: string; defaultExpanded?: boolean }>
> = ({ title, defaultExpanded = false, children }) => {
  const [expanded, setExpanded] = useState(defaultExpanded);

  return (
    <View className="gap-2">
      <Pressable
        className="flex flex-row items-center justify-between bg-surface-primary-rice rounded-md p-3"
        onPress={() => setExpanded((v) => !v)}
      >
        <GlobalText className="diatype-m-bold capitalize">{title}</GlobalText>
        <IconChevronDown
          className={`w-5 h-5 text-ink-tertiary-500 ${expanded ? "rotate-180" : ""}`}
        />
      </Pressable>
      {expanded ? children : null}
    </View>
  );
};

export const ExplorerAssetsList: React.FC<{ balances: Coins }> = ({ balances }) => {
  const { coins } = useConfig();
  const { getPrice } = usePrices();
  const { settings } = useApp();

  const data = useMemo(() => {
    return Object.entries(balances).map(([denom, amount]) => {
      const coin = coins.getCoinInfo(denom);
      const humanizedAmount = formatUnits(amount, coin.decimals);
      const price = getPrice(humanizedAmount, denom, {
        format: true,
        formatOptions: settings.formatNumberOptions,
      });

      return {
        denom,
        symbol: coin.symbol,
        amount: humanizedAmount,
        price,
      };
    });
  }, [balances, coins, getPrice, settings.formatNumberOptions]);

  if (!data.length) return null;

  return (
    <ExplorerSectionCard title={m["explorer.contracts.details.balances"]()}>
      {data.map((asset) => (
        <View
          key={asset.denom}
          className="flex flex-row items-center justify-between py-2 border-b border-outline-secondary-gray"
        >
          <GlobalText className="diatype-m-bold">{asset.symbol}</GlobalText>
          <View className="items-end">
            <GlobalText className="diatype-sm-medium text-ink-secondary-700">
              {asset.amount}
            </GlobalText>
            <GlobalText className="diatype-sm-regular text-ink-tertiary-500">
              ${asset.price}
            </GlobalText>
          </View>
        </View>
      ))}
    </ExplorerSectionCard>
  );
};

type TransactionsListProps = {
  transactions: ExplorerTransactionRow[];
  onOpenTx: (hash: string) => void;
  onOpenBlock: (height: number) => void;
  onOpenAddress: (url: string) => void;
  pagination?: {
    isLoading: boolean;
    goNext: () => void;
    goPrev: () => void;
    hasNextPage: boolean;
    hasPreviousPage: boolean;
  };
};

export const ExplorerTransactionsList: React.FC<TransactionsListProps> = ({
  transactions,
  onOpenTx,
  onOpenBlock,
  onOpenAddress,
  pagination,
}) => {
  if (!transactions.length) return null;

  return (
    <ExplorerSectionCard title={m["explorer.txs.title"]()}>
      {transactions.map((tx) => (
        <View
          key={getExplorerTransactionKey(tx)}
          className="rounded-md border border-outline-secondary-gray p-3 gap-2"
        >
          <ExplorerKeyValueRow label="Hash">
            {tx.hash ? (
              <Pressable className="flex-row items-center gap-1" onPress={() => onOpenTx(tx.hash)}>
                <GlobalText className="diatype-sm-bold text-ink-secondary-blue">
                  {tx.hash.slice(0, 18)}...
                </GlobalText>
                <IconLink className="w-4 h-4 text-ink-secondary-blue" />
              </Pressable>
            ) : (
              <GlobalText>—</GlobalText>
            )}
          </ExplorerKeyValueRow>

          <ExplorerKeyValueRow label={m["explorer.txs.block"]()}>
            <Pressable
              className="flex-row items-center gap-1"
              onPress={() => onOpenBlock(tx.blockHeight)}
            >
              <GlobalText className="diatype-sm-bold text-ink-secondary-blue">
                {tx.blockHeight}
              </GlobalText>
              <IconLink className="w-4 h-4 text-ink-secondary-blue" />
            </Pressable>
          </ExplorerKeyValueRow>

          <ExplorerKeyValueRow label={m["explorer.txs.sender"]()}>
            {tx.sender ? (
              <AddressVisualizer
                withIcon
                address={tx.sender as Address}
                classNames={{ text: "diatype-sm-bold" }}
                onClick={onOpenAddress}
              />
            ) : (
              <GlobalText>—</GlobalText>
            )}
          </ExplorerKeyValueRow>

          {tx.involvement ? (
            <ExplorerKeyValueRow label={m["explorer.txs.role"]()} valueClassName="flex-row gap-1">
              {tx.involvement.map((role) => (
                <Badge
                  key={role}
                  color={role === "sender" ? "blue" : "rice"}
                  text={getRoleLabel(role)}
                />
              ))}
            </ExplorerKeyValueRow>
          ) : null}

          <ExplorerKeyValueRow label={m["explorer.txs.result"]({ result: "" })}>
            <Badge
              color={tx.hasSucceeded ? "green" : "red"}
              text={tx.hasSucceeded ? m["explorer.txs.success"]() : m["explorer.txs.failed"]()}
            />
          </ExplorerKeyValueRow>
        </View>
      ))}

      {pagination ? (
        <View className="flex-row justify-end gap-2 pt-2">
          <Button
            variant="secondary"
            size="sm"
            isDisabled={!pagination.hasPreviousPage || pagination.isLoading}
            onPress={pagination.goPrev}
          >
            <GlobalText>{m["pagination.previous"]()}</GlobalText>
          </Button>
          <Button
            variant="secondary"
            size="sm"
            isDisabled={!pagination.hasNextPage || pagination.isLoading}
            onPress={pagination.goNext}
          >
            <GlobalText>{m["pagination.next"]()}</GlobalText>
          </Button>
        </View>
      ) : null}
    </ExplorerSectionCard>
  );
};

export const ExplorerHashValue: React.FC<{ value: string }> = ({ value }) => {
  return (
    <View className="flex-row items-center flex-wrap gap-1">
      <GlobalText className="diatype-sm-medium break-all">{value}</GlobalText>
      <TextCopy className="w-4 h-4 text-ink-tertiary-500" copyText={value} />
    </View>
  );
};
