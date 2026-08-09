import { useQuery } from "@tanstack/react-query";
import { usePublicClient } from "../usePublicClient.js";
import { getArchiveApi } from "../../archive/api.js";

import type { IndexedTransaction } from "@left-curve/types";

export type UseExplorerTransactionReturn = IndexedTransaction | null;

export function useExplorerTransaction(txHash: string) {
  const client = usePublicClient();
  const archive = getArchiveApi(client.chain);

  return useQuery<UseExplorerTransactionReturn>({
    queryKey: ["tx", txHash, archive ? "archive" : "public"],
    queryFn: async () => {
      const txs = await (archive ?? client).searchTxs({ hash: txHash });
      if (!txs.nodes.length) return null;
      return txs.nodes[0] || null;
    },
    enabled: !!txHash,
  });
}
