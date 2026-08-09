import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";
import type { Client, PerpsCancelConditionalOrderRequest, Signer } from "@left-curve/types";

export type CancelConditionalOrderParameters = {
  sender: Address;
  request: PerpsCancelConditionalOrderRequest;
};

export type CancelConditionalOrderReturnType = SignAndBroadcastTxReturnType;

export async function cancelConditionalOrder(
  client: Client<Signer>,
  parameters: CancelConditionalOrderParameters,
): CancelConditionalOrderReturnType {
  const { sender, request } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    trade: {
      cancelConditionalOrder: request,
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
