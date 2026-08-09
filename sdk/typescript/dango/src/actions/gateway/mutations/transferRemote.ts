import { getAppConfig } from "#actions/app/queries/getAppConfig.js";
import { type ExecuteReturnType, execute } from "#actions/app/mutations/execute.js";

import type { Addr32, Address, Client, Coins, Remote, Signer } from "@left-curve/types";

export type TransferRemoteParameters = {
  remote: Remote;
  recipient: Addr32;
  sender: Address;
  funds: Coins;
};

export type TransferRemoteReturnType = ExecuteReturnType;

export async function transferRemote(
  client: Client<Signer>,
  parameters: TransferRemoteParameters,
): TransferRemoteReturnType {
  const { remote, recipient, sender, funds } = parameters;

  const { addresses } = await getAppConfig(client);

  const msg = {
    transferRemote: {
      remote,
      recipient,
    },
  };

  return await execute(client, {
    execute: {
      contract: addresses.gateway,
      msg,
      funds,
    },
    sender,
  });
}
