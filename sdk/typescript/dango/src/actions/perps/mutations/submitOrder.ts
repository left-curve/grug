import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";
import type { ChildOrder, Client, PerpsOrderKind, Signer } from "@left-curve/types";

export type SubmitPerpsOrderParameters = {
  sender: Address;
  pairId: string;
  size: string;
  kind: PerpsOrderKind;
  reduceOnly: boolean;
  tp?: ChildOrder;
  sl?: ChildOrder;
};

export type SubmitPerpsOrderReturnType = SignAndBroadcastTxReturnType;

export async function submitPerpsOrder(
  client: Client<Signer>,
  parameters: SubmitPerpsOrderParameters,
): SubmitPerpsOrderReturnType {
  const { sender, pairId, size, kind, reduceOnly, tp, sl } = parameters;

  const { addresses } = await getAppConfig(client);

  const buildChildOrder = (child: ChildOrder) => ({
    triggerPrice: child.triggerPrice,
    maxSlippage: child.maxSlippage,
    ...(child.size ? { size: child.size } : {}),
  });

  // Strip a `null` / `undefined` `clientOrderId` from the limit body so it's
  // absent (not `null`) in the JSON message, keeping the signed canonical form
  // stable.
  const limitHasClientOrderId = "limit" in kind && kind.limit.clientOrderId != null;
  const normalizedKind: PerpsOrderKind =
    "limit" in kind
      ? {
          limit: {
            limitPrice: kind.limit.limitPrice,
            timeInForce: kind.limit.timeInForce,
            ...(limitHasClientOrderId ? { clientOrderId: kind.limit.clientOrderId as string } : {}),
          },
        }
      : kind;

  const msg = {
    trade: {
      submitOrder: {
        pairId,
        size,
        kind: normalizedKind,
        reduceOnly,
        ...(tp ? { tp: buildChildOrder(tp) } : {}),
        ...(sl ? { sl: buildChildOrder(sl) } : {}),
      },
    },
  };

  return await execute(client, {
    sender,
    execute: {
      msg,
      contract: addresses.perps,
    },
  });
}
