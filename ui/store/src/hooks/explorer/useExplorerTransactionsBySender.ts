import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { getArchiveApi } from "../../archive/api.js";
import { usePublicClient } from "../usePublicClient.js";
import { useQueryWithPagination } from "../useQueryWithPagination.js";

import type { Address } from "@left-curve/types";

const PAGE_SIZE = 10;

type ArchivePaginationState = {
  address: Address;
  after?: string;
  previousAfters: (string | undefined)[];
};

export function useExplorerTransactionsBySender(address: Address, enabled = true) {
  const client = usePublicClient();
  const archive = getArchiveApi(client.chain);
  const [archivePagination, setArchivePagination] = useState<ArchivePaginationState>({
    address,
    after: undefined,
    previousAfters: [],
  });

  const archiveState =
    archivePagination.address === address
      ? archivePagination
      : { address, after: undefined, previousAfters: [] };

  const archiveQuery = useQuery({
    enabled: Boolean(archive && enabled && address),
    queryKey: ["explorer_transactions", address, "archive", archiveState.after],
    queryFn: () =>
      archive!.searchTxs({
        senderAddress: address,
        first: PAGE_SIZE,
        after: archiveState.after,
      }),
    placeholderData: keepPreviousData,
  });

  const publicQuery = useQueryWithPagination({
    enabled: !archive && enabled && !!address,
    queryKey: ["explorer_transactions", address],
    queryFn: async ({ pageParam }) => client.searchTxs({ senderAddress: address, ...pageParam }),
  });

  if (archive) {
    const hasPreviousPage = archiveState.previousAfters.length > 0;

    return {
      ...archiveQuery,
      pagination: {
        goNext: () => {
          const nextAfter = archiveQuery.data?.pageInfo.endCursor ?? undefined;
          if (!archiveQuery.data?.pageInfo.hasNextPage || !nextAfter) return;
          setArchivePagination((state) => ({
            address,
            after: nextAfter,
            previousAfters:
              state.address === address
                ? [...state.previousAfters, archiveState.after]
                : [undefined],
          }));
        },
        goPrev: () => {
          if (!hasPreviousPage) return;
          setArchivePagination((state) => {
            const previousAfters = state.address === address ? state.previousAfters : [];
            const nextPreviousAfters = previousAfters.slice(0, -1);
            return {
              address,
              after: previousAfters.at(-1),
              previousAfters: nextPreviousAfters,
            };
          });
        },
        hasNextPage: archiveQuery.data?.pageInfo.hasNextPage ?? false,
        hasPreviousPage,
      },
    };
  }

  return publicQuery;
}
