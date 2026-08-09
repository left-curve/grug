import type {
  Address,
  Chain,
  GraphqlQueryResult,
  IndexedBlock,
  IndexedMessage,
  IndexedTransaction,
  Json,
} from "@left-curve/types";

const ARCHIVE_URL_BY_CHAIN_ID: Record<string, string> = {
  "dango-1": "https://api-archive-mainnet.dango.zone",
  "dango-testnet-1": "https://api-archive-testnet.dango.zone",
};
const EMPTY_PAGE_INFO = {
  hasNextPage: false,
  hasPreviousPage: false,
  startCursor: null,
  endCursor: null,
};

type ArchiveApi = {
  queryBlock: (height?: number) => Promise<IndexedBlock | null>;
  searchTxs: (
    parameters: ArchiveSearchTxsParameters,
  ) => Promise<GraphqlQueryResult<IndexedTransaction>>;
};

type ArchiveSearchTxsParameters = {
  hash?: string;
  senderAddress?: string;
  involvedAddress?: string;
  first?: number;
  after?: string;
  sortBy?: string;
};

type ArchivePage<T> = {
  items: T[];
  pageInfo: {
    hasNextPage: boolean;
    endCursor?: string | null;
  };
};

type ArchiveTransaction = {
  blockHeight: number;
  idx: number;
  kind: "transaction" | "cron";
  hash?: string | null;
  sender?: string | null;
  success: boolean;
  timestamp: string;
  tx?: ArchiveRawTx | null;
  outcome?: ArchiveUnitOutcome | null;
};

type ArchiveFullBlock = {
  block: {
    info: {
      height: number;
      timestamp: unknown;
      hash: string;
    };
    txs: [ArchiveRawTx, string][];
  };
  outcome: {
    app_hash?: string;
    appHash?: string;
    cron_outcomes?: unknown[];
    cronOutcomes?: unknown[];
    tx_outcomes?: ArchiveTxOutcome[];
    txOutcomes?: ArchiveTxOutcome[];
  };
};

type ArchiveRawTx = {
  sender?: string;
  msgs?: unknown[];
  gas_limit?: number | string;
  gasLimit?: number | string;
};

type ArchiveUnitOutcome = {
  transaction?: ArchiveTxOutcome;
  cron?: unknown;
};

type ArchiveTxOutcome = {
  gas_limit?: number | string;
  gasLimit?: number | string;
  gas_used?: number | string;
  gasUsed?: number | string;
  result?: unknown;
  events?: unknown;
};

export function getArchiveApi(chain: Chain | undefined): ArchiveApi | null {
  const archiveUrl = chain ? ARCHIVE_URL_BY_CHAIN_ID[chain.id] : undefined;
  if (!archiveUrl) return null;

  return {
    queryBlock: (height) => queryBlock(archiveUrl, height),
    searchTxs: (parameters) => searchTxs(archiveUrl, parameters),
  };
}

async function queryBlock(archiveUrl: string, height?: number): Promise<IndexedBlock | null> {
  const blockPath = height === undefined ? "/blocks/latest" : `/blocks/${height}`;
  const block = await archiveFetch<ArchiveFullBlock>(archiveUrl, blockPath, undefined, {
    allowNotFound: true,
  });
  return block ? normalizeBlock(block) : null;
}

async function searchTxs(
  archiveUrl: string,
  parameters: ArchiveSearchTxsParameters,
): Promise<GraphqlQueryResult<IndexedTransaction>> {
  if (parameters.hash) {
    const hash = parameters.hash.replace(/^0x/i, "").toUpperCase();
    const items = await archiveFetch<ArchiveTransaction[]>(
      archiveUrl,
      `/transactions/${encodeURIComponent(hash)}`,
    );
    return toGraphqlResult((items ?? []).map(normalizeTransaction));
  }

  const address = parameters.involvedAddress ?? parameters.senderAddress;

  if (address) {
    const page = await archiveFetch<ArchivePage<ArchiveTransaction>>(
      archiveUrl,
      `/transactions/involving/${encodeURIComponent(address)}`,
      {
        role: parameters.involvedAddress ? undefined : "sender",
        first: String(parameters.first ?? 10),
        after: parameters.after,
      },
    );
    if (!page) return toGraphqlResult([]);
    return toGraphqlResult(page.items.map(normalizeTransaction), page.pageInfo);
  }

  return toGraphqlResult([]);
}

async function archiveFetch<T>(
  archiveUrl: string,
  path: string,
  query?: Record<string, string | undefined>,
  options: { allowNotFound?: boolean } = {},
): Promise<T | null> {
  const url = new URL(path, archiveUrl);
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value !== undefined) url.searchParams.set(key, value);
  }

  const response = await fetch(url);
  if (response.status === 404 && options.allowNotFound) return null;
  if (!response.ok) {
    throw new Error(`archive request failed (${response.status}) for ${url.pathname}`);
  }
  return (await response.json()) as T;
}

function normalizeBlock(fullBlock: ArchiveFullBlock): IndexedBlock {
  const blockHeight = fullBlock.block.info.height;
  const createdAt = normalizeTimestamp(fullBlock.block.info.timestamp);
  const txOutcomes = fullBlock.outcome.tx_outcomes ?? fullBlock.outcome.txOutcomes ?? [];
  const cronOutcomes = fullBlock.outcome.cron_outcomes ?? fullBlock.outcome.cronOutcomes ?? [];

  return {
    blockHeight,
    createdAt,
    hash: fullBlock.block.info.hash,
    appHash: fullBlock.outcome.app_hash ?? fullBlock.outcome.appHash ?? "",
    cronsOutcomes: cronOutcomes.map((outcome) =>
      stringifyJson(outcome),
    ) as unknown as IndexedBlock["cronsOutcomes"],
    transactions: fullBlock.block.txs.map(([tx, hash], idx) =>
      normalizeRawTransaction({
        blockHeight,
        createdAt,
        hash,
        idx,
        sender: tx.sender,
        tx,
        txOutcome: txOutcomes[idx],
      }),
    ),
  };
}

function normalizeTransaction(transaction: ArchiveTransaction): IndexedTransaction {
  const txOutcome = transaction.outcome?.transaction;

  return normalizeRawTransaction({
    blockHeight: transaction.blockHeight,
    createdAt: transaction.timestamp,
    hash: transaction.hash ?? "",
    idx: transaction.idx,
    kind: transaction.kind,
    sender: transaction.sender ?? transaction.tx?.sender,
    success: transaction.success,
    tx: transaction.tx ?? undefined,
    txOutcome,
  });
}

function normalizeRawTransaction(parameters: {
  blockHeight: number;
  createdAt: string;
  hash: string;
  idx: number;
  kind?: ArchiveTransaction["kind"];
  sender?: string | null;
  success?: boolean;
  tx?: ArchiveRawTx;
  txOutcome?: ArchiveTxOutcome;
}): IndexedTransaction {
  const success = parameters.success ?? isSuccessResult(parameters.txOutcome?.result);
  const sender = (parameters.sender ?? "") as Address;
  const gasWanted = toNumber(
    parameters.tx?.gas_limit ??
      parameters.tx?.gasLimit ??
      parameters.txOutcome?.gas_limit ??
      parameters.txOutcome?.gasLimit,
  );
  const gasUsed = toNumber(parameters.txOutcome?.gas_used ?? parameters.txOutcome?.gasUsed);

  return {
    blockHeight: parameters.blockHeight,
    createdAt: parameters.createdAt,
    transactionType: parameters.kind === "cron" ? "CRON" : "TX",
    transactionIdx: parameters.idx,
    sender,
    hash: parameters.hash,
    hasSucceeded: success,
    errorMessage: extractErrorMessage(parameters.txOutcome?.result, success),
    gasWanted,
    gasUsed,
    messages: normalizeMessages(parameters.tx?.msgs, {
      blockHeight: parameters.blockHeight,
      createdAt: parameters.createdAt,
      sender,
    }),
    nestedEvents: stringifyJson(parameters.txOutcome?.events ?? []),
  };
}

function normalizeMessages(
  messages: unknown[] | undefined,
  context: { blockHeight: number; createdAt: string; sender: Address },
): IndexedMessage[] {
  return (messages ?? []).map((message, orderIdx) => {
    const { methodName, payload } = getMessageParts(message);
    const contractAddr = getStringProperty(payload, "contract") ?? "";

    return {
      methodName,
      blockHeight: context.blockHeight,
      contractAddr: contractAddr as Address,
      senderAddr: context.sender,
      orderIdx,
      createdAt: context.createdAt,
      data: (isRecord(message) ? message : { [methodName]: payload }) as Record<string, Json>,
    };
  });
}

function getMessageParts(message: unknown): { methodName: string; payload: unknown } {
  if (!isRecord(message)) return { methodName: "message", payload: message };
  const [methodName, payload] = Object.entries(message)[0] ?? ["message", message];
  return { methodName, payload };
}

function getStringProperty(value: unknown, key: string): string | undefined {
  if (!isRecord(value)) return undefined;
  const property = value[key];
  return typeof property === "string" ? property : undefined;
}

function toGraphqlResult<T>(
  nodes: T[],
  pageInfo?: ArchivePage<T>["pageInfo"],
): GraphqlQueryResult<T> {
  return {
    pageInfo: {
      ...EMPTY_PAGE_INFO,
      hasNextPage: pageInfo?.hasNextPage ?? false,
      endCursor: pageInfo?.endCursor ?? null,
    },
    edge: nodes.map((node, index) => ({
      cursor: index === nodes.length - 1 ? (pageInfo?.endCursor ?? "") : "",
      node,
    })),
    nodes,
  };
}

function isSuccessResult(result: unknown): boolean {
  if (!isRecord(result)) return true;
  if ("ok" in result || "Ok" in result) return true;
  if ("err" in result || "Err" in result || "error" in result) return false;
  return true;
}

function extractErrorMessage(result: unknown, success: boolean): string {
  if (success) return "";
  if (!isRecord(result)) return stringifyJson(result);

  const error = result.err ?? result.Err ?? result.error ?? result;
  return typeof error === "string" ? error : stringifyJson(error);
}

function normalizeTimestamp(timestamp: unknown): string {
  if (typeof timestamp === "string") {
    const seconds = timestamp.match(/^(\d+)(?:\.(\d+))?$/);
    if (seconds) {
      return secondsToIso(seconds[1], seconds[2]);
    }
    return timestamp;
  }

  if (typeof timestamp === "number" && Number.isFinite(timestamp)) {
    return new Date(timestamp * 1000).toISOString();
  }

  return stringifyJson(timestamp);
}

function stringifyJson(value: unknown): string {
  if (typeof value === "string") return value;
  try {
    return JSON.stringify(value ?? null);
  } catch {
    return String(value);
  }
}

function toNumber(value: number | string | undefined): number {
  if (value === undefined) return 0;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function secondsToIso(seconds: string, fraction = ""): string {
  const milliseconds = Number(seconds) * 1000 + Number(fraction.padEnd(3, "0").slice(0, 3));
  return new Date(milliseconds).toISOString();
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
