import type { Address, Client, Coins, Signer } from "@left-curve/types";
import { type SignAndBroadcastTxReturnType, signAndBroadcastTx } from "./signAndBroadcastTx.js";

export type TransferParameters = {
  sender: Address;
  transfer: Record<Address, Coins>;
};

export type TransferReturnType = SignAndBroadcastTxReturnType;

export async function transfer(
  client: Client<Signer>,
  parameters: TransferParameters,
): TransferReturnType {
  const { sender, transfer } = parameters;
  const transferMsg = { transfer };

  return await signAndBroadcastTx(client, { sender, messages: [transferMsg] });
}
