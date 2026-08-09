import type { Address, Client, Hex, Json, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type MigrateParameters = {
  sender: Address;
  contract: Address;
  newCodeHash: Hex;
  msg: Json;
};

export type MigrateReturnType = Promise<SignAndBroadcastTxReturnType>;

export async function migrate(
  client: Client<Signer>,
  parameters: MigrateParameters,
): MigrateReturnType {
  const { sender, contract, msg, newCodeHash } = parameters;
  const migrateMsg = {
    migrate: {
      contract,
      msg,
      newCodeHash,
    },
  };

  return await signAndBroadcastTx(client, { sender, messages: [migrateMsg] });
}
