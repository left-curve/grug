import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { type ExecuteReturnType, execute } from "#actions/app/mutations/execute.js";

import type { Address, Client, Funds, Signer, TxParameters } from "@left-curve/types";

export type RegisterAccountParameters = {
  sender: Address;
  funds?: Funds;
};

export type RegisterAccountReturnType = ExecuteReturnType;

export async function registerAccount(
  client: Client<Signer>,
  parameters: RegisterAccountParameters,
  txParameters: TxParameters = {},
): RegisterAccountReturnType {
  const { sender, funds = {} } = parameters;
  const { gasLimit } = txParameters;
  const msg = { registerAccount: {} };

  const { addresses } = await getAppConfig(client);

  return await execute(client, {
    execute: {
      contract: addresses.accountFactory,
      msg,
      funds,
    },
    sender,
    gasLimit,
  });
}
