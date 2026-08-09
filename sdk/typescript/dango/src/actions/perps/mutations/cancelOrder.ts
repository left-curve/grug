import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { execute } from "#actions/app/mutations/execute.js";

import type { Address } from "@left-curve/types";
import type { SignAndBroadcastTxReturnType } from "#actions/app/mutations/signAndBroadcastTx.js";
import type { Client, PerpsCancelOrderRequest, Signer } from "@left-curve/types";

export type CancelPerpsOrderParameters = {
  sender: Address;
  request: PerpsCancelOrderRequest;
};

export type CancelPerpsOrderReturnType = SignAndBroadcastTxReturnType;

export async function cancelPerpsOrder(
  client: Client<Signer>,
  parameters: CancelPerpsOrderParameters,
): CancelPerpsOrderReturnType {
  const { sender, request } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    trade: {
      cancelOrder: request,
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
