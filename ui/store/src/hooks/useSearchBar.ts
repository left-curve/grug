import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useMemo, useReducer, useState } from "react";
import { getArchiveApi } from "../archive/api.js";
import { usePublicClient } from "./usePublicClient.js";
import { useAppConfig } from "./useAppConfig.js";

import { wait } from "@left-curve/utils";
import { isValidAddress } from "@left-curve/sdk";
import fuzzysort from "fuzzysort";

import type { AppletMetadata } from "../types/applets.js";
import type {
  AccountDetails,
  Address,
  ContractInfo,
  IndexedBlock,
  IndexedTransaction,
  User,
} from "@left-curve/types";

export type UseSearchBarParameters = {
  debounceMs?: number;
  applets: Record<string, AppletMetadata>;
  favApplets: string[];
};

export type SearchBarResult = {
  block?: IndexedBlock;
  txs: IndexedTransaction[];
  applets: AppletMetadata[];
  contracts: (ContractInfo & { address: Address })[];
  account?: AccountDetails;
  user?: User;
};

export function useSearchBar(parameters: UseSearchBarParameters) {
  const applets = Object.values(parameters.applets);
  const { debounceMs = 300, favApplets } = parameters;
  const { data: appConfig } = useAppConfig();
  const [searchText, setSearchText] = useState("");

  const allContracts = useMemo(() => {
    return Object.entries(appConfig.addresses)
      .filter(([key]) => !key.startsWith("0x"))
      .map(([key, value]) => ({ label: key, address: value })) as (ContractInfo & {
      address: Address;
    })[];
  }, [appConfig]);

  const noResult: SearchBarResult = useMemo(
    () => ({
      block: undefined,
      txs: [],
      applets: Object.values(applets.filter((applet) => favApplets.includes(applet.id))),
      contracts: allContracts,
      account: undefined,
      user: undefined,
    }),
    [applets, favApplets, allContracts],
  );

  const [searchResult, setSearchResult] = useReducer(
    (os: SearchBarResult, ns: Partial<SearchBarResult>) => ({ ...os, ...ns }),
    noResult,
  );

  const queryClient = useQueryClient();
  const client = usePublicClient();
  const archive = getArchiveApi(client.chain);

  const allNotFavApplets = useMemo(() => {
    return Object.values(applets).filter((applet) => !favApplets.includes(applet.id));
  }, [applets, favApplets]);

  const { data: _, ...query } = useQuery({
    queryKey: ["searchBar", searchText, favApplets, archive ? "archive" : "public"],
    queryFn: async ({ signal }) => {
      if (!searchText.length) {
        setSearchResult(noResult);
        return null;
      }

      setSearchResult({
        applets: fuzzysort
          .go(searchText, applets, {
            threshold: 0.5,
            all: false,
            keys: ["title", "description", (obj: AppletMetadata) => obj.keywords?.join()],
          })
          .map(({ obj }) => obj),
        contracts: fuzzysort
          .go(searchText, allContracts, {
            threshold: 0.5,
            all: false,
            key: "label",
          })
          .map(({ obj }) => obj),
      });

      await wait(debounceMs);
      if (signal.aborted) return;

      const promises: Promise<unknown>[] = [];
      const { accountFactory } = appConfig;
      const transactionHash = getTransactionHash(searchText);

      if (isValidAddress(searchText)) {
        // search for contract
        promises.push(
          (async () => {
            const contractInfo = await client.getContractInfo({ address: searchText as Address });
            const isAccount = accountFactory.codeHash === contractInfo.codeHash;

            if (isAccount) {
              const account = await client.getAccountInfo({ address: searchText as Address });
              setSearchResult({ account: account ? account : undefined });
            } else {
              setSearchResult({
                contracts: [{ ...contractInfo, address: searchText as Address }],
              });
            }
          })(),
        );
      } else if (transactionHash) {
        // search for tx hash
        promises.push(
          (async () => {
            const txs = await (archive ?? client).searchTxs({ hash: transactionHash });
            if (txs.nodes.length) {
              setSearchResult({ txs: txs.nodes });
              queryClient.setQueryData(["tx", searchText], txs.nodes[0]);
            }
          })(),
        );
      } else if (!Number.isNaN(Number(searchText))) {
        promises.push(
          (async () => {
            const block = archive
              ? await archive.queryBlock(+searchText)
              : await client.queryBlock({ height: +searchText });
            setSearchResult({ block: block ?? undefined });
            if (block) queryClient.setQueryData(["block", searchText], block);
          })(),
        );
      } else {
        promises.push(
          (async () => {
            try {
              const user = await client.getUser({ userIndexOrName: { name: searchText } });
              if (user) {
                setSearchResult({ user });
              }
            } catch {
              setSearchResult({ user: undefined });
            }
          })(),
        );
      }

      return await Promise.allSettled(promises);
    },
  });

  return { searchText, setSearchText, searchResult, allNotFavApplets, ...query };
}

function getTransactionHash(value: string): string | null {
  const hash = value.replace(/^0x/i, "");
  return /^[a-fA-F0-9]{64}$/.test(hash) ? hash : null;
}
