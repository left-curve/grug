import type { Address, Client, Funds, Json, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type ExecuteParameters = {
  sender: Address;
  execute: ExecuteMsg | ExecuteMsg[];
  gasLimit?: number;
};

export type ExecuteMsg = {
  contract: Address;
  msg: Json;
  funds?: Funds;
};

export type ExecuteReturnType = SignAndBroadcastTxReturnType;

export async function execute(
  client: Client<Signer>,
  parameters: ExecuteParameters,
): ExecuteReturnType {
  const { sender, gasLimit, execute } = parameters;

  const executeMsgs = Array.isArray(execute) ? execute : [execute];

  const messages = executeMsgs.map(({ contract, msg, funds = {} }) => ({
    execute: {
      contract,
      msg,
      funds,
    },
  }));

  return await signAndBroadcastTx(client, { sender, messages, gasLimit });
}
