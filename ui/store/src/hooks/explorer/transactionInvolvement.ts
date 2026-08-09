import type { Address, GraphqlQueryResult, IndexedTransaction } from "@left-curve/types";

export type ExplorerTransactionRole = "sender" | "participant";

export type ExplorerTransaction = IndexedTransaction & {
  involvement: ExplorerTransactionRole[];
};

const ROLE_ORDER: ExplorerTransactionRole[] = ["sender", "participant"];

export function getExplorerTransactionKey(transaction: IndexedTransaction): string {
  return [transaction.blockHeight, transaction.transactionType, transaction.transactionIdx].join(
    ":",
  );
}

export function addTransactionInvolvement(
  transaction: IndexedTransaction,
  address: Address,
  supportsParticipants: boolean,
): ExplorerTransaction {
  const isSender =
    !supportsParticipants || transaction.sender?.toLowerCase() === address.toLowerCase();

  return {
    ...transaction,
    involvement: [supportsParticipants && !isSender ? "participant" : "sender"],
  };
}

export function addResultInvolvement(
  result: GraphqlQueryResult<IndexedTransaction>,
  address: Address,
  supportsParticipants: boolean,
): GraphqlQueryResult<ExplorerTransaction> {
  return {
    ...result,
    nodes: result.nodes.map((transaction) =>
      addTransactionInvolvement(transaction, address, supportsParticipants),
    ),
    edge: (result.edge ?? []).map((edge) => ({
      ...edge,
      node: addTransactionInvolvement(edge.node, address, supportsParticipants),
    })),
  };
}

export function mergeExplorerTransactions(
  transactions: ExplorerTransaction[],
): ExplorerTransaction[] {
  const transactionsByKey = new Map<string, ExplorerTransaction>();

  for (const transaction of transactions) {
    const key = getExplorerTransactionKey(transaction);
    const existing = transactionsByKey.get(key);

    if (!existing) {
      transactionsByKey.set(key, transaction);
      continue;
    }

    const involvement = ROLE_ORDER.filter(
      (role) => existing.involvement.includes(role) || transaction.involvement.includes(role),
    );
    transactionsByKey.set(key, { ...existing, involvement });
  }

  return [...transactionsByKey.values()];
}
