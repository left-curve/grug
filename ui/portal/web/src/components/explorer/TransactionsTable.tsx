import { useNavigate } from "@tanstack/react-router";

import { m } from "@left-curve/foundation/paraglide/messages.js";
import { getExplorerTransactionKey, type ExplorerTransactionRole } from "@left-curve/store";

import { Badge, Cell, CursorPagination, Table } from "@left-curve/applets-kit";

import type { TableClassNames, TableColumn } from "@left-curve/applets-kit";
import type { IndexedTransaction } from "@left-curve/types";

type TransactionRow = IndexedTransaction & {
  involvement?: ExplorerTransactionRole[];
};

function getRoleLabel(role: ExplorerTransactionRole): string {
  return role === "sender"
    ? m["explorer.txs.roles.sender"]()
    : m["explorer.txs.roles.participant"]();
}

type TransactionsTableProps = {
  transactions?: TransactionRow[];
  pagination?: {
    isLoading: boolean;
    goNext: () => void;
    goPrev: () => void;
    hasNextPage: boolean;
    hasPreviousPage: boolean;
  };
  classNames?: TableClassNames;
};

export const TransactionsTable: React.FC<TransactionsTableProps> = ({
  transactions,
  pagination,
  classNames,
}) => {
  const navigate = useNavigate();
  const showRole = transactions?.some((transaction) => transaction.involvement !== undefined);

  const columns: TableColumn<TransactionRow> = [
    {
      header: "Hash",
      cell: ({ row }) => {
        const { hash } = row.original;
        if (!hash) return <span>—</span>;

        return (
          <Cell.TxHash
            hash={hash}
            href={`/tx/${hash}`}
            navigate={() => navigate({ to: `/tx/${hash}` })}
          />
        );
      },
    },
    {
      header: "Block",
      cell: ({ row }) => (
        <Cell.BlockHeight
          blockHeight={row.original.blockHeight}
          href={`/block/${row.original.blockHeight}`}
          navigate={() => navigate({ to: `/block/${row.original.blockHeight}` })}
        />
      ),
    },
    {
      header: "Age",
      cell: ({ row }) => <Cell.Age date={row.original.createdAt} addSuffix />,
    },
    {
      header: "Sender",
      cell: ({ row }) => {
        if (!row.original.sender) return <span>—</span>;

        return (
          <Cell.Sender sender={row.original.sender} navigate={(url) => navigate({ to: url })} />
        );
      },
    },
    ...(showRole
      ? ([
          {
            header: m["explorer.txs.role"](),
            cell: ({ row }) => (
              <div className="flex items-center gap-1">
                {row.original.involvement?.map((role) => (
                  <Badge
                    key={role}
                    color={role === "sender" ? "blue" : "rice"}
                    text={getRoleLabel(role)}
                  />
                ))}
              </div>
            ),
          },
        ] satisfies TableColumn<TransactionRow>)
      : []),
    {
      header: "Actions",
      cell: ({ row }) =>
        row.original.messages.length ? <Cell.TxMessages messages={row.original.messages} /> : "—",
    },
    {
      header: "Result",
      cell: ({ row }) => {
        const { hasSucceeded, messages } = row.original;

        return (
          <Cell.TxResult
            className="justify-end"
            isSuccess={hasSucceeded}
            text={m["explorer.txs.result"]({ result: String(hasSucceeded) })}
            total={messages.length}
          />
        );
      },
    },
  ];

  if (!transactions?.length) return null;

  return (
    <Table
      data={transactions}
      columns={columns}
      getRowId={getExplorerTransactionKey}
      classNames={classNames}
      bottomContent={
        pagination ? (
          <CursorPagination
            {...pagination}
            className="flex w-full justify-end gap-2"
            nextLabel={m["pagination.next"]()}
            previousLabel={m["pagination.previous"]()}
          />
        ) : null
      }
    />
  );
};
